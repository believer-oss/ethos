use crate::state::AppState;
use axum::extract::{Query, State};
use axum::{debug_handler, Json};
use ethos_core::clients::git::SaveSnapshotIndexOption;
use ethos_core::types::errors::CoreError;
use ethos_core::types::repo::Snapshot;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Deserialize, Serialize)]
pub struct SaveSnapshotRequest {
    pub files: Vec<String>,
}

pub async fn list_snapshots(
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<Snapshot>>, CoreError> {
    let snapshots = state.git().list_snapshots().await?;
    Ok(Json(snapshots))
}

#[debug_handler]
pub async fn save_snapshot(
    State(state): State<Arc<AppState>>,
    Json(req): Json<SaveSnapshotRequest>,
) -> Result<(), CoreError> {
    state
        .git()
        .save_snapshot(req.files, SaveSnapshotIndexOption::KeepIndex)
        .await?;

    Ok(())
}

#[derive(Deserialize, Serialize)]
pub struct DeleteSnapshotParams {
    pub commit: String,
}

pub async fn delete_snapshot(
    State(state): State<Arc<AppState>>,
    params: Query<DeleteSnapshotParams>,
) -> Result<(), CoreError> {
    state.git().delete_snapshot(&params.commit).await?;

    Ok(())
}

#[derive(Deserialize, Serialize)]
pub struct RestoreSnapshotRequest {
    pub commit: String,
}

pub async fn restore_snapshot(
    State(state): State<Arc<AppState>>,
    Json(req): Json<RestoreSnapshotRequest>,
) -> Result<(), CoreError> {
    // get modified files
    let modified_files = state.repo_status.read().clone().modified_files;
    state
        .git()
        .restore_snapshot(&req.commit, modified_files.0)
        .await?;

    Ok(())
}
