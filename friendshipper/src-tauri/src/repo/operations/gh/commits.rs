use crate::engine::EngineProvider;
use crate::state::AppState;
use anyhow::anyhow;
use axum::extract::State;
use axum::Json;
use ethos_core::clients::github::CommitStatusMap;
use ethos_core::types::errors::CoreError;
use tracing::instrument;

#[instrument(skip(state))]
pub async fn get_commit_statuses<T>(
    State(state): State<AppState<T>>,
) -> Result<Json<CommitStatusMap>, CoreError>
where
    T: EngineProvider,
{
    let owner: String;
    let repo: String;
    {
        let status = state.repo_status.read();
        owner = status.repo_owner.clone();
        repo = status.repo_name.clone();
    }

    let gh_client = state.github_client.read().clone();
    match gh_client {
        Some(client) => {
            let merge_queue = client.get_commit_statuses(&owner, &repo, 100).await?;
            Ok(Json(merge_queue))
        }
        None => Err(anyhow!("No GitHub client found").into()),
    }
}
