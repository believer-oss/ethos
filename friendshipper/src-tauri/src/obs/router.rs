use axum::extract::State;
use axum::routing::post;
use axum::Router;

use crate::engine::EngineProvider;
use ethos_core::clients::obs;
use ethos_core::types::errors::CoreError;

use crate::state::AppState;

pub fn router<T>() -> Router<AppState<T>>
where
    T: EngineProvider,
{
    Router::new()
        .route("/start", post(start_recording))
        .route("/stop", post(stop_recording))
}

pub async fn start_recording<T>(State(_state): State<AppState<T>>) -> Result<(), CoreError>
where
    T: EngineProvider,
{
    let client = obs::Client::default();

    client.start_recording().await
}

pub async fn stop_recording<T>(State(_state): State<AppState<T>>) -> Result<(), CoreError>
where
    T: EngineProvider,
{
    let client = obs::Client::default();

    client.stop_recording().await
}
