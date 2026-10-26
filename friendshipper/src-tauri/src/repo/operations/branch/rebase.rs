use std::path::PathBuf;
use std::sync::Arc;

use anyhow::anyhow;
use axum::extract::State;
use axum::{async_trait, Json};
use tracing::instrument;

use super::rebase_state::{build_rebase_state, derive_rebase_state, REBASE_OP_LOCK};
use crate::engine::EngineProvider;
use crate::repo::RepoStatusRef;
use crate::state::AppState;
use ethos_core::clients::git;
use ethos_core::types::config::{AppConfigRef, RepoConfigRef};
use ethos_core::types::errors::CoreError;
use ethos_core::types::repo::{
    OpenConflictFileRequest, RebaseCommitInfo, RebaseFinishResponse, RebaseKeep, RebasePreflight,
    RebaseState, ResolveConflictRequest, UnresolveConflictRequest,
};
use ethos_core::worker::Task;

fn not_implemented() -> CoreError {
    CoreError::Internal(anyhow!("Guided rebase is not implemented yet"))
}

#[derive(Clone)]
pub struct RebaseStartOp<T> {
    pub git_client: git::Git,
    pub engine: T,
    pub app_config: AppConfigRef,
    pub repo_config: RepoConfigRef,
    pub repo_status: RepoStatusRef,
    pub sync_phase_tx: std::sync::mpsc::Sender<String>,
}

#[async_trait]
impl<T> Task for RebaseStartOp<T>
where
    T: EngineProvider,
{
    async fn execute(&self) -> Result<(), CoreError> {
        // TODO: Implement in Stage 009
        Err(not_implemented())
    }

    fn get_name(&self) -> String {
        String::from("RebaseStartOp")
    }
}

#[derive(Clone)]
pub struct RebaseResolveOp<T> {
    pub git_client: git::Git,
    pub engine: T,
    pub app_config: AppConfigRef,
    pub repo_config: RepoConfigRef,
    pub repo_status: RepoStatusRef,
    pub sync_phase_tx: std::sync::mpsc::Sender<String>,
    pub path: String,
    pub keep: RebaseKeep,
}

#[async_trait]
impl<T> Task for RebaseResolveOp<T>
where
    T: EngineProvider,
{
    async fn execute(&self) -> Result<(), CoreError> {
        // TODO: Implement in Stage 009
        Err(not_implemented())
    }

    fn get_name(&self) -> String {
        String::from("RebaseResolveOp")
    }
}

#[derive(Clone)]
pub struct RebaseUnresolveOp<T> {
    pub git_client: git::Git,
    pub engine: T,
    pub app_config: AppConfigRef,
    pub repo_config: RepoConfigRef,
    pub repo_status: RepoStatusRef,
    pub sync_phase_tx: std::sync::mpsc::Sender<String>,
    pub path: String,
}

#[async_trait]
impl<T> Task for RebaseUnresolveOp<T>
where
    T: EngineProvider,
{
    async fn execute(&self) -> Result<(), CoreError> {
        // TODO: Implement in Stage 009
        Err(not_implemented())
    }

    fn get_name(&self) -> String {
        String::from("RebaseUnresolveOp")
    }
}

#[derive(Clone)]
pub struct RebaseContinueOp<T> {
    pub git_client: git::Git,
    pub engine: T,
    pub app_config: AppConfigRef,
    pub repo_config: RepoConfigRef,
    pub repo_status: RepoStatusRef,
    pub sync_phase_tx: std::sync::mpsc::Sender<String>,
}

#[async_trait]
impl<T> Task for RebaseContinueOp<T>
where
    T: EngineProvider,
{
    async fn execute(&self) -> Result<(), CoreError> {
        // TODO: Implement in Stage 011
        Err(not_implemented())
    }

    fn get_name(&self) -> String {
        String::from("RebaseContinueOp")
    }
}

#[derive(Clone)]
pub struct RebaseAbortOp<T> {
    pub git_client: git::Git,
    pub engine: T,
    pub app_config: AppConfigRef,
    pub repo_config: RepoConfigRef,
    pub repo_status: RepoStatusRef,
    pub sync_phase_tx: std::sync::mpsc::Sender<String>,
}

#[async_trait]
impl<T> Task for RebaseAbortOp<T>
where
    T: EngineProvider,
{
    async fn execute(&self) -> Result<(), CoreError> {
        // TODO: Implement in Stage 011
        Err(not_implemented())
    }

    fn get_name(&self) -> String {
        String::from("RebaseAbortOp")
    }
}

#[derive(Clone)]
pub struct RebaseFinishOp<T> {
    pub git_client: git::Git,
    pub engine: T,
    pub app_config: AppConfigRef,
    pub repo_config: RepoConfigRef,
    pub repo_status: RepoStatusRef,
    pub sync_phase_tx: std::sync::mpsc::Sender<String>,
    pub result: Arc<parking_lot::Mutex<Option<RebaseFinishResponse>>>,
}

#[async_trait]
impl<T> Task for RebaseFinishOp<T>
where
    T: EngineProvider,
{
    async fn execute(&self) -> Result<(), CoreError> {
        // TODO: Implement in Stage 013
        Err(not_implemented())
    }

    fn get_name(&self) -> String {
        String::from("RebaseFinishOp")
    }
}

#[derive(Clone)]
pub struct RebaseRedoOp<T> {
    pub git_client: git::Git,
    pub engine: T,
    pub app_config: AppConfigRef,
    pub repo_config: RepoConfigRef,
    pub repo_status: RepoStatusRef,
    pub sync_phase_tx: std::sync::mpsc::Sender<String>,
}

#[async_trait]
impl<T> Task for RebaseRedoOp<T>
where
    T: EngineProvider,
{
    async fn execute(&self) -> Result<(), CoreError> {
        // TODO: Implement in Stage 013
        Err(not_implemented())
    }

    fn get_name(&self) -> String {
        String::from("RebaseRedoOp")
    }
}

#[derive(Clone)]
pub struct RebaseResetToBackupOp<T> {
    pub git_client: git::Git,
    pub engine: T,
    pub app_config: AppConfigRef,
    pub repo_config: RepoConfigRef,
    pub repo_status: RepoStatusRef,
    pub sync_phase_tx: std::sync::mpsc::Sender<String>,
}

#[async_trait]
impl<T> Task for RebaseResetToBackupOp<T>
where
    T: EngineProvider,
{
    async fn execute(&self) -> Result<(), CoreError> {
        // TODO: Implement in Stage 013
        Err(not_implemented())
    }

    fn get_name(&self) -> String {
        String::from("RebaseResetToBackupOp")
    }
}

#[derive(Clone)]
pub struct EditorRebuildOp<T> {
    pub git_client: git::Git,
    pub engine: T,
    pub app_config: AppConfigRef,
    pub repo_config: RepoConfigRef,
    pub repo_status: RepoStatusRef,
    pub sync_phase_tx: std::sync::mpsc::Sender<String>,
    pub result: Arc<parking_lot::Mutex<Option<RebaseCommitInfo>>>,
}

#[async_trait]
impl<T> Task for EditorRebuildOp<T>
where
    T: EngineProvider,
{
    async fn execute(&self) -> Result<(), CoreError> {
        // TODO: Implement in Stage 013
        Err(not_implemented())
    }

    fn get_name(&self) -> String {
        String::from("EditorRebuildOp")
    }
}

#[instrument(skip_all)]
pub async fn guided_rebase_state_handler<T>(
    State(state): State<AppState<T>>,
) -> Result<Json<RebaseState>, CoreError>
where
    T: EngineProvider,
{
    let repo_path = PathBuf::from(state.app_config.read().repo_path.clone());
    if repo_path.as_os_str().is_empty() {
        return Ok(Json(RebaseState::default()));
    }
    let engine_path = state
        .app_config
        .read()
        .load_engine_path_from_repo(&state.repo_config.read())
        .unwrap_or_default();
    let git = state.git();

    // Conflicts are cached only while no rebase op runs, so a half-applied step never is.
    let op_guard = REBASE_OP_LOCK.try_lock();
    let derived = derive_rebase_state(&git, &repo_path)
        .await
        .map_err(CoreError::Internal)?;
    let rebase_state = build_rebase_state(
        &git,
        &repo_path,
        &state.engine,
        &engine_path,
        derived,
        op_guard.is_ok(),
    )
    .await
    .map_err(CoreError::Internal)?;
    drop(op_guard);

    Ok(Json(rebase_state))
}

#[instrument(skip_all)]
pub async fn guided_rebase_preflight_handler<T>(
    State(_state): State<AppState<T>>,
) -> Result<Json<RebasePreflight>, CoreError>
where
    T: EngineProvider,
{
    // TODO: Implement in Stage 009
    Err(not_implemented())
}

#[instrument(skip_all)]
pub async fn guided_rebase_start_handler<T>(
    State(_state): State<AppState<T>>,
) -> Result<Json<RebaseState>, CoreError>
where
    T: EngineProvider,
{
    // TODO: Implement in Stage 009
    Err(not_implemented())
}

#[instrument(skip_all)]
pub async fn guided_rebase_resolve_handler<T>(
    State(_state): State<AppState<T>>,
    Json(_req): Json<ResolveConflictRequest>,
) -> Result<Json<RebaseState>, CoreError>
where
    T: EngineProvider,
{
    // TODO: Implement in Stage 009
    Err(not_implemented())
}

#[instrument(skip_all)]
pub async fn guided_rebase_unresolve_handler<T>(
    State(_state): State<AppState<T>>,
    Json(_req): Json<UnresolveConflictRequest>,
) -> Result<Json<RebaseState>, CoreError>
where
    T: EngineProvider,
{
    // TODO: Implement in Stage 009
    Err(not_implemented())
}

#[instrument(skip_all)]
pub async fn guided_rebase_open_file_handler<T>(
    State(_state): State<AppState<T>>,
    Json(_req): Json<OpenConflictFileRequest>,
) -> Result<(), CoreError>
where
    T: EngineProvider,
{
    // TODO: Implement in Stage 009
    Err(not_implemented())
}

#[instrument(skip_all)]
pub async fn guided_rebase_dismiss_handler<T>(
    State(_state): State<AppState<T>>,
) -> Result<Json<RebaseState>, CoreError>
where
    T: EngineProvider,
{
    // TODO: Implement in Stage 009
    Err(not_implemented())
}

#[instrument(skip_all)]
pub async fn guided_rebase_continue_handler<T>(
    State(_state): State<AppState<T>>,
) -> Result<Json<RebaseState>, CoreError>
where
    T: EngineProvider,
{
    // TODO: Implement in Stage 011
    Err(not_implemented())
}

#[instrument(skip_all)]
pub async fn guided_rebase_abort_handler<T>(
    State(_state): State<AppState<T>>,
) -> Result<Json<RebaseState>, CoreError>
where
    T: EngineProvider,
{
    // TODO: Implement in Stage 011
    Err(not_implemented())
}

#[instrument(skip_all)]
pub async fn guided_rebase_finish_handler<T>(
    State(_state): State<AppState<T>>,
) -> Result<Json<RebaseFinishResponse>, CoreError>
where
    T: EngineProvider,
{
    // TODO: Implement in Stage 013
    Err(not_implemented())
}

#[instrument(skip_all)]
pub async fn guided_rebase_redo_handler<T>(
    State(_state): State<AppState<T>>,
) -> Result<Json<RebaseState>, CoreError>
where
    T: EngineProvider,
{
    // TODO: Implement in Stage 013
    Err(not_implemented())
}

#[instrument(skip_all)]
pub async fn guided_rebase_reset_to_backup_handler<T>(
    State(_state): State<AppState<T>>,
) -> Result<Json<RebaseState>, CoreError>
where
    T: EngineProvider,
{
    // TODO: Implement in Stage 013
    Err(not_implemented())
}

#[instrument(skip_all)]
pub async fn editor_rebuild_handler<T>(
    State(_state): State<AppState<T>>,
) -> Result<Json<RebaseCommitInfo>, CoreError>
where
    T: EngineProvider,
{
    // TODO: Implement in Stage 013
    Err(not_implemented())
}
