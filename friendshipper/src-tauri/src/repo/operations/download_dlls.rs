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
use ethos_core::clients::aws::ensure_aws_client;
use ethos_core::clients::git;
use ethos_core::longtail;
use ethos_core::msg::LongtailMsg;
use ethos_core::storage::ArtifactBuildConfig;
use ethos_core::storage::ArtifactConfig;
use ethos_core::storage::ArtifactKind;
use ethos_core::storage::ArtifactStorage;
use ethos_core::storage::Platform;
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
    pub longtail: longtail::Longtail,
    pub tx: Sender<LongtailMsg>,
    pub aws_client: AWSClient,
    pub artifact_prefix: String,
    pub engine: T,
}

#[async_trait]
impl<T> Task for DownloadDllsOp<T>
where
    T: EngineProvider,
{
    async fn execute(&self) -> Result<(), CoreError> {
        self.engine.check_ready_to_sync_repo().await?;

        if self.dll_commit.is_empty() {
            return Err(CoreError::Internal(anyhow!(
                "No DLL archive found for current branch."
            )));
        }

        let mut binaries_staging_path =
            Path::join(&self.longtail.download_path.0, Path::new("editor_staging"));

        let mut binaries_cache_path =
            Path::join(&self.longtail.download_path.0, Path::new("editor_cache"));

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

        self.aws_client.check_config().await?;

        let editor_config = ArtifactConfig::new(
            self.artifact_prefix.as_str().into(),
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
                self.artifact_prefix.as_str().into(),
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

        let dll_download_result = self.longtail.get_archive(
            &binaries_staging_path,
            Some(longtail::CacheControl {
                path: binaries_cache_path,
                max_size_bytes: 5 * 1024 * 1024 * 1024, // 5 GB
            }),
            &archive_urls,
            self.tx.clone(),
            self.aws_client.get_credentials().await,
        );
        match dll_download_result {
            Ok(()) => {}
            Err(e) => {
                return Err(CoreError::Internal(anyhow!(
                    "Failed to download dll: {:?}",
                    e
                )));
            }
        }

        info!(
            "download done. copying binaries from '{:?}' to: '{:?}'",
            binaries_staging_path, &self.git_client.repo_path
        );

        copy_recursively(&binaries_staging_path, &binaries_destination_path)
            .context("Failed to copy dlls to target directory")?;

        const LONGTAIL_INDEX_FILENAME: &str = ".longtail.index.cache.lvi";
        let longtail_index_path = self.git_client.repo_path.join(LONGTAIL_INDEX_FILENAME);
        if Path::exists(&longtail_index_path) {
            _ = fs::remove_file(longtail_index_path)
        }

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
        let tx_lock = state.longtail_tx.clone();
        let project_name = RepoConfig::get_project_name(&state.repo_config.read().uproject_path)
            .unwrap_or("unknown_project".to_string());

        let storage = state
            .storage
            .read()
            .clone()
            .context("Storage not configured. AWS may still be initializing.")?;

        let artifact_prefix = state
            .app_config
            .read()
            .clone()
            .selected_artifact_project
            .context("Project not configured. Repo may still be initializing.")?;

        DownloadDllsOp {
            git_client: state.git(),
            project_name,
            dll_commit: state.repo_status.read().dll_commit_remote.clone(),
            download_symbols: state.app_config.read().editor_download_symbols,
            storage,
            longtail: state.longtail.clone(),
            tx: tx_lock.clone(),
            aws_client: aws_client.clone(),
            artifact_prefix,
            engine: state.engine.clone(),
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
