use anyhow::anyhow;
use axum::extract::State;
use axum::Json;
use tracing::instrument;

use crate::engine::EngineProvider;
use ethos_core::types::errors::CoreError;
use ethos_core::types::github::merge_queue::get_merge_queue::GetMergeQueueRepositoryMergeQueue;

use crate::state::AppState;

#[instrument(skip(state))]
pub async fn get_merge_queue<T>(
    State(state): State<AppState<T>>,
) -> Result<Json<GetMergeQueueRepositoryMergeQueue>, CoreError>
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
            let merge_queue = client.get_merge_queue(&owner, &repo).await?;
            Ok(Json(merge_queue))
        }
        None => Err(anyhow!("No GitHub client found").into()),
    }
}
