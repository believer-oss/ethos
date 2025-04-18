use crate::state::AppState;
use crate::EngineProvider;
use axum::extract::{Query, State};
use axum::routing::post;
use axum::{Json, Router};
use ethos_core::types::errors::CoreError;
use serde::Deserialize;
use serde::Serialize;
use tracing::instrument;

pub fn router<T>() -> Router<AppState<T>>
where
    T: EngineProvider,
{
    Router::new()
        .route("/notify-state", post(notify_state))
        .route("/open-url", post(open_url_for_path))
        .route("/check-engine-ready", post(check_engine_ready))
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NotifyStateParams {
    in_slow_task: bool,
}

#[instrument(skip(state))]
pub async fn notify_state<T>(
    State(state): State<AppState<T>>,
    params: Query<NotifyStateParams>,
) -> Result<String, CoreError>
where
    T: EngineProvider,
{
    state.engine.set_state(params.in_slow_task);
    Ok("ok".to_string())
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenUrlForPathRequest {
    pub path: String,
}

#[instrument(skip(state))]
pub async fn open_url_for_path<T>(
    State(state): State<AppState<T>>,
    Json(request): Json<OpenUrlForPathRequest>,
) -> Result<String, CoreError>
where
    T: EngineProvider,
{
    let url = state.engine.get_url_for_path(&request.path);
    if let Some(url) = url {
        open::that(url)?;
    } else {
        return Err(CoreError::Input(anyhow::anyhow!("No URL found for path")));
    }

    Ok("ok".to_string())
}

#[instrument(skip(state))]
pub async fn check_engine_ready<T>(
    State(state): State<AppState<T>>,
) -> Result<Json<bool>, CoreError>
where
    T: EngineProvider,
{
    match state.engine.check_ready_to_sync_repo().await {
        Ok(()) => Ok(Json(true)),
        Err(_) => Ok(Json(false)),
    }
}
