use axum::extract::State;

use ethos_core::operations::MaintenanceOp;
use ethos_core::types::errors::CoreError;
use ethos_core::worker::TaskSequence;

use crate::engine::EngineProvider;
use crate::state::AppState;

/// Run a fuller maintenance pass than the Diagnostics gc button: reflog expiry, gc, and a
/// commit-graph rewrite.
///
/// Queued through `state.operation_tx` rather than executed directly, and that is load-bearing.
/// `RepoWorker` processes one `TaskSequence` at a time and brackets each with
/// `pause_file_watcher`, so routing through it gives mutual exclusion against every other repo
/// operation — sync, submit, revert, snapshot — and pauses the file watcher and the periodic fetch
/// loop for the duration, even for users who have not disabled background operations. That is what
/// makes the frontend's locking modal correspond to a real lock rather than merely a blocked UI.
///
/// Blocks until the worker finishes so the caller can hold its modal up for the real duration.
pub async fn run_maintenance_handler<T>(State(state): State<AppState<T>>) -> Result<(), CoreError>
where
    T: EngineProvider,
{
    let (tx, rx) = tokio::sync::oneshot::channel::<Option<CoreError>>();
    let mut sequence = TaskSequence::new().with_completion_tx(tx);

    sequence.push(Box::new(MaintenanceOp {
        git_client: state.git(),
    }));

    let _ = state.operation_tx.send(sequence).await;
    if let Ok(Some(err)) = rx.await {
        return Err(err);
    }

    Ok(())
}
