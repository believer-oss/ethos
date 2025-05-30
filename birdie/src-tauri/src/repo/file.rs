use anyhow::anyhow;
use axum::extract::{Query, State};
use axum::Json;
use chrono::DateTime;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::sync::Arc;
use tracing::{debug, info, instrument};
use walkdir::{DirEntry, WalkDir};

use ethos_core::types::commits::Commit;
use ethos_core::types::errors::CoreError;

use crate::repo::locks::LockCacheEntry;
use crate::state::AppState;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum FileType {
    Directory,
    File,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum LocalFileLFSState {
    None,
    Untracked,
    Local,
    Stub,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct File {
    pub path: String,
    pub name: String,
    pub size: u64,
    pub file_type: FileType,
    pub lfs_state: LocalFileLFSState,
    pub locked: bool,
    pub lock_info: Option<LockCacheEntry>,
}

impl PartialEq for File {
    fn eq(&self, other: &Self) -> bool {
        self.path == other.path
            && self.name == other.name
            && self.size == other.size
            && self.file_type == other.file_type
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SingleFileRequest {
    pub path: String,
}

#[instrument(skip(state))]
pub async fn get_file(
    State(state): State<Arc<AppState>>,
    Json(request): Json<SingleFileRequest>,
) -> Result<Json<File>, CoreError> {
    debug!("get_file: {:?}", request.path);
    let repo_path = state.app_config.read().repo_path.clone();

    let file_path = format!("{}/{}", repo_path, request.path.clone());

    let file: File = {
        let lock_cache = state.lock_cache.read().await;

        let metadata = fs::metadata(file_path.clone())?;

        let file_type = if metadata.is_dir() {
            FileType::Directory
        } else {
            FileType::File
        };

        let lfs_state = match file_type {
            FileType::File => {
                let mut f = fs::File::open(file_path.clone())?;
                // 19 bytes gets "version https://git", which is convincing
                // enough to determine if a file is an LFS stub
                let mut buffer = [0; 19];

                f.read_exact(&mut buffer).ok();
                if buffer.eq(b"version https://git") {
                    LocalFileLFSState::Stub
                } else {
                    LocalFileLFSState::Local
                }
            }
            FileType::Directory => LocalFileLFSState::None,
        };

        let size = match lfs_state {
            LocalFileLFSState::Stub => {
                let reader = BufReader::new(fs::File::open(file_path)?);

                // get 3rd line, split by space, get 2nd element, parse to u64
                let size: u64 = reader
                    .lines()
                    .nth(2)
                    .and_then(|line| line.ok())
                    .and_then(|line| line.clone().split(' ').nth(1).map(String::from))
                    .and_then(|size_str| size_str.parse().ok())
                    .ok_or(CoreError::Input(anyhow!("Failed to parse LFS size")))?;

                size
            }
            _ => metadata.len(),
        };

        let lock_info = lock_cache.get(&request.path.clone()).cloned();

        let name = Path::new(&request.path)
            .file_stem()
            .unwrap()
            .to_string_lossy()
            .to_string();

        File {
            path: request.path.clone(),
            name,
            size,
            file_type,
            lfs_state,
            locked: lock_info.is_some(),
            lock_info,
        }
    };

    Ok(Json(file))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileParams {
    pub root: Option<String>,
}

#[instrument(skip(state))]
pub async fn get_files(
    State(state): State<Arc<AppState>>,
    params: Query<FileParams>,
) -> Result<Json<Vec<File>>, CoreError> {
    let repo_path = state.app_config.read().repo_path.clone();

    let query_path = if let Some(root) = &params.root {
        format!("{repo_path}/{root}")
    } else {
        repo_path
    };

    debug!("query_path: {}", query_path);

    let files: Vec<File> = {
        let lock_cache = state.lock_cache.read().await;

        fs::read_dir(query_path)?
            .filter_map(|entry| {
                let entry = entry.ok()?;

                // Skip hidden files
                if entry.file_name().to_string_lossy().starts_with('.') {
                    return None;
                }

                let metadata = entry.metadata().ok()?;
                let file_type = if metadata.is_dir() {
                    FileType::Directory
                } else {
                    FileType::File
                };

                let lfs_state = match file_type {
                    FileType::File => {
                        let mut f = fs::File::open(entry.path()).ok()?;

                        // 19 bytes gets "version https://git", which is convincing
                        // enough to determine if a file is an LFS stub
                        let mut buffer = [0; 19];

                        f.read_exact(&mut buffer).ok()?;
                        if buffer.eq(b"version https://git") {
                            LocalFileLFSState::Stub
                        } else {
                            LocalFileLFSState::Local
                        }
                    }
                    FileType::Directory => LocalFileLFSState::None,
                };

                let size = match lfs_state {
                    LocalFileLFSState::Stub => {
                        let reader = BufReader::new(fs::File::open(entry.path()).ok()?);

                        // get 3rd line, split by space, get 2nd element, parse to u64
                        let size: u64 = reader
                            .lines()
                            .nth(2)
                            .unwrap()
                            .unwrap()
                            .split(' ')
                            .nth(1)
                            .unwrap()
                            .parse()
                            .unwrap();

                        size
                    }
                    _ => metadata.len(),
                };

                let full_path = match params.root {
                    Some(ref root) => format!("{}/{}", root, entry.file_name().to_string_lossy()),
                    None => entry.file_name().to_string_lossy().to_string(),
                };

                let lock_info = lock_cache.get(&full_path).cloned();

                let local_path: String = if full_path.starts_with('/') {
                    full_path.trim_start_matches('/').to_string()
                } else {
                    full_path
                };

                Some(File {
                    path: local_path,
                    name: entry.file_name().to_string_lossy().to_string(),
                    size,
                    file_type,
                    locked: lock_info.is_some(),
                    lock_info,
                    lfs_state,
                })
            })
            .collect()
    };

    // sort files by file type, then by name
    let mut files = files;
    files.sort_by(|a, b| {
        let file_type_cmp = a.file_type.cmp(&b.file_type);
        if file_type_cmp == std::cmp::Ordering::Equal {
            a.name.cmp(&b.name)
        } else {
            file_type_cmp
        }
    });

    debug!("files: {:?}", files);
    Ok(Json(files))
}

fn is_not_hidden(entry: &DirEntry) -> bool {
    entry
        .file_name()
        .to_str()
        .map(|s| entry.depth() == 0 || !s.starts_with('.'))
        .unwrap_or(false)
}

#[instrument(skip(state))]
pub async fn get_all_files(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<String>>, CoreError> {
    let repo_path = state.app_config.read().repo_path.clone();

    let files: Vec<String> = WalkDir::new(repo_path.clone())
        .into_iter()
        .filter_entry(is_not_hidden)
        .filter_map(|entry| {
            let entry = entry.ok()?;

            // Skip directories
            if entry.file_type().is_dir() {
                return None;
            }

            // remove repo path and trailing slash from the full path
            let full_path = entry.path().strip_prefix(&repo_path).ok()?;

            Some(full_path.to_string_lossy().to_string().replace('\\', "/"))
        })
        .collect();

    Ok(Json(files))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileHistoryParams {
    pub file: String,
    pub limit: Option<u32>,
}

#[instrument(skip(state))]
pub async fn get_file_history(
    State(state): State<Arc<AppState>>,
    params: Query<FileHistoryParams>,
) -> Result<Json<Vec<Commit>>, CoreError> {
    let limit = params.limit.unwrap_or(100);
    let output = state
        .git()
        .run_and_collect_output(
            &[
                "log",
                "--pretty=format:%h|%s|%an|%aI",
                "--follow",
                "--max-count",
                limit.to_string().as_str(),
                "--",
                &params.file,
            ],
            Default::default(),
        )
        .await?;

    let result = output
        .lines()
        .map(|line| {
            let parts = line.split('|').collect::<Vec<_>>();

            let timestamp = DateTime::parse_from_rfc3339(parts[3]).unwrap();

            Commit {
                sha: parts[0].to_string(),
                message: Some(parts[1].to_string()),
                author: Some(parts[2].to_string()),
                timestamp: Some(timestamp.with_timezone(&chrono::Local).to_string()),
                status: None,
            }
        })
        .collect::<Vec<_>>();

    info!("result: {:?}", result);

    Ok(Json(result))
}
