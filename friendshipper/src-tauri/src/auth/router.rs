use std::sync::Arc;

use anyhow::{anyhow, Result};
use axum::extract::State;
use axum::routing::{get, post};
use axum::{debug_handler, Json, Router};
use ethos_core::AWSClient;

use crate::APP_NAME;
use ethos_core::clients::aws::ensure_aws_client;
use ethos_core::types::errors::CoreError;

use crate::state::AppState;

pub fn router(shared_state: Arc<AppState>) -> Router {
    Router::new()
        .route("/status", get(get_status))
        .route("/refresh", post(refresh_aws_credentials))
        .with_state(shared_state)
}

async fn get_status(State(state): State<Arc<AppState>>) -> Json<bool> {
    let aws_client = match state.aws_client.read().await.clone() {
        Some(client) => client,
        None => {
            return Json(true);
        }
    };

    Json(aws_client.login_required().await)
}

#[debug_handler]
async fn refresh_aws_credentials(State(state): State<Arc<AppState>>) -> Result<(), CoreError> {
    if state.aws_client.read().await.is_none() {
        let aws_config = match state.app_config.read().aws_config.clone() {
            Some(config) => config,
            None => {
                return Err(CoreError::from(anyhow!(
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

        state.replace_aws_client(new_aws_client).await?;
    };

    let aws_client = ensure_aws_client(state.aws_client.read().await.clone())?;
    match aws_client.refresh_token(true).await {
        Ok(_) => Ok(()),
        Err(e) => Err(CoreError::from(anyhow!(
            "Failed to refresh AWS credentials: {}",
            e
        ))),
    }
}
