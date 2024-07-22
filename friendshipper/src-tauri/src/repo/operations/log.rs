use crate::engine::EngineProvider;
use anyhow::anyhow;
use axum::extract::{Query, State};
use axum::Json;
use ethos_core::clients::aws::ensure_aws_client;
use ethos_core::operations::{LogOp, LogResponse};
use ethos_core::types::errors::CoreError;
use ethos_core::worker::{NoOp, TaskSequence};
use serde::{Deserialize, Serialize};
use tracing::instrument;

use crate::state::AppState;

use super::StatusOp;

#[derive(Debug, Serialize, Deserialize)]
pub struct LogParams {
    #[serde(default = "default_limit")]
    pub limit: usize,

    #[serde(default)]
    pub use_remote: bool,

    // Whether to force a fetch before getting the log
    #[serde(default)]
    pub update: bool,
}

fn default_limit() -> usize {
    10
}

#[instrument(skip(state))]
pub async fn log_handler<T>(
    State(state): State<AppState<T>>,
    params: Query<LogParams>,
) -> Result<Json<LogResponse>, CoreError>
where
    T: EngineProvider,
{
    let aws_client = ensure_aws_client(state.aws_client.read().await.clone())?;

    // Make sure we wait for any queued updates
    let (tx, rx) = tokio::sync::oneshot::channel::<Option<anyhow::Error>>();
    let mut sequence = TaskSequence::new().with_completion_tx(tx);

    if params.update {
        let status_op = {
            StatusOp {
                repo_status: state.repo_status.clone(),
                app_config: state.app_config.clone(),
                repo_config: state.repo_config.clone(),
                engine: state.engine.clone(),
                git_client: state.git(),
                aws_client: aws_client.clone(),
                storage: state.storage.read().clone().unwrap(),
                skip_fetch: false,
                skip_dll_check: false,
                allow_offline_communication: false,
            }
        };

        sequence.push(Box::new(status_op));
    } else {
        sequence.push(Box::new(NoOp));
    }

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
