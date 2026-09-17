use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::sync::mpsc::Sender;

use anyhow::anyhow;
use anyhow::Context;
use axum::extract::State;
use axum::{async_trait, Json};
use serde::Deserialize;
use serde::Serialize;
use tokio::sync::oneshot::error::RecvError;
use tracing::info;
use tracing::warn;

use crate::engine::EngineProvider;
use ethos_core::artifact_sync;
use ethos_core::artifact_sync::SyncEvent;
use ethos_core::artifact_sync::{
    DownloadCancellation, SyncError, SyncKind, SyncRequest, TARGET_INDEX_CACHE_NAME,
};
use ethos_core::clients::aws::ensure_aws_client;
use ethos_core::clients::git;
use ethos_core::storage::config::Project;
use ethos_core::storage::ArtifactStorage;
use ethos_core::storage::{ArtifactBuildConfig, ArtifactConfig, ArtifactKind, Platform};
use ethos_core::types::config::RepoConfig;
use ethos_core::types::errors::CoreError;
use ethos_core::worker::{Task, TaskSequence};
use ethos_core::AWSClient;

use crate::AppState;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadResponse {
    pub download_attempted: bool,
}

#[derive(Clone)]
pub struct DownloadDllsOp<T> {
    pub git_client: git::Git,
    pub project_name: String,
    pub dll_commit: String,
    pub download_symbols: bool,
    pub storage: ArtifactStorage,
    pub artifact_sync: artifact_sync::ArtifactSync,
    pub downloads: DownloadCancellation,
    pub tx: Sender<SyncEvent>,
    pub aws_client: AWSClient,
    pub project: Project,
    pub engine: T,
    pub engine_path: PathBuf,
    pub max_cache_size_bytes: u64,
    pub transfer_acceleration: bool,
}

#[async_trait]
impl<T> Task for DownloadDllsOp<T>
where
    T: EngineProvider,
{
    #[tracing::instrument(
        name = "DownloadDllsOp::execute",
        ret,
        skip(self),
        fields(
            project_name = %self.project_name,
            dll_commit = %self.dll_commit,
            download_symbols = %self.download_symbols,
            project = %self.project
        )
    )]
    async fn execute(&self) -> Result<(), CoreError> {
        self.engine.check_ready_to_sync_repo().await?;

        if self.dll_commit.is_empty() {
            return Err(CoreError::Internal(anyhow!(
                "No DLL archive found for current branch."
            )));
        }

        let mut binaries_staging_path = Path::join(
            &self.artifact_sync.download_path.0,
            Path::new("editor_staging"),
        );

        let mut binaries_cache_path = Path::join(
            &self.artifact_sync.download_path.0,
            Path::new("editor_cache"),
        );

        let mut binaries_destination_path = PathBuf::from(&self.git_client.repo_path);

        // If the project has a "Source" directory in the root, we place the DLLs there, otherwise
        // we expect it to be underneath a subdirectory with the project name.
        let source_exists = Path::new(&self.git_client.repo_path)
            .join("Source")
            .is_dir();
        if !source_exists {
            binaries_cache_path = binaries_cache_path.join(&self.project_name);
            binaries_staging_path = binaries_staging_path.join(&self.project_name);
            binaries_destination_path = binaries_destination_path.join(&self.project_name);
        };

        let editor_config = ArtifactConfig::new(
            self.project.clone(),
            ArtifactKind::Editor,
            ArtifactBuildConfig::Development,
            Platform::Win64,
        );

        let archive_url = match self
            .storage
            .get_from_short_sha(editor_config, &self.dll_commit)
            .await
        {
            Err(e) => {
                return Err(CoreError::Internal(anyhow!(
                    "Failed to determine editor dll archive URL: {}",
                    e
                )));
            }
            Ok(archive) => archive,
        };

        info!(
            "downloading editor binaries from url {} to {:?}...",
            &archive_url, &binaries_staging_path
        );

        let mut archive_urls: Vec<String> = vec![archive_url];

        if self.download_symbols {
            let symbols_config = ArtifactConfig::new(
                self.project.clone(),
                ArtifactKind::EditorSymbols,
                ArtifactBuildConfig::Development,
                Platform::Win64,
            );

            match self
                .storage
                .get_from_short_sha(symbols_config, &self.dll_commit)
                .await
            {
                Err(e) => {
                    warn!("Failed to determine symbols archive URL. Symbols will be unavailable. Error: {}", e)
                }
                Ok(url) => {
                    info!(
                        "downloading editor symbols from url {} to {:?}...",
                        &url, &binaries_staging_path
                    );
                    archive_urls.push(url);
                }
            };
        }

        let Some(download) = self.downloads.begin(SyncKind::EditorDlls) else {
            return Err(CoreError::Internal(anyhow!(
                "An editor binaries download is already running."
            )));
        };
        // Everything in the staging directory is copied into the user's repo below, and
        // longtail writes its cached scan of the target into the target. Not writing one
        // keeps it out of the repo; the cost is a scan of a staging directory that was
        // scanned every time anyway.
        let request =
            SyncRequest::download(SyncKind::EditorDlls, &binaries_staging_path, &archive_urls)
                .with_cache(Some(artifact_sync::CacheControl {
                    path: binaries_cache_path,
                    max_size_bytes: self.max_cache_size_bytes,
                }))
                .with_transfer_acceleration(self.transfer_acceleration)
                .without_target_index();
        let result = self
            .artifact_sync
            .get_archive(request, self.tx.clone(), &self.aws_client, download.token())
            .await;
        result.map_err(SyncError::into_core_error)?;

        T::post_download(&self.engine_path).await;

        // The download above asks for no target index, but the old CLI wrote one into
        // staging on every sync, and it is still sitting there on every machine that
        // synced before this. The copy below takes the staging tree wholesale, so it would
        // carry that file into the user's repo - which is exactly what the old post-copy
        // cleanup existed to undo. Clear it from both ends instead, so it is
        // neither copied in nor left behind.
        discard_target_index(&binaries_staging_path);
        discard_target_index(&binaries_destination_path);

        info!(
            "download done. copying binaries from '{:?}' to: '{:?}'",
            binaries_staging_path, &self.git_client.repo_path
        );

        copy_recursively(&binaries_staging_path, &binaries_destination_path)
            .context("Failed to copy dlls to target directory")?;

        info!("dll download and copy to local repo finished");

        Ok(())
    }

    fn get_name(&self) -> String {
        String::from("DownloadDlls")
    }
}

pub async fn download_dlls_handler<T>(
    State(state): State<AppState<T>>,
) -> Result<Json<DownloadResponse>, CoreError>
where
    T: EngineProvider,
{
    let aws_client = ensure_aws_client(state.aws_client.read().await.clone())?;

    if !state.app_config.read().pull_dlls {
        return Err(CoreError::Internal(anyhow!(
            "You must enable 'Download DLLs' in Preferences to force a DLL download."
        )));
    }

    let download_op = {
        let tx_lock = state.sync_event_tx.clone();
        let project_name = RepoConfig::get_project_name(&state.repo_config.read().uproject_path)
            .unwrap_or("unknown_project".to_string());

        let storage = state
            .storage
            .read()
            .clone()
            .context("Storage not configured. AWS may still be initializing.")?;

        let project = state
            .app_config
            .read()
            .clone()
            .selected_artifact_project
            .context("Project not configured. Repo may still be initializing.")?
            .as_str()
            .into();

        let engine_path = state
            .app_config
            .read()
            .load_engine_path_from_repo(&state.repo_config.read())?;

        DownloadDllsOp {
            git_client: state.git(),
            project_name,
            dll_commit: state.repo_status.read().dll_commit_remote.clone(),
            download_symbols: state.app_config.read().editor_download_symbols,
            storage,
            artifact_sync: state.artifact_sync.clone(),
            downloads: state.downloads.clone(),
            tx: tx_lock.clone(),
            aws_client: aws_client.clone(),
            project,
            engine: state.engine.clone(),
            engine_path,
            max_cache_size_bytes: state.app_config.read().editor_cache_size_bytes(),
            transfer_acceleration: state.app_config.read().s3_transfer_acceleration,
        }
    };

    let (tx, rx) = tokio::sync::oneshot::channel::<Option<CoreError>>();
    let mut sequence = TaskSequence::new().with_completion_tx(tx);
    sequence.push(Box::new(download_op));
    let _ = state.operation_tx.send(sequence).await;

    let res: Result<Option<CoreError>, RecvError> = rx.await;
    if let Ok(Some(e)) = res {
        return Err(e);
    }

    Ok(Json(DownloadResponse {
        download_attempted: true,
    }))
}

/// Remove longtail's cached scan of a directory, if one is there.
///
/// It is longtail's own bookkeeping, never part of a build and never needed once the sync
/// that wrote it has ended. In the user's repo it is an untracked file of several
/// megabytes that nobody can account for.
fn discard_target_index(root: &Path) {
    let path = root.join(TARGET_INDEX_CACHE_NAME);
    match fs::remove_file(&path) {
        Ok(()) => info!("Removed a leftover {path:?}"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => warn!("Could not remove {path:?}: {e}"),
    }
}

/// Copy files from source to destination recursively.
/// From: https://nick.groenen.me/notes/recursively-copy-files-in-rust/
pub fn copy_recursively(
    source: impl AsRef<Path>,
    destination: impl AsRef<Path>,
) -> std::io::Result<()> {
    fs::create_dir_all(&destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let filetype = entry.file_type()?;
        if filetype.is_dir() {
            copy_recursively(entry.path(), destination.as_ref().join(entry.file_name()))?;
        } else {
            fs::copy(entry.path(), destination.as_ref().join(entry.file_name()))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The old CLI wrote one of these into staging on every sync, so it is sitting in the
    /// staging directory of every machine that synced before this. The copy into the repo
    /// takes the staging tree wholesale, so it would go along with the binaries.
    #[test]
    fn a_leftover_target_index_is_discarded() {
        let dir = tempfile::tempdir().unwrap();
        let staging = dir.path();
        fs::create_dir_all(staging.join("Binaries/Win64")).unwrap();
        fs::write(staging.join("Binaries/Win64/Game.dll"), b"x").unwrap();
        fs::write(staging.join(TARGET_INDEX_CACHE_NAME), b"index").unwrap();

        discard_target_index(staging);

        assert!(!staging.join(TARGET_INDEX_CACHE_NAME).exists());
        assert!(
            staging.join("Binaries/Win64/Game.dll").exists(),
            "only longtail's own bookkeeping is removed"
        );
    }

    /// The common case by far: there is none, and that is not a failure.
    #[test]
    fn discarding_a_target_index_that_is_not_there_is_fine() {
        let dir = tempfile::tempdir().unwrap();
        discard_target_index(dir.path());
        assert!(!dir.path().join(TARGET_INDEX_CACHE_NAME).exists());
    }
}
