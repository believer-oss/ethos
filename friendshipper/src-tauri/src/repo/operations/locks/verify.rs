use anyhow::anyhow;
use axum::{extract::State, Json};

use crate::engine;
use crate::engine::EngineProvider;
use crate::state::AppState;
use ethos_core::types::errors::CoreError;
use ethos_core::types::locks::Lock;
use ethos_core::types::locks::VerifyLocksResponse;

pub async fn verify_locks_handler<T>(
    State(state): State<AppState<T>>,
) -> Result<Json<VerifyLocksResponse>, CoreError>
where
    T: EngineProvider,
{
    let mut response_data: VerifyLocksResponse = match state.git().verify_locks().await {
        Ok(output) => output,
        Err(e) => {
            return Err(CoreError(anyhow!(
                "Error executing diff: {}",
                e.to_string()
            )))
        }
    };

    let mut ours_paths: Vec<String> = response_data.ours.iter().map(|v| v.path.clone()).collect();
    let mut theirs_paths: Vec<String> = response_data
        .theirs
        .iter()
        .map(|v| v.path.clone())
        .collect();

    let ours_len = ours_paths.len();
    let theirs_len = ours_paths.len();

    let mut combined_paths: Vec<String> = Vec::with_capacity(ours_len + theirs_len);
    combined_paths.append(&mut ours_paths);
    combined_paths.append(&mut theirs_paths);

    let engine_path = state
        .app_config
        .read()
        .load_engine_path_from_repo(&state.repo_config.read())
        .unwrap_or_default();

    let display_names = state
        .engine
        .get_asset_display_names(
            engine::CommunicationType::OfflineFallback,
            &engine_path,
            &combined_paths,
        )
        .await;

    #[allow(clippy::needless_range_loop)]
    for i in 0..combined_paths.len() {
        let vec_index: usize;
        let vec: &mut [Lock] = if i < ours_len {
            vec_index = i;
            &mut response_data.ours
        } else {
            vec_index = i - ours_len;
            &mut response_data.theirs
        };
        if !display_names.is_empty() {
            vec[vec_index].display_name = Some(display_names[i].clone());
        }
    }

    Ok(Json(response_data))
}
