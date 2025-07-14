use std::path::PathBuf;
use std::sync::mpsc::Sender;

use anyhow::{anyhow, Context};
use async_trait::async_trait;
use axum::extract::{Path, State};
use ethos_core::storage::config::Project;
use tokio::sync::oneshot::error::RecvError;
use tracing::{error, info};

use crate::engine::EngineProvider;
use crate::repo::operations::{DownloadDllsOp, StatusOp, UpdateEngineOp};
use crate::state::AppState;
use ethos_core::clients::aws::ensure_aws_client;
use ethos_core::clients::git;
use ethos_core::clients::github::GraphQLClient;
use ethos_core::longtail::Longtail;
use ethos_core::msg::LongtailMsg;
use ethos_core::storage::ArtifactStorage;
use ethos_core::types::config::{AppConfigRef, RepoConfig, RepoConfigRef, UProject};
use ethos_core::types::errors::CoreError;
use ethos_core::types::repo::RepoStatusRef;
use ethos_core::worker::{Task, TaskSequence};
use ethos_core::AWSClient;

pub async fn reset_repo<T>(State(state): State<AppState<T>>) -> Result<(), CoreError>
where
    T: EngineProvider,
{
    let branch = state.app_config.read().target_branch.clone();
    state.git().hard_reset(&branch).await.map_err(|e| e.into())
}

#[derive(Clone)]
pub struct ResetToCommitOp<T> {
    pub commit: String,
    pub app_config: AppConfigRef,
    pub repo_config: RepoConfigRef,
    pub repo_status: RepoStatusRef,
    pub longtail: Longtail,
    pub longtail_tx: Sender<LongtailMsg>,
    pub aws_client: AWSClient,
    pub storage: ArtifactStorage,
    pub git_client: git::Git,
    pub github_client: Option<GraphQLClient>,
    pub engine: T,
}

#[async_trait]
impl<T> Task for ResetToCommitOp<T>
where
    T: EngineProvider,
{
    async fn execute(&self) -> Result<(), CoreError> {
        // reset repo to commit
        info!("Resetting repo to commit: {}", self.commit);
        self.git_client
            .run(&["reset", "--keep", &self.commit], Default::default())
            .await?;

        // save engine association before the .uproject potentially gets updated
        let repo_path = self.app_config.read().repo_path.clone();
        let uproject_path_relative = self.repo_config.read().uproject_path.clone();
        let uproject_path = PathBuf::from(&repo_path).join(&uproject_path_relative);
        let old_uproject: Option<UProject> = match UProject::load(&uproject_path) {
            Err(e) => {
                error!(
                    "Failed to load uproject before sync, skipping engine update. Error: {}",
                    e
                );
                None
            }
            Ok(uproject) => Some(uproject),
        };

        // get status of repo
        let github_username = self
            .github_client
            .clone()
            .map_or(String::default(), |x| x.username.clone());
        let status_op = StatusOp {
            git_client: self.git_client.clone(),
            github_username: github_username.clone(),
            repo_status: self.repo_status.clone(),
            app_config: self.app_config.clone(),
            repo_config: self.repo_config.clone(),
            engine: self.engine.clone(),
            aws_client: Some(self.aws_client.clone()),
            storage: Some(self.storage.clone()),
            allow_offline_communication: false,
            skip_display_names: true,
            skip_engine_update: false,
        };
        status_op.execute().await?;

        // download dlls
        let project = self
            .app_config
            .read()
            .selected_artifact_project
            .clone()
            .context("No selected artifact project found in config.")?
            .as_str()
            .into();

        let uproject = UProject::load(&uproject_path)?;
        let engine_path = self.app_config.read().get_engine_path(&uproject);
        match RepoConfig::get_project_name(&uproject_path_relative) {
            Some(project_name) => {
                let download_dlls_op = DownloadDllsOp {
                    git_client: self.git_client.clone(),
                    project_name,
                    dll_commit: self.repo_status.read().dll_commit_remote.clone(),
                    download_symbols: self.app_config.read().editor_download_symbols,
                    storage: self.storage.clone(),
                    longtail: self.longtail.clone(),
                    tx: self.longtail_tx.clone(),
                    aws_client: self.aws_client.clone(),
                    project,
                    engine: self.engine.clone(),
                    engine_path: engine_path.clone(),
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

        // update engine
        let new_uproject: Option<UProject> = match UProject::load(&uproject_path) {
            Err(e) => {
                error!(
                    "Failed to load uproject after sync, skipping engine update. Error: {}",
                    e
                );
                None
            }
            Ok(uproject) => Some(uproject),
        };
        if new_uproject.is_some() && old_uproject.is_some() {
            let new_uproject = new_uproject.unwrap();
            let old_uproject = old_uproject.unwrap();

            if new_uproject.engine_association != old_uproject.engine_association {
                info!("Engine association changed, updating engine.");
                let repo_owner = self.repo_status.read().repo_owner.clone();
                let repo_name = self.repo_status.read().repo_name.clone();
                let project = if repo_owner.is_empty() || repo_name.is_empty() {
                    let selected_artifact_project =
                        self.app_config.read().selected_artifact_project.clone();
                    let (owner, repo) = match selected_artifact_project {
                        Some(ref project) => {
                            let (owner, repo) =
                                project.split_once('-').ok_or(anyhow!("Invalid project"))?;

                            (owner, repo)
                        }
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
                let update_engine_op = UpdateEngineOp {
                    engine_path: engine_path.clone(),
                    old_uproject: Some(old_uproject.clone()),
                    new_uproject: new_uproject.clone(),
                    engine_type: self.app_config.read().engine_type,
                    longtail: self.longtail.clone(),
                    longtail_tx: self.longtail_tx.clone(),
                    aws_client: self.aws_client.clone(),
                    git_client: self.git_client.clone(),
                    download_symbols: self.app_config.read().engine_download_symbols,
                    storage: self.storage.clone(),
                    project,
                    engine: self.engine.clone(),
                };
                update_engine_op.execute().await?;
            }
        }

        Ok(())
    }

    fn get_name(&self) -> String {
        String::from("RepoResetToCommit")
    }
}

pub async fn reset_repo_to_commit<T>(
    State(state): State<AppState<T>>,
    Path(commit): Path<String>,
) -> Result<(), CoreError>
where
    T: EngineProvider,
{
    let aws_client = ensure_aws_client(state.aws_client.read().await.clone())?;
    let storage = match state.storage.read().clone() {
        Some(storage) => storage,
        None => {
            return Err(CoreError::Internal(anyhow!(
                "Storage not configured. AWS may still be initializing."
            )))
        }
    };

    let (tx, rx) = tokio::sync::oneshot::channel::<Option<CoreError>>();
    let mut sequence = TaskSequence::new().with_completion_tx(tx);

    let reset_to_commit_op = ResetToCommitOp {
        commit,
        app_config: state.app_config.clone(),
        repo_config: state.repo_config.clone(),
        repo_status: state.repo_status.clone(),
        longtail: state.longtail.clone(),
        longtail_tx: state.longtail_tx.clone(),
        aws_client: aws_client.clone(),
        storage,
        git_client: state.git(),
        github_client: state.github_client.read().clone(),
        engine: state.engine.clone(),
    };
    sequence.push(Box::new(reset_to_commit_op));
    let _ = state.operation_tx.send(sequence).await;

    let res: Result<Option<CoreError>, RecvError> = rx.await;
    if let Ok(Some(e)) = res {
        return Err(e);
    }

    Ok(())
}
