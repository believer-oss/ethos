use crate::engine::EngineProvider;
use crate::state::AppState;
use axum::extract::State;
use ethos_core::types::errors::CoreError;

pub async fn reset_repo<T>(State(state): State<AppState<T>>) -> Result<(), CoreError>
where
    T: EngineProvider,
{
    let branch = state.repo_config.read().trunk_branch.clone();
    state.git().hard_reset(&branch).await.map_err(|e| e.into())
}
