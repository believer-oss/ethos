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
    // Get the primary and content branches from app config
    let (primary_branch, content_branch) = {
        let app_config = state.app_config.read();
        let repo_config = state.repo_config.read();
        let primary = app_config.get_primary_branch(&repo_config);
        let content = app_config.get_content_branch(&repo_config);
        (primary, content)
    };

    let branch_compare_op = BranchCompareOp {
        limit: params.limit,
        repo_path: state.app_config.read().repo_path.clone(),
        repo_status: state.repo_status.clone(),
        git_client: state.git(),
        primary_branch,
        content_branch,
    };

    match branch_compare_op.run().await {
        Ok(output) => Ok(Json(output)),
        Err(e) => Err(CoreError::Internal(anyhow!(
            "Error executing branch comparison: {}",
            e.to_string()
        ))),
    }
}
