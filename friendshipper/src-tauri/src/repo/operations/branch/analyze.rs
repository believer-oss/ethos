use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::anyhow;
use axum::extract::State;
use axum::{async_trait, Json};
use tracing::instrument;

use super::{branch_management_trunk, run_sequence, trunk_uses_merge_queue, GITHUB_PAT_MISSING};
use crate::engine::{CommunicationType, EngineProvider};
use crate::repo::operations::is_valid_sha;
use crate::state::AppState;
use ethos_core::clients::git::{self, GitVersionCheck, MergeTreeOutcome};
use ethos_core::operations::parse_log_output;
use ethos_core::types::config::{AppConfigRef, RepoConfigRef};
use ethos_core::types::errors::CoreError;
use ethos_core::types::repo::{
    BranchAnalysis, BranchLocalSync, BranchPullRequest, BranchUpstream, FileTouch, LockableFileRow,
};
use ethos_core::worker::Task;

const LOCKABLE_ROW_CAP: usize = 1000;
const LOG_SCAN_LIMIT: usize = 2000;
const COMMIT_LIST_LIMIT: usize = 200;

pub(super) fn quiet<'a>() -> git::Opts<'a> {
    git::Opts::new_without_logs().with_skip_notify_frontend()
}

/// Whether a merged PR whose head is `head_ref_oid` contains the current `origin/<branch>` tip.
/// Any failure means "not merged".
pub(crate) async fn merged_pr_covers_tip(git: &git::Git, branch: &str, head_ref_oid: &str) -> bool {
    let tracking_ref = format!("refs/remotes/origin/{branch}");
    let tracking_commit = format!("{tracking_ref}^{{commit}}");
    if git
        .run(
            &["rev-parse", "--verify", "--quiet", &tracking_commit],
            quiet(),
        )
        .await
        .is_err()
    {
        return false;
    }

    merged_pr_covers_commit(git, &tracking_ref, head_ref_oid).await
}

/// Whether `head_ref_oid` (fetched from origin when missing locally) contains `tip`.
/// Any failure means "not merged".
pub(crate) async fn merged_pr_covers_commit(git: &git::Git, tip: &str, head_ref_oid: &str) -> bool {
    if !is_valid_sha(head_ref_oid) {
        return false;
    }

    let oid_commit = format!("{head_ref_oid}^{{commit}}");
    if git
        .run(&["cat-file", "-e", &oid_commit], quiet())
        .await
        .is_err()
    {
        let _ = git
            .run(
                &[
                    "fetch",
                    "--no-auto-maintenance",
                    "--no-write-fetch-head",
                    "origin",
                    head_ref_oid,
                ],
                quiet(),
            )
            .await;
        if git
            .run(&["cat-file", "-e", &oid_commit], quiet())
            .await
            .is_err()
        {
            return false;
        }
    }

    git.run(&["merge-base", "--is-ancestor", tip, head_ref_oid], quiet())
        .await
        .is_ok()
}

/// Drops a MERGED PR that does not describe the current branch (reused branch names).
async fn apply_merged_rule(
    git: &git::Git,
    branch: &str,
    upstream: BranchUpstream,
    pr: Option<BranchPullRequest>,
) -> Option<BranchPullRequest> {
    let pr = pr?;
    if pr.state != "MERGED" {
        return Some(pr);
    }
    match upstream {
        BranchUpstream::Gone => Some(pr),
        BranchUpstream::NotPushed => None,
        BranchUpstream::Present => merged_pr_covers_tip(git, branch, &pr.head_ref_oid)
            .await
            .then_some(pr),
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct BranchFacts {
    pub git: GitVersionCheck,
    pub upstream: BranchUpstream,
    pub local_sync: BranchLocalSync,
    pub local_ahead: u32,
    pub local_behind: u32,
    pub commits_ahead: u32,
    pub commits_behind: u32,
    pub merge_base: Option<String>,
    pub predicted_conflicts: Vec<String>,
}

fn local_sync_from(ahead: u32, behind: u32) -> BranchLocalSync {
    match (ahead > 0, behind > 0) {
        (false, false) => BranchLocalSync::InSync,
        (true, false) => BranchLocalSync::Ahead,
        (false, true) => BranchLocalSync::Behind,
        (true, true) => BranchLocalSync::Diverged,
    }
}

async fn fetch_trunk_and_branch(
    git: &git::Git,
    trunk: &str,
    branch: Option<&str>,
    opts: git::Opts<'_>,
) -> anyhow::Result<()> {
    let mut refspecs = vec![format!("+refs/heads/{trunk}:refs/remotes/origin/{trunk}")];
    if let Some(branch) = branch {
        refspecs.push(format!("+refs/heads/{branch}:refs/remotes/origin/{branch}"));
    }
    let mut args = vec![
        "fetch",
        "--no-auto-maintenance",
        "--no-write-fetch-head",
        "origin",
    ];
    args.extend(refspecs.iter().map(String::as_str));
    git.run(&args, opts.with_complete_error()).await
}

/// Fetches `<trunk>` (and `<branch>` when on origin) and compares them, without touching the
/// working tree, index, stash or local branches.
pub(crate) async fn collect_branch_facts(
    git: &git::Git,
    trunk: &str,
    branch: &str,
    opts: git::Opts<'_>,
) -> Result<BranchFacts, CoreError> {
    let check = git.check_min_version().await?;
    if !check.supported {
        return Ok(BranchFacts {
            git: check,
            ..Default::default()
        });
    }

    if git.current_branch().await? != branch {
        return Err(CoreError::Input(anyhow!(
            "The checked-out branch changed. Refresh."
        )));
    }

    let mut present = git.has_remote_branch_exact(branch).await?;

    if let Err(e) = fetch_trunk_and_branch(git, trunk, present.then_some(branch), opts).await {
        // The branch can vanish between ls-remote and fetch.
        if !(present && e.to_string().contains("couldn't find remote ref")) {
            return Err(e.into());
        }
        present = false;
        fetch_trunk_and_branch(git, trunk, None, opts).await?;
    }

    if !present {
        let merge_key = format!("branch.{branch}.merge");
        let merge = git
            .run_and_collect_output(&["config", "--get", &merge_key], opts)
            .await?;
        let upstream =
            if !merge.trim().is_empty() || git.has_local_remote_tracking_branch(branch).await? {
                BranchUpstream::Gone
            } else {
                BranchUpstream::NotPushed
            };
        return Ok(BranchFacts {
            git: check,
            upstream,
            local_sync: BranchLocalSync::Unknown,
            ..Default::default()
        });
    }

    let trunk_ref = format!("refs/remotes/origin/{trunk}");
    let branch_ref = format!("refs/remotes/origin/{branch}");

    let merge_base = git
        .run_and_collect_output(&["merge-base", &trunk_ref, &branch_ref], opts)
        .await
        .map_err(|e| {
            CoreError::Internal(anyhow!("{branch} has no common history with {trunk}: {e}"))
        })?
        .trim()
        .to_string();

    let (commits_ahead, commits_behind) = git.get_ahead_behind(&trunk_ref, &branch_ref).await?;
    let local_ref = format!("refs/heads/{branch}");
    let (local_ahead, local_behind) = git.get_ahead_behind(&branch_ref, &local_ref).await?;

    let predicted_conflicts = if commits_ahead > 0 {
        match git.predict_merge_conflicts(&trunk_ref, &branch_ref).await? {
            MergeTreeOutcome::Clean => vec![],
            MergeTreeOutcome::Conflicts(paths) => paths,
        }
    } else {
        vec![]
    };

    Ok(BranchFacts {
        git: check,
        upstream: BranchUpstream::Present,
        local_sync: local_sync_from(local_ahead, local_behind),
        local_ahead,
        local_behind,
        commits_ahead,
        commits_behind,
        merge_base: Some(merge_base),
        predicted_conflicts,
    })
}

/// One row per branch-side lockable path, both-sided changes first, then by path.
pub(crate) fn assemble_lockable_rows(
    branch_paths: &[String],
    branch_touch: &HashMap<String, FileTouch>,
    main_set: &HashSet<String>,
    main_touch: &HashMap<String, FileTouch>,
    cap: usize,
) -> Vec<LockableFileRow> {
    let mut rows: Vec<LockableFileRow> = branch_paths
        .iter()
        .map(|path| {
            let changed_on_main = main_set.contains(path);
            LockableFileRow {
                path: path.clone(),
                display_name: String::new(),
                branch_commit: branch_touch.get(path).cloned(),
                changed_on_main,
                main_commit: if changed_on_main {
                    main_touch.get(path).cloned()
                } else {
                    None
                },
            }
        })
        .collect();
    rows.sort_by_cached_key(|row| (!row.changed_on_main, row.path.to_lowercase()));
    rows.truncate(cap);
    rows
}

#[derive(Clone)]
pub struct BranchAnalysisOp<T> {
    pub git_client: git::Git,
    pub engine: T,
    pub app_config: AppConfigRef,
    pub repo_config: RepoConfigRef,
    pub trunk_branch: String,
    pub branch: String,
    pub result: Arc<parking_lot::Mutex<Option<BranchAnalysis>>>,
}

impl<T> BranchAnalysisOp<T>
where
    T: EngineProvider,
{
    /// Lockable paths changed between `merge_base` and `tip`, as git prints them.
    async fn lockable_paths_changed(
        &self,
        merge_base: &str,
        tip: &str,
    ) -> Result<Vec<String>, CoreError> {
        let output = self
            .git_client
            .run_and_collect_output(
                &[
                    "-c",
                    "core.quotePath=false",
                    "diff",
                    "--name-only",
                    "--no-renames",
                    merge_base,
                    tip,
                    "--",
                ],
                quiet(),
            )
            .await?;
        let mut seen = HashSet::new();
        Ok(output
            .lines()
            .filter(|path| !path.is_empty() && self.engine.is_lockable_file(path))
            .filter(|path| seen.insert(path.to_string()))
            .map(str::to_string)
            .collect())
    }

    async fn fill_lockable_files(
        &self,
        analysis: &mut BranchAnalysis,
        merge_base: &str,
        trunk_ref: &str,
        branch_ref: &str,
    ) -> Result<(), CoreError> {
        let branch_paths = self.lockable_paths_changed(merge_base, branch_ref).await?;
        if branch_paths.is_empty() {
            return Ok(());
        }

        let branch_set: HashSet<&String> = branch_paths.iter().collect();
        let main_set: HashSet<String> = self
            .lockable_paths_changed(merge_base, trunk_ref)
            .await?
            .into_iter()
            .filter(|path| branch_set.contains(path))
            .collect();

        let branch_touch = self
            .git_client
            .last_touch_by_path(&format!("{merge_base}..{branch_ref}"), LOG_SCAN_LIMIT)
            .await?;
        let main_touch = if main_set.is_empty() {
            HashMap::new()
        } else {
            self.git_client
                .last_touch_by_path(&format!("{merge_base}..{trunk_ref}"), LOG_SCAN_LIMIT)
                .await?
        };

        let mut rows = assemble_lockable_rows(
            &branch_paths,
            &branch_touch,
            &main_set,
            &main_touch,
            LOCKABLE_ROW_CAP,
        );

        let engine_path = self
            .app_config
            .read()
            .load_engine_path_from_repo(&self.repo_config.read())
            .unwrap_or_default();
        let paths: Vec<String> = rows.iter().map(|row| row.path.clone()).collect();
        let names = self
            .engine
            .get_asset_display_names(CommunicationType::None, &engine_path, &paths)
            .await;
        if names.len() == rows.len() {
            for (row, name) in rows.iter_mut().zip(names) {
                row.display_name = name;
            }
        }

        analysis.lockable_files = rows;
        analysis.lockable_file_count = branch_paths.len() as u32;
        Ok(())
    }
}

#[async_trait]
impl<T> Task for BranchAnalysisOp<T>
where
    T: EngineProvider,
{
    #[instrument(name = "BranchAnalysisOp::execute", skip(self))]
    async fn execute(&self) -> Result<(), CoreError> {
        let facts =
            collect_branch_facts(&self.git_client, &self.trunk_branch, &self.branch, quiet())
                .await?;

        let uses_merge_queue = trunk_uses_merge_queue(&self.repo_config.read(), &self.trunk_branch);

        let mut analysis = BranchAnalysis {
            git_supported: facts.git.supported,
            git_version: facts.git.version.clone(),
            branch: self.branch.clone(),
            trunk_branch: self.trunk_branch.clone(),
            uses_merge_queue,
            upstream: facts.upstream,
            local_sync: facts.local_sync,
            local_ahead_of_origin: facts.local_ahead,
            local_behind_origin: facts.local_behind,
            commits_ahead: facts.commits_ahead,
            commits_behind: facts.commits_behind,
            merge_base: facts.merge_base.clone(),
            predicted_conflicts: facts.predicted_conflicts.clone(),
            ..Default::default()
        };

        if facts.git.supported
            && facts.upstream == BranchUpstream::Present
            && facts.commits_ahead > 0
        {
            let trunk_ref = format!("refs/remotes/origin/{}", self.trunk_branch);
            let branch_ref = format!("refs/remotes/origin/{}", self.branch);

            let log = self
                .git_client
                .log(COMMIT_LIST_LIMIT, &format!("{trunk_ref}..{branch_ref}"))
                .await?;
            analysis.commits = parse_log_output(&log);

            if let Some(merge_base) = &facts.merge_base {
                self.fill_lockable_files(&mut analysis, merge_base, &trunk_ref, &branch_ref)
                    .await?;
            }
        }

        *self.result.lock() = Some(analysis);
        Ok(())
    }

    fn get_name(&self) -> String {
        String::from("BranchAnalysisOp")
    }
}

/// Runs the merged rule on the worker so its fetch and ref reads never interleave with Sync.
#[derive(Clone)]
struct MergedRuleOp {
    git_client: git::Git,
    branch: String,
    upstream: BranchUpstream,
    pr: Option<BranchPullRequest>,
    result: Arc<parking_lot::Mutex<Option<BranchPullRequest>>>,
}

#[async_trait]
impl Task for MergedRuleOp {
    #[instrument(name = "MergedRuleOp::execute", skip(self))]
    async fn execute(&self) -> Result<(), CoreError> {
        *self.result.lock() = apply_merged_rule(
            &self.git_client,
            &self.branch,
            self.upstream,
            self.pr.clone(),
        )
        .await;
        Ok(())
    }

    fn get_name(&self) -> String {
        String::from("MergedRuleOp")
    }
}

#[instrument(skip(state))]
pub async fn branch_analysis_handler<T>(
    State(state): State<AppState<T>>,
) -> Result<Json<BranchAnalysis>, CoreError>
where
    T: EngineProvider,
{
    let (branch, owner, repo) = {
        let status = state.repo_status.read();
        (
            status.branch.clone(),
            status.repo_owner.clone(),
            status.repo_name.clone(),
        )
    };
    let trunk = branch_management_trunk(&branch, &state.repo_config.read())?;

    let result = Arc::new(parking_lot::Mutex::new(None));
    let op = BranchAnalysisOp {
        git_client: state.git(),
        engine: state.engine.clone(),
        app_config: state.app_config.clone(),
        repo_config: state.repo_config.clone(),
        trunk_branch: trunk.clone(),
        branch: branch.clone(),
        result: result.clone(),
    };

    // The GitHub lookup runs off the worker so a slow GitHub never delays Sync or status.
    let github_client = state.github_client.read().clone();
    let pr_future = async {
        let Some(client) = github_client else {
            return (None, Some(GITHUB_PAT_MISSING.to_string()));
        };
        if owner.is_empty() || repo.is_empty() {
            return (
                None,
                Some("Repository information not yet available".to_string()),
            );
        }
        match client
            .get_branch_pull_request(&owner, &repo, &branch, &trunk)
            .await
        {
            Ok(pr) => (pr, None),
            Err(e) => (None, Some(e.to_string())),
        }
    };

    let (worker, (pr, pr_error)) = tokio::join!(run_sequence(&state, Box::new(op)), pr_future);
    worker?;

    let mut analysis = result
        .lock()
        .take()
        .ok_or_else(|| CoreError::Internal(anyhow!("Branch analysis produced no result")))?;

    let needs_tip_check = analysis.upstream == BranchUpstream::Present
        && pr.as_ref().is_some_and(|pr| pr.state == "MERGED");
    analysis.pr = if needs_tip_check {
        let merged = Arc::new(parking_lot::Mutex::new(None));
        let op = MergedRuleOp {
            git_client: state.git(),
            branch,
            upstream: analysis.upstream,
            pr,
            result: merged.clone(),
        };
        run_sequence(&state, Box::new(op)).await?;
        let pr = merged.lock().take();
        pr
    } else {
        apply_merged_rule(&state.git(), &branch, analysis.upstream, pr).await
    };
    analysis.pr_error = pr_error;

    Ok(Json(analysis))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Write;
    use std::path::Path;
    use std::process::{Command, Stdio};
    use std::sync::mpsc;

    use parking_lot::RwLock;

    use super::*;
    use crate::engine::UnrealEngineProvider;
    use ethos_core::types::config::{AppConfig, RepoConfig, TargetBranchConfig};

    const BRANCH: &str = "f11r-branch-x";
    const MAIN_REF: &str = "refs/remotes/origin/main";
    const BRANCH_REF: &str = "refs/remotes/origin/f11r-branch-x";

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

    /// Runs git directly (no wrapper) for hand cross-checks; returns (exit code, stdout).
    fn raw_git(dir: &Path, args: &[&str]) -> (Option<i32>, Vec<u8>) {
        let out = Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("spawn git");
        (out.status.code(), out.stdout)
    }

    fn write_bytes(dir: &Path, path: &str, contents: &[u8]) {
        let full = dir.join(path);
        fs::create_dir_all(full.parent().expect("parent dir")).unwrap();
        fs::write(full, contents).unwrap();
    }

    fn slash_path(path: &Path) -> String {
        path.to_str().expect("utf8 path").replace('\\', "/")
    }

    fn touch(sha: &str, author: &str) -> FileTouch {
        FileTouch {
            sha: sha.to_string(),
            author: author.to_string(),
            date: "2026-01-01T00:00:00+00:00".to_string(),
        }
    }

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    /// A bare origin with `main` (seed.txt, Content/Rename.uasset) and `f11r-branch-x`; a user
    /// clone on `f11r-branch-x` tracking origin; and a collaborator clone. Hooks come only from
    /// per-clone fixture dirs.
    struct Fixture {
        _origin_dir: tempfile::TempDir,
        user_dir: tempfile::TempDir,
        collab_dir: tempfile::TempDir,
        _user_hooks: tempfile::TempDir,
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

            write_bytes(user_dir.path(), "seed.txt", b"one\ntwo\nthree\n");
            write_bytes(user_dir.path(), "Content/Rename.uasset", b"\0base asset\n");
            run_git(&user, &["checkout", "-q", "-b", "main"]).await;
            run_git(&user, &["add", "."]).await;
            run_git(&user, &["commit", "-qm", "seed"]).await;
            run_git(&user, &["push", "-q", "-u", "origin", "main"]).await;
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
                _user_hooks: user_hooks,
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

        /// Whether the local git supports `merge-tree --write-tree`; tests return early if not.
        async fn git_supported(&self) -> bool {
            let check = self.git_check().await;
            if !check.supported {
                println!(
                    "skipping: git {} lacks merge-tree --write-tree",
                    check.version
                );
            }
            check.supported
        }

        async fn git_check(&self) -> GitVersionCheck {
            self.user.check_min_version().await.expect("git version")
        }

        async fn facts(&self) -> Result<BranchFacts, CoreError> {
            self.facts_for("main", BRANCH).await
        }

        async fn facts_for(&self, trunk: &str, branch: &str) -> Result<BranchFacts, CoreError> {
            collect_branch_facts(&self.user, trunk, branch, quiet()).await
        }

        /// Collaborator writes `path` on `branch` (from origin) and pushes; returns the sha.
        async fn collab_push(&self, branch: &str, path: &str, contents: &[u8]) -> String {
            run_git(&self.collab, &["fetch", "-q", "origin"]).await;
            let tracking = format!("origin/{branch}");
            run_git(&self.collab, &["checkout", "-q", "-B", branch, &tracking]).await;
            write_bytes(self.collab_dir.path(), path, contents);
            run_git(&self.collab, &["add", path]).await;
            run_git(&self.collab, &["commit", "-qm", &format!("collab {path}")]).await;
            let refspec = format!("HEAD:refs/heads/{branch}");
            run_git(&self.collab, &["push", "-q", "origin", &refspec]).await;
            git_output(&self.collab, &["rev-parse", "HEAD"]).await
        }

        /// Collaborator renames `from` to `to` on `branch` and pushes.
        async fn collab_rename(&self, branch: &str, from: &str, to: &str) {
            run_git(&self.collab, &["fetch", "-q", "origin"]).await;
            let tracking = format!("origin/{branch}");
            run_git(&self.collab, &["checkout", "-q", "-B", branch, &tracking]).await;
            run_git(&self.collab, &["mv", from, to]).await;
            run_git(&self.collab, &["commit", "-qm", "collab rename"]).await;
            let refspec = format!("HEAD:refs/heads/{branch}");
            run_git(&self.collab, &["push", "-q", "origin", &refspec]).await;
        }

        /// User commits `path` on the current branch without pushing; returns the sha.
        async fn user_commit(&self, path: &str, contents: &[u8], message: &str) -> String {
            write_bytes(self.user_path(), path, contents);
            run_git(&self.user, &["add", path]).await;
            run_git(&self.user, &["commit", "-qm", message]).await;
            self.head().await
        }

        async fn user_push(&self) {
            let refspec = format!("HEAD:refs/heads/{BRANCH}");
            run_git(&self.user, &["push", "-q", "origin", &refspec]).await;
        }

        async fn head(&self) -> String {
            git_output(&self.user, &["rev-parse", "HEAD"]).await
        }

        async fn origin_sha(&self, branch: &str) -> String {
            let refname = format!("refs/heads/{branch}");
            git_output(&self.origin, &["rev-parse", &refname]).await
        }

        fn op(&self, targets: &[(&str, bool)]) -> BranchAnalysisOp<UnrealEngineProvider> {
            let mut app_config = AppConfig::new(crate::APP_NAME);
            app_config.repo_path = self.user_path().to_str().expect("utf8").to_string();
            app_config.target_branch = "main".to_string();
            let repo_config = RepoConfig {
                trunk_branch: "main".to_string(),
                target_branches: targets
                    .iter()
                    .map(|(name, uses_merge_queue)| TargetBranchConfig {
                        name: name.to_string(),
                        uses_merge_queue: *uses_merge_queue,
                        blocked_file_globs: vec![],
                    })
                    .collect(),
                ..Default::default()
            };
            let engine =
                UnrealEngineProvider::new_from_config(app_config.clone(), repo_config.clone());
            BranchAnalysisOp {
                git_client: self.user.clone(),
                engine,
                app_config: Arc::new(RwLock::new(app_config)),
                repo_config: Arc::new(RwLock::new(repo_config)),
                trunk_branch: "main".to_string(),
                branch: BRANCH.to_string(),
                result: Arc::new(parking_lot::Mutex::new(None)),
            }
        }

        async fn analyze(&self, targets: &[(&str, bool)]) -> BranchAnalysis {
            let op = self.op(targets);
            op.execute().await.expect("analysis");
            let analysis = op.result.lock().take();
            analysis.expect("analysis result")
        }
    }

    /// Local state the analysis must never change; only `refs/remotes/origin/*` may move.
    #[derive(Debug, PartialEq, Eq)]
    struct LocalState {
        head: String,
        porcelain: String,
        stash: String,
        heads: String,
    }

    impl LocalState {
        async fn capture(fx: &Fixture) -> Self {
            Self {
                head: fx.head().await,
                porcelain: git_output(&fx.user, &["status", "--porcelain"]).await,
                stash: git_output(&fx.user, &["stash", "list"]).await,
                heads: git_output(
                    &fx.user,
                    &[
                        "for-each-ref",
                        "--format=%(refname) %(objectname)",
                        "refs/heads",
                    ],
                )
                .await,
            }
        }
    }

    fn assert_input(result: Result<BranchFacts, CoreError>, expected: &str) {
        match result {
            Err(CoreError::Input(e)) => assert_eq!(e.to_string(), expected),
            other => panic!("expected Input error, got {other:?}"),
        }
    }

    fn assert_internal(result: Result<BranchFacts, CoreError>, contains: &str) {
        match result {
            Err(CoreError::Internal(e)) => assert!(e.to_string().contains(contains), "{e}"),
            other => panic!("expected Internal error, got {other:?}"),
        }
    }

    // assemble_lockable_rows

    #[test]
    fn rows_flag_both_sided_and_attach_touch() {
        let branch_paths = strings(&["Content/b.uasset", "Content/A.uasset"]);
        let branch_touch = HashMap::from([
            ("Content/A.uasset".to_string(), touch("aaaaaaaa", "User")),
            ("Content/b.uasset".to_string(), touch("bbbbbbbb", "User")),
        ]);
        let main_set = HashSet::from(["Content/b.uasset".to_string()]);
        let main_touch = HashMap::from([
            ("Content/b.uasset".to_string(), touch("cccccccc", "Collab")),
            ("Content/A.uasset".to_string(), touch("dddddddd", "Collab")),
        ]);

        let rows = assemble_lockable_rows(&branch_paths, &branch_touch, &main_set, &main_touch, 10);

        assert_eq!(
            rows,
            vec![
                LockableFileRow {
                    path: "Content/b.uasset".to_string(),
                    display_name: String::new(),
                    branch_commit: Some(touch("bbbbbbbb", "User")),
                    changed_on_main: true,
                    main_commit: Some(touch("cccccccc", "Collab")),
                },
                LockableFileRow {
                    path: "Content/A.uasset".to_string(),
                    display_name: String::new(),
                    branch_commit: Some(touch("aaaaaaaa", "User")),
                    changed_on_main: false,
                    main_commit: None,
                },
            ]
        );
    }

    #[test]
    fn rows_sort_conflicts_first_then_case_insensitive_path() {
        let branch_paths = strings(&["c.uasset", "B.uasset", "a.uasset", "D.uasset", "e.uasset"]);
        let main_set = HashSet::from(["e.uasset".to_string(), "D.uasset".to_string()]);
        let rows = assemble_lockable_rows(
            &branch_paths,
            &HashMap::new(),
            &main_set,
            &HashMap::new(),
            100,
        );
        let paths: Vec<&str> = rows.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(
            paths,
            ["D.uasset", "e.uasset", "a.uasset", "B.uasset", "c.uasset"]
        );
    }

    #[test]
    fn rows_missing_touch_is_none() {
        let branch_paths = strings(&["x.uasset"]);
        let main_set = HashSet::from(["x.uasset".to_string()]);
        let rows = assemble_lockable_rows(
            &branch_paths,
            &HashMap::new(),
            &main_set,
            &HashMap::new(),
            10,
        );
        assert_eq!(rows.len(), 1);
        assert!(rows[0].changed_on_main);
        assert_eq!(rows[0].branch_commit, None);
        assert_eq!(rows[0].main_commit, None);
    }

    #[test]
    fn rows_respect_cap_after_sorting() {
        let branch_paths = strings(&["z.uasset", "y.uasset", "a.uasset", "b.uasset"]);
        let main_set = HashSet::from(["z.uasset".to_string()]);
        let rows = assemble_lockable_rows(
            &branch_paths,
            &HashMap::new(),
            &main_set,
            &HashMap::new(),
            2,
        );
        let paths: Vec<&str> = rows.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(paths, ["z.uasset", "a.uasset"]);
        assert!(
            assemble_lockable_rows(&[], &HashMap::new(), &main_set, &HashMap::new(), 2).is_empty()
        );
    }

    // merged_pr_covers_tip and the merged rule

    #[tokio::test]
    async fn covers_tip_when_oid_is_the_tip() {
        let fx = Fixture::new().await;
        let tip = fx.origin_sha(BRANCH).await;
        assert!(merged_pr_covers_tip(&fx.user, BRANCH, &tip).await);
    }

    #[tokio::test]
    async fn covers_tip_when_oid_is_a_descendant_fetched_by_oid() {
        let fx = Fixture::new().await;
        // A commit on top of the branch that exists on origin only under another ref,
        // like a PR head the user never fetched.
        let tip = fx.origin_sha(BRANCH).await;
        let sha = fx.collab_push(BRANCH, "pr.txt", b"pr\n").await;
        let pull_ref = format!("{sha}:refs/pull/1/head");
        run_git(&fx.collab, &["push", "-q", "origin", &pull_ref]).await;
        let reset = format!("+{tip}:refs/heads/{BRANCH}");
        run_git(&fx.collab, &["push", "-q", "origin", &reset]).await;
        run_git(
            &fx.origin,
            &["config", "uploadpack.allowReachableSHA1InWant", "true"],
        )
        .await;
        assert!(fx
            .user
            .run(&["cat-file", "-e", &sha], quiet())
            .await
            .is_err());

        assert!(merged_pr_covers_tip(&fx.user, BRANCH, &sha).await);
    }

    #[tokio::test]
    async fn analysis_fetches_leave_fetch_head_alone() {
        let fx = Fixture::new().await;
        if !fx.git_supported().await {
            return;
        }
        let fetch_head = fx.user_path().join(".git").join("FETCH_HEAD");
        let before = fs::read(&fetch_head).expect("the fixture's fetch writes FETCH_HEAD");

        fx.collab_push("main", "main.txt", b"main\n").await;
        let tip = fx.origin_sha(BRANCH).await;
        let sha = fx.collab_push(BRANCH, "pr.txt", b"pr\n").await;
        let pull_ref = format!("{sha}:refs/pull/1/head");
        run_git(&fx.collab, &["push", "-q", "origin", &pull_ref]).await;
        let reset = format!("+{tip}:refs/heads/{BRANCH}");
        run_git(&fx.collab, &["push", "-q", "origin", &reset]).await;
        run_git(
            &fx.origin,
            &["config", "uploadpack.allowReachableSHA1InWant", "true"],
        )
        .await;

        let facts = fx.facts().await.expect("facts");
        assert_eq!(facts.commits_behind, 1, "the trunk fetch ran");
        assert!(
            merged_pr_covers_tip(&fx.user, BRANCH, &sha).await,
            "the OID fetch ran"
        );
        assert_eq!(fs::read(&fetch_head).expect("FETCH_HEAD"), before);
    }

    #[tokio::test]
    async fn does_not_cover_when_tip_moved_after_merge() {
        let fx = Fixture::new().await;
        let merged_oid = fx.origin_sha(BRANCH).await;
        fx.user_commit("after.txt", b"after\n", "after merge").await;
        fx.user_push().await;
        run_git(&fx.user, &["fetch", "-q", "origin"]).await;

        assert!(!merged_pr_covers_tip(&fx.user, BRANCH, &merged_oid).await);
    }

    #[tokio::test]
    async fn does_not_cover_unknown_oid() {
        let fx = Fixture::new().await;
        let unknown = "0123456789abcdef0123456789abcdef01234567";
        assert!(!merged_pr_covers_tip(&fx.user, BRANCH, unknown).await);
    }

    #[tokio::test]
    async fn does_not_cover_invalid_oid() {
        let fx = Fixture::new().await;
        let too_long = "a".repeat(65);
        for oid in ["", "--upload-pack=x", "HEAD", "zzzz", too_long.as_str()] {
            assert!(!merged_pr_covers_tip(&fx.user, BRANCH, oid).await, "{oid}");
        }
    }

    #[tokio::test]
    async fn does_not_cover_without_tracking_ref() {
        let fx = Fixture::new().await;
        let tip = fx.origin_sha(BRANCH).await;
        assert!(!merged_pr_covers_tip(&fx.user, "never-pushed", &tip).await);
    }

    fn merged_pr(oid: &str) -> BranchPullRequest {
        BranchPullRequest {
            number: 46,
            state: "MERGED".to_string(),
            head_ref_oid: oid.to_string(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn merged_rule_by_upstream() {
        let fx = Fixture::new().await;
        let tip = fx.origin_sha(BRANCH).await;
        let stale = "0123456789abcdef0123456789abcdef01234567";
        let git = &fx.user;

        assert_eq!(
            apply_merged_rule(git, BRANCH, BranchUpstream::Present, None).await,
            None
        );
        assert_eq!(
            apply_merged_rule(git, BRANCH, BranchUpstream::Present, Some(merged_pr(&tip))).await,
            Some(merged_pr(&tip))
        );
        assert_eq!(
            apply_merged_rule(git, BRANCH, BranchUpstream::Present, Some(merged_pr(stale))).await,
            None
        );
        assert_eq!(
            apply_merged_rule(git, BRANCH, BranchUpstream::Gone, Some(merged_pr(stale))).await,
            Some(merged_pr(stale))
        );
        assert_eq!(
            apply_merged_rule(
                git,
                BRANCH,
                BranchUpstream::NotPushed,
                Some(merged_pr(&tip))
            )
            .await,
            None
        );

        let open = BranchPullRequest {
            state: "OPEN".to_string(),
            ..merged_pr(stale)
        };
        assert_eq!(
            apply_merged_rule(git, BRANCH, BranchUpstream::NotPushed, Some(open.clone())).await,
            Some(open)
        );
    }

    // collect_branch_facts

    #[tokio::test]
    async fn facts_not_pushed() {
        let fx = Fixture::new().await;
        if !fx.git_supported().await {
            return;
        }
        run_git(&fx.user, &["checkout", "-q", "-b", "local-only"]).await;

        assert_eq!(
            fx.facts_for("main", "local-only").await.expect("facts"),
            BranchFacts {
                git: fx.git_check().await,
                upstream: BranchUpstream::NotPushed,
                local_sync: BranchLocalSync::Unknown,
                ..Default::default()
            }
        );
    }

    #[tokio::test]
    async fn facts_gone_by_tracking_config() {
        let fx = Fixture::new().await;
        if !fx.git_supported().await {
            return;
        }
        run_git(&fx.user, &["checkout", "-q", "-b", "gone-cfg"]).await;
        run_git(&fx.user, &["config", "branch.gone-cfg.remote", "origin"]).await;
        run_git(
            &fx.user,
            &["config", "branch.gone-cfg.merge", "refs/heads/gone-cfg"],
        )
        .await;

        assert_eq!(
            fx.facts_for("main", "gone-cfg").await.expect("facts"),
            BranchFacts {
                git: fx.git_check().await,
                upstream: BranchUpstream::Gone,
                local_sync: BranchLocalSync::Unknown,
                ..Default::default()
            }
        );
    }

    #[tokio::test]
    async fn facts_gone_by_stale_tracking_ref() {
        let fx = Fixture::new().await;
        if !fx.git_supported().await {
            return;
        }
        run_git(&fx.user, &["checkout", "-q", "-b", "stale"]).await;
        run_git(&fx.user, &["push", "-q", "origin", "stale"]).await;
        run_git(&fx.user, &["fetch", "-q", "origin"]).await;
        run_git(&fx.origin, &["branch", "-D", "stale"]).await;
        assert_eq!(
            git_output(&fx.user, &["config", "--get", "branch.stale.merge"]).await,
            ""
        );

        assert_eq!(
            fx.facts_for("main", "stale").await.expect("facts"),
            BranchFacts {
                git: fx.git_check().await,
                upstream: BranchUpstream::Gone,
                local_sync: BranchLocalSync::Unknown,
                ..Default::default()
            }
        );
    }

    #[tokio::test]
    async fn facts_fresh_branch_in_sync() {
        let fx = Fixture::new().await;
        if !fx.git_supported().await {
            return;
        }
        let main_sha = fx.origin_sha("main").await;

        assert_eq!(
            fx.facts().await.expect("facts"),
            BranchFacts {
                git: fx.git_check().await,
                upstream: BranchUpstream::Present,
                local_sync: BranchLocalSync::InSync,
                merge_base: Some(main_sha),
                ..Default::default()
            }
        );
    }

    #[tokio::test]
    async fn facts_ahead_of_trunk() {
        let fx = Fixture::new().await;
        if !fx.git_supported().await {
            return;
        }
        let main_sha = fx.origin_sha("main").await;
        fx.user_commit("branch.txt", b"branch\n", "branch work")
            .await;
        fx.user_push().await;

        assert_eq!(
            fx.facts().await.expect("facts"),
            BranchFacts {
                git: fx.git_check().await,
                upstream: BranchUpstream::Present,
                local_sync: BranchLocalSync::InSync,
                commits_ahead: 1,
                merge_base: Some(main_sha),
                ..Default::default()
            }
        );
    }

    #[tokio::test]
    async fn facts_behind_trunk_after_collaborator_push() {
        let fx = Fixture::new().await;
        if !fx.git_supported().await {
            return;
        }
        let base = fx.origin_sha("main").await;
        fx.collab_push("main", "main.txt", b"main\n").await;
        // The user has not fetched: only the analysis' own fetch can see it.
        assert_eq!(git_output(&fx.user, &["rev-parse", MAIN_REF]).await, base);

        assert_eq!(
            fx.facts().await.expect("facts"),
            BranchFacts {
                git: fx.git_check().await,
                upstream: BranchUpstream::Present,
                local_sync: BranchLocalSync::InSync,
                commits_behind: 1,
                merge_base: Some(base),
                ..Default::default()
            }
        );
    }

    #[tokio::test]
    async fn facts_local_ahead() {
        let fx = Fixture::new().await;
        if !fx.git_supported().await {
            return;
        }
        let main_sha = fx.origin_sha("main").await;
        fx.user_commit("local.txt", b"local\n", "unpushed").await;

        assert_eq!(
            fx.facts().await.expect("facts"),
            BranchFacts {
                git: fx.git_check().await,
                upstream: BranchUpstream::Present,
                local_sync: BranchLocalSync::Ahead,
                local_ahead: 1,
                merge_base: Some(main_sha),
                ..Default::default()
            }
        );
    }

    #[tokio::test]
    async fn facts_local_behind_after_collaborator_push() {
        let fx = Fixture::new().await;
        if !fx.git_supported().await {
            return;
        }
        let main_sha = fx.origin_sha("main").await;
        let before = git_output(&fx.user, &["rev-parse", BRANCH_REF]).await;
        fx.collab_push(BRANCH, "collab.txt", b"collab\n").await;
        assert_eq!(
            git_output(&fx.user, &["rev-parse", BRANCH_REF]).await,
            before
        );

        assert_eq!(
            fx.facts().await.expect("facts"),
            BranchFacts {
                git: fx.git_check().await,
                upstream: BranchUpstream::Present,
                local_sync: BranchLocalSync::Behind,
                local_behind: 1,
                commits_ahead: 1,
                merge_base: Some(main_sha),
                ..Default::default()
            }
        );
    }

    #[tokio::test]
    async fn facts_diverged_without_local_side_effects() {
        let fx = Fixture::new().await;
        if !fx.git_supported().await {
            return;
        }
        let main_sha = fx.origin_sha("main").await;
        fx.collab_push(BRANCH, "collab.txt", b"collab\n").await;
        fx.user_commit("local.txt", b"local\n", "unpushed").await;
        write_bytes(fx.user_path(), "untracked.txt", b"untracked\n");
        write_bytes(fx.user_path(), "seed.txt", b"edited\n");

        let before = LocalState::capture(&fx).await;
        let facts = fx.facts().await.expect("facts");
        assert_eq!(LocalState::capture(&fx).await, before);

        assert_eq!(
            facts,
            BranchFacts {
                git: fx.git_check().await,
                upstream: BranchUpstream::Present,
                local_sync: BranchLocalSync::Diverged,
                local_ahead: 1,
                local_behind: 1,
                commits_ahead: 1,
                merge_base: Some(main_sha),
                ..Default::default()
            }
        );
    }

    #[tokio::test]
    async fn facts_text_conflict_without_local_side_effects() {
        let fx = Fixture::new().await;
        if !fx.git_supported().await {
            return;
        }
        let base = fx.origin_sha("main").await;
        fx.user_commit("seed.txt", b"one\nbranch\nthree\n", "branch edit")
            .await;
        fx.user_push().await;
        fx.collab_push("main", "seed.txt", b"one\nmain\nthree\n")
            .await;

        let before = LocalState::capture(&fx).await;
        let facts = fx.facts().await.expect("facts");
        assert_eq!(LocalState::capture(&fx).await, before);

        assert_eq!(
            facts,
            BranchFacts {
                git: fx.git_check().await,
                upstream: BranchUpstream::Present,
                local_sync: BranchLocalSync::InSync,
                commits_ahead: 1,
                commits_behind: 1,
                merge_base: Some(base),
                predicted_conflicts: strings(&["seed.txt"]),
                ..Default::default()
            }
        );
    }

    #[tokio::test]
    async fn facts_refuse_when_branch_changed() {
        let fx = Fixture::new().await;
        if !fx.git_supported().await {
            return;
        }
        run_git(&fx.user, &["checkout", "-q", "-b", "other"]).await;
        assert_input(fx.facts().await, "The checked-out branch changed. Refresh.");
    }

    #[tokio::test]
    async fn facts_fail_when_trunk_missing_on_origin() {
        let fx = Fixture::new().await;
        if !fx.git_supported().await {
            return;
        }
        fx.user_commit("branch.txt", b"branch\n", "branch work")
            .await;
        fx.user_push().await;
        assert_internal(
            fx.facts_for("no-such-trunk", BRANCH).await,
            "couldn't find remote ref",
        );

        // Same when the branch is not on origin either.
        run_git(&fx.user, &["checkout", "-q", "-b", "local-only"]).await;
        assert_internal(
            fx.facts_for("no-such-trunk", "local-only").await,
            "couldn't find remote ref",
        );
    }

    #[tokio::test]
    async fn facts_fail_without_common_history() {
        let fx = Fixture::new().await;
        if !fx.git_supported().await {
            return;
        }
        run_git(&fx.user, &["checkout", "-q", "--orphan", "orphan"]).await;
        run_git(&fx.user, &["commit", "-qm", "orphan root"]).await;
        run_git(&fx.user, &["push", "-q", "-u", "origin", "orphan"]).await;
        assert_internal(fx.facts_for("main", "orphan").await, "no common history");
    }

    // BranchAnalysisOp end to end

    #[tokio::test]
    async fn analysis_lists_commits_and_lockable_files() {
        let fx = Fixture::new().await;
        if !fx.git_supported().await {
            return;
        }
        let base = fx.origin_sha("main").await;

        write_bytes(fx.user_path(), "Content/A.uasset", b"\0branch A\n");
        write_bytes(fx.user_path(), "Content/b.uasset", b"\0branch b\n");
        run_git(&fx.user, &["add", "Content"]).await;
        run_git(&fx.user, &["commit", "-qm", "branch assets"]).await;
        let assets_sha = fx.head().await;
        fx.user_commit("notes.txt", b"notes\n", "second commit")
            .await;
        fx.user_push().await;
        let main_a = fx
            .collab_push("main", "Content/A.uasset", b"\0main A\n")
            .await;

        let before = LocalState::capture(&fx).await;
        let analysis = fx.analyze(&[("main", true), ("content-main", false)]).await;
        assert_eq!(LocalState::capture(&fx).await, before);

        assert!(analysis.git_supported);
        assert_eq!(analysis.git_version, fx.git_check().await.version);
        assert_eq!(analysis.branch, BRANCH);
        assert_eq!(analysis.trunk_branch, "main");
        assert!(analysis.uses_merge_queue);
        assert_eq!(analysis.upstream, BranchUpstream::Present);
        assert_eq!(analysis.local_sync, BranchLocalSync::InSync);
        assert_eq!(
            (analysis.local_ahead_of_origin, analysis.local_behind_origin),
            (0, 0)
        );
        assert_eq!((analysis.commits_ahead, analysis.commits_behind), (2, 1));
        assert_eq!(analysis.merge_base.as_deref(), Some(base.as_str()));
        assert_eq!(analysis.predicted_conflicts, strings(&["Content/A.uasset"]));

        let messages: Vec<&str> = analysis
            .commits
            .iter()
            .map(|c| c.message.as_deref().unwrap_or_default().trim())
            .collect();
        assert_eq!(messages, ["second commit", "branch assets"]);
        assert_eq!(analysis.commits[1].sha, assets_sha[..8]);

        assert_eq!(analysis.lockable_file_count, 2);
        let rows = &analysis.lockable_files;
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].path, "Content/A.uasset");
        assert!(rows[0].changed_on_main);
        assert_eq!(rows[0].display_name, "");
        let branch_a = rows[0].branch_commit.as_ref().expect("branch touch");
        assert_eq!(branch_a.sha, assets_sha[..8]);
        assert_eq!(branch_a.author, "User");
        let main_touch = rows[0].main_commit.as_ref().expect("main touch");
        assert_eq!(main_touch.sha, main_a[..8]);
        assert_eq!(main_touch.author, "Collaborator");
        assert_eq!(rows[1].path, "Content/b.uasset");
        assert!(!rows[1].changed_on_main);
        assert_eq!(rows[1].display_name, "");
        assert_eq!(rows[1].main_commit, None);
        assert_eq!(
            rows[1].branch_commit.as_ref().map(|t| t.sha.as_str()),
            Some(&assets_sha[..8])
        );

        assert!(analysis.pr.is_none());
        assert!(analysis.pr_error.is_none());

        // Hand-run git agrees with the analysis.
        let dir = fx.user_path();
        let (code, out) = raw_git(
            dir,
            &[
                "merge-tree",
                "--write-tree",
                "--name-only",
                "-z",
                MAIN_REF,
                BRANCH_REF,
            ],
        );
        let records: Vec<String> = out
            .split(|b| *b == 0)
            .skip(1)
            .take_while(|r| !r.is_empty())
            .map(|r| String::from_utf8_lossy(r).to_string())
            .collect();
        println!("fixture: {}", dir.display());
        println!("merge-tree exit {code:?}: {records:?}");
        assert_eq!(code, Some(1));
        assert_eq!(records, analysis.predicted_conflicts);

        let range = format!("{MAIN_REF}...{BRANCH_REF}");
        let (_, out) = raw_git(dir, &["rev-list", "--count", "--left-right", &range]);
        let counts = String::from_utf8_lossy(&out).trim().to_string();
        println!("rev-list --left-right: {counts}");
        assert_eq!(counts, "1\t2");

        let (_, out) = raw_git(
            dir,
            &["diff", "--name-only", "--no-renames", &base, BRANCH_REF],
        );
        let diff = String::from_utf8_lossy(&out).to_string();
        println!("branch diff: {diff:?}");
        let lockable: Vec<&str> = diff.lines().filter(|l| l.ends_with(".uasset")).collect();
        assert_eq!(lockable, ["Content/A.uasset", "Content/b.uasset"]);
        let (_, out) = raw_git(
            dir,
            &["diff", "--name-only", "--no-renames", &base, MAIN_REF],
        );
        println!("main diff: {:?}", String::from_utf8_lossy(&out));
        assert_eq!(String::from_utf8_lossy(&out).trim(), "Content/A.uasset");
    }

    #[tokio::test]
    async fn analysis_flags_old_path_renamed_on_main() {
        let fx = Fixture::new().await;
        if !fx.git_supported().await {
            return;
        }
        fx.user_commit("Content/Rename.uasset", b"\0branch edit\n", "edit asset")
            .await;
        fx.user_push().await;
        fx.collab_rename("main", "Content/Rename.uasset", "Content/Renamed.uasset")
            .await;

        let analysis = fx.analyze(&[("main", true)]).await;

        assert_eq!(analysis.lockable_file_count, 1);
        assert_eq!(analysis.lockable_files.len(), 1);
        let row = &analysis.lockable_files[0];
        assert_eq!(row.path, "Content/Rename.uasset");
        assert!(row.changed_on_main);
        assert!(row.main_commit.is_some());
        println!("predicted conflicts: {:?}", analysis.predicted_conflicts);
    }

    #[tokio::test]
    async fn analysis_one_sided_lockable() {
        let fx = Fixture::new().await;
        if !fx.git_supported().await {
            return;
        }
        let sha = fx
            .user_commit("Content/B.uasset", b"\0only branch\n", "branch asset")
            .await;
        fx.user_commit("readme.txt", b"text\n", "text only").await;
        fx.user_push().await;

        let analysis = fx.analyze(&[("main", true)]).await;

        assert!(analysis.predicted_conflicts.is_empty());
        assert_eq!(analysis.lockable_file_count, 1);
        assert_eq!(analysis.lockable_files.len(), 1);
        let row = &analysis.lockable_files[0];
        assert_eq!(row.path, "Content/B.uasset");
        assert!(!row.changed_on_main);
        assert_eq!(row.main_commit, None);
        assert_eq!(
            row.branch_commit.as_ref().map(|t| t.sha.as_str()),
            Some(&sha[..8])
        );
    }

    #[tokio::test]
    async fn analysis_skips_details_with_nothing_ahead() {
        let fx = Fixture::new().await;
        if !fx.git_supported().await {
            return;
        }
        fx.collab_push("main", "Content/Main.uasset", b"\0main\n")
            .await;

        let analysis = fx.analyze(&[("main", true)]).await;

        assert_eq!((analysis.commits_ahead, analysis.commits_behind), (0, 1));
        assert!(analysis.commits.is_empty());
        assert!(analysis.lockable_files.is_empty());
        assert_eq!(analysis.lockable_file_count, 0);
        assert!(analysis.predicted_conflicts.is_empty());
    }

    #[tokio::test]
    async fn analysis_uses_merge_queue_from_trunk_target() {
        let fx = Fixture::new().await;
        if !fx.git_supported().await {
            return;
        }
        assert!(!fx.analyze(&[("main", false)]).await.uses_merge_queue);
        assert!(
            fx.analyze(&[("main", true), ("content-main", false)])
                .await
                .uses_merge_queue
        );
        assert!(
            fx.analyze(&[("content-main", false)])
                .await
                .uses_merge_queue
        );
        assert!(fx.analyze(&[]).await.uses_merge_queue);
    }

    #[tokio::test]
    async fn analysis_limits_commit_list() {
        let fx = Fixture::new().await;
        if !fx.git_supported().await {
            return;
        }
        let total = COMMIT_LIST_LIMIT + 1;
        let mut stream = format!("reset refs/heads/{BRANCH}\nfrom {}\n\n", fx.head().await);
        for i in 0..total {
            let message = format!("bulk {i}");
            stream.push_str(&format!(
                "commit refs/heads/{BRANCH}\ncommitter User <user@example.com> {} +0000\ndata {}\n{message}\n",
                1_700_000_000 + i,
                message.len()
            ));
        }
        let mut child = Command::new("git")
            .args(["fast-import", "--quiet"])
            .current_dir(fx.user_path())
            .stdin(Stdio::piped())
            .spawn()
            .expect("spawn fast-import");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(stream.as_bytes())
            .expect("write stream");
        assert!(child.wait().expect("fast-import").success());
        fx.user_push().await;

        let analysis = fx.analyze(&[("main", true)]).await;

        assert_eq!(analysis.commits_ahead as usize, total);
        assert_eq!(analysis.commits.len(), COMMIT_LIST_LIMIT);
        let newest = format!("bulk {}", total - 1);
        assert_eq!(
            analysis.commits[0].message.as_deref().map(str::trim),
            Some(newest.as_str())
        );
    }
}
