use crate::engine::EngineProvider;
use anyhow::anyhow;
use axum::extract::{Query, State};
use axum::Json;
use ethos_core::clients::github::CommitStatusMap;
use ethos_core::operations::{LogOp, LogResponse};
use ethos_core::types::errors::CoreError;
use serde::{Deserialize, Serialize};
use tracing::{debug, instrument, warn};

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

    match log_op.run().await {
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
            "Error executing log: {}",
            e.to_string()
        ))),
    }
}
