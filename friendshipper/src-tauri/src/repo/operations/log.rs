use crate::engine::EngineProvider;
use anyhow::anyhow;
use axum::extract::{Query, State};
use axum::Json;
use ethos_core::clients::github::{CommitStatusMap, MergeTimestampMap};
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
    let branch = if params.use_remote {
        let remote_branch = state.repo_status.read().remote_branch.clone();
        remote_branch
            .strip_prefix("origin/")
            .unwrap_or(&remote_branch)
            .to_string()
    } else {
        state.repo_status.read().branch.clone()
    };

    let github_client = state.github_client.read().clone();
    let (statuses, merge_timestamps): (Option<CommitStatusMap>, Option<MergeTimestampMap>) =
        match github_client {
            Some(github_client) => {
                // Skip GitHub API calls if repo owner/name are not set (during startup)
                if owner.is_empty() || repo.is_empty() {
                    debug!("Skipping commit status fetch: repo owner/name not yet configured (owner='{}', repo='{}')", owner, repo);
                    (None, None)
                } else {
                    let (statuses_result, merge_timestamps_result) = tokio::join!(
                        github_client.get_commit_statuses(&owner, &repo, 100),
                        github_client.get_commit_merge_timestamps(&owner, &repo, &branch, 100),
                    );
                    let statuses = match statuses_result {
                        Ok(statuses) => Some(statuses),
                        Err(e) => {
                            warn!(
                                "Error getting commit statuses for {}/{}: {}",
                                owner, repo, e
                            );
                            None
                        }
                    };
                    let merge_timestamps = match merge_timestamps_result {
                        Ok(timestamps) => Some(timestamps),
                        Err(e) => {
                            warn!(
                                "Error getting merge timestamps for {}/{}: {}",
                                owner, repo, e
                            );
                            None
                        }
                    };
                    (statuses, merge_timestamps)
                }
            }
            None => (None, None),
        };

    match log_op.run().await {
        Ok(mut output) => {
            output.iter_mut().for_each(|commit| {
                if let Some(statuses) = &statuses {
                    if let Some(status) = statuses.get(&commit.sha) {
                        commit.status = Some(status.clone());
                    }
                }
                if let Some(merge_timestamps) = &merge_timestamps {
                    if let Some(ts) = merge_timestamps.get(&commit.sha) {
                        commit.merge_timestamp = Some(ts.clone());
                    }
                }
            });
            Ok(Json(output))
        }
        Err(e) => Err(CoreError::Internal(anyhow!("Error executing log: {}", e))),
    }
}
