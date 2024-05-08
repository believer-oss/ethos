use std::sync::Arc;

use anyhow::anyhow;
use axum::{debug_handler, extract::State, Json};
use tracing::info;

use ethos_core::operations::LockOp;
use ethos_core::types::errors::CoreError;
use ethos_core::types::locks::{LockOperation, LockResponse};
use ethos_core::types::repo::LockRequest;

use crate::state::AppState;

#[debug_handler]
pub async fn acquire_locks_handler(
    State(state): State<Arc<AppState>>,
    Json(request): Json<LockRequest>,
) -> Result<Json<LockResponse>, CoreError> {
    info!("lock request: {:?}", request);

    internal_lock_handler(state, request, LockOperation::Lock).await
}

#[debug_handler]
pub async fn release_locks_handler(
    State(state): State<Arc<AppState>>,
    Json(request): Json<LockRequest>,
) -> Result<Json<LockResponse>, CoreError> {
    info!("unlock request: {:?}", request);

    internal_lock_handler(state, request, LockOperation::Unlock).await
}

async fn internal_lock_handler(
    state: Arc<AppState>,
    request: LockRequest,
    op: LockOperation,
) -> Result<Json<LockResponse>, CoreError> {
    let github_pat = state.app_config.read().ensure_github_pat()?;

    let lock_op = {
        LockOp {
            git_client: state.git(),
            paths: request.paths,
            op,
            github_pat,
            force: request.force,
        }
    };

    match lock_op.run().await {
        Ok(response) => Ok(Json(response)),
        Err(e) => Err(CoreError(anyhow!(
            "Error executing lock op: {}",
            e.to_string()
        ))),
    }
}
