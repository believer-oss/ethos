use std::collections::{HashMap, HashSet};

use axum::extract::{Query, State};
use axum::Json;
use ethos_core::clients::git::Opts;
use ethos_core::types::errors::CoreError;
use ethos_core::types::repo::{
    FileState, RepoDirectoryEntry, RepoDirectoryListing, RepoFileKind, RepoFileState,
};
use serde::Deserialize;
use tracing::{debug, instrument};

use super::sanitize_repo_path;
use crate::engine::EngineProvider;
use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct BrowseParams {
    #[serde(default)]
    pub path: String,
}

#[instrument(skip(state))]
pub async fn list_directory_handler<T>(
    State(state): State<AppState<T>>,
    Query(params): Query<BrowseParams>,
) -> Result<Json<RepoDirectoryListing>, CoreError>
where
    T: EngineProvider,
{
    let path = sanitize_repo_path(&params.path)?;

    // Expected "doesn't resolve" cases we treat as an empty listing rather than a hard error:
    //   - "Not a valid object name": HEAD is unborn (fresh repo) or the path doesn't exist at HEAD
    //   - "not a tree object":       the path resolves to a blob, not a directory
    // Anything else (permissions, corruption, etc.) still bubbles up to the caller.
    const LS_TREE_IGNORED: &[&str] = &["Not a valid object name", "not a tree object"];

    let quiet_opts = Opts {
        skip_notify_frontend: true,
        should_log_stdout: false,
        ignored_errors: LS_TREE_IGNORED,
        ..Default::default()
    };

    // 1) Get the depth-1 tracked tree at HEAD for this path.
    //    `ls-tree HEAD` for root, `ls-tree HEAD:<path>` for sub-dirs.
    //    Output: "<mode> <type> <hash>\t<name>" per line, newline-terminated.
    let tree_ref = if path.is_empty() {
        "HEAD".to_string()
    } else {
        format!("HEAD:{}", path)
    };

    let tree_output = state
        .git()
        .run_and_collect_output_into_lines(&["ls-tree", &tree_ref], quiet_opts)
        .await
        .map_err(|e| CoreError::Internal(anyhow::anyhow!("git ls-tree failed: {}", e)))?;

    debug!(
        path = %path,
        tree_ref = %tree_ref,
        tree_lines = tree_output.len(),
        "list_directory ls-tree result"
    );

    let mut directories: Vec<RepoDirectoryEntry> = Vec::new();
    let mut files: Vec<RepoDirectoryEntry> = Vec::new();

    for line in &tree_output {
        // Split once on `\t` to separate metadata from name.
        let Some((meta, name)) = line.split_once('\t') else {
            continue;
        };
        // meta = "<mode> <type> <hash>"
        let mut meta_parts = meta.split_whitespace();
        let _mode = meta_parts.next();
        let kind = meta_parts.next().unwrap_or("");
        let full_path = if path.is_empty() {
            name.to_string()
        } else {
            format!("{}/{}", path, name)
        };

        match kind {
            "tree" => directories.push(RepoDirectoryEntry {
                name: name.to_string(),
                path: full_path,
                kind: RepoFileKind::Directory,
                state: RepoFileState::Unmodified,
                size: None,
            }),
            "blob" | "commit" => files.push(RepoDirectoryEntry {
                name: name.to_string(),
                path: full_path,
                kind: RepoFileKind::File,
                state: RepoFileState::Unmodified, // overlaid below
                size: None,
            }),
            _ => {}
        }
    }

    // 2) Overlay state from cached RepoStatus.
    let status = state.repo_status.read().clone();

    let mut state_by_path: HashMap<String, RepoFileState> = HashMap::new();
    for f in &status.modified_files.0 {
        let s = match f.state {
            FileState::Modified => RepoFileState::Modified,
            FileState::Added => RepoFileState::Added,
            FileState::Deleted => RepoFileState::Deleted,
            FileState::Unmerged => RepoFileState::Conflicted,
            FileState::Unknown => RepoFileState::Modified,
        };
        state_by_path.insert(f.path.clone(), s);
    }
    for p in &status.conflicts {
        state_by_path.insert(p.clone(), RepoFileState::Conflicted);
    }
    let upstream_modified: HashSet<String> = status.modified_upstream.iter().cloned().collect();

    for f in files.iter_mut() {
        if let Some(s) = state_by_path.get(&f.path).copied() {
            f.state = s;
        } else if upstream_modified.contains(&f.path) {
            f.state = RepoFileState::OutOfDate;
        }
    }

    // 3) Untracked files at this directory level. `untracked_files` from status is already parsed
    //    and covers the whole repo; filter to entries whose parent directory is exactly `path`.
    let prefix_with_slash = if path.is_empty() {
        String::new()
    } else {
        format!("{}/", path)
    };

    let mut untracked_dirs: HashSet<String> = HashSet::new();
    let existing_file_paths: HashSet<String> = files.iter().map(|f| f.path.clone()).collect();
    let existing_dir_paths: HashSet<String> = directories.iter().map(|d| d.path.clone()).collect();

    for f in &status.untracked_files.0 {
        // Only keep entries under the current path.
        let remainder = if prefix_with_slash.is_empty() {
            f.path.as_str()
        } else if let Some(r) = f.path.strip_prefix(&prefix_with_slash) {
            r
        } else {
            continue;
        };

        if let Some(slash_idx) = remainder.find('/') {
            // Untracked file lives inside a subdirectory of this folder — ensure the subdirectory
            // appears in the listing even if it has no tracked content.
            let dir_name = &remainder[..slash_idx];
            let dir_full = if path.is_empty() {
                dir_name.to_string()
            } else {
                format!("{}/{}", path, dir_name)
            };
            if !existing_dir_paths.contains(&dir_full) {
                untracked_dirs.insert(dir_name.to_string());
            }
        } else if !existing_file_paths.contains(&f.path) {
            // Direct untracked file in this folder.
            files.push(RepoDirectoryEntry {
                name: remainder.to_string(),
                path: f.path.clone(),
                kind: RepoFileKind::File,
                state: RepoFileState::Untracked,
                size: None,
            });
        }
    }

    for name in untracked_dirs {
        let dir_full = if path.is_empty() {
            name.clone()
        } else {
            format!("{}/{}", path, name)
        };
        directories.push(RepoDirectoryEntry {
            name,
            path: dir_full,
            kind: RepoFileKind::Directory,
            state: RepoFileState::Unmodified,
            size: None,
        });
    }

    directories.sort_by_key(|a| a.name.to_lowercase());
    files.sort_by_key(|a| a.name.to_lowercase());

    let mut entries = directories;
    entries.extend(files);

    debug!(
        path = %path,
        entry_count = entries.len(),
        "list_directory result"
    );

    Ok(Json(RepoDirectoryListing { path, entries }))
}
