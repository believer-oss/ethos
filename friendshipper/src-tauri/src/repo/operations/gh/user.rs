use anyhow::anyhow;
use axum::extract::State;
use axum::Json;

use crate::engine::EngineProvider;
use ethos_core::types::errors::CoreError;
use ethos_core::types::github::user::UserInfoResponse;

use crate::state::AppState;

pub async fn get_user<T>(
    State(state): State<AppState<T>>,
) -> Result<Json<UserInfoResponse>, CoreError>
where
    T: EngineProvider,
{
    let gh_client = state.github_client.read().clone();
    match gh_client {
        Some(client) => Ok(Json(UserInfoResponse {
            username: client.username,
        })),
        None => Err(anyhow!("No GitHub client found").into()),
    }
}
