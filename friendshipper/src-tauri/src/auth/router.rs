use anyhow::Result;
use axum::extract::{Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use ethos_core::{AWSClient, AWS_ACCESS_KEY_ID, AWS_ARTIFACT_BUCKET_NAME, AWS_SECRET_ACCESS_KEY};
use serde::Deserialize;
use tracing::error;

use crate::client::FriendshipperClient;
use crate::engine::EngineProvider;
use ethos_core::clients::aws::ensure_aws_client;
use ethos_core::types::errors::CoreError;

use crate::state::AppState;

pub fn router<T>() -> Router<AppState<T>>
where
    T: EngineProvider,
{
    Router::new()
        .route("/status", get(get_status))
        .route("/refresh", post(refresh_aws_credentials))
        .route("/logout", post(logout))
}

async fn get_status<T>(State(state): State<AppState<T>>) -> Json<bool>
where
    T: EngineProvider,
{
    let aws_client = match state.aws_client.read().await.clone() {
        Some(client) => client,
        None => {
            return Json(true);
        }
    };

    Json(aws_client.login_required().await)
}

#[derive(Deserialize)]
struct RefreshParams {
    pub token: String,
}

async fn refresh_aws_credentials<T>(
    State(state): State<AppState<T>>,
    Query(params): Query<RefreshParams>,
) -> Result<(), CoreError>
where
    T: EngineProvider,
{
    let new_aws_client = if state.app_config.read().serverless {
        let access_key_id = AWS_ACCESS_KEY_ID;
        let secret_access_key = AWS_SECRET_ACCESS_KEY;
        let artifact_bucket_name = AWS_ARTIFACT_BUCKET_NAME;

        AWSClient::from_static_creds(
            access_key_id,
            secret_access_key,
            None,
            None,
            artifact_bucket_name.to_string(),
        )
        .await
    } else {
        let client = FriendshipperClient::new(state.app_config.read().server_url.clone())?;
        let credentials = client.get_aws_credentials(&params.token).await?;

        // get config
        let friendshipper_config = client.get_config(&params.token).await?;

        AWSClient::from_static_creds(
            &credentials.access_key_id,
            &credentials.secret_access_key,
            credentials.session_token.as_deref(),
            credentials.expiration,
            friendshipper_config.artifact_bucket_name.clone(),
        )
        .await
    };

    let username = state.app_config.read().user_display_name.clone();
    let playtest_region = state.app_config.read().playtest_region.clone();
    match state
        .replace_aws_client(new_aws_client, playtest_region, &username)
        .await
    {
        Ok(_) => (),
        Err(e) => {
            error!("Failed to replace AWS client: {}", e);
        }
    }

    Ok(())
}

async fn logout<T>(State(state): State<AppState<T>>) -> Result<(), CoreError>
where
    T: EngineProvider,
{
    let aws_client = ensure_aws_client(state.aws_client.read().await.clone())?;
    aws_client.logout().await?;
    Ok(())
}
