use crate::state::AppState;
use crate::EngineProvider;
use axum::extract::State;
use ethos_core::types::errors::CoreError;

pub async fn refetch_repo<T>(State(state): State<AppState<T>>) -> Result<(), CoreError>
where
    T: EngineProvider,
{
    state.git().refetch().await?;
    state.git().rewrite_graph().await?;
    Ok(())
}
