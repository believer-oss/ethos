use anyhow::anyhow;
use axum::{extract::State, Json};
use tracing::{error, info, instrument};

use crate::engine::EngineProvider;
use ethos_core::operations::LockOp;
use ethos_core::types::errors::CoreError;
use ethos_core::types::locks::{LockOperation, LockResponse};
use ethos_core::types::repo::LockRequest;

use crate::state::AppState;

#[instrument(skip(state, request))]
pub async fn acquire_locks_handler<T>(
    State(state): State<AppState<T>>,
    Json(request): Json<LockRequest>,
) -> Result<Json<LockResponse>, CoreError>
where
    T: EngineProvider,
{
    info!("lock request: {:?}", request);

    let mut paths: Vec<String> = Vec::new();
    for path in request.paths {
        if let Some(lock) = state
            .repo_status
            .read()
            .locks_theirs
            .iter()
            .find(|l| l.path == path)
        {
            // do not attempt to lock any files owned by other users, instead log an error and abort
            if let Some(owner) = &lock.owner {
                error!(
                    "Locking failed: file {} is already checked out by {}",
                    path, owner.name
                );
                return Err(CoreError(anyhow!(
                    "Failed to lock a file checked out by {}. Check the log for more details.",
                    owner.name,
                )));
            }
        } else if state
            .repo_status
            .read()
            .modified_upstream
            .iter()
            .any(|p| p == &path)
        {
            // do not attempt to lock any files modified upstream by other users, instead log an error and abort
            error!(
                "Locking failed: files are modified upstream by other users. Sync and try again."
            );
            return Err(CoreError(anyhow!(
                "Files are modified upstream by other users. Sync and try again."
            )));
        } else {
            paths.push(path);
        }
    }

    let request_data = LockRequest {
        paths,
        force: request.force,
    };

    internal_lock_handler(state, request_data, LockOperation::Lock).await
}

#[instrument(skip(state, request))]
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

#[instrument(skip(state))]
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
