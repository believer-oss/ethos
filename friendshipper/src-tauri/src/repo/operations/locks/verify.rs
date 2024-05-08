use std::sync::Arc;

use anyhow::anyhow;
use axum::{extract::State, Json};

use ethos_core::types::errors::CoreError;
use ethos_core::types::locks::VerifyLocksResponse;

use crate::state::AppState;

pub async fn verify_locks_handler(
    State(state): State<Arc<AppState>>,
) -> Result<Json<VerifyLocksResponse>, CoreError> {
    match state.git().verify_locks().await {
        Ok(output) => Ok(Json(output)),
        Err(e) => Err(CoreError(anyhow!(
            "Error executing diff: {}",
            e.to_string()
        ))),
    }
}
