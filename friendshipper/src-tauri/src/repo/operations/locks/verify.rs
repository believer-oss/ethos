use std::path::PathBuf;
use std::sync::Arc;

use anyhow::anyhow;
use axum::{extract::State, Json};

use ethos_core::types::errors::CoreError;
use ethos_core::types::locks::Lock;
use ethos_core::types::locks::VerifyLocksResponse;

use crate::state::AppState;
use crate::system::unreal::CanUseCommandlet;
use crate::system::unreal::OFPANameCache;

pub async fn verify_locks_handler(
    State(state): State<Arc<AppState>>,
) -> Result<Json<VerifyLocksResponse>, CoreError> {
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

    let repo_path = state.app_config.read().repo_path.clone();
    let uproject_path = state
        .app_config
        .read()
        .get_uproject_path(&state.repo_config.read());
    let engine_path = state
        .app_config
        .read()
        .load_engine_path_from_repo(&state.repo_config.read())
        .unwrap_or_default();

    let ofpa_names = OFPANameCache::get_names(
        state.ofpa_cache.clone(),
        &PathBuf::from(repo_path),
        &uproject_path,
        &engine_path,
        &combined_paths,
        CanUseCommandlet::FallbackOnly,
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
        if !ofpa_names.is_empty() {
            vec[vec_index].display_name = Some(ofpa_names[i].clone());
        }
    }

    Ok(Json(response_data))
}
