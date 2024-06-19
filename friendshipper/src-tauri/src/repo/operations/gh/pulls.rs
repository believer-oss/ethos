use anyhow::anyhow;
use axum::extract::{Path, Query, State};
use axum::Json;
use serde::Deserialize;

use crate::engine::EngineProvider;
use ethos_core::types::errors::CoreError;
use ethos_core::types::github::pulls::get_pull_request::GetPullRequestRepositoryPullRequest;
use ethos_core::types::github::pulls::get_pull_requests::GetPullRequestsRepositoryPullRequestsNodes;

use crate::state::AppState;

pub async fn get_pull_request<T>(
    State(state): State<AppState<T>>,
    Path(id): Path<i64>,
) -> Result<Json<GetPullRequestRepositoryPullRequest>, CoreError>
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
            let pr = client
                .get_pull_request(owner.clone(), repo.clone(), id)
                .await?;
            Ok(Json(pr))
        }
        None => Err(anyhow!("No GitHub client found").into()),
    }
}

#[derive(Default, Deserialize)]
pub struct GetPullRequestsParams {
    limit: i64,
}

pub async fn get_pull_requests<T>(
    State(state): State<AppState<T>>,
    params: Query<GetPullRequestsParams>,
) -> Result<Json<Vec<GetPullRequestsRepositoryPullRequestsNodes>>, CoreError>
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
            let prs = client
                .get_pull_requests(owner.clone(), repo.clone(), params.limit)
                .await?;
            Ok(Json(prs))
        }
        None => Err(anyhow!("No GitHub client found").into()),
    }
}
