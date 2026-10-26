use std::collections::HashSet;
use std::str;
use std::sync::Arc;

use anyhow::anyhow;
use axum::extract::Query;
use axum::{async_trait, extract::State, Json};
use ethos_core::storage::config::Project;
use ethos_core::storage::{
    ArtifactBuildConfig, ArtifactConfig, ArtifactKind, ArtifactList, Platform,
};
use parking_lot::RwLock;
use serde::Deserialize;
use tracing::{debug, info, instrument, warn};

use crate::engine;
use crate::engine::EngineProvider;
use crate::repo::operations::branch::{
    is_active, rebase_dir_exists, recorded_rebase_kind, resolve_git_dir,
};
use crate::repo::operations::gh::submit::{is_branch_mode, is_quicksubmit_branch};
use crate::state::AppState;
use ethos_core::blocked_files::BlockedFileMatcher;
use ethos_core::clients::aws::ensure_aws_client;
use ethos_core::clients::git;
use ethos_core::storage::ArtifactStorage;
use ethos_core::types::config::AppConfigRef;
use ethos_core::types::config::RepoConfigRef;
use ethos_core::types::errors::CoreError;
use ethos_core::types::locks::Lock;
use ethos_core::types::repo::SubmitStatus;
use ethos_core::types::repo::{File, RepoStatus};
use ethos_core::types::repo::{FileList, FileState};
use ethos_core::worker::{Task, TaskSequence};
use ethos_core::AWSClient;

pub type RepoStatusRef = Arc<RwLock<RepoStatus>>;

struct UpstreamChanges {
    /// Blocking: becomes `RepoStatus::modified_upstream`.
    modified_upstream: Vec<String>,
    /// Non-blocking: becomes `RepoStatus::will_conflict_on_rebase`.
    will_conflict_on_rebase: Vec<String>,
    /// Non-blocking: becomes `RepoStatus::will_conflict_on_sync`.
    will_conflict_on_sync: Vec<String>,
}

/// Locally modified tracked files that also changed on `origin/<branch>`, excluding files that
/// already block. Sync's autostash merges them and leaves conflict markers where lines overlap.
fn will_conflict_on_sync(
    incoming: &[String],
    local_modified: &FileList,
    blocking: &[String],
) -> Vec<String> {
    incoming
        .iter()
        .filter(|file| local_modified.contains(file))
        .filter(|file| !blocking.contains(file))
        .cloned()
        .collect()
}

/*
git status --porcelain=v2 --branch --ignored

# branch.oid 06a3b67b10cf37ed685fbb8df4a0dd31f0c7fb05
# branch.head ar/exe-party
# branch.upstream origin/ar/exe-party
# branch.ab +0 -0
1 M. N... 100644 100644 100644 232ff8b3124704a5cca2bfab1c065cef6e30418f 75adf399778e40238255a7d19b4f42918e63a40b friendshipper-svc/src/repo/operations.rs

First column:
    # = branch info
    1 = tracked file
    2 = renamed file
    u = unmerged file
    ? = untracked file
    ! = ignored file

If line is a file, second column:
    M = modified
    A = added
    D = deleted
    R = renamed
    C = copied

    . before means unstaged, . after means staged.

Third column is always N... unless it's a submodule.

From there: octal file mode at HEAD, octal file mode in index, octal file mode in worktree, object name in HEAD, object name in index, file path
*/

#[derive(Clone, Debug)]
pub struct StatusOp<T>
where
    T: EngineProvider,
{
    pub git_client: git::Git,
    pub github_username: String,
    pub repo_status: RepoStatusRef,
    pub app_config: AppConfigRef,
    pub repo_config: RepoConfigRef,
    pub engine: T,
    pub aws_client: Option<AWSClient>,
    pub storage: Option<ArtifactStorage>,
    pub allow_offline_communication: bool,
    pub skip_display_names: bool,
    pub skip_engine_update: bool,
    /// Fetch before computing status. Set only by user-initiated refresh actions, and only acted on
    /// while background git operations are disabled.
    pub fetch_first: bool,
}

#[async_trait]
impl<T> Task for StatusOp<T>
where
    T: EngineProvider,
{
    #[instrument(name = "StatusOp::execute", skip(self))]
    async fn execute(&self) -> Result<(), CoreError> {
        let _ = self.run().await?;

        Ok(())
    }

    fn get_name(&self) -> String {
        String::from("RepoStatus")
    }
}

impl<T> StatusOp<T>
where
    T: EngineProvider,
{
    #[instrument(name = "StatusOp::run", err, skip_all)]
    pub(crate) async fn run(&self) -> Result<RepoStatus, CoreError> {
        // A user-initiated refresh fetches first, but only while the background fetch loop is
        // disabled — otherwise the loop has already done it seconds ago and this would just add a
        // network round-trip to a button that is currently instant.
        //
        // Upstream-conflict detection (`get_modified_upstream` below) diffs against the *local*
        // remote-tracking refs, so with the loop off those refs go stale and conflicts stop being
        // flagged. This is what makes the guard recoverable on demand.
        //
        // Read the bool and drop the guard before awaiting: `app_config` is a `parking_lot` lock
        // whose guard is `!Send`.
        let background_disabled = self.app_config.read().disable_background_git_operations;
        if self.fetch_first && background_disabled {
            self.git_client
                .fetch(git::ShouldPrune::Yes, git::Opts::new_without_logs())
                .await?;
        }

        let locks_future = self.git_client.verify_locks();
        let status_future = self.git_client.status(vec![]);

        let (locks, status_output) = futures::join!(locks_future, status_future);
        let locks = locks?;
        let status_output = status_output?;

        let status_lines = status_output.lines().collect::<Vec<_>>();

        let mut status = RepoStatus::new();
        let pull_dlls = self.app_config.read().pull_dlls;

        // make sure we always copy over the last push/sync status
        {
            let current_status = self.repo_status.read();

            // Preserve the Unix epoch marker for first sync detection if it's still set
            if current_status.last_updated == chrono::DateTime::UNIX_EPOCH {
                status.last_updated = chrono::DateTime::UNIX_EPOCH;
            }

            // because dll checking can be skipped, default to current values
            status.origin_has_new_dlls = current_status.origin_has_new_dlls;
            status.pull_dlls = pull_dlls;
            status
                .dll_commit_local
                .clone_from(&current_status.dll_commit_local);
            status
                .dll_commit_remote
                .clone_from(&current_status.dll_commit_remote);
        }

        for line in status_lines {
            status.parse_file_line(line);
        }

        if status.detached_head {
            warn!(
                "Detached head state detected for {:?}",
                self.git_client.repo_path
            );
        }

        // Status never fails over the guided rebase state; an unknown one is treated as a rebase
        // below, and the guard refuses other operations.
        let repo_path = &self.git_client.repo_path;
        let mut rebasing = rebase_dir_exists(&resolve_git_dir(repo_path));
        match recorded_rebase_kind(&self.git_client, repo_path).await {
            Ok(kind) => status.guided_rebase_active = kind.is_some_and(is_active),
            Err(e) => {
                warn!("Could not determine the guided rebase state: {e:#}");
                rebasing = true;
            }
        }
        rebasing |= status.guided_rebase_active;

        // if we're not on the main branch, we need to get the ahead/behind counts
        let target_branch = self.app_config.read().target_branch.clone();
        let remote_target_branch = format!("origin/{target_branch}");
        if status.branch != target_branch {
            let (commits_ahead, commits_behind) = self
                .git_client
                .get_ahead_behind(&remote_target_branch, &status.branch)
                .await?;
            status.commits_ahead_of_trunk = commits_ahead;
            status.commits_behind_trunk = commits_behind;
        } else {
            status.commits_ahead_of_trunk = status.commits_ahead;
            status.commits_behind_trunk = status.commits_behind;
        }

        status.commit_head = self
            .git_client
            .head_commit(git::CommitFormat::Long, git::CommitHead::Local)
            .await?;

        info!(
                %status.branch,
                %status.remote_branch,
                %status.commit_head,
                %status.detached_head,
                %status.commits_ahead_of_trunk,
                ?status.commits_behind_trunk,
                "local state"
        );

        {
            status.lock_user.clone_from(&self.github_username);
            status.locks_ours = locks.ours;
            status.locks_theirs = locks.theirs;

            info!(
                %status.lock_user,
                ?status.locks_ours,
                "locks"
            );
        }

        // get display names if available
        if !self.skip_display_names {
            // combine all the requested names into a single batch - this will avoid multiple potentially slow requests
            let mut all_filenames: Vec<String> = vec![];
            for file in status.modified_files.0.iter() {
                all_filenames.push(file.path.clone());
            }
            for file in status.untracked_files.0.iter() {
                all_filenames.push(file.path.clone());
            }
            for lock in status.locks_ours.iter() {
                all_filenames.push(lock.path.clone());
            }
            for lock in status.locks_theirs.iter() {
                all_filenames.push(lock.path.clone());
            }

            let communication = if self.allow_offline_communication {
                engine::CommunicationType::OfflineFallback
            } else {
                engine::CommunicationType::IpcOnly
            };

            let engine_path = self
                .app_config
                .read()
                .load_engine_path_from_repo(&self.repo_config.read())
                .unwrap_or_default();

            let mut display_names: Vec<String> = self
                .engine
                .get_asset_display_names(communication, &engine_path, &all_filenames)
                .await;

            assert_eq!(all_filenames.len(), display_names.len());

            let (names_modified, remaining_names) =
                display_names.split_at_mut(status.modified_files.0.len());
            let (names_untracked, remaining_names) =
                remaining_names.split_at_mut(status.untracked_files.0.len());
            let (names_locks_ours, names_locks_theirs) =
                remaining_names.split_at_mut(status.locks_ours.len());

            assert_eq!(names_modified.len(), status.modified_files.0.len());
            assert_eq!(names_untracked.len(), status.untracked_files.0.len());
            assert_eq!(names_locks_ours.len(), status.locks_ours.len());
            assert_eq!(names_locks_theirs.len(), status.locks_theirs.len());

            let update_files = |files: &mut Vec<File>, names: &mut [String]| {
                for (file, name) in files.iter_mut().zip(names.iter_mut()) {
                    file.display_name.clone_from(name);
                }
            };

            update_files(&mut status.modified_files.0, names_modified);
            update_files(&mut status.untracked_files.0, names_untracked);

            let update_locks = |locks: &mut Vec<Lock>, names: &mut [String]| {
                for (lock, name) in locks.iter_mut().zip(names.iter_mut()) {
                    lock.display_name = Some(name.to_string());
                }
            };

            update_locks(&mut status.locks_ours, names_locks_ours);
            update_locks(&mut status.locks_theirs, names_locks_theirs);
        }

        {
            status.commit_head_origin = self
                .git_client
                .head_commit(git::CommitFormat::Long, git::CommitHead::Remote)
                .await?;

            let remote_url = match self
                .git_client
                .run_and_collect_output(
                    &["config", "--get", "remote.origin.url"],
                    git::Opts::default(),
                )
                .await
            {
                Ok(url) => url,
                Err(_) => {
                    return Err(CoreError::Internal(anyhow!(
                        "Failed to get remote URL for repo"
                    )));
                }
            };

            // https://github.com/owner/repository.git
            let parts = remote_url.split('/');
            if parts.count() < 4 {
                status.repo_owner = "".to_string();
                status.repo_name = "".to_string();
            } else {
                status.repo_owner = remote_url.split('/').nth(3).unwrap().to_string();
                status.repo_name = remote_url
                    .split('/')
                    .nth(4)
                    .unwrap()
                    .trim_end()
                    .trim_end_matches(".git")
                    .to_string();
            }

            info!(
                status.remote_url = ?remote_url,
                ?status.repo_owner,
                ?status.repo_name,
                ?status.commit_head_origin,
                "remote info");

            // Since we aren't likely to have much contention on this lock, it's likely
            // cheaper to write than to read and then sometimes write.
            let new_selected_artifact_project = format!(
                "{}-{}",
                status.repo_owner.to_lowercase(),
                status.repo_name.to_lowercase()
            );
            {
                let mut app_config = self.app_config.write();
                app_config.selected_artifact_project = Some(new_selected_artifact_project);
            }
        }

        if let Some(aws_client) = &self.aws_client {
            let storage = self
                .storage
                .as_ref()
                .expect("ArtifactStorage was None, but AWSClient was valid");

            // if there's an aws client, we should always have a valid artifact storage
            self.find_dll_archive_url_info(&mut status, aws_client, storage)
                .await?;
            status.pull_dlls = pull_dlls;

            info!(
                %status.dll_commit_local,
                %status.dll_commit_remote,
                %status.origin_has_new_dlls,
                "editor binary archives");
        }

        // While rebasing, HEAD is built on the new trunk, so diffing it against the old
        // upstream would report main-side files as upstream changes.
        if rebasing {
            info!("Rebase in progress, skipping upstream changes");
        } else {
            let upstream = self.get_modified_upstream(&status).await?;
            status.modified_upstream = upstream.modified_upstream;
            status.will_conflict_on_rebase = upstream.will_conflict_on_rebase;
            status.will_conflict_on_sync = upstream.will_conflict_on_sync;

            // remove duplicates
            status.modified_upstream.sort_unstable();
            status.modified_upstream.dedup();
            status.will_conflict_on_rebase.sort_unstable();
            status.will_conflict_on_rebase.dedup();
            status.will_conflict_on_sync.sort_unstable();
            status.will_conflict_on_sync.dedup();

            status.conflicts = self.get_upstream_conflicts(&status.modified_files, &status);
            if !status.conflicts.is_empty() {
                status.conflict_upstream = true;
            }

            info!(?status.modified_upstream,
                ?status.conflicts,
                ?status.will_conflict_on_rebase,
                ?status.will_conflict_on_sync,
                "upstream");
        }

        {
            // Compiled once, outside the closure below, since that closure is
            // invoked twice (once for untracked files, once for modified
            // files). Warnings and invalid-pattern diagnostics are handled
            // once at config load (`initialize_repo_config`) and are
            // deliberately discarded here — this closure runs on every
            // status refresh (constantly, to keep the UI live) and must stay
            // silent, or logging would spam continuously.
            let blocked_matcher = {
                let globs = self
                    .repo_config
                    .read()
                    .blocked_globs_for_branch(&target_branch)
                    .to_vec();
                BlockedFileMatcher::compile(&globs).matcher
            };

            let update_files_submit_status = |files: &mut [File]| {
                for file in files.iter_mut() {
                    // `!blocked_matcher.is_empty()` is checked first because
                    // `GlobSet::is_match` (behind `is_blocked`) builds a
                    // `Candidate` — path normalization plus an allocation —
                    // before its own internal empty-set short-circuit. Most
                    // target branches configure no blocked globs at all, and
                    // this closure runs on every file on every status
                    // refresh, so skipping straight past that allocation
                    // when there is nothing to match against is worth doing
                    // explicitly rather than relying on GlobSet to notice.
                    if !blocked_matcher.is_empty()
                        && file.state != FileState::Deleted
                        && blocked_matcher.is_blocked(&file.path)
                    {
                        file.submit_status = SubmitStatus::Blocked;
                    } else if file.state == FileState::Unmerged {
                        file.submit_status = SubmitStatus::Unmerged;
                    } else if status.conflicts.iter().any(|x| x == &file.path) {
                        file.submit_status = SubmitStatus::Conflicted;
                    } else if self.engine.is_lockable_file(&file.path) {
                        if let Some(lock) = status.locks_theirs.iter().find(|x| x.path == file.path)
                        {
                            file.submit_status = SubmitStatus::CheckedOutByOtherUser;
                            file.locked_by = lock.owner.clone().map(|x| x.name).unwrap_or_default();
                        } else if status.locks_ours.iter().any(|x| x.path == file.path) {
                            file.locked_by.clone_from(&self.github_username);
                        } else {
                            file.submit_status = SubmitStatus::CheckoutRequired;
                        }
                    }

                    if let Some(url) = self.engine.get_url_for_path(&file.path) {
                        file.url = Some(url);
                    }
                }
            };

            update_files_submit_status(&mut status.untracked_files.0);
            update_files_submit_status(&mut status.modified_files.0);

            info!(
                status.untracked_files = ?status.untracked_files.0,
                status.modified_files = ?status.modified_files.0,
                "changes"
            );
        }

        if !self.skip_engine_update {
            self.engine.send_status_update(&status).await;
        }

        let mut repo_status = self.repo_status.write();
        *repo_status = status.clone();

        Ok(status)
    }

    async fn get_modified_upstream(
        &self,
        status: &RepoStatus,
    ) -> Result<UpstreamChanges, anyhow::Error> {
        let branch_mode = {
            let rc = self.repo_config.read();
            is_branch_mode(&rc, &status.branch)
        };
        if branch_mode {
            return Ok(self.get_branch_mode_upstream(status).await);
        }

        let branch = status.branch.as_str();
        let upstream_exists = self.upstream_branch_exists(branch).await;

        let commit_range = format!("HEAD...origin/{branch}");

        // check for files modified on the upstream branch
        let mut modified_upstream: Vec<String> = match upstream_exists {
            true => self.git_client.diff_filenames(&commit_range).await?,
            false => vec![],
        };

        // check for files modified on the target upstream branches
        let mut shared_files = vec![];
        let target_branches = self.repo_config.read().target_branches.clone();
        for target_branch in target_branches {
            let commit_range = format!("HEAD...origin/{}", target_branch.name);
            let modified_in_target_branch: Vec<String> =
                self.git_client.diff_filenames(&commit_range).await?;
            modified_upstream.extend(modified_in_target_branch);

            // find commits between the current target branch and each other target branch that share the same
            // message, then filter out files from those commits
            shared_files.extend(
                self.git_client
                    .get_shared_changed_files(branch, &target_branch.name)
                    .await?,
            );
        }

        // if the user is on a quicksubmit branch or a non-trunk branch, any conflicts with files modified by their own
        // user are most likely due to files they've already submitted and merged into trunk
        // from an earlier quicksubmit, so filter those out to avoid blocking them
        //
        // In the future, this could use some more complex hardening, because this logic isn't
        // actually doing a diff, it's assuming that a committed modification in both places
        // is the same modification.
        let target_branch = self.app_config.read().target_branch.clone();
        let user_modified_upstream: Vec<String> = if branch != target_branch {
            let commit_range = format!("HEAD...origin/{}", target_branch);
            let args = &[
                "log",
                "--pretty=",
                &format!("--committer={}", self.github_username),
                "--name-only",
                &commit_range,
            ];
            let mut files = self
                .git_client
                .run_and_collect_output_into_lines(args, git::Opts::default())
                .await?;

            files.dedup();
            files
        } else {
            vec![]
        };

        let filtered = modified_upstream
            .iter()
            .filter(|file| self.engine.is_lockable_file(file))
            .filter(|file| !user_modified_upstream.iter().any(|x| x == *file))
            .filter(|file| !shared_files.iter().any(|x| x == *file))
            .cloned()
            .collect::<Vec<_>>();

        Ok(UpstreamChanges {
            modified_upstream: filtered,
            will_conflict_on_rebase: vec![],
            will_conflict_on_sync: vec![],
        })
    }

    /// Only lockable `origin/<branch>` changes block; other local overlaps with `origin/<branch>`
    /// and trunk-side overlaps are reported as warnings. Git failures degrade to empty lists so
    /// status never fails on a missing or gone upstream.
    async fn get_branch_mode_upstream(&self, status: &RepoStatus) -> UpstreamChanges {
        let branch = status.branch.as_str();

        let (blocking, will_conflict_on_sync) = if self.upstream_branch_exists(branch).await {
            let commit_range = format!("HEAD...refs/remotes/origin/{branch}");
            let changed = self
                .git_client
                .diff_filenames(&commit_range)
                .await
                .unwrap_or_else(|e| {
                    warn!("Failed to diff {commit_range}: {e}");
                    vec![]
                });

            // rebased copies of our own commits share a message on both sides
            let shared = self
                .git_client
                .get_shared_changed_files(branch, branch)
                .await
                .unwrap_or_else(|e| {
                    warn!("Failed to find shared commits with origin/{branch}: {e}");
                    vec![]
                });

            let incoming: Vec<String> = changed
                .into_iter()
                .filter(|file| !shared.contains(file))
                .collect();
            let blocking: Vec<String> = incoming
                .iter()
                .filter(|file| self.engine.is_lockable_file(file))
                .cloned()
                .collect();
            let will_conflict_on_sync =
                will_conflict_on_sync(&incoming, &status.modified_files, &blocking);
            (blocking, will_conflict_on_sync)
        } else {
            (vec![], vec![])
        };

        let will_conflict_on_rebase = self.get_will_conflict_on_rebase(status, &blocking).await;

        UpstreamChanges {
            modified_upstream: blocking,
            will_conflict_on_rebase,
            will_conflict_on_sync,
        }
    }

    /// Lockable files changed on both `origin/<trunk>` and this branch (committed or local)
    /// since their merge-base, excluding anything already blocking.
    async fn get_will_conflict_on_rebase(
        &self,
        status: &RepoStatus,
        blocking: &[String],
    ) -> Vec<String> {
        let trunk = self.repo_config.read().trunk_branch.clone();
        if trunk.is_empty() {
            debug!("No trunk branch configured, skipping rebase conflict check");
            return vec![];
        }
        if !self
            .git_client
            .has_local_remote_tracking_branch(&trunk)
            .await
            .unwrap_or(false)
        {
            warn!("origin/{trunk} has not been fetched, skipping rebase conflict check");
            return vec![];
        }

        let main_range = format!("HEAD...refs/remotes/origin/{trunk}");
        let main_side = match self.git_client.diff_filenames(&main_range).await {
            Ok(files) => files,
            Err(e) => {
                warn!("Failed to diff {main_range}: {e}");
                return vec![];
            }
        };

        let branch_range = format!("refs/remotes/origin/{trunk}...HEAD");
        let branch_committed = match self.git_client.diff_filenames(&branch_range).await {
            Ok(files) => files,
            Err(e) => {
                warn!("Failed to diff {branch_range}: {e}");
                return vec![];
            }
        };

        let branch_side: HashSet<&str> = branch_committed
            .iter()
            .map(String::as_str)
            .chain(status.modified_files.0.iter().map(|f| f.path.as_str()))
            .chain(status.untracked_files.0.iter().map(|f| f.path.as_str()))
            .collect();

        main_side
            .into_iter()
            .filter(|file| self.engine.is_lockable_file(file))
            .filter(|file| branch_side.contains(file.as_str()))
            .filter(|file| !blocking.contains(file))
            .collect()
    }

    /// True only if `origin/<branch>` exists on the remote and has been fetched.
    async fn upstream_branch_exists(&self, branch: &str) -> bool {
        if branch.is_empty() {
            return false;
        }
        let on_remote = self
            .git_client
            .has_remote_branch_exact(branch)
            .await
            .unwrap_or(false);
        on_remote
            && self
                .git_client
                .has_local_remote_tracking_branch(branch)
                .await
                .unwrap_or(false)
    }

    fn get_upstream_conflicts(
        &self,
        local_modified: &FileList,
        repo_status: &RepoStatus,
    ) -> Vec<String> {
        repo_status
            .modified_upstream
            .iter()
            .filter(|file| {
                local_modified.contains(file)
                    || repo_status.modified_files.contains(file)
                    || repo_status.untracked_files.contains(file)
            })
            .cloned()
            .collect::<Vec<_>>()
    }

    #[instrument(skip_all)]
    async fn find_dll_archive_url_info(
        &self,
        status: &mut RepoStatus,
        aws_client: &AWSClient,
        storage: &ArtifactStorage,
    ) -> Result<(), CoreError> {
        debug!("parsing remote URL for repo id");

        let repo_id = if status.repo_owner.is_empty() || status.repo_name.is_empty() {
            warn!("No URL configured for this repo - this is likely a test. Using default url id 'friendshipper'");
            "friendshipper".to_string()
        } else {
            let repo_id = format!("{}-{}", status.repo_owner, status.repo_name);
            let repo_id = repo_id.replace('/', "-");
            repo_id.to_lowercase()
        };
        debug!("Parsed the repo id as {:?}", repo_id);

        debug!("fetching s3 editor entries list");

        aws_client.check_expiration().await?;

        let project = if status.repo_owner.is_empty() || status.repo_name.is_empty() {
            let app_config = self.app_config.read();
            let (owner, repo) = match app_config.selected_artifact_project {
                Some(ref project) => {
                    let (owner, repo) = project
                        .split_once('-')
                        .ok_or(CoreError::Input(anyhow!("Invalid project")))?;
                    (owner, repo)
                }
                None => return Err(CoreError::Input(anyhow!("No project selected"))),
            };

            Project::new(owner, repo)
        } else {
            Project::new(&status.repo_owner, &status.repo_name)
        };

        let artifact_config = ArtifactConfig::new(
            project,
            ArtifactKind::Editor,
            ArtifactBuildConfig::Development,
            Platform::Win64,
        );

        let mut list = storage.artifact_list(artifact_config).await;

        let builds = list.sort_by_last_modified();

        debug!("s3 entries list: {:?}", builds);

        let dll_branch = match is_quicksubmit_branch(&status.branch) {
            true => self.repo_config.read().trunk_branch.clone(),
            false => status.branch.clone(),
        };
        let dll_branch_remote = format!("origin/{dll_branch}");

        let git_opts = git::Opts::new_without_logs();
        let local_commit_shas: String = self
            .git_client
            .run_and_collect_output(&["log", "--format=\"%H\"", "-1000", &dll_branch], git_opts)
            .await
            .unwrap_or(String::new());
        let remote_commit_shas: String = self
            .git_client
            .run_and_collect_output(
                &["log", "--format=\"%H\"", "-1000", &dll_branch_remote],
                git_opts,
            )
            .await
            .unwrap_or(String::new());

        let dll_commit_local = find_dll_commit(builds, &local_commit_shas, "local");
        let dll_commit_remote = find_dll_commit(builds, &remote_commit_shas, "remote");

        status.origin_has_new_dlls = dll_commit_local != dll_commit_remote;
        status.dll_commit_local = dll_commit_local;
        status.dll_commit_remote = dll_commit_remote;

        Ok(())
    }

    #[instrument(skip(self))]
    pub async fn update_filelist_display_names(
        &self,
        communication: engine::CommunicationType,
        files: &mut [File],
    ) {
        let filenames: Vec<String> = files.iter().map(|v| v.path.clone()).collect();

        let engine_path = self
            .app_config
            .read()
            .load_engine_path_from_repo(&self.repo_config.read())
            .unwrap_or_default();

        let asset_names: Vec<String> = self
            .engine
            .get_asset_display_names(communication, &engine_path, &filenames)
            .await;

        assert_eq!(files.len(), asset_names.len());

        // the suggested replacement to use an enumerator with an index and value is *more* complicated than
        // this simple parallel array code...
        #[allow(clippy::needless_range_loop)]
        for i in 0..files.len() {
            files[i].display_name.clone_from(&asset_names[i]);

            debug!(
                "updating file {} to have display name '{}'",
                files[i].path, asset_names[i]
            );
        }
    }
}

#[instrument(skip(files))]
fn find_dll_commit(files: &ArtifactList, long_shas: &str, context: &str) -> String {
    for sha in long_shas.lines() {
        let sha = sha.replace('"', "");
        debug!("checking sha {} against s3 entries...", sha);
        if files.iter().any(|entry| entry.key.0.contains(&sha)) {
            return sha.to_string();
        }
    }

    warn!(
        "Failed to find editor binaries matching any commits for context {}",
        context
    );
    String::new()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusParams {
    #[serde(default)]
    pub skip_fetch: bool,
    #[serde(default)]
    pub skip_dll_check: bool,
    #[serde(default)]
    pub allow_offline_communication: bool,
    #[serde(default)]
    pub skip_display_names: bool,
    #[serde(default)]
    pub skip_engine_update: bool,
    #[serde(default)]
    pub fetch_first: bool,
}

pub async fn status_handler<T>(
    State(state): State<AppState<T>>,
    params: Query<StatusParams>,
) -> Result<Json<RepoStatus>, CoreError>
where
    T: EngineProvider,
{
    let aws_client: Option<AWSClient> = if params.skip_dll_check {
        None
    } else {
        let client = ensure_aws_client(state.aws_client.read().await.clone())?;

        // Make sure AWS credentials still valid
        client.check_expiration().await?;
        Some(client)
    };

    let storage: Option<ArtifactStorage> = if params.skip_dll_check {
        None
    } else {
        match state.storage.read().clone() {
            Some(storage) => Some(storage),
            None => {
                return Err(CoreError::Internal(anyhow!(
                    "No storage configured for this app. AWS may still be initializing."
                )))
            }
        }
    };

    let status_op = StatusOp {
        repo_status: state.repo_status.clone(),
        app_config: state.app_config.clone(),
        repo_config: state.repo_config.clone(),
        engine: state.engine.clone(),
        git_client: state.git(),
        github_username: state.github_username(),
        aws_client,
        storage,
        allow_offline_communication: params.allow_offline_communication,
        skip_display_names: params.skip_display_names,
        skip_engine_update: params.skip_engine_update,
        fetch_first: params.fetch_first,
    };

    // make sure this status operation is executed behind any queued operations
    let (tx, rx) = tokio::sync::oneshot::channel::<Option<CoreError>>();

    let mut sequence = TaskSequence::new().with_completion_tx(tx);
    sequence.push(Box::new(status_op));

    state.operation_tx.send(sequence).await?;

    match rx.await {
        Ok(e) => {
            if let Some(e) = e {
                return Err(e);
            }

            let status = state.repo_status.read();

            Ok(Json(status.clone()))
        }
        Err(_) => Err(CoreError::Internal(anyhow!(
            "Error executing status operation"
        ))),
    }
}

#[cfg(test)]
mod tests {

    use std::fs;
    use std::sync::mpsc;

    use ethos_core::storage::ArtifactEntry;
    use ethos_core::types::config::{AppConfig, RepoConfig, TargetBranchConfig};

    use super::*;
    use crate::repo::operations::branch::rebase_fixture::{guided_record, start_raw_rebase};
    use crate::repo::operations::branch::{json_path, write_rebase_file};

    /// Initialize a local git repo with a real (also-local) `origin` remote,
    /// carrying both `main` and `content-main` branches — the two branches
    /// `blocked_globs_for_branch` tests below key off. `StatusOp::run()`
    /// unconditionally diffs against `origin/<name>` for every configured
    /// `target_branches` entry (see `get_modified_upstream`), so every
    /// branch name a test's `RepoConfig` mentions must exist on this remote
    /// or `run()` fails outright — this fixture exists to make that always
    /// true regardless of which of the two branches an individual test uses.
    async fn make_test_repo_with_remote() -> (tempfile::TempDir, tempfile::TempDir, git::Git) {
        let local_dir = tempfile::tempdir().expect("create local tempdir");
        let remote_dir = tempfile::tempdir().expect("create remote tempdir");
        let (tx, _rx) = mpsc::channel::<String>();
        let local_git = git::Git::new(local_dir.path().to_path_buf(), tx.clone());
        let remote_git = git::Git::new(remote_dir.path().to_path_buf(), tx);

        remote_git
            .run(&["init", "-q"], git::Opts::default())
            .await
            .expect("git init remote");
        remote_git
            .run(
                &["config", "user.email", "test@example.com"],
                git::Opts::default(),
            )
            .await
            .expect("set remote user.email");
        remote_git
            .run(&["config", "user.name", "Test"], git::Opts::default())
            .await
            .expect("set remote user.name");
        // Keep the remote's checked-out HEAD off of `main`/`content-main` so
        // pushing either from local doesn't trip git's "refusing to update
        // checked out branch" guard.
        remote_git
            .run(&["checkout", "-q", "-b", "temp"], git::Opts::default())
            .await
            .expect("create remote temp branch");

        local_git
            .run(&["init", "-q"], git::Opts::default())
            .await
            .expect("git init local");
        local_git
            .run(
                &["config", "user.email", "test@example.com"],
                git::Opts::default(),
            )
            .await
            .expect("set local user.email");
        local_git
            .run(&["config", "user.name", "Test"], git::Opts::default())
            .await
            .expect("set local user.name");
        local_git
            .run(
                &[
                    "remote",
                    "add",
                    "origin",
                    remote_dir.path().to_str().expect("utf8 remote path"),
                ],
                git::Opts::default(),
            )
            .await
            .expect("add origin remote");
        local_git
            .run(&["checkout", "-q", "-b", "main"], git::Opts::default())
            .await
            .expect("checkout main");

        fs::write(local_dir.path().join("seed.txt"), "seed\n").expect("write seed");
        local_git
            .run(&["add", "seed.txt"], git::Opts::default())
            .await
            .expect("git add seed");
        local_git
            .run(&["commit", "-qm", "seed"], git::Opts::default())
            .await
            .expect("initial commit");
        local_git
            .run(
                &["push", "-q", "-u", "origin", "main"],
                git::Opts::default(),
            )
            .await
            .expect("push main");

        local_git
            .run(
                &["checkout", "-q", "-b", "content-main"],
                git::Opts::default(),
            )
            .await
            .expect("checkout content-main");
        local_git
            .run(
                &["push", "-q", "-u", "origin", "content-main"],
                git::Opts::default(),
            )
            .await
            .expect("push content-main");
        local_git
            .run(&["checkout", "-q", "main"], git::Opts::default())
            .await
            .expect("back to main");

        (local_dir, remote_dir, local_git)
    }

    /// Build a `StatusOp<UnrealEngineProvider>` against a real repo fixture.
    /// `skip_display_names`/`skip_engine_update` are both `true` so no
    /// network or IPC calls are attempted — this is a fully hermetic,
    /// offline exercise of the exact code path production uses.
    fn build_status_op(
        git_client: git::Git,
        repo_path: &std::path::Path,
        target_branch: &str,
        target_branches: Vec<TargetBranchConfig>,
    ) -> (StatusOp<crate::engine::UnrealEngineProvider>, RepoStatusRef) {
        let mut app_config = AppConfig::new(crate::APP_NAME);
        app_config.repo_path = repo_path.to_str().expect("utf8 repo path").to_string();
        app_config.target_branch = target_branch.to_string();
        let app_config: AppConfigRef = Arc::new(RwLock::new(app_config));

        let repo_config = RepoConfig {
            target_branches,
            ..Default::default()
        };
        let repo_config: RepoConfigRef = Arc::new(RwLock::new(repo_config));

        let engine = crate::engine::UnrealEngineProvider::new_from_config(
            app_config.read().clone(),
            repo_config.read().clone(),
        );

        let repo_status: RepoStatusRef = Arc::new(RwLock::new(RepoStatus::new()));

        let status_op = StatusOp {
            git_client,
            github_username: "test_user".to_string(),
            repo_status: repo_status.clone(),
            app_config,
            repo_config,
            engine,
            aws_client: None,
            storage: None,
            allow_offline_communication: false,
            skip_display_names: true,
            skip_engine_update: true,
            fetch_first: false,
        };

        (status_op, repo_status)
    }

    /// Did a fetch happen? `git fetch` writes `.git/FETCH_HEAD`; the fixture only ever pushes, so
    /// the file's appearance is an unambiguous signal.
    fn fetch_head_path(repo: &std::path::Path) -> std::path::PathBuf {
        repo.join(".git/FETCH_HEAD")
    }

    fn clear_fetch_marker(repo: &std::path::Path) {
        let _ = std::fs::remove_file(fetch_head_path(repo));
        assert!(
            !fetch_head_path(repo).exists(),
            "precondition: FETCH_HEAD should be absent before the run"
        );
    }

    /// The full gate: a fetch happens only when the refresh was user-initiated **and** the
    /// background fetch loop is disabled.
    ///
    /// The `fetch_first && !disabled` cell is the one that keeps this feature invisible to anyone
    /// who has not opted in (resolved Q1): with the loop running it already fetched seconds ago, so
    /// refreshing must not add a network round-trip to a button that is currently instant.
    async fn run_with(fetch_first: bool, disabled: bool) -> bool {
        let (dir, _remote, git) = make_test_repo_with_remote().await;
        let repo = dir.path();

        let (mut status_op, _status) = build_status_op(git, repo, "main", vec![]);
        status_op.fetch_first = fetch_first;
        status_op
            .app_config
            .write()
            .disable_background_git_operations = disabled;

        clear_fetch_marker(repo);
        status_op.run().await.expect("status run");

        fetch_head_path(repo).exists()
    }

    /// The full gate, as one test on purpose.
    ///
    /// `Git::fetch` guards on a process-global `GIT_FETCH_LOCK`, and its contended branch waits for
    /// the in-flight fetch and then returns `Ok(())` **without fetching** (`git.rs:248-253`). Two
    /// of these cases running in parallel can therefore make each other silently skip, so they are
    /// sequenced rather than split into separate `#[tokio::test]` functions. Splitting them was
    /// tried and was flaky under mutation.
    ///
    /// The `fetch_first && !disabled` cell is the one that keeps this feature invisible to anyone
    /// who has not opted in (resolved Q1): the background loop already fetched seconds ago, so a
    /// refresh must not add a network round-trip to a button that is currently instant.
    #[tokio::test]
    async fn fetch_on_refresh_is_gated_on_both_conditions() {
        assert!(
            run_with(true, true).await,
            "user-initiated refresh with the background loop disabled must fetch"
        );
        assert!(
            !run_with(true, false).await,
            "must not fetch while the background loop is running: non-adopters keep today's behavior"
        );
        assert!(
            !run_with(false, true).await,
            "page loads, file-watcher refreshes and post-operation refreshes must not fetch"
        );
        assert!(
            !run_with(false, false).await,
            "the default path must not fetch"
        );
    }

    #[tokio::test]
    async fn blocked_path_gets_blocked_status() {
        let (dir, _remote, git) = make_test_repo_with_remote().await;
        fs::create_dir_all(dir.path().join("Content")).unwrap();
        fs::write(dir.path().join("Content/Foo.uasset"), "v1\n").unwrap();

        let target_branches = vec![
            TargetBranchConfig {
                name: "main".to_string(),
                uses_merge_queue: false,
                blocked_file_globs: vec!["**/*.uasset".to_string()],
            },
            TargetBranchConfig {
                name: "content-main".to_string(),
                uses_merge_queue: false,
                blocked_file_globs: vec![],
            },
        ];
        let (status_op, _repo_status) = build_status_op(git, dir.path(), "main", target_branches);

        let status = status_op.run().await.expect("status run");
        let file = status
            .untracked_files
            .get("Content/Foo.uasset")
            .expect("file present in untracked files");
        assert_eq!(file.submit_status, SubmitStatus::Blocked);
    }

    #[tokio::test]
    async fn deleted_blocked_path_is_not_blocked() {
        // A blocked pattern that also covers a non-lockable extension, so a
        // deleted match falls all the way through the chain to `Ok` rather
        // than being intercepted by the (unrelated, pre-existing) lockable
        // asset-checkout branch — keeping this test's assertion about
        // exactly one thing: deletions are exempt from blocking.
        let (dir, _remote, git) = make_test_repo_with_remote().await;
        let path = "Content/Removed.dat";
        fs::create_dir_all(dir.path().join("Content")).unwrap();
        fs::write(dir.path().join(path), "doomed\n").unwrap();
        git.run(&["add", path], git::Opts::default()).await.unwrap();
        git.run(&["commit", "-qm", "add doomed file"], git::Opts::default())
            .await
            .unwrap();
        fs::remove_file(dir.path().join(path)).unwrap();

        let target_branches = vec![TargetBranchConfig {
            name: "main".to_string(),
            uses_merge_queue: false,
            blocked_file_globs: vec!["**/*.uasset".to_string(), "**/*.dat".to_string()],
        }];
        let (status_op, _repo_status) = build_status_op(git, dir.path(), "main", target_branches);

        let status = status_op.run().await.expect("status run");
        let file = status
            .modified_files
            .get(path)
            .expect("deleted file present in modified files");
        assert_eq!(file.state, FileState::Deleted, "fixture sanity check");
        assert_eq!(
            file.submit_status,
            SubmitStatus::Ok,
            "a deleted file matching a blocked glob must remain submittable"
        );
    }

    #[tokio::test]
    async fn blocked_wins_over_unmerged() {
        let (dir, _remote, git) = make_test_repo_with_remote().await;
        let path = "Content/mm.uasset";
        fs::create_dir_all(dir.path().join("Content")).unwrap();
        fs::write(dir.path().join(path), "base\n").unwrap();
        git.run(&["add", path], git::Opts::default()).await.unwrap();
        git.run(&["commit", "-qm", "add base"], git::Opts::default())
            .await
            .unwrap();

        git.run(&["checkout", "-q", "-b", "feature-a"], git::Opts::default())
            .await
            .unwrap();
        fs::write(dir.path().join(path), "a-version\n").unwrap();
        git.run(&["commit", "-aqm", "feature a edit"], git::Opts::default())
            .await
            .unwrap();

        git.run(&["checkout", "-q", "main"], git::Opts::default())
            .await
            .unwrap();
        git.run(&["checkout", "-q", "-b", "feature-b"], git::Opts::default())
            .await
            .unwrap();
        fs::write(dir.path().join(path), "b-version\n").unwrap();
        git.run(&["commit", "-aqm", "feature b edit"], git::Opts::default())
            .await
            .unwrap();

        git.run(&["checkout", "-q", "feature-a"], git::Opts::default())
            .await
            .unwrap();
        // Expected to fail with a real conflict — we want the resulting
        // conflicted working tree, not a successful merge.
        let _ = git
            .run(&["merge", "feature-b", "--no-edit"], git::Opts::default())
            .await;

        let target_branches = vec![TargetBranchConfig {
            name: "main".to_string(),
            uses_merge_queue: false,
            blocked_file_globs: vec!["**/*.uasset".to_string()],
        }];
        let (status_op, _repo_status) = build_status_op(git, dir.path(), "main", target_branches);

        let status = status_op.run().await.expect("status run");
        let file = status
            .modified_files
            .get(path)
            .expect("conflicted file present in modified files");
        assert_eq!(
            file.state,
            FileState::Unmerged,
            "fixture sanity check: file must actually be unmerged"
        );
        assert_eq!(file.submit_status, SubmitStatus::Blocked);
    }

    #[tokio::test]
    async fn blocked_wins_over_conflicted() {
        let (dir, _remote, git) = make_test_repo_with_remote().await;
        let path = "Content/Foo.uasset";
        fs::create_dir_all(dir.path().join("Content")).unwrap();
        fs::write(dir.path().join(path), "v1\n").unwrap();
        git.run(&["add", path], git::Opts::default()).await.unwrap();
        git.run(&["commit", "-qm", "add foo v1"], git::Opts::default())
            .await
            .unwrap();
        git.run(&["push", "-q", "origin", "main"], git::Opts::default())
            .await
            .unwrap();

        // Move local `main` back a commit so the file diverges from
        // `origin/main`, then recreate it locally with different content as
        // an untracked file — the shape `get_upstream_conflicts` looks for
        // (a path that's both modified upstream and present locally).
        git.run(&["reset", "-q", "--hard", "HEAD~1"], git::Opts::default())
            .await
            .unwrap();
        fs::create_dir_all(dir.path().join("Content")).unwrap();
        fs::write(dir.path().join(path), "v2\n").unwrap();

        let target_branches = vec![TargetBranchConfig {
            name: "main".to_string(),
            uses_merge_queue: false,
            blocked_file_globs: vec!["**/*.uasset".to_string()],
        }];
        let (status_op, _repo_status) = build_status_op(git, dir.path(), "main", target_branches);

        let status = status_op.run().await.expect("status run");
        assert!(
            status.conflicts.iter().any(|p| p == path),
            "fixture sanity check: file must actually be an upstream conflict, got {:?}",
            status.conflicts
        );
        let file = status
            .untracked_files
            .get(path)
            .expect("file present in untracked files");
        assert_eq!(file.submit_status, SubmitStatus::Blocked);
    }

    #[tokio::test]
    async fn blocked_wins_over_checkout_required() {
        let (dir, _remote, git) = make_test_repo_with_remote().await;
        let path = "Content/NeedsCheckout.uasset";
        fs::create_dir_all(dir.path().join("Content")).unwrap();
        fs::write(dir.path().join(path), "v1\n").unwrap();

        let target_branches = vec![TargetBranchConfig {
            name: "main".to_string(),
            uses_merge_queue: false,
            blocked_file_globs: vec!["**/*.uasset".to_string()],
        }];
        let (status_op, _repo_status) = build_status_op(git, dir.path(), "main", target_branches);

        let status = status_op.run().await.expect("status run");
        let file = status
            .untracked_files
            .get(path)
            .expect("file present in untracked files");
        assert_eq!(file.submit_status, SubmitStatus::Blocked);
        assert_ne!(file.submit_status, SubmitStatus::CheckoutRequired);
    }

    #[tokio::test]
    async fn blocked_only_on_configured_branch() {
        let (dir, _remote, git) = make_test_repo_with_remote().await;
        let path = "Content/Foo.uasset";
        fs::create_dir_all(dir.path().join("Content")).unwrap();
        fs::write(dir.path().join(path), "v1\n").unwrap();

        let target_branches = vec![
            TargetBranchConfig {
                name: "main".to_string(),
                uses_merge_queue: false,
                blocked_file_globs: vec!["**/*.uasset".to_string()],
            },
            TargetBranchConfig {
                name: "content-main".to_string(),
                uses_merge_queue: false,
                blocked_file_globs: vec![],
            },
        ];

        let (status_op, _repo_status) =
            build_status_op(git.clone(), dir.path(), "main", target_branches.clone());
        let status = status_op.run().await.expect("status run on main");
        let file = status
            .untracked_files
            .get(path)
            .expect("file present on main");
        assert_eq!(file.submit_status, SubmitStatus::Blocked);

        git.run(&["checkout", "-q", "content-main"], git::Opts::default())
            .await
            .unwrap();
        let (status_op, _repo_status) =
            build_status_op(git, dir.path(), "content-main", target_branches);
        let status = status_op.run().await.expect("status run on content-main");
        let file = status
            .untracked_files
            .get(path)
            .expect("file present on content-main");
        assert_ne!(file.submit_status, SubmitStatus::Blocked);
    }

    #[tokio::test]
    async fn unknown_target_branch_degrades_to_nothing_blocked() {
        let (dir, _remote, git) = make_test_repo_with_remote().await;
        let path = "Content/Foo.uasset";
        fs::create_dir_all(dir.path().join("Content")).unwrap();
        fs::write(dir.path().join(path), "v1\n").unwrap();

        // Check out a branch whose name mirrors the (unconfigured) target
        // branch, so the ahead/behind lookup — which only runs when the
        // checked-out branch differs from the target branch — doesn't need
        // a matching remote ref. This test is only about
        // `blocked_globs_for_branch` degrading safely for an unknown
        // branch, not about ahead/behind accounting.
        git.run(&["checkout", "-q", "-b", "release"], git::Opts::default())
            .await
            .unwrap();

        let target_branches = vec![
            TargetBranchConfig {
                name: "main".to_string(),
                uses_merge_queue: false,
                blocked_file_globs: vec!["**/*.uasset".to_string()],
            },
            TargetBranchConfig {
                name: "content-main".to_string(),
                uses_merge_queue: false,
                blocked_file_globs: vec![],
            },
        ];
        let (status_op, _repo_status) =
            build_status_op(git, dir.path(), "release", target_branches);

        let status = status_op
            .run()
            .await
            .expect("status run must not error for an unconfigured target branch");
        let file = status
            .untracked_files
            .get(path)
            .expect("file present in untracked files");
        assert_ne!(file.submit_status, SubmitStatus::Blocked);
    }

    const FEATURE: &str = "f11r-branch-x";

    async fn run_git(git: &git::Git, args: &[&str]) {
        git.run(args, git::Opts::default())
            .await
            .unwrap_or_else(|e| panic!("git {args:?} failed: {e}"));
    }

    fn write_file(dir: &std::path::Path, path: &str, contents: &str) {
        let full = dir.join(path);
        fs::create_dir_all(full.parent().expect("parent dir")).unwrap();
        fs::write(full, contents).unwrap();
    }

    /// A user clone and a collaborator clone of one remote. `main` and `content-main` both carry
    /// `Content/{A,B,C,D}.uasset`.
    struct BranchFixture {
        dir: tempfile::TempDir,
        _remote: tempfile::TempDir,
        _collab_dir: tempfile::TempDir,
        _hooks: tempfile::TempDir,
        git: git::Git,
        collab: git::Git,
    }

    impl BranchFixture {
        async fn new() -> Self {
            let (dir, remote, git) = make_test_repo_with_remote().await;
            for path in ["A", "B", "C", "D"] {
                write_file(dir.path(), &format!("Content/{path}.uasset"), "base\n");
            }
            run_git(&git, &["add", "Content"]).await;
            run_git(&git, &["commit", "-qm", "seed assets"]).await;
            run_git(&git, &["push", "-q", "origin", "main", "main:content-main"]).await;

            let collab_dir = tempfile::tempdir().expect("create collaborator tempdir");
            run_git(
                &git,
                &[
                    "clone",
                    "-q",
                    remote.path().to_str().expect("utf8 remote path"),
                    collab_dir.path().to_str().expect("utf8 collaborator path"),
                ],
            )
            .await;
            let (tx, _rx) = mpsc::channel::<String>();
            let collab = git::Git::new(collab_dir.path().to_path_buf(), tx);
            run_git(&collab, &["config", "user.email", "collab@example.com"]).await;
            run_git(&collab, &["config", "user.name", "Collaborator"]).await;

            // Hooks come only from an empty temp dir.
            let hooks = tempfile::tempdir().expect("create hooks tempdir");
            let hooks_path = hooks.path().to_str().expect("utf8 hooks path");
            for clone in [&git, &collab] {
                run_git(clone, &["config", "core.hooksPath", hooks_path]).await;
                run_git(clone, &["config", "commit.gpgsign", "false"]).await;
            }

            Self {
                dir,
                _remote: remote,
                _collab_dir: collab_dir,
                _hooks: hooks,
                git,
                collab,
            }
        }

        fn git_dir(&self) -> std::path::PathBuf {
            self.dir.path().join(".git")
        }

        /// Commits a local edit of `path` on the current branch and pushes it.
        async fn push_edit(&self, path: &str, contents: &str, message: &str) {
            self.edit(path, contents);
            run_git(&self.git, &["commit", "-qam", message]).await;
            run_git(&self.git, &["push", "-q"]).await;
        }

        /// Writes the guided rebase record for the current branch and rebases onto
        /// `origin/main`; returns git's exit code.
        async fn guided_rebase(&self) -> i32 {
            let file = guided_record(&self.git, "refs/remotes/origin/main").await;
            write_rebase_file(&self.git_dir(), &file).expect("write rebase record");
            start_raw_rebase(&self.git, &file.onto).await
        }

        /// Creates `FEATURE` from local `main` and pushes it with an upstream.
        async fn push_feature_branch(&self) {
            run_git(&self.git, &["checkout", "-q", "-b", FEATURE]).await;
            run_git(&self.git, &["push", "-q", "-u", "origin", FEATURE]).await;
        }

        /// Collaborator commits `path` on `branch` and pushes it.
        async fn collab_push(&self, branch: &str, path: &str, contents: &str, message: &str) {
            let tracking = format!("origin/{branch}");
            run_git(&self.collab, &["fetch", "-q", "origin"]).await;
            run_git(&self.collab, &["checkout", "-q", "-B", branch, &tracking]).await;
            write_file(&self.collab.repo_path, path, contents);
            run_git(&self.collab, &["add", path]).await;
            run_git(&self.collab, &["commit", "-qm", message]).await;
            run_git(&self.collab, &["push", "-q", "origin", branch]).await;
        }

        async fn fetch(&self) {
            run_git(&self.git, &["fetch", "-q", "--prune", "origin"]).await;
        }

        fn edit(&self, path: &str, contents: &str) {
            write_file(self.dir.path(), path, contents);
        }

        /// Trunk `main`, targets `main` and `content-main`.
        fn status_op(&self, trunk: &str) -> StatusOp<crate::engine::UnrealEngineProvider> {
            let target_branches = ["main", "content-main"]
                .into_iter()
                .map(|name| TargetBranchConfig {
                    name: name.to_string(),
                    uses_merge_queue: false,
                    blocked_file_globs: vec![],
                })
                .collect();
            let (status_op, _repo_status) =
                build_status_op(self.git.clone(), self.dir.path(), "main", target_branches);
            status_op.repo_config.write().trunk_branch = trunk.to_string();
            status_op
        }

        async fn status(&self) -> RepoStatus {
            self.status_op("main").run().await.expect("status run")
        }
    }

    fn submit_status_of(status: &RepoStatus, path: &str) -> SubmitStatus {
        status
            .modified_files
            .get(path)
            .or_else(|| status.untracked_files.get(path))
            .unwrap_or_else(|| panic!("{path} missing from local changes"))
            .submit_status
            .clone()
    }

    #[tokio::test]
    async fn feature_branch_main_side_change_warns_but_does_not_block() {
        let fx = BranchFixture::new().await;
        fx.push_feature_branch().await;

        // B is changed on the branch (committed), A is edited locally, E is untracked locally.
        fx.edit("Content/B.uasset", "branch\n");
        run_git(&fx.git, &["commit", "-qam", "branch edit b"]).await;
        run_git(&fx.git, &["push", "-q"]).await;

        fx.collab_push("main", "Content/A.uasset", "main\n", "main edit a")
            .await;
        fx.collab_push("main", "Content/B.uasset", "main\n", "main edit b")
            .await;
        fx.collab_push("main", "Content/E.uasset", "main\n", "main add e")
            .await;
        fx.fetch().await;

        fx.edit("Content/A.uasset", "local\n");
        fx.edit("Content/E.uasset", "local\n");

        let status = fx.status().await;
        assert_eq!(
            status.will_conflict_on_rebase,
            vec![
                "Content/A.uasset".to_string(),
                "Content/B.uasset".to_string(),
                "Content/E.uasset".to_string(),
            ]
        );
        assert!(status.modified_upstream.is_empty(), "{status:?}");
        assert!(status.conflicts.is_empty(), "{status:?}");
        assert!(!status.conflict_upstream);
        for path in ["Content/A.uasset", "Content/E.uasset"] {
            assert_ne!(submit_status_of(&status, path), SubmitStatus::Conflicted);
        }
    }

    #[tokio::test]
    async fn feature_branch_main_only_change_is_not_listed() {
        let fx = BranchFixture::new().await;
        fx.push_feature_branch().await;
        fx.collab_push("main", "Content/C.uasset", "main\n", "main edit c")
            .await;
        fx.fetch().await;
        fx.edit("Content/A.uasset", "local\n");

        let status = fx.status().await;
        assert!(status.will_conflict_on_rebase.is_empty(), "{status:?}");
        assert!(status.modified_upstream.is_empty(), "{status:?}");
        assert!(status.conflicts.is_empty(), "{status:?}");
    }

    #[tokio::test]
    async fn feature_branch_upstream_change_blocks() {
        let fx = BranchFixture::new().await;
        fx.push_feature_branch().await;
        fx.collab_push("main", "Content/B.uasset", "main\n", "main edit b")
            .await;
        fx.collab_push(FEATURE, "Content/B.uasset", "collab\n", "collab edit b")
            .await;
        fx.fetch().await;
        fx.edit("Content/B.uasset", "local\n");

        let status = fx.status().await;
        assert!(!status.guided_rebase_active);
        assert_eq!(
            status.modified_upstream,
            vec!["Content/B.uasset".to_string()]
        );
        assert_eq!(status.conflicts, vec!["Content/B.uasset".to_string()]);
        assert!(status.conflict_upstream);
        assert!(status.will_conflict_on_rebase.is_empty(), "{status:?}");
        assert_eq!(
            submit_status_of(&status, "Content/B.uasset"),
            SubmitStatus::Conflicted
        );
    }

    #[tokio::test]
    async fn feature_branch_rebased_copy_of_own_commit_does_not_block() {
        let fx = BranchFixture::new().await;
        fx.push_feature_branch().await;

        fx.edit("Content/C.uasset", "mine\n");
        run_git(&fx.git, &["commit", "-qam", "edit c"]).await;
        fx.collab_push(FEATURE, "Content/C.uasset", "mine\n", "edit c")
            .await;
        fx.fetch().await;
        fx.edit("Content/C.uasset", "local\n");

        let upstream_diff = fx
            .git
            .diff_filenames(&format!("HEAD...origin/{FEATURE}"))
            .await
            .unwrap();
        assert!(
            upstream_diff.iter().any(|p| p == "Content/C.uasset"),
            "fixture sanity check: C must differ upstream, got {upstream_diff:?}"
        );

        let status = fx.status().await;
        assert!(status.modified_upstream.is_empty(), "{status:?}");
        assert!(status.conflicts.is_empty(), "{status:?}");
    }

    #[tokio::test]
    async fn feature_branch_ignores_other_target_branches() {
        let fx = BranchFixture::new().await;
        fx.push_feature_branch().await;
        fx.collab_push(
            "content-main",
            "Content/D.uasset",
            "content\n",
            "content edit d",
        )
        .await;
        fx.fetch().await;
        fx.edit("Content/D.uasset", "local\n");

        let status = fx.status().await;
        assert!(status.modified_upstream.is_empty(), "{status:?}");
        assert!(status.will_conflict_on_rebase.is_empty(), "{status:?}");
        assert!(status.conflicts.is_empty(), "{status:?}");
    }

    #[tokio::test]
    async fn feature_branch_without_upstream_has_empty_blocking_list() {
        let fx = BranchFixture::new().await;
        run_git(&fx.git, &["checkout", "-q", "-b", FEATURE]).await;
        fx.collab_push("main", "Content/A.uasset", "main\n", "main edit a")
            .await;
        fx.fetch().await;
        fx.edit("Content/A.uasset", "local\n");

        let status = fx.status().await;
        assert_eq!(status.branch, FEATURE);
        assert_eq!(status.remote_branch, "");
        assert!(status.modified_upstream.is_empty(), "{status:?}");
        assert!(status.conflicts.is_empty(), "{status:?}");
        assert_eq!(
            status.will_conflict_on_rebase,
            vec!["Content/A.uasset".to_string()]
        );
    }

    #[tokio::test]
    async fn feature_branch_with_gone_upstream_status_ok() {
        let fx = BranchFixture::new().await;
        fx.push_feature_branch().await;
        fx.collab_push("main", "Content/A.uasset", "main\n", "main edit a")
            .await;
        run_git(&fx.collab, &["push", "-q", "origin", "--delete", FEATURE]).await;
        fx.fetch().await;
        fx.edit("Content/A.uasset", "local\n");

        let status = fx.status().await;
        assert_eq!(status.remote_branch, format!("origin/{FEATURE}"));
        assert!(status.modified_upstream.is_empty(), "{status:?}");
        assert!(status.conflicts.is_empty(), "{status:?}");
        assert_eq!(
            status.will_conflict_on_rebase,
            vec!["Content/A.uasset".to_string()]
        );
    }

    #[tokio::test]
    async fn feature_branch_empty_trunk_skips_warning_list() {
        let fx = BranchFixture::new().await;
        fx.push_feature_branch().await;
        fx.collab_push("main", "Content/A.uasset", "main\n", "main edit a")
            .await;
        fx.fetch().await;
        fx.edit("Content/A.uasset", "local\n");

        let status = fx.status_op("").run().await.expect("status run");
        assert!(status.will_conflict_on_rebase.is_empty(), "{status:?}");
        assert!(status.modified_upstream.is_empty(), "{status:?}");
    }

    #[tokio::test]
    async fn protected_branch_keeps_target_branch_union() {
        let fx = BranchFixture::new().await;
        fx.collab_push("main", "Content/A.uasset", "main\n", "main edit a")
            .await;
        fx.collab_push(
            "content-main",
            "Content/D.uasset",
            "content\n",
            "content edit d",
        )
        .await;
        fx.fetch().await;
        fx.edit("Content/A.uasset", "local\n");

        let status = fx.status().await;
        assert_eq!(status.branch, "main");
        assert_eq!(
            status.modified_upstream,
            vec![
                "Content/A.uasset".to_string(),
                "Content/D.uasset".to_string()
            ]
        );
        assert_eq!(status.conflicts, vec!["Content/A.uasset".to_string()]);
        assert!(status.will_conflict_on_rebase.is_empty(), "{status:?}");
    }

    #[tokio::test]
    async fn quick_submit_branch_keeps_target_branch_union() {
        let fx = BranchFixture::new().await;
        run_git(&fx.git, &["checkout", "-q", "-b", "f11r-main-test-1"]).await;
        fx.collab_push("main", "Content/A.uasset", "main\n", "main edit a")
            .await;
        fx.collab_push(
            "content-main",
            "Content/D.uasset",
            "content\n",
            "content edit d",
        )
        .await;
        fx.fetch().await;
        fx.edit("Content/A.uasset", "local\n");

        let status = fx.status().await;
        assert_eq!(status.branch, "f11r-main-test-1");
        assert_eq!(
            status.modified_upstream,
            vec![
                "Content/A.uasset".to_string(),
                "Content/D.uasset".to_string()
            ]
        );
        assert_eq!(status.conflicts, vec!["Content/A.uasset".to_string()]);
        assert!(status.will_conflict_on_rebase.is_empty(), "{status:?}");
    }

    #[tokio::test]
    async fn detached_head_status_ok() {
        let fx = BranchFixture::new().await;
        fx.collab_push("main", "Content/A.uasset", "main\n", "main edit a")
            .await;
        fx.fetch().await;
        run_git(&fx.git, &["checkout", "-q", "--detach"]).await;
        fx.edit("Content/A.uasset", "local\n");

        let status = fx.status().await;
        assert!(status.detached_head);
        assert_eq!(status.branch, "");
        assert!(status.will_conflict_on_rebase.is_empty(), "{status:?}");
    }

    fn assert_no_upstream_lists(status: &RepoStatus) {
        assert!(status.modified_upstream.is_empty(), "{status:?}");
        assert!(status.conflicts.is_empty(), "{status:?}");
        assert!(status.will_conflict_on_rebase.is_empty(), "{status:?}");
        assert!(status.will_conflict_on_sync.is_empty(), "{status:?}");
        assert!(!status.conflict_upstream);
    }

    /// `FEATURE` and `main` both changed `Content/A.uasset`.
    async fn diverge_on_a(fx: &BranchFixture) {
        fx.push_feature_branch().await;
        fx.push_edit("Content/A.uasset", "branch\n", "branch edit a")
            .await;
        fx.collab_push("main", "Content/A.uasset", "main\n", "main edit a")
            .await;
        fx.fetch().await;
    }

    #[tokio::test]
    async fn guided_rebase_in_progress_status_ok() {
        let fx = BranchFixture::new().await;
        diverge_on_a(&fx).await;
        assert_eq!(fx.guided_rebase().await, 1, "the rebase should stop");
        fx.edit("Content/D.uasset", "local\n");

        let status = fx.status().await;
        assert!(status.detached_head);
        assert_eq!(status.branch, "");
        assert!(status.guided_rebase_active);
        assert_no_upstream_lists(&status);
        assert_eq!(
            submit_status_of(&status, "Content/A.uasset"),
            SubmitStatus::Unmerged
        );
        assert_ne!(
            submit_status_of(&status, "Content/D.uasset"),
            SubmitStatus::Conflicted
        );
    }

    /// Completes a guided rebase of `FEATURE`, then a collaborator changes `Content/C.uasset` on
    /// `origin/FEATURE`, which the user also edits locally.
    async fn completed_rebase_with_upstream_change(fx: &BranchFixture) {
        fx.push_feature_branch().await;
        fx.push_edit("Content/B.uasset", "branch\n", "branch edit b")
            .await;
        fx.collab_push("main", "Content/A.uasset", "main\n", "main edit a")
            .await;
        fx.fetch().await;
        assert_eq!(fx.guided_rebase().await, 0);
        fx.collab_push(FEATURE, "Content/C.uasset", "collab\n", "collab edit c")
            .await;
        fx.fetch().await;
        fx.edit("Content/C.uasset", "local\n");
    }

    #[tokio::test]
    async fn guided_rebase_completed_skips_upstream_changes() {
        let fx = BranchFixture::new().await;
        completed_rebase_with_upstream_change(&fx).await;

        let status = fx.status().await;
        assert_eq!(status.branch, FEATURE);
        assert!(status.guided_rebase_active);
        assert_no_upstream_lists(&status);
        assert_ne!(
            submit_status_of(&status, "Content/C.uasset"),
            SubmitStatus::Conflicted
        );

        // The same repo without the record reports the collaborator's change.
        fs::remove_file(json_path(&fx.git_dir())).unwrap();
        let status = fx.status().await;
        assert!(!status.guided_rebase_active);
        assert_eq!(
            status.modified_upstream,
            vec!["Content/C.uasset".to_string()]
        );
        assert_eq!(status.conflicts, vec!["Content/C.uasset".to_string()]);
        assert!(status.conflict_upstream);
    }

    #[tokio::test]
    async fn external_rebase_status_ok() {
        let fx = BranchFixture::new().await;
        diverge_on_a(&fx).await;
        assert_eq!(
            start_raw_rebase(&fx.git, "refs/remotes/origin/main").await,
            1
        );

        let status = fx.status().await;
        assert!(status.detached_head);
        assert!(!status.guided_rebase_active);
        assert_no_upstream_lists(&status);
    }

    #[tokio::test]
    async fn unknown_rebase_state_does_not_fail_status() {
        let fx = BranchFixture::new().await;
        diverge_on_a(&fx).await;
        assert_eq!(fx.guided_rebase().await, 1);
        // A record that exists but can't be read.
        let json = json_path(&fx.git_dir());
        fs::remove_file(&json).unwrap();
        fs::create_dir(&json).unwrap();

        let status = fx.status().await;
        assert!(!status.guided_rebase_active);
        assert_no_upstream_lists(&status);
    }

    #[tokio::test]
    async fn unknown_rebase_state_skips_upstream_changes() {
        let fx = BranchFixture::new().await;
        completed_rebase_with_upstream_change(&fx).await;
        let json = json_path(&fx.git_dir());
        fs::remove_file(&json).unwrap();
        fs::create_dir(&json).unwrap();

        let status = fx.status().await;
        assert!(!status.guided_rebase_active);
        assert_no_upstream_lists(&status);
    }

    #[test]
    fn will_conflict_on_sync_rule() {
        let strings = |xs: &[&str]| xs.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let local = FileList(
            [
                "notes.txt",
                "Content/B.uasset",
                "Content/Local.uasset",
                "gone.txt",
            ]
            .into_iter()
            .map(|path| File {
                path: path.to_string(),
                ..Default::default()
            })
            .collect(),
        );
        let incoming = strings(&[
            "notes.txt",
            "Content/B.uasset",
            "Content/Upstream.uasset",
            "gone.txt",
        ]);
        let blocking = strings(&["Content/B.uasset", "Content/Upstream.uasset"]);

        assert_eq!(
            will_conflict_on_sync(&incoming, &local, &blocking),
            strings(&["notes.txt", "gone.txt"])
        );
        assert!(will_conflict_on_sync(&[], &local, &blocking).is_empty());
        assert!(will_conflict_on_sync(&incoming, &FileList::default(), &[]).is_empty());
    }

    /// Commits `notes.txt` on `main` and pushes it, so branches made afterwards carry it.
    async fn seed_text_file(fx: &BranchFixture) {
        fx.edit("notes.txt", "base\n");
        run_git(&fx.git, &["add", "notes.txt"]).await;
        run_git(&fx.git, &["commit", "-qm", "seed notes"]).await;
        run_git(&fx.git, &["push", "-q", "origin", "main"]).await;
    }

    #[tokio::test]
    async fn feature_branch_lists_any_local_file_changed_on_origin_branch() {
        let fx = BranchFixture::new().await;
        seed_text_file(&fx).await;
        fx.push_feature_branch().await;
        for (path, message) in [
            ("notes.txt", "collab edit notes"),
            ("Content/B.uasset", "collab edit b"),
            ("Content/C.uasset", "collab edit c"),
        ] {
            fx.collab_push(FEATURE, path, "collab\n", message).await;
        }
        fx.fetch().await;
        fx.edit("notes.txt", "local\n");
        fx.edit("Content/B.uasset", "local\n");
        fx.edit("Content/A.uasset", "local\n");

        let status = fx.status().await;
        assert_eq!(status.will_conflict_on_sync, vec!["notes.txt".to_string()]);
        assert_eq!(status.conflicts, vec!["Content/B.uasset".to_string()]);
        assert_ne!(
            submit_status_of(&status, "notes.txt"),
            SubmitStatus::Conflicted
        );
    }

    #[tokio::test]
    async fn feature_branch_without_upstream_has_empty_sync_warning() {
        let fx = BranchFixture::new().await;
        seed_text_file(&fx).await;
        run_git(&fx.git, &["checkout", "-q", "-b", FEATURE]).await;
        fx.collab_push("main", "notes.txt", "collab\n", "collab edit notes")
            .await;
        fx.fetch().await;
        fx.edit("notes.txt", "local\n");

        let status = fx.status().await;
        assert!(status.will_conflict_on_sync.is_empty(), "{status:?}");
    }

    #[tokio::test]
    async fn protected_branch_has_empty_sync_warning() {
        let fx = BranchFixture::new().await;
        seed_text_file(&fx).await;
        fx.collab_push("main", "notes.txt", "collab\n", "collab edit notes")
            .await;
        fx.fetch().await;
        fx.edit("notes.txt", "local\n");

        let status = fx.status().await;
        assert_eq!(status.branch, "main");
        assert!(status.will_conflict_on_sync.is_empty(), "{status:?}");
    }

    #[test]
    fn test_find_dll_commit() {
        let base = std::time::SystemTime::now();
        let ac = ArtifactConfig::new(
            "fake-project".into(),
            ArtifactKind::Editor,
            ArtifactBuildConfig::Development,
            Platform::Win64,
        );
        let mut list = ArtifactList::new(ac, "s3://fakebucket/".into());
        let mut entry = ArtifactEntry::new("v1/believerco-gameprototypemp/editor/win64/development/0266eafeecd51b155d3621469ac689bcd485020d.json".to_string());
        entry.last_modified = base - std::time::Duration::from_secs(5);
        list.entries.push(entry);
        let mut entry = ArtifactEntry::new("v1/believerco-gameprototypemp/editor/win64/development/9c351d7dacd6c412f55a825d77727761d9c1268b.json".to_string());
        entry.last_modified = base - std::time::Duration::from_secs(10);
        list.entries.push(entry);
        let mut entry = ArtifactEntry::new("v1/believerco-gameprototypemp/editor/win64/development/de0bb3ad8454d29083665ebb3db0dd0c29a2d1d0.json".to_string());
        entry.last_modified = base - std::time::Duration::from_secs(20);
        list.entries.push(entry);
        list.sort_by_last_modified();

        let long_shas = [
            "0123456789abcde0123456789abcde0123456789",
            "9c351d7dacd6c412f55a825d77727761d9c1268b",
            "1123456789abcde0123456789abcde0123456789",
            "2123456789abcde0123456789abcde0123456789",
            "3123456789abcde0123456789abcde0123456789",
        ]
        .join("\n");

        println!("{long_shas:?}");
        let sha = find_dll_commit(&list, &long_shas, "test");
        assert_eq!(sha, "9c351d7dacd6c412f55a825d77727761d9c1268b");
    }
}
