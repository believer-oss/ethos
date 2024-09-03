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
    let repo_path = state.app_config.read().repo_path.clone();
    let mut fetch_include_paths: Vec<String> = Vec::new();
    for file_path in request.files.iter() {
        // recursively flatten paths into all child files
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
        for path in request.files.iter() {
            let full_path = PathBuf::from(state.app_config.read().repo_path.clone()).join(path);
            let local_path = PathBuf::from(path.clone());
            let flattened_paths = flatten_path(full_path, local_path);
            for file_path in flattened_paths {
                all_paths_vec.retain(|x| x != &file_path);
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

pub async fn set_fetch_include(repo_path: String, paths: Vec<String>) -> Result<(), CoreError> {
    let config_path = PathBuf::from(repo_path).join(".git/config");
    let mut git_config =
        gix_config::File::from_path_no_includes(config_path.clone(), Source::Local)?;

    let mut all_paths = String::new();
    match git_config.raw_value("lfs.fetchinclude") {
        Ok(value) => {
            for path in paths {
                // remove duplicates
                if !value.to_string().contains(&path) {
                    all_paths.push_str(&path);
                    all_paths.push(',');
                }
            }
            if !value.is_empty() {
                // append existing lfs.fetchinclude value
                all_paths.push_str(&value.to_string())
            }
        }
        Err(_) => {
            // no existing lfs.fetchinclude value
            all_paths = paths.join(",");
        }
    }

    git_config.set_raw_value(&"lfs.fetchinclude", all_paths.as_str())?;
    git_config.set_raw_value(&"lfs.fetchexclude", "")?;

    // clear current config and write new config to disk
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
        flat_paths.push(local_path.to_str().unwrap().to_string().replace("\\", "/"));
    }
    flat_paths
}
