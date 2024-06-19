use anyhow::anyhow;
use axum::extract::State;
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::engine::EngineProvider;
use ethos_core::types::errors::CoreError;

use crate::state::AppState;

#[derive(Default, Deserialize, Serialize)]
pub struct UserInfoResponse {
    pub username: String,
}

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
