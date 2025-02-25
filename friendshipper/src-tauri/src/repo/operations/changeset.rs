use crate::engine::EngineProvider;
use crate::state::AppState;
use anyhow::anyhow;
use axum::extract::State;
use axum::Json;
use ethos_core::types::errors::CoreError;
use ethos_core::types::repo::{ChangeSet, File};
use serde::{Deserialize, Serialize};
use tracing::instrument;

pub const CHANGE_SETS_PATH: &str = "changesets.json";
pub const FRIENDSHIPPER_APPDATA_DIR: &str = "com.believer.friendshipper";

#[derive(Debug, Deserialize, Serialize)]
pub struct SaveChangeSetRequest {
    pub change_sets: Vec<ChangeSet>,
}

#[instrument(skip(state), err)]
pub async fn save_changeset<T>(
    State(state): State<AppState<T>>,
    Json(req): Json<SaveChangeSetRequest>,
) -> Result<(), CoreError>
where
    T: EngineProvider,
{
    let app_data_dir = dirs::data_local_dir().ok_or_else(|| {
        CoreError::Internal(anyhow!(
            "Could not find local app data path, unable to load changesets."
        ))
    })?;
    let repo_name = state
        .app_config
        .read()
        .selected_artifact_project
        .clone()
        .ok_or_else(|| {
            CoreError::Internal(anyhow!(
                "No selected artifact project found, unable to load changesets."
            ))
        })?;
    let save_file = std::path::Path::new(&app_data_dir)
        .join(FRIENDSHIPPER_APPDATA_DIR)
        .join(&repo_name)
        .join(CHANGE_SETS_PATH);

    if let Some(parent) = save_file.parent() {
        if !parent.exists() {
            std::fs::create_dir_all(parent).map_err(|e| {
                CoreError::Internal(anyhow!(
                    "Failed to create directory {}: {}",
                    parent.display(),
                    e
                ))
            })?;
        }
    }

    let json = serde_json::to_string_pretty(&req.change_sets)
        .map_err(|e| CoreError::Internal(anyhow!(e)))?;

    std::fs::write(&save_file, json).map_err(|e| {
        CoreError::Internal(anyhow!("Failed to write to {}: {}", save_file.display(), e))
    })?;

    Ok(())
}

#[instrument(skip(state), err)]
pub async fn load_changeset<T>(
    State(state): State<AppState<T>>,
) -> Result<Json<Vec<ChangeSet>>, CoreError>
where
    T: EngineProvider,
{
    let app_data_dir = dirs::data_local_dir().ok_or_else(|| {
        CoreError::Internal(anyhow!(
            "Could not find local app data path, unable to load changesets."
        ))
    })?;
    let repo_name = state
        .app_config
        .read()
        .selected_artifact_project
        .clone()
        .ok_or_else(|| {
            CoreError::Internal(anyhow!(
                "No selected artifact project found, unable to load changesets."
            ))
        })?;

    let deprecated_save_file = std::path::Path::new(&app_data_dir)
        .join(FRIENDSHIPPER_APPDATA_DIR)
        .join(CHANGE_SETS_PATH);

    let save_file = std::path::Path::new(&app_data_dir)
        .join(FRIENDSHIPPER_APPDATA_DIR)
        .join(&repo_name)
        .join(CHANGE_SETS_PATH);

    if deprecated_save_file.exists() {
        // if we still have changesets in the deprecated location, migrate them over to {active repo name}/CHANGE_SETS_PATH
        let old_changesets = std::fs::read_to_string(&deprecated_save_file)
            .map_err(|e| CoreError::Internal(anyhow!(e)))?;

        if let Some(parent) = save_file.parent() {
            if !parent.exists() {
                std::fs::create_dir_all(parent).map_err(|e| CoreError::Internal(anyhow!(e)))?;
            }
        }

        std::fs::write(&save_file, old_changesets).map_err(|e| {
            CoreError::Internal(anyhow!("Failed to write to {}: {}", save_file.display(), e))
        })?;

        std::fs::remove_file(&deprecated_save_file).map_err(|e| {
            CoreError::Internal(anyhow!(
                "Failed to remove {}: {}",
                deprecated_save_file.display(),
                e
            ))
        })?;
    }

    let mut changesets: Vec<ChangeSet>;
    if !save_file.exists() {
        let repo_status = state.repo_status.read().clone();
        let mut all_changes: Vec<File> = Vec::new();
        all_changes.extend(repo_status.modified_files.clone());
        all_changes.extend(repo_status.untracked_files.clone());

        changesets = vec![ChangeSet {
            name: "default".to_string(),
            files: all_changes,
            open: true,
            checked: false,
            indeterminate: false,
        }];
    } else {
        let json = std::fs::read_to_string(&save_file).map_err(|e| {
            CoreError::Internal(anyhow!(
                "Failed to read from {}: {}",
                save_file.display(),
                e
            ))
        })?;
        changesets = serde_json::from_str(&json).map_err(|e| {
            CoreError::Internal(anyhow!(
                "Failed to parse changesets from {}: {}",
                save_file.display(),
                e
            ))
        })?;

        for changeset in &mut changesets {
            // reset checked state for UI
            changeset.checked = false;
            changeset.indeterminate = false;
        }
    }
    Ok(Json(changesets))
}
