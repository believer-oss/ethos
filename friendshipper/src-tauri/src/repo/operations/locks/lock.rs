use anyhow::anyhow;
use axum::{extract::State, Json};
use tracing::{info, instrument};

use crate::engine::EngineProvider;
use crate::repo::operations::gh::submit::is_branch_mode;
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

    let (branch, branch_mode) = {
        let branch = state.repo_status.read().branch.clone();
        let branch_mode = is_branch_mode(&state.repo_config.read(), &branch);
        (branch, branch_mode)
    };

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
                return Err(CoreError::Input(anyhow!(
                    "Locking failed: file {} is already checked out by {}",
                    path,
                    owner.name
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
            return Err(CoreError::Input(anyhow!(modified_upstream_message(
                branch_mode,
                &branch
            ))));
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

fn modified_upstream_message(branch_mode: bool, branch: &str) -> String {
    if branch_mode {
        format!("Files are modified on origin/{branch} by other users. Sync to get your collaborators' changes, then try again.")
    } else {
        "Files are modified upstream by other users. Sync and try again.".to_string()
    }
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
        .ok_or(CoreError::Internal(anyhow!(
            "No github pat found. Please set a github pat in the config"
        )))?;
    let github_username = state.github_username();
    let (response_tx, mut response_rx) = tokio::sync::mpsc::channel::<LockResponse>(1);

    let lock_op = LockOp {
        git_client: state.git(),
        paths: request.paths,
        op,
        response_tx: Some(response_tx.clone()),
        github_pat: github_pat.to_string(),
        repo_status: state.repo_status.clone(),
        github_username,
        force: request.force,
    };

    // Emit a phase label so any progress modal showing during a lock/unlock
    // displays a user-friendly step name. Goes through `sync_phase_tx`, the
    // same channel pull/submit/revert use; emitted synchronously here so the
    // modal is updated by the time the worker picks up the LockOp.
    let phase = match op {
        LockOperation::Lock => "Locking files",
        LockOperation::Unlock => "Unlocking files",
    };
    let _ = state.sync_phase_tx.send(phase.to_string());

    let (task_tx, task_rx) = tokio::sync::oneshot::channel::<Option<CoreError>>();

    let mut sequence = TaskSequence::new().with_completion_tx(task_tx);
    sequence.push(Box::new(lock_op));

    state.operation_tx.send(sequence).await?;

    match task_rx.await {
        Ok(e) => {
            if let Some(e) = e {
                return Err(e);
            }
        }
        Err(_) => {
            return Err(CoreError::Internal(anyhow!(
                "Error executing lock operation ({:?})",
                op
            )))
        }
    }

    match response_rx.recv().await {
        Some(response) => Ok(Json(response)),
        None => Err(CoreError::Internal(anyhow!("Failed to get lock response"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modified_upstream_message_names_branch_in_branch_mode() {
        assert_eq!(
            modified_upstream_message(true, "f11r-branch-x"),
            "Files are modified on origin/f11r-branch-x by other users. Sync to get your collaborators' changes, then try again."
        );
    }

    #[test]
    fn modified_upstream_message_is_unchanged_off_branch_mode() {
        assert_eq!(
            modified_upstream_message(false, "main"),
            "Files are modified upstream by other users. Sync and try again."
        );
    }
}
