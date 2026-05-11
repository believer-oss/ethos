use crate::engine::EngineProvider;
use crate::state::AppState;
use axum::extract::{Query, State};
use axum::Json;
use ethos_core::clients::git::SaveSnapshotIndexOption;
use ethos_core::types::errors::CoreError;
use ethos_core::types::repo::Snapshot;
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
pub struct RestoreSnapshotRequest {
    pub commit: String,
}

pub async fn restore_snapshot<T>(
    State(state): State<AppState<T>>,
    Json(req): Json<RestoreSnapshotRequest>,
) -> Result<(), CoreError>
where
    T: EngineProvider,
{
    // get modified files
    let modified_files = state.repo_status.read().clone().modified_files;
    state
        .git()
        .restore_snapshot(&req.commit, modified_files.0, true) // UI restore: prefer snapshot versions
        .await?;

    Ok(())
}
