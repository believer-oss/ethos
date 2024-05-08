use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::sync::Arc;

use anyhow::anyhow;
use anyhow::bail;
use axum::extract::State;
use axum::{async_trait, debug_handler};
use ethos_core::fs::LocalDownloadPath;
use tokio::sync::oneshot::error::RecvError;
use tracing::info;
use tracing::warn;

use ethos_core::clients::aws::ensure_aws_client;
use ethos_core::clients::git;
use ethos_core::longtail;
use ethos_core::msg::LongtailMsg;
use ethos_core::storage::{
    config::Project, ArtifactBuildConfig, ArtifactConfig, ArtifactKind, ArtifactStorage, Platform,
};
use ethos_core::types::config::EngineType;
use ethos_core::types::config::UProject;
use ethos_core::types::errors::CoreError;
use ethos_core::worker::{Task, TaskSequence};
use ethos_core::AWSClient;

use crate::system::unreal;
use crate::AppState;

#[derive(Clone)]
pub struct UpdateEngineOp {
    pub engine_path: PathBuf,
    pub old_uproject: Option<UProject>,
    pub new_uproject: UProject,
    pub engine_type: EngineType,
    pub longtail: longtail::Longtail,
    pub longtail_tx: Sender<LongtailMsg>,
    pub aws_client: AWSClient,
    pub git_client: git::Git,
    pub download_symbols: bool,
    pub storage: ArtifactStorage,
    pub project: Project,
}

#[async_trait]
impl Task for UpdateEngineOp {
    async fn execute(&self) -> anyhow::Result<()> {
        // If there's no match we assume it's using an Epic distribution of the engine build so we don't have any work to do
        if self.new_uproject.is_custom_engine() {
            info!("Engine association has been updated to non-installed version {}, attempting to sync new engine", self.new_uproject.engine_association);

            let commit_sha_short: String = self.new_uproject.get_custom_engine_sha()?;

            if self.engine_type == EngineType::Prebuilt {
                let storage = &self.storage;

                let artifact_config = ArtifactConfig::new(
                    Project::new(crate::DEFAULT_ENGINE_OWNER, crate::DEFAULT_ENGINE_REPO),
                    ArtifactKind::Engine,
                    ArtifactBuildConfig::Development,
                    Platform::Win64,
                );

                let archive_url = match storage
                    .get_from_short_sha(artifact_config, &commit_sha_short)
                    .await
                {
                    Ok(url) => {
                        info!(
                            "Downloading engine from URL {} to {:?}",
                            url, &self.engine_path
                        );
                        url
                    }
                    Err(e) => {
                        bail!("Failed to determine engine archive URL: {:?}", e);
                    }
                };

                let mut archive_urls: Vec<String> = vec![archive_url];

                if self.download_symbols {
                    let symbols_config = ArtifactConfig::new(
                        Project::new(crate::DEFAULT_ENGINE_OWNER, crate::DEFAULT_ENGINE_REPO),
                        ArtifactKind::EngineSymbols,
                        ArtifactBuildConfig::Development,
                        Platform::Win64,
                    );

                    match self
                        .storage
                        .get_from_short_sha(symbols_config, &commit_sha_short)
                        .await
                    {
                        Err(e) => {
                            warn!("Failed to determine symbols archive URL. Symbols will be unavailable. Error: {}", e)
                        }
                        Ok(url) => {
                            info!(
                                "downloading engine symbols from URL {} to {:?}...",
                                &url, &self.engine_path
                            );
                            archive_urls.push(url)
                        }
                    };
                }

                let cache_path = self.longtail.download_path.0.join("engine_cache/");

                let download_result = self.longtail.get_archive(
                    &PathBuf::from(&self.engine_path),
                    Some(longtail::CacheControl {
                        path: cache_path,
                        max_size_bytes: 50 * 1024 * 1024 * 1024, // 50 GB
                    }),
                    &archive_urls,
                    self.longtail_tx.clone(),
                    self.aws_client.get_credentials().await,
                );
                match download_result {
                    Ok(()) => {}
                    Err(e) => {
                        bail!("Failed to download engine archive: {:?}", e);
                    }
                }
            } else {
                assert_eq!(self.engine_type, EngineType::Source);

                info!(
                    "Updating engine repo at {:?} to commit {}",
                    &self.engine_path, commit_sha_short
                );

                let engine_head_commit = self
                    .git_client
                    .head_commit(git::CommitFormat::Short, git::CommitHead::Local)
                    .await?;

                if engine_head_commit != commit_sha_short {
                    info!(
                        "Engine repo out of date at commit {}, updating to {}",
                        engine_head_commit, commit_sha_short
                    );

                    let did_stash = self.git_client.stash(git::StashAction::Push).await?;
                    self.git_client.fetch(git::ShouldPrune::Yes).await?;
                    self.git_client.checkout(&commit_sha_short).await?;
                    if did_stash {
                        self.git_client.stash(git::StashAction::Pop).await?;
                    }
                } else {
                    info!(
                        "Engine repo already at commit {}, no update needed.",
                        commit_sha_short
                    );
                }
            }

            info!("Engine update complete. Updating registry...");

            // update global engine association registry keys to ensure the correct engine can be used
            // to open the project
            unreal::update_engine_association_registry(
                &self.engine_path,
                &self.new_uproject,
                &self.old_uproject,
            )?;
        } else {
            info!(
                "Found standard engine association {}, no custom engine to update.",
                &self.new_uproject.engine_association
            );
        }

        Ok(())
    }

    fn get_name(&self) -> String {
        String::from("UpdateEngine")
    }
}

#[debug_handler]
pub async fn update_engine_handler(State(state): State<Arc<AppState>>) -> Result<(), CoreError> {
    let aws_client = ensure_aws_client(state.aws_client.read().await.clone())?;

    let storage = match state.storage.read().clone() {
        Some(storage) => storage,
        None => {
            return Err(CoreError(anyhow!(
                "Storage not configured. AWS may still be initializing."
            )));
        }
    };

    let update_op = {
        let tx_lock = state.longtail_tx.clone();
        let app_config = state.app_config.read();

        let uproject_path =
            PathBuf::from(&app_config.repo_path).join(&state.repo_config.read().uproject_path);
        let uproject = match UProject::load(&uproject_path) {
            Ok(p) => p,
            Err(e) => {
                return Err(CoreError(anyhow!(
                    "Unable to update engine due to missing uproject at {}. Error: {}",
                    uproject_path.display(),
                    e
                )));
            }
        };

        // FIXME: Temporary reverse compat for the engine path. This should be removed
        // after everyone is working with the source engine.
        //
        // The engine path should be less than 50 characters, and the old scheme was
        // too long.
        //
        // IF:
        //      Engine path matches the old default
        //  and no files are found at that path (or doesn't exist)
        // THEN:
        //      force their config to the new default
        let mut old_default_engine_path = LocalDownloadPath::default().to_path_buf();
        old_default_engine_path.push("engine_prebuilt");
        let engine_path = state.app_config.read().engine_prebuilt_path.clone();
        if engine_path == old_default_engine_path.to_string_lossy()
            && (!PathBuf::from(&engine_path).exists()
                || PathBuf::from(&engine_path)
                    .read_dir()
                    .unwrap()
                    .next()
                    .is_none())
        {
            warn!(
                "Detected old engine path at {:?}, no files found. Forcing new default engine path.",
                engine_path
            );
            let new_engine_path = PathBuf::from("C:\\").join("f11r_engine_prebuilt");
            warn!(
                "New default engine path has been set to {:?}",
                new_engine_path
            );

            state.app_config.write().engine_prebuilt_path =
                new_engine_path.to_string_lossy().to_string();

            // Write the new config to disk
            // This was copied from ../../config/router.rs
            let file = std::fs::OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(&state.config_file)
                .unwrap();

            let mut config = state.app_config.read().clone();
            let repo_config = config.initialize_repo_config();

            // Get rid of the PAT
            config.github_pat = None;

            // Get rid of the selected artifact project
            config.selected_artifact_project = None;

            {
                let mut lock = state.repo_config.write();
                *lock = repo_config;
            }

            serde_yaml::to_writer(file, &config).unwrap();
            info!("Preferences successfully saved!");
        }

        let status = state.repo_status.read().clone();
        let project = if status.repo_owner.is_empty() || status.repo_name.is_empty() {
            let (owner, repo) = match app_config.selected_artifact_project {
                Some(ref project) => {
                    let (owner, repo) =
                        project.split_once('-').ok_or(anyhow!("Invalid project"))?;
                    (owner, repo)
                }
                None => return Err(CoreError(anyhow!("No project selected"))),
            };

            Project::new(owner, repo)
        } else {
            Project::new(&status.repo_owner, &status.repo_name)
        };

        UpdateEngineOp {
            engine_path: app_config.get_engine_path(&uproject),
            old_uproject: None,
            new_uproject: uproject,
            engine_type: app_config.engine_type,
            longtail: state.longtail.clone(),
            longtail_tx: tx_lock.clone(),
            aws_client,
            git_client: state.git(),
            download_symbols: app_config.engine_download_symbols,
            storage,
            project,
        }
    };

    let (tx, rx) = tokio::sync::oneshot::channel::<Option<anyhow::Error>>();
    let mut sequence = TaskSequence::new().with_completion_tx(tx);
    sequence.push(Box::new(update_op));
    let _ = state.operation_tx.send(sequence).await;

    let res: Result<Option<anyhow::Error>, RecvError> = rx.await;
    if let Ok(Some(e)) = res {
        return Err(CoreError(e));
    }

    Ok(())
}
