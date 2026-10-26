use anyhow::anyhow;
use axum::extract::State;
use axum::Json;
use tracing::instrument;

use crate::engine::EngineProvider;
use crate::state::AppState;
use ethos_core::types::errors::CoreError;
use ethos_core::types::repo::EditorBinariesStatus;

#[instrument(skip_all)]
pub async fn editor_binaries_handler<T>(
    State(_state): State<AppState<T>>,
) -> Result<Json<EditorBinariesStatus>, CoreError>
where
    T: EngineProvider,
{
    // TODO: Implement in Stage 013
    Err(CoreError::Internal(anyhow!(
        "Guided rebase is not implemented yet"
    )))
}
