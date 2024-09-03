use crate::engine::EngineProvider;
use anyhow::anyhow;
use axum::extract::{Query, State};
use axum::Json;
use ethos_core::operations::{LogOp, LogResponse};
use ethos_core::types::errors::CoreError;
use serde::{Deserialize, Serialize};
use tracing::instrument;

use crate::state::AppState;

#[derive(Debug, Serialize, Deserialize)]
pub struct LogParams {
    #[serde(default = "default_limit")]
    pub limit: usize,

    #[serde(default)]
    pub use_remote: bool,
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
    let log_op = LogOp {
        limit: params.limit,
        use_remote: params.use_remote,
        repo_status: state.repo_status.clone(),
        repo_path: state.app_config.read().repo_path.clone(),
        git_client: state.git(),
    };

    match log_op.run().await {
        Ok(output) => Ok(Json(output)),
        Err(e) => Err(CoreError::Internal(anyhow!(
            "Error executing log: {}",
            e.to_string()
        ))),
    }
}
