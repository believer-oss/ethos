use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::anyhow;
use axum::extract::State;
use axum::{async_trait, Json};
use tracing::{error, info, warn};

use super::post_head_change::{run_post_head_change, PostHeadChangeCtx};
use super::{guard_state, is_valid_branch_name, require_aws_and_storage, run_guarded_sequence};
use crate::engine::EngineProvider;
use crate::repo::operations::gh::submit::is_quicksubmit_branch;
use crate::repo::operations::validate::validate_repo_state;
use crate::repo::operations::StatusOp;
use crate::state::AppState;
use ethos_core::clients::git;
use ethos_core::storage::ArtifactStorage;
use ethos_core::types::config::UProject;
use ethos_core::types::errors::CoreError;
use ethos_core::types::repo::{SwitchBranchRequest, SwitchBranchResponse};
use ethos_core::worker::Task;
use ethos_core::AWSClient;

#[derive(Clone)]
pub struct SwitchBranchOp<T> {
    pub branch: String,
    pub(crate) ctx: PostHeadChangeCtx<T>,
    /// Set once checkout succeeds; later failures leave the user on `branch`.
    pub(crate) head_moved: Arc<AtomicBool>,
}

impl<T> SwitchBranchOp<T> {
    fn emit_phase(&self, phase: impl Into<String>) {
        let _ = self.ctx.sync_phase_tx.send(phase.into());
    }
}

#[async_trait]
impl<T> Task for SwitchBranchOp<T>
where
    T: EngineProvider,
{
    async fn execute(&self) -> Result<(), CoreError> {
        let branch = self.branch.as_str();
        if !is_valid_branch_name(branch) {
            return Err(CoreError::Input(anyhow!(
                "Invalid branch name '{}'.",
                branch
            )));
        }
        if is_quicksubmit_branch(branch) {
            return Err(CoreError::Input(anyhow!(
                "Quick Submit branches can't be checked out."
            )));
        }

        self.emit_phase("Checking repository status");
        let ctx = &self.ctx;
        ctx.engine
            .check_ready_to_sync_repo()
            .await
            .map_err(CoreError::Input)?;

        let github_username = ctx
            .github_client
            .clone()
            .map_or(String::default(), |x| x.username.clone());
        let status_op = StatusOp {
            git_client: ctx.git_client.clone(),
            github_username,
            repo_status: ctx.repo_status.clone(),
            app_config: ctx.app_config.clone(),
            repo_config: ctx.repo_config.clone(),
            engine: ctx.engine.clone(),
            aws_client: None,
            storage: None,
            allow_offline_communication: false,
            skip_display_names: true,
            skip_engine_update: true,
            // internal pre-operation refresh, not a user-initiated one
            fetch_first: false,
        };
        status_op.execute().await?;

        let repo_path = PathBuf::from(ctx.app_config.read().repo_path.clone());
        let repo_status = ctx.repo_status.read().clone();
        validate_repo_state(&repo_path, &repo_status)?;

        // Leaving without Sync would strand the Quick Submit branch on origin.
        if is_quicksubmit_branch(&repo_status.branch) {
            let target = ctx.app_config.read().target_branch.clone();
            return Err(CoreError::Input(anyhow!(
                "Sync to return to '{target}' from a Quick Submit branch."
            )));
        }

        if repo_status.branch == branch {
            info!("Already on branch {branch}, nothing to switch");
            return Ok(());
        }

        if !repo_status.modified_files.is_empty() || !repo_status.untracked_files.is_empty() {
            return Err(CoreError::Input(anyhow!(
                "Commit, snapshot, or revert your changes before switching branches"
            )));
        }

        self.emit_phase(format!("Fetching {branch}"));
        // An explicit refspec updates origin/<branch> even when it was force-pushed, and never
        // matches a tag of the same name.
        let refspec = format!("+refs/heads/{branch}:refs/remotes/origin/{branch}");
        if let Err(e) = ctx
            .git_client
            .run(
                &["fetch", "--no-auto-maintenance", "origin", &refspec],
                git::Opts::new_with_complete_error(),
            )
            .await
        {
            if e.to_string().contains("couldn't find remote ref") {
                return Err(CoreError::Input(anyhow!(
                    "Branch '{}' does not exist on origin.",
                    branch
                )));
            }
            return Err(e.into());
        }
        if !ctx
            .git_client
            .has_local_remote_tracking_branch(branch)
            .await?
        {
            return Err(CoreError::Input(anyhow!(
                "Branch '{}' does not exist on origin.",
                branch
            )));
        }
        ensure_branch_has_repo_config(&ctx.git_client, branch).await?;

        // save engine association before the .uproject potentially gets updated
        let uproject_path = repo_path.join(ctx.repo_config.read().uproject_path.clone());
        let old_engine_association = match UProject::load(&uproject_path) {
            Err(e) => {
                error!(
                    "Failed to load uproject before branch switch, skipping engine update. Error: {}",
                    e
                );
                None
            }
            Ok(uproject) => Some(uproject.engine_association),
        };

        self.emit_phase(format!("Switching to {branch}"));
        ctx.git_client.wait_for_lock().await;
        let remote_ref = format!("origin/{branch}");
        let opts = git::Opts::default().with_complete_error();
        if !ctx.git_client.has_local_branch(branch).await? {
            ctx.git_client
                .run(&["checkout", "-b", branch, "--track", &remote_ref], opts)
                .await?;
            self.head_moved.store(true, Ordering::SeqCst);
        } else {
            // `--` keeps a same-named path from being treated as a file checkout.
            ctx.git_client
                .run(&["checkout", branch, "--"], opts)
                .await?;
            self.head_moved.store(true, Ordering::SeqCst);

            let (ahead, behind) = ctx.git_client.get_ahead_behind(&remote_ref, branch).await?;
            if ahead == 0 && behind > 0 {
                info!("Fast-forwarding {branch} by {behind} commits");
                ctx.git_client
                    .run(&["merge", "--ff-only", &remote_ref], opts)
                    .await?;
            } else if ahead > 0 && behind > 0 {
                warn!(
                    "Local {branch} has diverged from {remote_ref} ({ahead} local, {behind} remote); leaving it as is"
                );
            }
        }

        run_post_head_change(ctx, old_engine_association).await
    }

    fn get_name(&self) -> String {
        String::from("RepoSwitchBranch")
    }
}

/// Refuses branches whose fetched tip lacks `friendshipper.yaml`, which startup needs.
async fn ensure_branch_has_repo_config(
    git_client: &git::Git,
    branch: &str,
) -> Result<(), CoreError> {
    let object = format!("refs/remotes/origin/{branch}:friendshipper.yaml");
    if git_client
        .run(&["cat-file", "-e", &object], git::Opts::new_without_logs())
        .await
        .is_err()
    {
        return Err(CoreError::Input(anyhow!(
            "'{branch}' has no friendshipper.yaml, so Friendshipper can't work on it."
        )));
    }
    Ok(())
}

pub(crate) fn build_switch_op<T: EngineProvider>(
    state: &AppState<T>,
    branch: String,
    aws_client: AWSClient,
    storage: ArtifactStorage,
) -> SwitchBranchOp<T> {
    SwitchBranchOp {
        branch,
        ctx: PostHeadChangeCtx::new(state, aws_client, storage),
        head_moved: Arc::new(AtomicBool::new(false)),
    }
}

/// `Ok(warning)` when HEAD reached `branch`, even if a later step failed; the error otherwise.
pub(crate) fn switch_outcome(
    branch: &str,
    result: Result<(), CoreError>,
    head_moved: bool,
) -> Result<Option<String>, CoreError> {
    match result {
        Ok(()) => Ok(None),
        Err(e) if head_moved => {
            let e = e.to_string();
            Ok(Some(format!(
                "Switched to '{branch}', but updating editor binaries or engine failed: {}. Retry from the warning banner or Preferences.",
                e.trim_end_matches('.')
            )))
        }
        Err(e) => Err(e),
    }
}

/// Runs a switch to `branch` as its own sequence; see [`switch_outcome`].
pub(crate) async fn run_switch<T: EngineProvider>(
    state: &AppState<T>,
    branch: &str,
    aws_client: AWSClient,
    storage: ArtifactStorage,
) -> Result<Option<String>, CoreError> {
    let op = build_switch_op(state, branch.to_string(), aws_client, storage);
    let head_moved = op.head_moved.clone();
    let result = run_guarded_sequence(state, Box::new(op)).await;
    switch_outcome(branch, result, head_moved.load(Ordering::SeqCst))
}

pub async fn switch_branch_handler<T>(
    State(state): State<AppState<T>>,
    Json(req): Json<SwitchBranchRequest>,
) -> Result<Json<SwitchBranchResponse>, CoreError>
where
    T: EngineProvider,
{
    guard_state(&state).await?;

    let (aws_client, storage) = require_aws_and_storage(&state).await?;

    let branch = req.branch;
    let post_head_change_warning = run_switch(&state, &branch, aws_client, storage).await?;

    let diverged_warning = match state
        .git()
        .get_ahead_behind(&format!("origin/{branch}"), &branch)
        .await
    {
        Ok((ahead, behind)) if ahead > 0 && behind > 0 => Some(format!(
            "Local branch '{branch}' has diverged from origin/{branch} ({ahead} local and {behind} remote commits) and was not updated. Your local commits are untouched."
        )),
        _ => None,
    };

    let warning = match (diverged_warning, post_head_change_warning) {
        (Some(a), Some(b)) => Some(format!("{a} {b}")),
        (a, b) => a.or(b),
    };

    Ok(Json(SwitchBranchResponse { branch, warning }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::process::Command;

    fn git_ok(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("run git");
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[tokio::test]
    async fn ensure_branch_has_repo_config_checks_origin_tip() {
        let origin = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        git_ok(origin.path(), &["init", "--bare"]);
        let dir = repo.path();
        for args in [
            &["init"][..],
            &["config", "user.email", "test@example.com"],
            &["config", "user.name", "test"],
            &["config", "commit.gpgsign", "false"],
        ] {
            git_ok(dir, args);
        }
        std::fs::write(
            dir.join("friendshipper.yaml"),
            "trunkBranch: main
",
        )
        .unwrap();
        git_ok(dir, &["add", "."]);
        git_ok(dir, &["commit", "-m", "with config"]);
        git_ok(dir, &["checkout", "-b", "code-main"]);
        git_ok(dir, &["rm", "friendshipper.yaml"]);
        git_ok(dir, &["commit", "-m", "without config"]);
        let origin_path = origin.path().to_string_lossy().to_string();
        git_ok(dir, &["remote", "add", "origin", &origin_path]);
        git_ok(
            dir,
            &["push", "origin", "HEAD~1:refs/heads/main", "code-main"],
        );
        git_ok(dir, &["fetch", "origin"]);

        let (tx, _rx) = std::sync::mpsc::channel();
        let git_client = git::Git::new(dir.to_path_buf(), tx);

        assert!(ensure_branch_has_repo_config(&git_client, "main")
            .await
            .is_ok());
        let err = ensure_branch_has_repo_config(&git_client, "code-main")
            .await
            .unwrap_err();
        assert!(matches!(err, CoreError::Input(e)
            if e.to_string() == "'code-main' has no friendshipper.yaml, so Friendshipper can't work on it."));
        // The working tree still has the file; only origin's tip matters.
        std::fs::write(dir.join("friendshipper.yaml"), "x").unwrap();
        assert!(ensure_branch_has_repo_config(&git_client, "code-main")
            .await
            .is_err());
    }

    #[test]
    fn switch_outcome_success() {
        assert!(matches!(switch_outcome("b", Ok(()), true), Ok(None)));
        assert!(matches!(switch_outcome("b", Ok(()), false), Ok(None)));
    }

    #[test]
    fn switch_outcome_failure_before_head_moved() {
        let result = switch_outcome("b", Err(CoreError::Input(anyhow!("dirty"))), false);
        assert!(matches!(result, Err(CoreError::Input(e)) if e.to_string() == "dirty"));
        let result = switch_outcome("b", Err(CoreError::Internal(anyhow!("boom"))), false);
        assert!(matches!(result, Err(CoreError::Internal(e)) if e.to_string() == "boom"));
    }

    #[test]
    fn switch_outcome_failure_after_head_moved() {
        for err in [
            CoreError::Internal(anyhow!("download failed.")),
            CoreError::Input(anyhow!("download failed")),
        ] {
            assert_eq!(
                switch_outcome("f11r-branch-x", Err(err), true)
                    .unwrap()
                    .unwrap(),
                "Switched to 'f11r-branch-x', but updating editor binaries or engine failed: download failed. Retry from the warning banner or Preferences."
            );
        }
    }
}
