use std::sync::Arc;

use crate::state::AppState;
use axum::extract::State;
use ethos_core::types::errors::CoreError;

pub async fn refetch_repo(State(state): State<Arc<AppState>>) -> Result<(), CoreError> {
    state.git().refetch().await?;
    state.git().rewrite_graph().await?;
    Ok(())
}
