use crate::state::AppState;
use anyhow::anyhow;
use axum::extract::State;
use axum::Json;
use ethos_core::types::errors::CoreError;
use gix_config::Source;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use tracing::info;

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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DeleteFetchIncludeRequest {
    pub files: Vec<String>,
}

pub async fn del_fetch_include(
    State(state): State<Arc<AppState>>,
    Json(request): Json<DeleteFetchIncludeRequest>,
) -> Result<(), CoreError> {
    let config_path = PathBuf::from(state.app_config.read().repo_path.clone()).join(".git/config");
    let mut git_config =
        gix_config::File::from_path_no_includes(config_path.clone(), Source::Local)?;

    // get current lfs.fetchinclude value
    if let Ok(value) = git_config.raw_value("lfs.fetchinclude") {
        let mut all_paths_vec: Vec<String> = value
            .to_string()
            .split(',')
            .map(|s| s.to_string())
            .collect();

        // remove unfavorited paths from lfs.fetchinclude if they exist
        for file in request.files.iter() {
            if let Some(index) = all_paths_vec.iter().position(|x| x == file) {
                all_paths_vec.remove(index);
            }
        }

        all_paths_vec.retain(|x| !x.is_empty()); // remove empty paths to prevent trailing commas
        let all_paths_str = all_paths_vec.join(",");
        if all_paths_vec.is_empty() {
            // set lfs.fetchexclude to * to prevent all files from being downloaded
            git_config.set_raw_value(&"lfs.fetchexclude", "*")?;
        }
        git_config.set_raw_value(&"lfs.fetchinclude", all_paths_str.as_str())?;

        // clear current config and write new config to disk
        match std::fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&config_path)
        {
            Ok(mut writable_git_config) => match git_config.write_to(&mut writable_git_config) {
                Ok(_) => {
                    info!("Successfully set lfs.fetchinclude to {}", all_paths_str);
                }
                Err(e) => {
                    return Err(CoreError(anyhow!(
                        "Failed to write to git config file: {}",
                        e
                    )));
                }
            },
            Err(e) => {
                return Err(CoreError(anyhow!(
                    "Failed to open git config file for writing: {}",
                    e
                )));
            }
        }
    }
    Ok(())
}
