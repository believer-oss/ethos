use std::sync::Arc;

use anyhow::anyhow;
use axum::extract::{Query, State};
use axum::Json;
use serde::Deserialize;

use ethos_core::operations::{LogOp, LogResponse};
use ethos_core::types::errors::CoreError;

use crate::state::AppState;

#[derive(Default, Deserialize)]
pub struct LogParams {
    #[serde(default = "default_limit")]
    pub limit: usize,

    #[serde(default)]
    pub use_remote: bool,
}

fn default_limit() -> usize {
    10
}

pub async fn log_handler(
    State(state): State<Arc<AppState>>,
    params: Query<LogParams>,
) -> Result<Json<LogResponse>, CoreError> {
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
