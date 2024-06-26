use std::{fs, path::PathBuf};

use anyhow::anyhow;
use axum::routing::post;
use axum::{extract::State, routing::get, Json, Router};
use tracing::info;

use ethos_core::clients::github::GraphQLClient;
use ethos_core::clients::kube::ensure_kube_client;
use ethos_core::types::config::{AppConfig, DynamicConfig};
use ethos_core::types::config::{ConfigValidationError, RepoConfig};
use ethos_core::types::errors::CoreError;
use ethos_core::types::project::ProjectConfig;
use ethos_core::types::repo::CloneRequest;
use ethos_core::AWSClient;

use crate::engine::EngineProvider;
use crate::repo::operations::{clone_handler, download_dlls_handler, update_engine_handler};
use crate::state::AppState;
use crate::{APP_NAME, KEYRING_USER};

pub fn router<T>() -> Router<AppState<T>>
where
    T: EngineProvider,
{
    Router::new()
        .route("/", get(get_config).post(update_config))
        .route("/repo", get(get_repo_config))
        .route("/dynamic", get(get_dynamic_config))
        .route("/projects", get(get_project_config))
        .route("/reset", post(reset_config))
}

async fn get_config<T>(State(state): State<AppState<T>>) -> Result<Json<AppConfig>, CoreError>
where
    T: EngineProvider,
{
    let config = state.app_config.read().clone();

    Ok(Json(config))
}

async fn get_repo_config<T>(State(state): State<AppState<T>>) -> Result<Json<RepoConfig>, CoreError>
where
    T: EngineProvider,
{
    let config = state.repo_config.read();

    if config.trunk_branch.is_empty() {
        return Err(anyhow!(ConfigValidationError(
            "Trunk branch is not configured in the repository. Check friendshipper.yaml in the root of your project.".to_string()
        ))
            .into());
    }

    Ok(Json(config.clone()))
}

async fn get_project_config<T>(
    State(state): State<AppState<T>>,
) -> Result<Json<Vec<ProjectConfig>>, CoreError>
where
    T: EngineProvider,
{
    let kube_client = ensure_kube_client(state.kube_client.read().clone())?;

    Ok(Json(kube_client.get_project_configs().await?))
}

async fn get_dynamic_config<T>(State(state): State<AppState<T>>) -> Json<DynamicConfig>
where
    T: EngineProvider,
{
    let config = state.dynamic_config.read().clone();
    Json(config)
}

async fn update_config<T>(
    State(state): State<AppState<T>>,
    Json(payload): Json<AppConfig>,
) -> Result<String, CoreError>
where
    T: EngineProvider,
{
    let mut payload = payload;

    info!("Updating config with payload: {:?}", payload);
    let current_config = state.app_config.read().clone();
    if payload.user_display_name.is_empty() {
        return Err(anyhow!(ConfigValidationError(
            "Display name cannot be empty.".to_string()
        ))
        .into());
    }

    if current_config.selected_artifact_project.is_some() {
        payload
            .selected_artifact_project
            .clone_from(&current_config.selected_artifact_project)
    }

    // Make sure if repo_url changed repo_path also changed, and vice versa, but only validate
    // if neither is empty.
    #[allow(clippy::collapsible_if)]
    if !current_config.repo_url.is_empty() && !current_config.repo_path.is_empty() {
        if (payload.repo_url != current_config.repo_url
            && current_config.repo_path == payload.repo_path)
            || (payload.repo_path != current_config.repo_path
                && current_config.repo_url == payload.repo_url)
        {
            return Err(anyhow!(ConfigValidationError(
                "Repo URL and Repo Path should change together".to_string()
            ))
            .into());
        }
    }

    if let Some(aws_config) = payload.aws_config.clone() {
        let refresh_client: bool = match state.aws_client.read().await.clone() {
            Some(client) => match client.config {
                Some(ref config) => config != &aws_config,
                None => true,
            },
            None => true,
        };

        if refresh_client {
            info!("Initializing AWS client with new config");
            let new_aws_client = AWSClient::new(
                Some(state.notification_tx.clone()),
                APP_NAME.to_string(),
                aws_config.clone(),
            )
            .await?;

            // update the aws config in the app state
            {
                let mut lock = state.app_config.write();
                lock.aws_config = Some(aws_config.clone());
            }

            state.replace_aws_client(new_aws_client).await?;
        }
    }

    if !payload.repo_path.is_empty() {
        let git_dir = PathBuf::from(payload.repo_path.clone()).join(".git");
        // If the config hasn't been initialized, the user hasn't finished the welcome flow. We should allow
        // the user to save their repo path regardless of whether or not it's a git dir because a repo clone
        // may not have been started yet.
        if !git_dir.exists() && current_config.initialized {
            let req = CloneRequest {
                url: payload.repo_url.clone(),
                path: payload.repo_path.clone(),
            };

            // get the name of the repo from the url
            let repo_name = req.url.split('/').last().unwrap().trim_end_matches(".git");
            let full_repo_path = PathBuf::from(&req.path.clone()).join(repo_name);

            // hold on to the current repo path
            let current_repo_path = state.app_config.read().repo_path.clone();

            {
                // update state to point git at new directory
                // we need to short circuit updating the state's repo_path because state.git() relies
                // on it.
                let mut state = state.app_config.write();
                state.repo_path = full_repo_path.clone().to_str().unwrap().to_string();
            }

            // call the clone handler
            match clone_handler(State(state.clone()), Json(req.clone())).await {
                Ok(_) => {
                    info!(
                        "Successfully cloned repo into {}",
                        full_repo_path.to_str().unwrap()
                    );
                }
                Err(e) => {
                    // put the repo path back
                    {
                        let mut state = state.app_config.write();
                        state.repo_path = current_repo_path;
                    }

                    return Err(anyhow!(ConfigValidationError(format!(
                        "Error cloning repo: {}",
                        e
                    )))
                    .into());
                }
            }

            payload.repo_path = full_repo_path.to_str().unwrap().to_string();

            // call the DLL download handler
            let _ = download_dlls_handler(State(state.clone())).await?;

            // call the engine update handler
            update_engine_handler(State(state.clone())).await?;
        }

        match payload.github_pat.clone() {
            Some(pat) => {
                match GraphQLClient::new(pat.clone()).await {
                    Ok(client) => {
                        state.github_client.write().replace(client);
                    }
                    Err(e) => {
                        return Err(anyhow!(ConfigValidationError(format!(
                            "Error creating GitHub client: {}",
                            e
                        )))
                        .into());
                    }
                }

                // store pat in keyring
                let entry = keyring::Entry::new(APP_NAME, KEYRING_USER)?;
                entry.set_password(&pat)?;
            }
            None => {
                return Err(anyhow!(ConfigValidationError(
                    "GitHub Personal Access Token cannot be empty.".to_string()
                ))
                .into());
            }
        }
    }

    {
        let mut lock = state.app_config.write();
        *lock = payload;
        lock.initialized = true;
    }

    save_config_to_file(state, "Preferences successfully saved!")?;

    Ok("ok".to_string())
}

async fn reset_config<T>(State(state): State<AppState<T>>) -> Result<(), CoreError>
where
    T: EngineProvider,
{
    // delete file at config path
    fs::remove_file(state.config_file).map_err(|e| CoreError(anyhow!(e)))
}

fn save_config_to_file<T>(state: AppState<T>, log_msg: &str) -> Result<(), CoreError>
where
    T: EngineProvider,
{
    let file = fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&state.config_file)
        .unwrap();

    let mut config = state.app_config.read().clone();
    let repo_config = config.initialize_repo_config()?;

    // Get rid of the PAT
    config.github_pat = None;

    // Get rid of the selected artifact project
    config.selected_artifact_project = None;

    {
        let mut lock = state.repo_config.write();
        *lock = repo_config;
    }

    serde_yaml::to_writer(file, &config).unwrap();

    info!("{}", log_msg);

    Ok(())
}
