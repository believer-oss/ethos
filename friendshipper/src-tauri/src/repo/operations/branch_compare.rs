use crate::engine::EngineProvider;
use anyhow::anyhow;
use axum::extract::{Query, State};
use axum::Json;
use ethos_core::clients::github::CommitStatusMap;
use ethos_core::operations::{BranchCompareOp, LogResponse};
use ethos_core::types::errors::CoreError;
use serde::{Deserialize, Serialize};
use tracing::{debug, instrument, warn};

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

    let owner = state.repo_status.read().repo_owner.clone();
    let repo = state.repo_status.read().repo_name.clone();

    let github_client = state.github_client.read().clone();
    let statuses: Option<CommitStatusMap> = match github_client {
        Some(github_client) => {
            // Skip GitHub API calls if repo owner/name are not set (during startup)
            if owner.is_empty() || repo.is_empty() {
                debug!("Skipping commit status fetch: repo owner/name not yet configured (owner='{}', repo='{}')", owner, repo);
                None
            } else {
                match github_client.get_commit_statuses(&owner, &repo, 100).await {
                    Ok(statuses) => Some(statuses),
                    Err(e) => {
                        warn!(
                            "Error getting commit statuses for {}/{}: {}",
                            owner,
                            repo,
                            e.to_string()
                        );
                        None
                    }
                }
            }
        }
        None => None,
    };

    match branch_compare_op.run().await {
        Ok(mut output) => {
            return if let Some(statuses) = statuses {
                output.iter_mut().for_each(|commit| {
                    if let Some(status) = statuses.get(&commit.sha) {
                        commit.status = Some(status.clone());
                    }
                });

                Ok(Json(output))
            } else {
                Ok(Json(output))
            }
        }
        Err(e) => Err(CoreError::Internal(anyhow!(
            "Error executing branch comparison: {}",
            e.to_string()
        ))),
    }
}
