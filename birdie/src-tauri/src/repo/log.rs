use std::sync::Arc;

use anyhow::anyhow;
use axum::extract::{Query, State};
use axum::Json;
use serde::Deserialize;

use ethos_core::operations::{LogOp, LogResponse};
use ethos_core::types::errors::CoreError;
use ethos_core::worker::{NoOp, TaskSequence};

use crate::state::AppState;

#[derive(Default, Deserialize)]
pub struct LogParams {
    #[serde(default = "default_limit")]
    pub limit: usize,

    #[serde(default)]
    pub use_remote: bool,

    // Whether to force a fetch before getting the log
    // This parameter does not get read
    #[allow(dead_code)]
    #[serde(default)]
    pub update: bool,
}

fn default_limit() -> usize {
    10
}

pub async fn log_handler(
    State(state): State<Arc<AppState>>,
    params: Query<LogParams>,
) -> Result<Json<LogResponse>, CoreError> {
    // Make sure we wait for any queued updates
    let (tx, rx) = tokio::sync::oneshot::channel::<Option<anyhow::Error>>();
    let mut sequence = TaskSequence::new().with_completion_tx(tx);

    sequence.push(Box::new(NoOp));
    let _ = state.operation_tx.send(sequence).await;
    let _ = rx.await;

    let log_op = LogOp {
        limit: params.limit,
        use_remote: params.use_remote,
        repo_status: state.repo_status.clone(),
        repo_path: state.app_config.read().repo_path.clone(),
        git_client: state.git(),
    };

    match log_op.run().await {
        Ok(output) => Ok(Json(output)),
        Err(e) => Err(CoreError(anyhow!("Error executing log: {}", e.to_string()))),
    }
}
