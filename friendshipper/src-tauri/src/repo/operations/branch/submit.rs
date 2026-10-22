use std::collections::HashSet;
use std::path::PathBuf;

use anyhow::anyhow;
use axum::extract::State;
use axum::{async_trait, Json};
use tracing::{instrument, warn};

use super::run_sequence;
use crate::engine::EngineProvider;
use crate::repo::operations::gh::submit::{
    commit_files, is_branch_mode, reject_files_with_conflict_markers, reject_unsubmittable_files,
    restore_pre_submit_state,
};
use crate::repo::operations::validate::validate_repo_state;
use crate::repo::operations::StatusOp;
use crate::repo::RepoStatusRef;
use crate::state::AppState;
use ethos_core::clients::git;
use ethos_core::types::config::{AppConfigRef, RepoConfigRef};
use ethos_core::types::errors::CoreError;
use ethos_core::types::repo::PushRequest;
use ethos_core::worker::Task;

const MAX_STALE_PATHS: usize = 10;

#[derive(Clone)]
pub struct BranchSubmitOp<T> {
    pub files: Vec<String>,
    pub commit_message: String,
    /// The branch the user reviewed; the submit is refused if HEAD is now on another one.
    pub expected_branch: Option<String>,

    pub app_config: AppConfigRef,
    pub repo_config: RepoConfigRef,
    pub repo_status: RepoStatusRef,
    pub engine: T,

    pub git_client: git::Git,
    pub github_username: String,
    pub sync_phase_tx: std::sync::mpsc::Sender<String>,
}

fn input_error(message: String) -> CoreError {
    CoreError::Input(anyhow!(message))
}

fn detached_error() -> CoreError {
    input_error(
        "Repository is in a detached HEAD state. Check out a branch before continuing.".to_string(),
    )
}

fn protected_branch_error(branch: &str) -> CoreError {
    input_error(format!(
        "'{branch}' is a protected branch. Use Quick Submit to change it."
    ))
}

fn quick_submit_branch_error(branch: &str, target: &str) -> CoreError {
    input_error(format!(
        "'{branch}' is a Quick Submit branch. Use Quick Submit, or Sync to return to '{target}'."
    ))
}

fn branch_changed_error(actual: &str) -> CoreError {
    input_error(format!(
        "The current branch changed to '{actual}' since you reviewed this submit. Review it again."
    ))
}

fn no_message_error() -> CoreError {
    input_error("Commit message is required".to_string())
}

fn stale_files_error(paths: &[&str]) -> CoreError {
    let mut list = paths
        .iter()
        .take(MAX_STALE_PATHS)
        .copied()
        .collect::<Vec<_>>()
        .join(", ");
    if paths.len() > MAX_STALE_PATHS {
        list.push_str(&format!(", and {} more", paths.len() - MAX_STALE_PATHS));
    }
    input_error(format!(
        "Some selected files are no longer modified: {list}. Refresh and try again."
    ))
}

fn gone_error(branch: &str, target: &str) -> CoreError {
    input_error(format!(
        "Branch '{branch}' does not exist on origin (it may have been merged or deleted). Return to '{target}' from Branch Management."
    ))
}

fn behind_error() -> CoreError {
    input_error("Branch has new commits on origin — Sync before submitting.".to_string())
}

fn nothing_to_push_error() -> CoreError {
    input_error("No files to submit".to_string())
}

fn push_race_error(branch: &str) -> CoreError {
    input_error(format!(
        "Origin has new commits on '{branch}'. Your commit is saved locally. Sync, then push it with Submit to branch."
    ))
}

fn push_failed_error(git_output: &str) -> CoreError {
    CoreError::Internal(anyhow!(
        "Push failed: {git_output}. Your commit is saved locally."
    ))
}

/// Whether a failed push was rejected because origin moved ahead of us.
pub(crate) fn is_push_race(git_output: &str) -> bool {
    let output = git_output.to_lowercase();
    (output.contains("[rejected]")
        && (output.contains("fetch first")
            || output.contains("non-fast-forward")
            || output.contains("stale info")))
        || (output.contains("[remote rejected]")
            && (output.contains("incorrect old value provided")
                || output.contains("cannot lock ref")))
        || output.contains("updates were rejected because the remote contains work")
}

/// `HEAD` compared against a freshly fetched `origin/<branch>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OriginComparison {
    pub ahead: u32,
    pub behind: u32,
    /// The `origin/<branch>` SHA the comparison was made against.
    pub origin_sha: String,
}

/// Fetches `origin/<branch>` and compares `HEAD` against it.
pub(crate) async fn fetch_and_compare_to_origin(
    git: &git::Git,
    branch: &str,
    target: &str,
) -> Result<OriginComparison, CoreError> {
    if !git.has_remote_branch_exact(branch).await? {
        return Err(gone_error(branch, target));
    }

    // An explicit refspec updates origin/<branch> even after a force-push, and never matches a
    // tag of the same name.
    let tracking_ref = format!("refs/remotes/origin/{branch}");
    let refspec = format!("+refs/heads/{branch}:{tracking_ref}");
    if let Err(e) = git
        .run(
            &["fetch", "--no-auto-maintenance", "origin", &refspec],
            git::Opts::new_with_complete_error(),
        )
        .await
    {
        // The branch can vanish between ls-remote and fetch.
        if e.to_string().contains("couldn't find remote ref") {
            return Err(gone_error(branch, target));
        }
        return Err(e.into());
    }

    let origin_sha = git
        .run_and_collect_output(&["rev-parse", &tracking_ref], git::Opts::default())
        .await?
        .trim()
        .to_string();
    let (ahead, behind) = git.get_ahead_behind(&origin_sha, "HEAD").await?;
    Ok(OriginComparison {
        ahead,
        behind,
        origin_sha,
    })
}

/// The SHA of `refs/heads/<branch>` on origin, or `None` if the branch does not exist.
async fn remote_branch_sha(git: &git::Git, branch: &str) -> anyhow::Result<Option<String>> {
    let refname = format!("refs/heads/{branch}");
    let output = git
        .run_and_collect_output(&["ls-remote", "origin", &refname], git::Opts::default())
        .await?;
    Ok(output.lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        let sha = parts.next()?;
        (parts.next() == Some(refname.as_str())).then(|| sha.to_string())
    }))
}

/// Pushes `HEAD` to `origin/<branch>` only if origin still points at `origin_sha`.
pub(crate) async fn push_head_to_branch(
    git: &git::Git,
    branch: &str,
    target: &str,
    origin_sha: &str,
) -> Result<(), CoreError> {
    let lease = format!("--force-with-lease=refs/heads/{branch}:{origin_sha}");
    let refspec = format!("HEAD:refs/heads/{branch}");
    // HEAD contains origin_sha, so the lease only ever lets this fast-forward.
    let Err(e) = git
        .run(
            &["push", "--porcelain", &lease, "origin", &refspec],
            git::Opts::default().with_complete_error(),
        )
        .await
    else {
        return Ok(());
    };

    let text = e.to_string();
    match remote_branch_sha(git, branch).await {
        Ok(None) => Err(gone_error(branch, target)),
        Ok(Some(sha)) if sha != origin_sha => Err(push_race_error(branch)),
        Ok(Some(_)) => Err(push_failed_error(&text)),
        Err(ls_error) => {
            warn!("Failed to read origin/{branch} after a failed push: {ls_error}");
            if is_push_race(&text) {
                Err(push_race_error(branch))
            } else {
                Err(push_failed_error(&text))
            }
        }
    }
}

impl<T> BranchSubmitOp<T>
where
    T: EngineProvider,
{
    fn emit_phase(&self, phase: impl Into<String>) {
        let _ = self.sync_phase_tx.send(phase.into());
    }

    fn status_op(&self, skip_engine_update: bool) -> StatusOp<T> {
        StatusOp {
            repo_status: self.repo_status.clone(),
            app_config: self.app_config.clone(),
            repo_config: self.repo_config.clone(),
            engine: self.engine.clone(),
            git_client: self.git_client.clone(),
            github_username: self.github_username.clone(),
            aws_client: None,
            storage: None,
            allow_offline_communication: false,
            skip_display_names: true,
            skip_engine_update,
            // internal refresh, not a user-initiated one
            fetch_first: false,
        }
    }

    /// Every requested path must still be a local change, so a stale selection never reaches
    /// `git add` or the restore path.
    fn reject_stale_files(&self) -> Result<(), CoreError> {
        let status = self.repo_status.read().clone();
        let changed: HashSet<&str> = status
            .modified_files
            .0
            .iter()
            .chain(status.untracked_files.0.iter())
            .map(|file| file.path.as_str())
            .collect();
        let stale: Vec<&str> = self
            .files
            .iter()
            .map(String::as_str)
            .filter(|path| !changed.contains(path))
            .collect();
        if stale.is_empty() {
            Ok(())
        } else {
            Err(stale_files_error(&stale))
        }
    }
}

#[async_trait]
impl<T> Task for BranchSubmitOp<T>
where
    T: EngineProvider,
{
    #[instrument(name = "BranchSubmitOp::execute", skip(self))]
    async fn execute(&self) -> Result<(), CoreError> {
        // 1. No rebase or merge in progress.
        {
            let repo_path = PathBuf::from(self.app_config.read().repo_path.clone());
            let repo_status = self.repo_status.read().clone();
            validate_repo_state(&repo_path, &repo_status)?;
        }

        // 2. The checked-out branch must submit with Submit to branch.
        let branch = self.git_client.current_branch().await?;
        if branch.is_empty() {
            return Err(detached_error());
        }
        let target = self.app_config.read().target_branch.clone();
        let (branch_mode, protected) = {
            let repo_config = self.repo_config.read();
            (
                is_branch_mode(&repo_config, &branch),
                repo_config.is_protected_branch(&branch),
            )
        };
        if !branch_mode {
            return Err(if protected {
                protected_branch_error(&branch)
            } else {
                quick_submit_branch_error(&branch, &target)
            });
        }
        if let Some(expected) = &self.expected_branch {
            if *expected != branch {
                return Err(branch_changed_error(&branch));
            }
        }

        // 3. Authoritative up-to-date check, before the status read and the snapshot.
        self.emit_phase("Checking branch is up to date with origin");
        let origin = fetch_and_compare_to_origin(&self.git_client, &branch, &target).await?;
        if origin.behind > 0 {
            return Err(behind_error());
        }
        if self.files.is_empty() && origin.ahead == 0 {
            return Err(nothing_to_push_error());
        }

        // 4. Fresh status for the file checks and the snapshot.
        self.emit_phase("Checking repository status");
        self.status_op(true).execute().await?;

        // 5. File checks; none of them write.
        if !self.files.is_empty() {
            if self.commit_message.trim().is_empty() {
                return Err(no_message_error());
            }
            self.reject_stale_files()?;
            {
                let repo_status = self.repo_status.read().clone();
                reject_unsubmittable_files(
                    &self.files,
                    &repo_status,
                    &self.app_config,
                    &self.repo_config,
                    &self.engine,
                )
                .await?;
            }
            reject_files_with_conflict_markers(&self.git_client, &self.files).await?;
        }

        if !self.files.is_empty() {
            // 6. Snapshot every local change.
            let all_files: Vec<String> = {
                let status = self.repo_status.read();
                status
                    .modified_files
                    .0
                    .iter()
                    .chain(status.untracked_files.0.iter())
                    .map(|file| file.path.clone())
                    .collect()
            };
            if !all_files.is_empty() {
                self.emit_phase("Snapshotting local changes");
            }
            let snapshot = self
                .git_client
                .save_snapshot("pre-submit", all_files)
                .await?;

            // 7. Commit. The snapshot is restored only here: nothing earlier changed the
            // working tree, and nothing later may, because the commit must survive a failed push.
            self.emit_phase("Committing changes");
            if let Err(e) = commit_files(
                &self.git_client,
                &self.repo_status,
                &self.files,
                &self.commit_message,
            )
            .await
            {
                restore_pre_submit_state(&self.git_client, &self.engine, &branch, &snapshot.commit)
                    .await?;
                return Err(e);
            }
        }

        // 8. Push. On failure the commit stays local.
        self.emit_phase("Pushing to origin");
        push_head_to_branch(&self.git_client, &branch, &target, &origin.origin_sha).await?;

        // 9. Locks are kept. Refresh status so the engine sees the new state.
        if let Err(e) = self.status_op(false).execute().await {
            warn!("Failed to refresh status after submitting to {branch}: {e}");
        }

        Ok(())
    }

    fn get_name(&self) -> String {
        String::from("BranchSubmitOp")
    }
}

pub async fn branch_submit_handler<T>(
    State(state): State<AppState<T>>,
    Json(request): Json<PushRequest>,
) -> Result<Json<String>, CoreError>
where
    T: EngineProvider,
{
    let op = BranchSubmitOp {
        files: request.files,
        commit_message: request.commit_message,
        expected_branch: request.expected_branch,

        app_config: state.app_config.clone(),
        repo_config: state.repo_config.clone(),
        repo_status: state.repo_status.clone(),
        engine: state.engine.clone(),

        git_client: state.git(),
        github_username: state.github_username(),
        sync_phase_tx: state.sync_phase_tx.clone(),
    };

    run_sequence(&state, Box::new(op)).await?;

    Ok(Json("ok".to_string()))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::sync::{mpsc, Arc};

    use parking_lot::RwLock;

    use super::*;
    use crate::engine::UnrealEngineProvider;
    use ethos_core::types::config::{AppConfig, RepoConfig, TargetBranchConfig};
    use ethos_core::types::repo::RepoStatus;

    const BRANCH: &str = "f11r-branch-x";

    const PHASES_WITH_FILES: [&str; 5] = [
        "Checking branch is up to date with origin",
        "Checking repository status",
        "Snapshotting local changes",
        "Committing changes",
        "Pushing to origin",
    ];

    fn new_git(path: &Path) -> git::Git {
        let (tx, _rx) = mpsc::channel::<String>();
        git::Git::new(path.to_path_buf(), tx)
    }

    async fn run_git(git: &git::Git, args: &[&str]) {
        git.run(args, git::Opts::default())
            .await
            .unwrap_or_else(|e| panic!("git {args:?} failed: {e}"));
    }

    async fn git_output(git: &git::Git, args: &[&str]) -> String {
        git.run_and_collect_output(args, git::Opts::default())
            .await
            .unwrap_or_else(|e| panic!("git {args:?} failed: {e}"))
            .trim()
            .to_string()
    }

    fn write_file(dir: &Path, path: &str, contents: &str) {
        let full = dir.join(path);
        fs::create_dir_all(full.parent().expect("parent dir")).unwrap();
        fs::write(full, contents).unwrap();
    }

    fn slash_path(path: &Path) -> String {
        path.to_str().expect("utf8 path").replace('\\', "/")
    }

    fn input_message(result: Result<(), CoreError>) -> String {
        match result {
            Err(CoreError::Input(e)) => e.to_string(),
            other => panic!("expected CoreError::Input, got {other:?}"),
        }
    }

    /// A bare origin with `main`, `content-main` and `f11r-branch-x`; a user clone on
    /// `f11r-branch-x` with an upstream; and a collaborator clone. Hooks come only from
    /// per-clone fixture dirs.
    struct Fixture {
        _origin_dir: tempfile::TempDir,
        user_dir: tempfile::TempDir,
        collab_dir: tempfile::TempDir,
        user_hooks: tempfile::TempDir,
        _collab_hooks: tempfile::TempDir,
        origin: git::Git,
        user: git::Git,
        collab: git::Git,
    }

    impl Fixture {
        async fn new() -> Self {
            let origin_dir = tempfile::tempdir().expect("origin tempdir");
            let user_dir = tempfile::tempdir().expect("user tempdir");
            let collab_dir = tempfile::tempdir().expect("collab tempdir");
            let user_hooks = tempfile::tempdir().expect("user hooks tempdir");
            let collab_hooks = tempfile::tempdir().expect("collab hooks tempdir");

            let origin = new_git(origin_dir.path());
            run_git(&origin, &["init", "-q", "--bare"]).await;
            let origin_url = slash_path(origin_dir.path());

            let user = new_git(user_dir.path());
            run_git(
                &user,
                &["clone", "-q", &origin_url, &slash_path(user_dir.path())],
            )
            .await;
            Self::configure(&user, "user@example.com", "User", user_hooks.path()).await;

            write_file(user_dir.path(), "seed.txt", "seed\n");
            run_git(&user, &["checkout", "-q", "-b", "main"]).await;
            run_git(&user, &["add", "seed.txt"]).await;
            run_git(&user, &["commit", "-qm", "seed"]).await;
            run_git(&user, &["push", "-q", "-u", "origin", "main"]).await;
            run_git(&user, &["push", "-q", "origin", "main:content-main"]).await;
            run_git(&user, &["checkout", "-q", "-b", BRANCH]).await;
            run_git(&user, &["push", "-q", "-u", "origin", BRANCH]).await;
            run_git(&user, &["fetch", "-q", "origin"]).await;

            let collab = new_git(collab_dir.path());
            run_git(
                &collab,
                &["clone", "-q", &origin_url, &slash_path(collab_dir.path())],
            )
            .await;
            Self::configure(
                &collab,
                "collab@example.com",
                "Collaborator",
                collab_hooks.path(),
            )
            .await;

            Self {
                _origin_dir: origin_dir,
                user_dir,
                collab_dir,
                user_hooks,
                _collab_hooks: collab_hooks,
                origin,
                user,
                collab,
            }
        }

        async fn configure(git: &git::Git, email: &str, name: &str, hooks: &Path) {
            run_git(git, &["config", "user.email", email]).await;
            run_git(git, &["config", "user.name", name]).await;
            run_git(git, &["config", "commit.gpgsign", "false"]).await;
            run_git(git, &["config", "core.autocrlf", "false"]).await;
            run_git(git, &["config", "core.hooksPath", &slash_path(hooks)]).await;
        }

        fn user_path(&self) -> &Path {
            self.user_dir.path()
        }

        fn edit(&self, path: &str, contents: &str) {
            write_file(self.user_path(), path, contents);
        }

        fn read(&self, path: &str) -> Vec<u8> {
            fs::read(self.user_path().join(path)).unwrap()
        }

        fn install_user_hook(&self, name: &str, body: &str) {
            let path = self.user_hooks.path().join(name);
            fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
            }
        }

        /// Collaborator commits a new file on `branch` without pushing.
        async fn collab_commit(&self, branch: &str, path: &str) -> String {
            run_git(&self.collab, &["fetch", "-q", "origin"]).await;
            let tracking = format!("origin/{branch}");
            run_git(&self.collab, &["checkout", "-q", "-B", branch, &tracking]).await;
            write_file(self.collab_dir.path(), path, "collab\n");
            run_git(&self.collab, &["add", path]).await;
            run_git(&self.collab, &["commit", "-qm", "collab commit"]).await;
            git_output(&self.collab, &["rev-parse", "HEAD"]).await
        }

        /// Collaborator commits a new file on `branch` and pushes it.
        async fn collab_push(&self, branch: &str, path: &str) -> String {
            let sha = self.collab_commit(branch, path).await;
            let refspec = format!("HEAD:refs/heads/{branch}");
            run_git(&self.collab, &["push", "-q", "origin", &refspec]).await;
            sha
        }

        async fn user_commit(&self, path: &str, message: &str) {
            self.edit(path, "committed\n");
            run_git(&self.user, &["add", path]).await;
            run_git(&self.user, &["commit", "-qm", message]).await;
        }

        async fn head(&self) -> String {
            git_output(&self.user, &["rev-parse", "HEAD"]).await
        }

        async fn origin_sha(&self, branch: &str) -> String {
            let refname = format!("refs/heads/{branch}");
            git_output(&self.origin, &["rev-parse", &refname]).await
        }

        async fn origin_subject(&self, branch: &str) -> String {
            let refname = format!("refs/heads/{branch}");
            git_output(&self.origin, &["log", "-1", "--format=%s", &refname]).await
        }

        async fn stash_count(&self) -> usize {
            git_output(&self.user, &["stash", "list"])
                .await
                .lines()
                .filter(|line| !line.is_empty())
                .count()
        }

        async fn porcelain(&self) -> String {
            git_output(&self.user, &["status", "--porcelain"]).await
        }

        fn op(
            &self,
            files: &[&str],
            message: &str,
        ) -> (
            BranchSubmitOp<UnrealEngineProvider>,
            mpsc::Receiver<String>,
            RepoStatusRef,
        ) {
            let mut app_config = AppConfig::new(crate::APP_NAME);
            app_config.repo_path = self.user_path().to_str().expect("utf8").to_string();
            app_config.target_branch = "main".to_string();
            let repo_config = RepoConfig {
                trunk_branch: "main".to_string(),
                target_branches: ["main", "content-main"]
                    .into_iter()
                    .map(|name| TargetBranchConfig {
                        name: name.to_string(),
                        uses_merge_queue: false,
                        blocked_file_globs: vec![],
                    })
                    .collect(),
                ..Default::default()
            };
            let engine =
                UnrealEngineProvider::new_from_config(app_config.clone(), repo_config.clone());
            let repo_status: RepoStatusRef = Arc::new(RwLock::new(RepoStatus::new()));
            let (sync_phase_tx, phases) = mpsc::channel::<String>();

            let op = BranchSubmitOp {
                files: files.iter().map(|f| f.to_string()).collect(),
                commit_message: message.to_string(),
                expected_branch: None,
                app_config: Arc::new(RwLock::new(app_config)),
                repo_config: Arc::new(RwLock::new(repo_config)),
                repo_status: repo_status.clone(),
                engine,
                git_client: self.user.clone(),
                github_username: "test_user".to_string(),
                sync_phase_tx,
            };
            (op, phases, repo_status)
        }
    }

    fn drain(phases: &mpsc::Receiver<String>) -> Vec<String> {
        phases.try_iter().collect()
    }

    /// Asserts nothing changed: HEAD, working tree status, stash and origin.
    struct Unchanged {
        head: String,
        porcelain: String,
        stash: usize,
        origin: String,
    }

    impl Unchanged {
        async fn capture(fx: &Fixture, branch: &str) -> Self {
            Self {
                head: fx.head().await,
                porcelain: fx.porcelain().await,
                stash: fx.stash_count().await,
                origin: fx.origin_sha(branch).await,
            }
        }

        async fn assert(&self, fx: &Fixture, branch: &str) {
            assert_eq!(fx.head().await, self.head, "HEAD moved");
            assert_eq!(fx.porcelain().await, self.porcelain, "working tree changed");
            assert_eq!(fx.stash_count().await, self.stash, "stash changed");
            assert_eq!(fx.origin_sha(branch).await, self.origin, "origin moved");
        }
    }

    #[test]
    fn is_push_race_matches_rejection_variants() {
        assert!(is_push_race(
            " ! [rejected]        HEAD -> f11r-branch-x (fetch first)"
        ));
        assert!(is_push_race(
            " ! [rejected]        HEAD -> f11r-branch-x (non-fast-forward)"
        ));
        assert!(is_push_race(
            "hint: Updates were rejected because the remote contains work that you do not have locally"
        ));
        assert!(is_push_race(
            "!\tHEAD:refs/heads/f11r-branch-x\t[rejected] (stale info)"
        ));
        assert!(is_push_race(
            " ! [remote rejected] HEAD -> f11r-branch-x (incorrect old value provided)"
        ));
        assert!(is_push_race(
            " ! [remote rejected] HEAD -> f11r-branch-x (cannot lock ref 'refs/heads/f11r-branch-x': is at abc but expected def)"
        ));
        assert!(!is_push_race(
            " ! [remote rejected] HEAD -> f11r-branch-x (pre-receive hook declined)"
        ));
        assert!(!is_push_race(
            "remote: error: GH006: Protected branch update failed for refs/heads/f11r-branch-x."
        ));
        assert!(!is_push_race(
            "fatal: unable to access 'https://github.com/x/y.git/': Could not resolve host: github.com"
        ));
        assert!(!is_push_race(
            "error: failed to push some refs to 'origin'\nhint: pre-push hook declined"
        ));
    }

    #[test]
    fn stale_files_message_truncates_after_ten() {
        let paths: Vec<String> = (0..12).map(|i| format!("missing-{i:02}.txt")).collect();
        let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
        let message = input_message(Err(stale_files_error(&refs)));
        assert_eq!(
            message,
            format!(
                "Some selected files are no longer modified: {}, and 2 more. Refresh and try again.",
                refs[..10].join(", ")
            )
        );
    }

    #[tokio::test]
    async fn fetch_and_compare_reports_behind_after_collaborator_push() {
        let fx = Fixture::new().await;
        fx.collab_push(BRANCH, "collab.txt").await;

        let result = fetch_and_compare_to_origin(&fx.user, BRANCH, "main")
            .await
            .unwrap();
        assert_eq!((result.ahead, result.behind), (0, 1));
        assert_eq!(result.origin_sha, fx.origin_sha(BRANCH).await);
    }

    #[tokio::test]
    async fn fetch_and_compare_reports_ahead_for_unpushed_commit() {
        let fx = Fixture::new().await;
        fx.user_commit("local.txt", "local commit").await;

        let result = fetch_and_compare_to_origin(&fx.user, BRANCH, "main")
            .await
            .unwrap();
        assert_eq!((result.ahead, result.behind), (1, 0));
        assert_eq!(result.origin_sha, fx.origin_sha(BRANCH).await);
    }

    #[tokio::test]
    async fn fetch_and_compare_missing_remote_branch_returns_gone_error() {
        let fx = Fixture::new().await;
        run_git(&fx.collab, &["push", "-q", "origin", "--delete", BRANCH]).await;

        let result = fetch_and_compare_to_origin(&fx.user, BRANCH, "main")
            .await
            .map(|_| ());
        assert_eq!(
            input_message(result),
            "Branch 'f11r-branch-x' does not exist on origin (it may have been merged or deleted). Return to 'main' from Branch Management."
        );
    }

    #[tokio::test]
    async fn push_head_to_branch_fast_forwards() {
        let fx = Fixture::new().await;
        let origin_sha = fx.origin_sha(BRANCH).await;
        fx.user_commit("local.txt", "local commit").await;

        push_head_to_branch(&fx.user, BRANCH, "main", &origin_sha)
            .await
            .unwrap();
        assert_eq!(fx.origin_sha(BRANCH).await, fx.head().await);
    }

    #[tokio::test]
    async fn push_head_to_branch_race_returns_race_error_and_keeps_local_commit() {
        let fx = Fixture::new().await;
        let verified = fx.origin_sha(BRANCH).await;
        let collab_sha = fx.collab_push(BRANCH, "collab.txt").await;
        fx.user_commit("local.txt", "local commit").await;
        let local_head = fx.head().await;

        let result = push_head_to_branch(&fx.user, BRANCH, "main", &verified).await;
        assert_eq!(
            input_message(result),
            "Origin has new commits on 'f11r-branch-x'. Your commit is saved locally. Sync, then push it with Submit to branch."
        );
        assert_eq!(fx.head().await, local_head);
        assert_eq!(
            git_output(&fx.user, &["log", "-1", "--format=%s"]).await,
            "local commit"
        );
        assert_eq!(fx.origin_sha(BRANCH).await, collab_sha);
    }

    #[tokio::test]
    async fn push_head_to_branch_after_branch_deleted_returns_gone_and_does_not_recreate() {
        let fx = Fixture::new().await;
        let verified = fetch_and_compare_to_origin(&fx.user, BRANCH, "main")
            .await
            .unwrap()
            .origin_sha;
        fx.user_commit("local.txt", "local commit").await;
        let local_head = fx.head().await;
        run_git(&fx.collab, &["push", "-q", "origin", "--delete", BRANCH]).await;

        let result = push_head_to_branch(&fx.user, BRANCH, "main", &verified).await;
        assert_eq!(
            input_message(result),
            "Branch 'f11r-branch-x' does not exist on origin (it may have been merged or deleted). Return to 'main' from Branch Management."
        );
        assert!(!fx.origin.has_local_branch(BRANCH).await.unwrap());
        assert_eq!(fx.head().await, local_head);
    }

    #[tokio::test]
    async fn push_head_to_branch_after_branch_moved_returns_race_and_keeps_commit() {
        let fx = Fixture::new().await;
        let verified = fetch_and_compare_to_origin(&fx.user, BRANCH, "main")
            .await
            .unwrap()
            .origin_sha;
        fx.user_commit("local.txt", "local commit").await;
        let local_head = fx.head().await;
        let collab_sha = fx.collab_push(BRANCH, "collab.txt").await;

        let result = push_head_to_branch(&fx.user, BRANCH, "main", &verified).await;
        assert_eq!(
            input_message(result),
            "Origin has new commits on 'f11r-branch-x'. Your commit is saved locally. Sync, then push it with Submit to branch."
        );
        assert_eq!(fx.origin_sha(BRANCH).await, collab_sha);
        assert_eq!(fx.head().await, local_head);
    }

    #[tokio::test]
    async fn push_head_to_branch_other_failure_is_internal_with_git_output() {
        let fx = Fixture::new().await;
        let origin_before = fx.origin_sha(BRANCH).await;
        fx.user_commit("local.txt", "local commit").await;
        fx.install_user_hook("pre-push", "echo hook-says-no >&2\nexit 1");

        match push_head_to_branch(&fx.user, BRANCH, "main", &origin_before).await {
            Err(CoreError::Internal(e)) => {
                let message = e.to_string();
                assert!(message.starts_with("Push failed: "), "{message}");
                assert!(
                    message.ends_with(". Your commit is saved locally."),
                    "{message}"
                );
                assert!(message.contains("hook-says-no"), "{message}");
            }
            other => panic!("expected CoreError::Internal, got {other:?}"),
        }
        assert_eq!(fx.origin_sha(BRANCH).await, origin_before);
    }

    #[tokio::test]
    async fn branch_submit_op_success_commits_and_pushes() {
        let fx = Fixture::new().await;
        fx.edit("seed.txt", "seed\nedited\n");
        fx.edit("notes.txt", "notes\n");
        let stash_before = fx.stash_count().await;
        let message = "[ci] add notes";

        let (op, phases, repo_status) = fx.op(&["seed.txt", "notes.txt"], message);
        op.execute().await.expect("branch submit");

        assert_eq!(fx.origin_sha(BRANCH).await, fx.head().await);
        assert_eq!(fx.origin_subject(BRANCH).await, message);
        assert_eq!(fx.porcelain().await, "");
        {
            let status = repo_status.read();
            assert!(status.modified_files.0.is_empty(), "{status:?}");
            assert!(status.untracked_files.0.is_empty(), "{status:?}");
        }
        assert_eq!(drain(&phases), PHASES_WITH_FILES);
        assert_eq!(fx.stash_count().await, stash_before + 1);
    }

    #[tokio::test]
    async fn branch_submit_op_commits_only_requested_files() {
        let fx = Fixture::new().await;
        fx.edit("seed.txt", "seed\nedited\n");
        fx.edit("notes.txt", "notes\n");
        fx.edit("other.txt", "keep me\n");

        let (op, _phases, _status) = fx.op(&["notes.txt"], "add notes");
        op.execute().await.expect("branch submit");

        assert_eq!(fx.origin_sha(BRANCH).await, fx.head().await);
        let committed = git_output(&fx.user, &["show", "--name-only", "--format=", "HEAD"]).await;
        assert_eq!(committed, "notes.txt");
        assert_eq!(fx.read("seed.txt"), b"seed\nedited\n");
        assert_eq!(fx.read("other.txt"), b"keep me\n");
    }

    #[tokio::test]
    async fn branch_submit_op_refused_on_protected_branch() {
        let fx = Fixture::new().await;
        run_git(&fx.user, &["checkout", "-q", "main"]).await;
        fx.edit("seed.txt", "seed\nedited\n");
        let before = Unchanged::capture(&fx, "main").await;

        let (op, _phases, _status) = fx.op(&["seed.txt"], "msg");
        assert_eq!(
            input_message(op.execute().await),
            "'main' is a protected branch. Use Quick Submit to change it."
        );
        before.assert(&fx, "main").await;
    }

    #[tokio::test]
    async fn branch_submit_op_refused_on_quick_submit_branch() {
        let fx = Fixture::new().await;
        run_git(&fx.user, &["checkout", "-q", "-b", "f11r-main-x", "main"]).await;
        fx.edit("seed.txt", "seed\nedited\n");
        let before = Unchanged::capture(&fx, "main").await;

        let (op, _phases, _status) = fx.op(&["seed.txt"], "msg");
        assert_eq!(
            input_message(op.execute().await),
            "'f11r-main-x' is a Quick Submit branch. Use Quick Submit, or Sync to return to 'main'."
        );
        before.assert(&fx, "main").await;
        assert!(
            !fx.origin
                .has_local_branch("f11r-main-x")
                .await
                .expect("branch lookup"),
            "Quick Submit branch reached origin"
        );
    }

    #[tokio::test]
    async fn branch_submit_op_refused_when_detached() {
        let fx = Fixture::new().await;
        run_git(&fx.user, &["checkout", "-q", "--detach"]).await;
        let before = Unchanged::capture(&fx, BRANCH).await;

        let (op, _phases, _status) = fx.op(&[], "");
        assert_eq!(
            input_message(op.execute().await),
            "Repository is in a detached HEAD state. Check out a branch before continuing."
        );
        before.assert(&fx, BRANCH).await;
    }

    #[tokio::test]
    async fn branch_submit_op_rejects_unlocked_lockable_file() {
        let fx = Fixture::new().await;
        fx.edit("Content/New.uasset", "asset\n");
        let before = Unchanged::capture(&fx, BRANCH).await;

        let (op, _phases, _status) = fx.op(&["Content/New.uasset"], "add asset");
        let message = input_message(op.execute().await);
        assert!(
            message.starts_with("Some files are not allowed to be submitted:"),
            "{message}"
        );
        assert!(message.contains("checked out (locked)"), "{message}");
        assert!(message.contains("Content/New.uasset"), "{message}");
        before.assert(&fx, BRANCH).await;
    }

    #[tokio::test]
    async fn branch_submit_op_rejects_stale_selection() {
        let fx = Fixture::new().await;
        fx.edit("notes.txt", "notes\n");
        let before = Unchanged::capture(&fx, BRANCH).await;

        let (op, _phases, _status) = fx.op(&["notes.txt", "seed.txt"], "msg");
        assert_eq!(
            input_message(op.execute().await),
            "Some selected files are no longer modified: seed.txt. Refresh and try again."
        );
        before.assert(&fx, BRANCH).await;

        let stale: Vec<String> = (0..12).map(|i| format!("missing-{i:02}.txt")).collect();
        let stale_refs: Vec<&str> = stale.iter().map(String::as_str).collect();
        let (op, _phases, _status) = fx.op(&stale_refs, "msg");
        let message = input_message(op.execute().await);
        assert!(
            message.ends_with("missing-09.txt, and 2 more. Refresh and try again."),
            "{message}"
        );
        assert!(!message.contains("missing-10.txt"), "{message}");
        before.assert(&fx, BRANCH).await;
    }

    #[tokio::test]
    async fn branch_submit_op_rejects_conflict_markers() {
        let fx = Fixture::new().await;
        fx.edit(
            "conflicted.txt",
            "<<<<<<< ours\nmine\n=======\ntheirs\n>>>>>>> theirs\n",
        );
        let before = Unchanged::capture(&fx, BRANCH).await;

        let (op, _phases, _status) = fx.op(&["conflicted.txt"], "msg");
        let message = input_message(op.execute().await);
        assert!(
            message.starts_with(
                "Cannot submit: 1 file(s) still contain unresolved merge conflict markers:"
            ),
            "{message}"
        );
        assert!(message.contains("conflicted.txt"), "{message}");
        before.assert(&fx, BRANCH).await;
    }

    #[tokio::test]
    async fn branch_submit_op_refuses_when_behind() {
        let fx = Fixture::new().await;
        let collab_sha = fx.collab_push(BRANCH, "collab.txt").await;
        fx.edit("seed.txt", "seed\nedited\n");
        fx.edit("notes.txt", "notes\n");
        let before = Unchanged::capture(&fx, BRANCH).await;

        let (op, phases, _status) = fx.op(&["seed.txt", "notes.txt"], "msg");
        assert_eq!(
            input_message(op.execute().await),
            "Branch has new commits on origin — Sync before submitting."
        );
        before.assert(&fx, BRANCH).await;
        assert_eq!(fx.origin_sha(BRANCH).await, collab_sha);
        assert_eq!(fx.read("seed.txt"), b"seed\nedited\n");
        assert_eq!(fx.read("notes.txt"), b"notes\n");
        assert_eq!(drain(&phases), PHASES_WITH_FILES[..1]);
    }

    #[tokio::test]
    async fn branch_submit_op_push_only_pushes_unpushed_commit() {
        let fx = Fixture::new().await;
        fx.user_commit("local.txt", "kept commit").await;
        let stash_before = fx.stash_count().await;

        let (op, phases, _status) = fx.op(&[], "");
        op.execute().await.expect("push-only submit");

        assert_eq!(fx.origin_sha(BRANCH).await, fx.head().await);
        assert_eq!(fx.origin_subject(BRANCH).await, "kept commit");
        assert_eq!(fx.stash_count().await, stash_before);
        assert_eq!(
            drain(&phases),
            [
                "Checking branch is up to date with origin",
                "Checking repository status",
                "Pushing to origin",
            ]
        );
    }

    #[tokio::test]
    async fn branch_submit_op_push_only_with_nothing_to_push() {
        let fx = Fixture::new().await;
        fx.edit("notes.txt", "notes\n");
        let before = Unchanged::capture(&fx, BRANCH).await;

        let (op, _phases, _status) = fx.op(&[], "");
        assert_eq!(input_message(op.execute().await), "No files to submit");
        before.assert(&fx, BRANCH).await;
    }

    #[tokio::test]
    async fn branch_submit_op_empty_message_with_files() {
        let fx = Fixture::new().await;
        fx.edit("notes.txt", "notes\n");
        let before = Unchanged::capture(&fx, BRANCH).await;

        let (op, _phases, _status) = fx.op(&["notes.txt"], "  ");
        assert_eq!(
            input_message(op.execute().await),
            "Commit message is required"
        );
        before.assert(&fx, BRANCH).await;
    }

    #[tokio::test]
    async fn branch_submit_op_refuses_when_branch_gone() {
        let fx = Fixture::new().await;
        run_git(&fx.collab, &["push", "-q", "origin", "--delete", BRANCH]).await;
        fx.edit("notes.txt", "notes\n");
        let head = fx.head().await;
        let stash = fx.stash_count().await;

        let (op, _phases, _status) = fx.op(&["notes.txt"], "msg");
        assert_eq!(
            input_message(op.execute().await),
            "Branch 'f11r-branch-x' does not exist on origin (it may have been merged or deleted). Return to 'main' from Branch Management."
        );
        assert_eq!(fx.head().await, head);
        assert_eq!(fx.stash_count().await, stash);
        assert!(!fx.origin.has_local_branch(BRANCH).await.unwrap());
    }

    /// The race is injected from a pre-commit hook, after the up-to-date check and before the
    /// push. A pre-push hook is too late: the client has already read origin's refs, so the
    /// server rejects with "incorrect old value provided" rather than a fetch-first rejection.
    #[tokio::test]
    async fn branch_submit_op_push_race_keeps_commit() {
        let fx = Fixture::new().await;
        let collab_sha = fx.collab_commit(BRANCH, "collab.txt").await;
        fx.install_user_hook(
            "pre-commit",
            &format!(
                "unset GIT_DIR GIT_INDEX_FILE GIT_WORK_TREE GIT_PREFIX\ngit -C '{}' push -q origin HEAD:refs/heads/{BRANCH}",
                slash_path(fx.collab_dir.path())
            ),
        );
        fx.edit("notes.txt", "notes\n");
        fx.edit("unsubmitted.txt", "not submitted\n");
        let head_before = fx.head().await;

        let (op, phases, _status) = fx.op(&["notes.txt"], "race commit");
        assert_eq!(
            input_message(op.execute().await),
            "Origin has new commits on 'f11r-branch-x'. Your commit is saved locally. Sync, then push it with Submit to branch."
        );

        assert_eq!(fx.origin_sha(BRANCH).await, collab_sha);
        assert_eq!(
            git_output(&fx.user, &["rev-parse", "HEAD~1"]).await,
            head_before
        );
        assert_eq!(
            git_output(&fx.user, &["log", "-1", "--format=%s"]).await,
            "race commit"
        );
        assert_eq!(fx.read("notes.txt"), b"notes\n");
        assert_eq!(fx.read("unsubmitted.txt"), b"not submitted\n");
        assert_eq!(fx.porcelain().await, "?? unsubmitted.txt");
        assert_eq!(drain(&phases), PHASES_WITH_FILES);
    }

    /// Origin's branch is deleted from a pre-commit hook, after the up-to-date check and before
    /// the push. The lease must stop the push from recreating it.
    #[tokio::test]
    async fn branch_submit_op_branch_deleted_before_push_is_not_recreated() {
        let fx = Fixture::new().await;
        fx.install_user_hook(
            "pre-commit",
            &format!(
                "unset GIT_DIR GIT_INDEX_FILE GIT_WORK_TREE GIT_PREFIX\ngit -C '{}' push -q origin --delete {BRANCH}",
                slash_path(fx.collab_dir.path())
            ),
        );
        fx.edit("notes.txt", "notes\n");
        let head_before = fx.head().await;

        let (op, phases, _status) = fx.op(&["notes.txt"], "orphan commit");
        assert_eq!(
            input_message(op.execute().await),
            "Branch 'f11r-branch-x' does not exist on origin (it may have been merged or deleted). Return to 'main' from Branch Management."
        );

        assert!(!fx.origin.has_local_branch(BRANCH).await.unwrap());
        assert_eq!(
            git_output(&fx.user, &["rev-parse", "HEAD~1"]).await,
            head_before
        );
        assert_eq!(
            git_output(&fx.user, &["log", "-1", "--format=%s"]).await,
            "orphan commit"
        );
        assert_eq!(drain(&phases), PHASES_WITH_FILES);
    }

    #[tokio::test]
    async fn branch_submit_op_refuses_when_branch_changed_since_review() {
        let fx = Fixture::new().await;
        fx.edit("notes.txt", "notes\n");
        let before = Unchanged::capture(&fx, BRANCH).await;

        let (mut op, phases, _status) = fx.op(&["notes.txt"], "msg");
        op.expected_branch = Some("f11r-branch-other".to_string());
        assert_eq!(
            input_message(op.execute().await),
            "The current branch changed to 'f11r-branch-x' since you reviewed this submit. Review it again."
        );
        before.assert(&fx, BRANCH).await;
        assert!(drain(&phases).is_empty());

        let (mut op, _phases, _status) = fx.op(&["notes.txt"], "msg");
        op.expected_branch = Some(BRANCH.to_string());
        op.execute().await.expect("branch submit");
        assert_eq!(fx.origin_sha(BRANCH).await, fx.head().await);
    }

    #[tokio::test]
    async fn branch_submit_op_commit_failure_restores_snapshot() {
        let fx = Fixture::new().await;
        let (probe, _phases, _status) = fx.op(&[], "");
        if probe.engine.check_ready_to_sync_repo().await.is_err() {
            eprintln!("skipping: Unreal Editor is running, so the snapshot is not restored");
            return;
        }

        fx.install_user_hook("pre-commit", "exit 1");
        fx.edit("seed.txt", "seed\nlocal edit\n");
        fx.edit("notes.txt", "untracked notes\n");
        let head = fx.head().await;
        let origin = fx.origin_sha(BRANCH).await;
        let stash = fx.stash_count().await;

        let (op, _phases, _status) = fx.op(&["seed.txt", "notes.txt"], "doomed");
        assert!(op.execute().await.is_err(), "commit should have failed");

        assert_eq!(fx.head().await, head);
        assert_eq!(fx.origin_sha(BRANCH).await, origin);
        assert_eq!(fx.read("seed.txt"), b"seed\nlocal edit\n");
        assert_eq!(fx.read("notes.txt"), b"untracked notes\n");
        assert_eq!(fx.stash_count().await, stash + 1);
    }
}
