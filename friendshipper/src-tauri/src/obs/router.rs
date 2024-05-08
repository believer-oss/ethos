use std::sync::Arc;

use axum::extract::State;
use axum::routing::post;
use axum::Router;

use ethos_core::clients::obs;
use ethos_core::types::errors::CoreError;

use crate::state::AppState;

pub fn router(shared_state: Arc<AppState>) -> Router {
    Router::new()
        .route("/start", post(start_recording))
        .route("/stop", post(stop_recording))
        .with_state(shared_state)
}

pub async fn start_recording(State(_state): State<Arc<AppState>>) -> Result<(), CoreError> {
    let client = obs::Client::default();

    client.start_recording().await
}

pub async fn stop_recording(State(_state): State<Arc<AppState>>) -> Result<(), CoreError> {
    let client = obs::Client::default();

    client.stop_recording().await
}
