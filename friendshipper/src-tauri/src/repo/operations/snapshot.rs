use crate::engine::EngineProvider;
use crate::state::AppState;
use axum::extract::{Query, State};
use axum::Json;
use ethos_core::clients::git::SaveSnapshotIndexOption;
use ethos_core::types::errors::CoreError;
use ethos_core::types::repo::{Snapshot, SnapshotPreviewEntry};
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize)]
pub struct SaveSnapshotRequest {
    pub message: String,
    pub files: Vec<String>,
}

pub async fn list_snapshots<T>(
    State(state): State<AppState<T>>,
) -> Result<Json<Vec<Snapshot>>, CoreError>
where
    T: EngineProvider,
{
    let snapshots = state.git().list_snapshots().await?;
    Ok(Json(snapshots))
}

pub async fn save_snapshot<T>(
    State(state): State<AppState<T>>,
    Json(req): Json<SaveSnapshotRequest>,
) -> Result<(), CoreError>
where
    T: EngineProvider,
{
    // An empty `files` list means "snapshot all my changes" from the UI. We
    // already track the dirty set in `repo_status` (same source the UI shows),
    // so resolve it here instead of letting core run `git add -A -- .`, which
    // would walk the entire working tree. On a large Unreal repo that walk
    // dominates snapshot latency for the all-files case.
    let files = if req.files.is_empty() {
        let status = state.repo_status.read();
        status
            .modified_files
            .0
            .iter()
            .chain(status.untracked_files.0.iter())
            .map(|f| f.path.clone())
            .collect()
    } else {
        req.files
    };

    state
        .git()
        .save_snapshot(&req.message, files, SaveSnapshotIndexOption::KeepIndex)
        .await?;

    Ok(())
}

#[derive(Deserialize, Serialize)]
pub struct DeleteSnapshotParams {
    pub commit: String,
}

pub async fn delete_snapshot<T>(
    State(state): State<AppState<T>>,
    params: Query<DeleteSnapshotParams>,
) -> Result<(), CoreError>
where
    T: EngineProvider,
{
    state.git().delete_snapshot(&params.commit).await?;

    Ok(())
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreSnapshotRequest {
    pub commit: String,
    /// Optional subset of repo-relative paths to restore. When `None` or
    /// empty, every file in the snapshot is restored using the existing
    /// cherry-pick path. When set, only the listed paths are touched.
    #[serde(default)]
    pub files: Option<Vec<String>>,
    /// When true, paths with uncommitted local changes are still overwritten
    /// with the snapshot's contents. When false (default), the restore is
    /// refused if any selected path conflicts with local work, so a stale
    /// modal can't silently clobber changes.
    #[serde(default)]
    pub overwrite_local: bool,
}

pub async fn restore_snapshot<T>(
    State(state): State<AppState<T>>,
    Json(req): Json<RestoreSnapshotRequest>,
) -> Result<(), CoreError>
where
    T: EngineProvider,
{
    // Pass BOTH modified and untracked as "local files" so the backend's
    // conflict gate matches the preview's `conflictsWithLocal` definition.
    // Otherwise a caller bypassing the UI could silently clobber an untracked
    // file with `overwrite_local: false` because only modified files were
    // being checked.
    let mut local_files = {
        let status = state.repo_status.read();
        let mut v = status.modified_files.0.clone();
        v.extend(status.untracked_files.0.iter().cloned());
        v
    };
    // De-dup defensively in case a path somehow shows up in both lists.
    local_files.sort_by(|a, b| a.path.cmp(&b.path));
    local_files.dedup_by(|a, b| a.path == b.path);

    // Treat an empty `Some([])` the same as `None` — the frontend's
    // "selectively choose files" toggle can hand us an empty array if the
    // user deselects everything, and we'd rather no-op than fall through to
    // a full restore in that case.
    let paths_filter = req.files.filter(|v| !v.is_empty());
    state
        .git()
        .restore_snapshot(&req.commit, local_files, req.overwrite_local, paths_filter)
        .await?;

    Ok(())
}

#[derive(Deserialize, Serialize)]
pub struct PreviewSnapshotParams {
    pub commit: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewSnapshotResponse {
    pub entries: Vec<SnapshotPreviewEntry>,
}

pub async fn preview_snapshot<T>(
    State(state): State<AppState<T>>,
    params: Query<PreviewSnapshotParams>,
) -> Result<Json<PreviewSnapshotResponse>, CoreError>
where
    T: EngineProvider,
{
    let raw_entries = state
        .git()
        .get_snapshot_entries_with_state(&params.commit)
        .await?;

    let (modified_files, untracked_files) = {
        let status = state.repo_status.read();
        (
            status.modified_files.clone(),
            status.untracked_files.clone(),
        )
    };
    let in_local =
        |rel: &str| -> bool { modified_files.contains(rel) || untracked_files.contains(rel) };

    let repo_path = state.app_config.read().repo_path.clone();
    let repo_root = std::path::PathBuf::from(repo_path);

    let entries: Vec<SnapshotPreviewEntry> = raw_entries
        .into_iter()
        .map(|(path, state_value)| {
            let exists_on_disk = repo_root.join(&path).exists();
            SnapshotPreviewEntry {
                conflicts_with_local: in_local(&path),
                exists_on_disk,
                path,
                state: state_value,
            }
        })
        .collect();

    Ok(Json(PreviewSnapshotResponse { entries }))
}
