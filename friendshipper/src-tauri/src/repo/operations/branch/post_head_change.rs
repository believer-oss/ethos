use std::path::PathBuf;
use std::sync::mpsc::Sender;

use anyhow::{anyhow, Context};
use tracing::{error, info};

use crate::engine::EngineProvider;
use crate::repo::operations::{DownloadDllsOp, StatusOp, UpdateEngineOp};
use crate::state::AppState;
use ethos_core::artifact_sync::SyncEvent;
use ethos_core::artifact_sync::{ArtifactSync, DownloadCancellation};
use ethos_core::clients::git;
use ethos_core::clients::github::GraphQLClient;
use ethos_core::storage::config::Project;
use ethos_core::storage::ArtifactStorage;
use ethos_core::types::config::{AppConfigRef, RepoConfig, RepoConfigRef, UProject};
use ethos_core::types::errors::CoreError;
use ethos_core::types::repo::RepoStatusRef;
use ethos_core::worker::Task;
use ethos_core::AWSClient;

#[derive(Clone)]
pub(crate) struct PostHeadChangeCtx<T> {
    pub app_config: AppConfigRef,
    pub repo_config: RepoConfigRef,
    pub repo_status: RepoStatusRef,
    pub artifact_sync: ArtifactSync,
    pub downloads: DownloadCancellation,
    pub sync_event_tx: Sender<SyncEvent>,
    pub sync_phase_tx: Sender<String>,
    pub aws_client: AWSClient,
    pub storage: ArtifactStorage,
    pub git_client: git::Git,
    pub github_client: Option<GraphQLClient>,
    pub engine: T,
}

impl<T: EngineProvider> PostHeadChangeCtx<T> {
    pub(crate) fn new(
        state: &AppState<T>,
        aws_client: AWSClient,
        storage: ArtifactStorage,
    ) -> Self {
        PostHeadChangeCtx {
            app_config: state.app_config.clone(),
            repo_config: state.repo_config.clone(),
            repo_status: state.repo_status.clone(),
            artifact_sync: state.artifact_sync.clone(),
            downloads: state.downloads.clone(),
            sync_event_tx: state.sync_event_tx.clone(),
            sync_phase_tx: state.sync_phase_tx.clone(),
            aws_client,
            storage,
            git_client: state.git(),
            github_client: state.github_client.read().clone(),
            engine: state.engine.clone(),
        }
    }
}

/// Brings the working copy's derived state in line with a new HEAD: refreshes status,
/// downloads editor binaries for the new commit (only when `pull_dlls` is on), and updates
/// the engine if the uproject's engine association changed.
///
/// Shared by operations that move HEAD to another commit, such as a branch switch.
/// `old_engine_association` is the association read before HEAD moved; `None` skips the
/// engine update.
pub(crate) async fn run_post_head_change<T: EngineProvider>(
    ctx: &PostHeadChangeCtx<T>,
    old_engine_association: Option<String>,
) -> Result<(), CoreError> {
    let emit_phase = |phase: &str| {
        let _ = ctx.sync_phase_tx.send(phase.to_string());
    };

    emit_phase("Refreshing status");
    let github_username = ctx
        .github_client
        .clone()
        .map_or(String::default(), |x| x.username.clone());
    let status_op = StatusOp {
        git_client: ctx.git_client.clone(),
        github_username,
        repo_status: ctx.repo_status.clone(),
        app_config: ctx.app_config.clone(),
        repo_config: ctx.repo_config.clone(),
        engine: ctx.engine.clone(),
        aws_client: Some(ctx.aws_client.clone()),
        storage: Some(ctx.storage.clone()),
        allow_offline_communication: false,
        skip_display_names: true,
        skip_engine_update: false,
        // internal post-operation refresh, not a user-initiated one
        fetch_first: false,
    };
    status_op.execute().await?;

    let repo_path = ctx.app_config.read().repo_path.clone();
    let uproject_path_relative = ctx.repo_config.read().uproject_path.clone();
    let uproject_path = PathBuf::from(&repo_path).join(&uproject_path_relative);

    let pull_dlls = ctx.app_config.read().pull_dlls;
    if pull_dlls {
        let project: Project = ctx
            .app_config
            .read()
            .selected_artifact_project
            .clone()
            .context("No selected artifact project found in config.")?
            .as_str()
            .into();

        let uproject = UProject::load(&uproject_path)?;
        let engine_path = ctx.app_config.read().get_engine_path(&uproject);
        match RepoConfig::get_project_name(&uproject_path_relative) {
            Some(project_name) => {
                emit_phase("Downloading latest binaries");
                let download_dlls_op = DownloadDllsOp {
                    git_client: ctx.git_client.clone(),
                    project_name,
                    dll_commit: ctx.repo_status.read().dll_commit_remote.clone(),
                    download_symbols: ctx.app_config.read().editor_download_symbols,
                    storage: ctx.storage.clone(),
                    artifact_sync: ctx.artifact_sync.clone(),
                    downloads: ctx.downloads.clone(),
                    tx: ctx.sync_event_tx.clone(),
                    aws_client: ctx.aws_client.clone(),
                    project,
                    engine: ctx.engine.clone(),
                    engine_path,
                    max_cache_size_bytes: ctx.app_config.read().editor_cache_size_bytes(),
                    transfer_acceleration: ctx.app_config.read().s3_transfer_acceleration,
                };
                download_dlls_op.execute().await?;
            }
            None => {
                error!(
                    "Unable to parse project name from uproject path {}. DLL download unavailable.",
                    &uproject_path_relative
                );
            }
        }
    } else {
        info!("pullDlls is off, skipping editor binary download");
    }

    let Some(old_engine_association) = old_engine_association else {
        return Ok(());
    };
    let new_uproject = match UProject::load(&uproject_path) {
        Err(e) => {
            error!(
                "Failed to load uproject after HEAD change, skipping engine update. Error: {}",
                e
            );
            return Ok(());
        }
        Ok(uproject) => uproject,
    };
    if new_uproject.engine_association == old_engine_association {
        return Ok(());
    }

    info!(
        "Engine association changed from {} to {}, updating engine.",
        old_engine_association, new_uproject.engine_association
    );
    emit_phase("Updating engine");
    let repo_owner = ctx.repo_status.read().repo_owner.clone();
    let repo_name = ctx.repo_status.read().repo_name.clone();
    let project = if repo_owner.is_empty() || repo_name.is_empty() {
        let selected_artifact_project = ctx.app_config.read().selected_artifact_project.clone();
        let (owner, repo) = match selected_artifact_project {
            Some(ref project) => project.split_once('-').ok_or(anyhow!("Invalid project"))?,
            None => {
                return Err(CoreError::Input(anyhow!(
                    "No selected artifact project found in config."
                )));
            }
        };

        Project::new(owner, repo)
    } else {
        Project::new(&repo_owner, &repo_name)
    };
    let engine_path = ctx.app_config.read().get_engine_path(&new_uproject);
    let update_engine_op = UpdateEngineOp {
        engine_path,
        old_uproject: Some(UProject {
            engine_association: old_engine_association,
        }),
        new_uproject,
        engine_type: ctx.app_config.read().engine_type,
        artifact_sync: ctx.artifact_sync.clone(),
        downloads: ctx.downloads.clone(),
        sync_event_tx: ctx.sync_event_tx.clone(),
        aws_client: ctx.aws_client.clone(),
        git_client: ctx.git_client.clone(),
        download_symbols: ctx.app_config.read().engine_download_symbols,
        storage: ctx.storage.clone(),
        project,
        engine: ctx.engine.clone(),
        max_cache_size_bytes: ctx.app_config.read().engine_cache_size_bytes(),
        transfer_acceleration: ctx.app_config.read().s3_transfer_acceleration,
    };
    update_engine_op.execute().await?;

    Ok(())
}
