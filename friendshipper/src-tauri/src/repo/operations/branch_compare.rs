use crate::engine::EngineProvider;
use anyhow::anyhow;
use axum::extract::{Query, State};
use axum::Json;
use ethos_core::clients::github::{CommitStatusMap, MergeTimestampMap};
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

    let primary_branch_for_query = primary_branch.clone();
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
                        github_client.get_commit_merge_timestamps(
                            &owner,
                            &repo,
                            &primary_branch_for_query,
                            100
                        ),
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

    match branch_compare_op.run().await {
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
        Err(e) => Err(CoreError::Internal(anyhow!(
            "Error executing branch comparison: {}",
            e
        ))),
    }
}
