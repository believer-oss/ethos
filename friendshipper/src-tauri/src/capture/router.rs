use axum::extract::{Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use ethos_core::capture::types::{CaptureStatus, PendingSummary};
use ethos_core::types::errors::CoreError;

use crate::engine::EngineProvider;
use crate::state::AppState;

pub fn router<T>() -> Router<AppState<T>>
where
    T: EngineProvider,
{
    Router::new()
        .route("/status", get(get_status))
        .route("/pending", get(get_pending))
        .route("/cancel", post(cancel))
}

async fn get_status<T>(State(state): State<AppState<T>>) -> Json<CaptureStatus>
where
    T: EngineProvider,
{
    Json(state.capture.status())
}

async fn get_pending<T>(
    State(state): State<AppState<T>>,
) -> Result<Json<Option<PendingSummary>>, CoreError>
where
    T: EngineProvider,
{
    Ok(Json(super::pending_all(&state).await?))
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CancelParams {
    session_id: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct CancelCaptureResponse {
    pub closed: u32,
}

async fn cancel<T>(
    State(state): State<AppState<T>>,
    params: Query<CancelParams>,
) -> Result<Json<CancelCaptureResponse>, CoreError>
where
    T: EngineProvider,
{
    let session_id = params
        .session_id
        .as_deref()
        .filter(|id| !id.is_empty())
        .ok_or_else(|| CoreError::Input(anyhow::anyhow!("sessionId is required")))?;

    Ok(Json(CancelCaptureResponse {
        closed: u32::from(state.capture.cancel_session(session_id)),
    }))
}
