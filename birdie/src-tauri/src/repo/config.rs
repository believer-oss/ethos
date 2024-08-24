use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::State;
use axum::Json;
use ethos_core::types::errors::CoreError;
use gix_config::Source;

use crate::state::AppState;

pub async fn get_fetch_include(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<String>>, CoreError> {
    let config_path = PathBuf::from(state.app_config.read().repo_path.clone()).join(".git/config");
    let git_config = gix_config::File::from_path_no_includes(config_path.clone(), Source::Local)?;

    let mut all_paths = Vec::<String>::new();
    if let Ok(value) = git_config.raw_value("lfs.fetchinclude") {
        all_paths = value
            .to_string()
            .split(',')
            .map(|s| s.to_string())
            .collect();
    }
    Ok(Json(all_paths))
}
