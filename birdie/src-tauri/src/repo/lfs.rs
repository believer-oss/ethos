use anyhow::anyhow;
use std::fs;
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
    // create params for setting fetch include
    let repo_path = state.app_config.read().repo_path.clone();
    let mut fetch_include_paths: Vec<String> = Vec::new();
    for file_path in request.files.iter() {
        // recursively flatten directory into all child files
        let full_path = PathBuf::from(repo_path.clone()).join(file_path);
        let local_path = PathBuf::from(file_path.clone());
        fetch_include_paths.extend(flatten_path(full_path, local_path));
    }
    set_fetch_include(repo_path, fetch_include_paths).await?;

    // format for command line lfs pull format
    let include_arg = request.files.join(",");

    state
        .git()
        .run(
            &["lfs", "pull", "--include", &include_arg, "--exclude", ""],
            Default::default(),
        )
        .await?;

    Ok(())
}

pub fn flatten_path(full_path: PathBuf, local_path: PathBuf) -> Vec<String> {
    let mut flat_paths: Vec<String> = Vec::new();
    if full_path.is_dir() {
        for child in fs::read_dir(full_path).unwrap() {
            let child = child.unwrap();
            let child_path = child.path();
            let child_local_path = local_path.join(child.file_name());
            flat_paths.extend(flatten_path(child_path, child_local_path));
        }
    } else {
        // append string with local file path, so it's easier for frontend to string match
        flat_paths.push(local_path.to_str().unwrap().to_string());
    }
    flat_paths
}

pub async fn set_fetch_include(repo_path: String, paths: Vec<String>) -> Result<(), CoreError> {
    let config_path = PathBuf::from(repo_path).join(".git/config");
    let mut git_config =
        gix_config::File::from_path_no_includes(config_path.clone(), Source::Local)?;

    let mut all_paths = String::new();
    match git_config.raw_value("lfs.fetchinclude") {
        Ok(value) => {
            for path in paths {
                // remove duplicates
                let normalized_path = path.replace("\\", "/");
                if !value.to_string().contains(&normalized_path) {
                    all_paths.push_str(&normalized_path);
                    all_paths.push(',');
                }
            }
            if !value.is_empty() {
                all_paths.push_str(&value.to_string())
            }
        }
        Err(_) => {
            all_paths = paths.join(",").replace("\\", "/");
        }
    }

    git_config.set_raw_value(&"lfs.fetchinclude", all_paths.as_str())?;
    git_config.set_raw_value(&"lfs.fetchexclude", "")?;

    // write new config to disk
    match std::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&config_path)
    {
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
