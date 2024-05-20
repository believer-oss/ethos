use std::sync::Arc;

use anyhow::anyhow;
use axum::extract::State;
use axum::Json;

use ethos_core::types::errors::CoreError;
use ethos_core::types::github::merge_queue::get_merge_queue::GetMergeQueueRepositoryMergeQueue;

use crate::state::AppState;

pub async fn get_merge_queue(
    State(state): State<Arc<AppState>>,
) -> Result<Json<GetMergeQueueRepositoryMergeQueue>, CoreError> {
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
