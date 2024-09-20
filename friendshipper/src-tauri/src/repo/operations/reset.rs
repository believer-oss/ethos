use anyhow::{anyhow, Context};
use async_trait::async_trait;
use axum::extract::{Path, State};

use ethos_core::clients::aws::ensure_aws_client;
use ethos_core::clients::git;
use ethos_core::types::config::{RepoConfig, UProject};
use ethos_core::types::errors::CoreError;
use ethos_core::worker::{Task, TaskSequence};

use crate::engine::EngineProvider;
use crate::repo::operations::{DownloadDllsOp, StatusOp, UpdateEngineOp};
use crate::state::AppState;
use ethos_core::storage::config::Project;
use ethos_core::storage::ArtifactStorage;
use ethos_core::AWSClient;
use std::path::PathBuf;
use tokio::sync::oneshot::error::RecvError;
use tracing::{error, info};

pub async fn reset_repo<T>(State(state): State<AppState<T>>) -> Result<(), CoreError>
where
    T: EngineProvider,
{
    let branch = state.repo_config.read().trunk_branch.clone();
    state.git().hard_reset(&branch).await.map_err(|e| e.into())
}

#[derive(Clone)]
pub struct ResetToCommitOp {
    pub commit: String,
    pub git_client: git::Git,
}

#[async_trait]
impl Task for ResetToCommitOp {
    async fn execute(&self) -> Result<(), CoreError> {
        self.git_client
            .run(&["reset", "--keep", &self.commit], Default::default())
            .await
            .map_err(CoreError::Internal)
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
    let (tx, rx) = tokio::sync::oneshot::channel::<Option<CoreError>>();
    let mut sequence = TaskSequence::new().with_completion_tx(tx);

    // reset repo to commit
    let reset_to_commit_op = ResetToCommitOp {
        commit,
        git_client: state.git(),
    };
    sequence.push(Box::new(reset_to_commit_op));

    // save engine association before the .uproject potentially gets updated
    let app_config = state.app_config.read().clone();
    let uproject_path_relative = state.repo_config.read().uproject_path.clone();
    let uproject_path = PathBuf::from(app_config.repo_path).join(&uproject_path_relative);
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

    // update status
    let aws_client: Option<AWSClient> = {
        let client = ensure_aws_client(state.aws_client.read().await.clone())?;

        // Make sure AWS credentials still valid
        client.check_config().await?;
        Some(client)
    };
    let storage: Option<ArtifactStorage> = {
        match state.storage.read().clone() {
            Some(storage) => Some(storage),
            None => {
                return Err(CoreError::Internal(anyhow!(
                    "No storage configured for this app. AWS may still be initializing."
                )))
            }
        }
    };
    let status_op = StatusOp {
        repo_status: state.repo_status.clone(),
        app_config: state.app_config.clone(),
        repo_config: state.repo_config.clone(),
        engine: state.engine.clone(),
        git_client: state.git(),
        github_username: state.github_username(),
        aws_client,
        storage,
        allow_offline_communication: false,
        skip_engine_update: false,
    };
    sequence.push(Box::new(status_op));

    // download dlls
    let aws_client = ensure_aws_client(state.aws_client.read().await.clone())?;
    let project_name = RepoConfig::get_project_name(&state.repo_config.read().uproject_path)
        .unwrap_or("unknown_project".to_string());
    let artifact_prefix = state
        .app_config
        .read()
        .clone()
        .selected_artifact_project
        .context("Project not configured. Repo may still be initializing.")?;
    let uproject = UProject::load(&uproject_path)?;
    let engine_path = state.app_config.read().get_engine_path(&uproject);

    let download_dlls_op = DownloadDllsOp {
        git_client: state.git(),
        project_name,
        dll_commit: state.repo_status.read().dll_commit_remote.clone(),
        download_symbols: state.app_config.read().editor_download_symbols,
        storage: state.storage.read().clone().unwrap(),
        longtail: state.longtail.clone(),
        tx: state.longtail_tx.clone(),
        aws_client: aws_client.clone(),
        artifact_prefix: artifact_prefix.clone(),
        engine: state.engine.clone(),
        engine_path,
    };
    sequence.push(Box::new(download_dlls_op));

    // update engine
    let app_config = state.app_config.read().clone();
    let uproject_path =
        PathBuf::from(&app_config.repo_path).join(&state.repo_config.read().uproject_path);
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

        info!(
            "Found engine association {} (previous was {}).",
            new_uproject.engine_association, old_uproject.engine_association
        );

        if new_uproject.engine_association != old_uproject.engine_association {
            let engine_path: PathBuf = app_config.get_engine_path(&new_uproject);

            let status = state.repo_status.read().clone();
            let project = if status.repo_owner.is_empty() || status.repo_name.is_empty() {
                let (owner, repo) = match app_config.selected_artifact_project {
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
                Project::new(&status.repo_owner, &status.repo_name)
            };

            let update_engine_op = UpdateEngineOp {
                engine_path,
                old_uproject: Some(old_uproject.clone()),
                new_uproject: new_uproject.clone(),
                engine_type: app_config.engine_type,
                longtail: state.longtail.clone(),
                longtail_tx: state.longtail_tx.clone(),
                aws_client: aws_client.clone(),
                git_client: state.git(),
                download_symbols: app_config.engine_download_symbols,
                storage: state.storage.read().clone().unwrap(),
                project,
                engine: state.engine.clone(),
            };
            sequence.push(Box::new(update_engine_op));
        }
    }

    let _ = state.operation_tx.send(sequence).await;
    let res: Result<Option<CoreError>, RecvError> = rx.await;
    if let Ok(Some(e)) = res {
        return Err(e);
    }

    Ok(())
}
