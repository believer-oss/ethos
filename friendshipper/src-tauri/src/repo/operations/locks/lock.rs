use anyhow::anyhow;
use axum::{extract::State, Json};
use tracing::info;

use crate::engine::EngineProvider;
use ethos_core::operations::LockOp;
use ethos_core::types::errors::CoreError;
use ethos_core::types::locks::{LockOperation, LockResponse};
use ethos_core::types::repo::LockRequest;

use crate::state::AppState;

pub async fn acquire_locks_handler<T>(
    State(state): State<AppState<T>>,
    Json(request): Json<LockRequest>,
) -> Result<Json<LockResponse>, CoreError>
where
    T: EngineProvider,
{
    info!("lock request: {:?}", request);

    internal_lock_handler(state, request, LockOperation::Lock).await
}

pub async fn release_locks_handler<T>(
    State(state): State<AppState<T>>,
    Json(request): Json<LockRequest>,
) -> Result<Json<LockResponse>, CoreError>
where
    T: EngineProvider,
{
    info!("unlock request: {:?}", request);

    internal_lock_handler(state, request, LockOperation::Unlock).await
}

async fn internal_lock_handler<T>(
    state: AppState<T>,
    request: LockRequest,
    op: LockOperation,
) -> Result<Json<LockResponse>, CoreError>
where
    T: EngineProvider,
{
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
