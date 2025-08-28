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
    state
        .git()
        .save_snapshot(&req.message, req.files, SaveSnapshotIndexOption::KeepIndex)
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
        .restore_snapshot(&req.commit, modified_files.0)
        .await?;

    Ok(())
}
