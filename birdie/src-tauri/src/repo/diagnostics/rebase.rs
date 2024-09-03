use std::path::PathBuf;
use std::sync::Arc;

use anyhow::anyhow;
use axum::extract::State;
use axum::{debug_handler, Json};
use tracing::info;

use ethos_core::operations::RebaseOp;
use ethos_core::types::errors::CoreError;
use ethos_core::types::repo::RebaseStatusResponse;
use ethos_core::worker::TaskSequence;

use crate::state::AppState;

#[debug_handler]
pub async fn rebase_handler(State(state): State<Arc<AppState>>) -> Result<(), CoreError> {
    let (tx, rx) = tokio::sync::oneshot::channel::<Option<CoreError>>();
    let mut sequence = TaskSequence::new().with_completion_tx(tx);

    let rebase = RebaseOp {
        git_client: state.git(),
        repo_status: state.repo_status.clone(),
    };

    sequence.push(Box::new(rebase));

    let _ = state.operation_tx.send(sequence).await;
    let _ = rx.await;

    Ok(())
}

#[debug_handler]
pub async fn rebase_status_handler(
    State(state): State<Arc<AppState>>,
) -> Result<Json<RebaseStatusResponse>, CoreError> {
    let repo_path = PathBuf::from(state.app_config.read().repo_path.clone());

    let rebase_merge_path = repo_path.join(".git/rebase-merge");
    let head_name_path = repo_path.join(".git/rebase-merge/head-name");

    Ok(Json(RebaseStatusResponse {
        rebase_merge_exists: rebase_merge_path.exists(),
        head_name_exists: head_name_path.exists(),
    }))
}

#[debug_handler]
pub async fn remediate_rebase_handler(State(state): State<Arc<AppState>>) -> Result<(), CoreError> {
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
