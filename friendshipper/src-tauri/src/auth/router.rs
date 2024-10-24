use anyhow::{anyhow, Result};
use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router};
use ethos_core::AWSClient;
use tracing::error;

use crate::engine::EngineProvider;
use crate::APP_NAME;
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

async fn refresh_aws_credentials<T>(State(state): State<AppState<T>>) -> Result<(), CoreError>
where
    T: EngineProvider,
{
    let aws_config = match state.app_config.read().aws_config.clone() {
        Some(config) => config,
        None => {
            return Err(CoreError::Internal(anyhow!(
                "No AWS config found in app config"
            )));
        }
    };

    let new_aws_client = AWSClient::new(
        Some(state.notification_tx.clone()),
        APP_NAME.to_string(),
        aws_config,
    )
    .await?;

    let username = state.app_config.read().user_display_name.clone();
    let playtest_region = state.app_config.read().playtest_region.clone();
    match state
        .replace_aws_client(
            new_aws_client,
            playtest_region,
            &username,
            state.app_config.clone(),
            state.config_file.clone(),
        )
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
