use anyhow::anyhow;
use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::State;
use axum::Json;
use gix_config::Source;
use serde::{Deserialize, Serialize};
use tracing::info;

use ethos_core::types::errors::CoreError;

use crate::state::AppState;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DownloadFilesRequest {
    pub files: Vec<String>,
}

pub async fn download_files(
    State(state): State<Arc<AppState>>,
    Json(request): Json<DownloadFilesRequest>,
) -> Result<(), CoreError> {
    // join file paths by comma
    let include_arg = request.files.join(",");

    let repo_path = state.app_config.read().repo_path.clone();
    set_fetch_include(repo_path, &include_arg).await?;

    state
        .git()
        .run(
            &["lfs", "pull", "--include", &include_arg, "--exclude", ""],
            Default::default(),
        )
        .await?;

    Ok(())
}

pub async fn set_fetch_include(repo_path: String, paths: &str) -> Result<(), CoreError> {
    let config_path = PathBuf::from(repo_path).join(".git/config");
    let mut git_config =
        gix_config::File::from_path_no_includes(config_path.clone(), Source::Local)?;

    let mut all_paths = paths.to_string();
    match git_config.raw_value("lfs.fetchinclude") {
        Ok(value) => {
            all_paths.push(',');
            all_paths.push_str(&value.to_string());
        }
        Err(_) => {
            // for proper priority, we need to set lfs.fetchexclude to empty string
            git_config.set_raw_value(&"lfs.fetchexclude", "")?;
        }
    }

    git_config.set_raw_value(&"lfs.fetchinclude", all_paths.as_str())?; // creates lfs.fetchinclude section or overwrites it

    // write new config to disk
    match std::fs::OpenOptions::new().write(true).open(&config_path) {
        Ok(mut writable_git_config) => match git_config.write_to(&mut writable_git_config) {
            Ok(_) => {
                info!("Successfully set lfs.fetchinclude to {}", all_paths);
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
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UnfavoriteFileParams {
    pub file: String,
}
