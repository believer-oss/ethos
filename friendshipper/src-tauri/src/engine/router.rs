use crate::state::AppState;
use crate::EngineProvider;
use axum::extract::{Query, State};
use axum::routing::post;
use axum::Router;
use ethos_core::types::errors::CoreError;
use serde::Deserialize;
use serde::Serialize;
use tracing::instrument;

pub fn router<T>() -> Router<AppState<T>>
where
    T: EngineProvider,
{
    Router::new().route("/notify-state", post(notify_state))
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
