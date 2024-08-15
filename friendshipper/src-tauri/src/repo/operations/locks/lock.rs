use anyhow::anyhow;
use axum::{extract::State, Json};
use tracing::{error, info, instrument};

use crate::engine::EngineProvider;
use ethos_core::operations::LockOp;
use ethos_core::types::errors::CoreError;
use ethos_core::types::locks::{LockOperation, LockResponse};
use ethos_core::types::repo::LockRequest;
use ethos_core::worker::TaskSequence;

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
    let github_pat = state
        .app_config
        .read()
        .github_pat
        .clone()
        .ok_or(CoreError(anyhow!(
            "No github pat found. Please set a github pat in the config"
        )))?;
    let github_username = state.github_username();
    let (response_tx, mut response_rx) = tokio::sync::mpsc::channel::<LockResponse>(1);

    let lock_op = LockOp {
        git_client: state.git(),
        paths: request.paths,
        op,
        response_tx: Some(response_tx.clone()),
        github_pat,
        repo_status: state.repo_status.clone(),
        github_username,
        force: request.force,
    };

    let (task_tx, task_rx) = tokio::sync::oneshot::channel::<Option<anyhow::Error>>();

    let mut sequence = TaskSequence::new().with_completion_tx(task_tx);
    sequence.push(Box::new(lock_op));

    state.operation_tx.send(sequence).await?;

    match task_rx.await {
        Ok(e) => {
            if let Some(e) = e {
                error!("Lock operation ({:?}) failed: {}", op, e);
                return Err(CoreError(e));
            }
        }
        Err(_) => {
            return Err(CoreError(anyhow!(
                "Error executing lock operation ({:?})",
                op
            )))
        }
    }

    match response_rx.recv().await {
        Some(response) => Ok(Json(response)),
        None => Err(CoreError(anyhow!("Failed to get lock response"))),
    }
}
