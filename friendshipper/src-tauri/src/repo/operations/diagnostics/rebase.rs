use std::path::Path;

use anyhow::anyhow;
use axum::extract::State;
use axum::Json;
use tracing::{info, warn};

use crate::engine::EngineProvider;
use ethos_core::clients::git;
use ethos_core::operations::RebaseOp;
use ethos_core::types::errors::CoreError;
use ethos_core::types::repo::RebaseStatusResponse;
use ethos_core::worker::TaskSequence;

use crate::repo::operations::branch::{is_active, recorded_rebase_kind, resolve_git_dir};
use crate::repo::operations::{guard_op, guard_state};
use crate::state::AppState;

pub async fn rebase_handler<T>(State(state): State<AppState<T>>) -> Result<(), CoreError>
where
    T: EngineProvider,
{
    guard_state(&state).await?;

    let (tx, rx) = tokio::sync::oneshot::channel::<Option<CoreError>>();
    let mut sequence = TaskSequence::new().with_completion_tx(tx);
    sequence.push(guard_op(&state));

    let rebase = RebaseOp {
        git_client: state.git(),
        repo_status: state.repo_status.clone(),
    };

    sequence.push(Box::new(rebase));

    let _ = state.operation_tx.send(sequence).await;
    let _ = rx.await;

    Ok(())
}

pub async fn rebase_status_handler<T>(
    State(state): State<AppState<T>>,
) -> Result<Json<RebaseStatusResponse>, CoreError>
where
    T: EngineProvider,
{
    let git = state.git();
    Ok(Json(rebase_status(&git, &git.repo_path).await))
}

async fn rebase_status(git: &git::Git, repo_path: &Path) -> RebaseStatusResponse {
    let git_dir = resolve_git_dir(repo_path);
    // When the state is unknown, report it as active so the remedies stay hidden; the guard
    // refuses them anyway.
    let guided_rebase_active = match recorded_rebase_kind(git, repo_path).await {
        Ok(kind) => kind.is_some_and(is_active),
        Err(e) => {
            warn!("Could not determine the guided rebase state: {e:#}");
            true
        }
    };

    RebaseStatusResponse {
        rebase_merge_exists: git_dir.join("rebase-merge").exists(),
        head_name_exists: git_dir.join("rebase-merge/head-name").exists(),
        guided_rebase_active,
    }
}

pub async fn remediate_rebase_handler<T>(State(state): State<AppState<T>>) -> Result<(), CoreError>
where
    T: EngineProvider,
{
    guard_state(&state).await?;

    if state.git().abort_rebase().await.is_ok() {
        info!("Rebase aborted successfully");

        return Ok(());
    }

    if state.git().quit_rebase().await.is_ok() {
        info!("Rebase quit successfully");

        return Ok(());
    }

    Err(CoreError::Internal(anyhow!(
        "Failed to abort or quit rebase"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::operations::branch::rebase_fixture::{
        diverge_on_a_txt, guided_record, guided_stop_on_a_txt, new_git, slash_path,
        start_raw_rebase, Fixture, BRANCH,
    };
    use crate::repo::operations::branch::write_rebase_file;

    async fn status(fx: &Fixture) -> (bool, bool, bool) {
        let s = rebase_status(&fx.git, fx.path()).await;
        (
            s.rebase_merge_exists,
            s.head_name_exists,
            s.guided_rebase_active,
        )
    }

    #[tokio::test]
    async fn idle() {
        let fx = Fixture::new().await;
        assert_eq!(status(&fx).await, (false, false, false));
    }

    #[tokio::test]
    async fn guided_rebase_in_progress_is_active() {
        let fx = Fixture::new().await;
        guided_stop_on_a_txt(&fx).await;
        assert_eq!(status(&fx).await, (true, true, true));
    }

    #[tokio::test]
    async fn completed_guided_rebase_is_active() {
        let fx = Fixture::new().await;
        fx.guided_complete().await;
        assert_eq!(status(&fx).await, (false, false, true));
    }

    #[tokio::test]
    async fn stale_record_is_not_active() {
        let fx = Fixture::new().await;
        fx.guided_complete().await;
        fx.checkout("main").await;
        assert_eq!(status(&fx).await, (false, false, false));
    }

    #[tokio::test]
    async fn external_rebase_is_not_active() {
        let fx = Fixture::new().await;
        diverge_on_a_txt(&fx).await;
        assert_eq!(fx.start_raw_rebase("main").await, 1);
        assert_eq!(status(&fx).await, (true, true, false));
    }

    #[tokio::test]
    async fn unknown_state_is_reported_active() {
        let fx = Fixture::new().await;
        fx.guided_complete().await;
        let broken = new_git(&fx.path().join("missing"));
        assert!(rebase_status(&broken, fx.path()).await.guided_rebase_active);
    }

    #[tokio::test]
    async fn paths_resolve_through_a_gitdir_file() {
        let fx = Fixture::new().await;
        diverge_on_a_txt(&fx).await;
        fx.checkout("main").await;
        let parent = tempfile::tempdir().unwrap();
        let path = parent.path().join("wt");
        fx.run(&["worktree", "add", "-q", &slash_path(&path), BRANCH])
            .await;
        assert!(path.join(".git").is_file());
        let wt = new_git(&path);
        let file = guided_record(&wt, "main").await;
        write_rebase_file(&resolve_git_dir(&path), &file).unwrap();
        assert_eq!(start_raw_rebase(&wt, &file.onto).await, 1);

        let s = rebase_status(&wt, &path).await;
        assert!(s.rebase_merge_exists && s.head_name_exists && s.guided_rebase_active);
        // The main worktree has no rebase of its own.
        assert_eq!(status(&fx).await, (false, false, false));
    }
}
