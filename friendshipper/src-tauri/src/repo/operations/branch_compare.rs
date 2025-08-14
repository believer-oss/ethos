use crate::engine::EngineProvider;
use anyhow::anyhow;
use axum::extract::{Query, State};
use axum::Json;
use ethos_core::operations::{BranchCompareOp, LogResponse};
use ethos_core::types::errors::CoreError;
use serde::{Deserialize, Serialize};
use tracing::instrument;

use crate::state::AppState;

#[derive(Debug, Serialize, Deserialize)]
pub struct BranchCompareParams {
    #[serde(default = "default_limit")]
    pub limit: usize,
}

fn default_limit() -> usize {
    50
}

#[instrument(skip(state))]
pub async fn branch_compare_handler<T>(
    State(state): State<AppState<T>>,
    params: Query<BranchCompareParams>,
) -> Result<Json<LogResponse>, CoreError>
where
    T: EngineProvider,
{
    // Clone the target branches in a way that preserves Send trait
    let target_branches = {
        let repo_config = state.repo_config.read();
        repo_config.target_branches.clone()
    };

    let branch_compare_op = BranchCompareOp {
        limit: params.limit,
        repo_path: state.app_config.read().repo_path.clone(),
        repo_status: state.repo_status.clone(),
        git_client: state.git(),
        target_branches,
    };

    match branch_compare_op.run().await {
        Ok(output) => Ok(Json(output)),
        Err(e) => Err(CoreError::Internal(anyhow!(
            "Error executing branch comparison: {}",
            e.to_string()
        ))),
    }
}
