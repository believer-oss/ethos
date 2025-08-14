use std::{fs, path::PathBuf};

use anyhow::anyhow;
use axum::extract::Query;
use axum::{extract::State, routing::get, Json, Router};
use ethos_core::AWSClient;
use serde::Deserialize;
use tracing::{info, instrument};

use ethos_core::clients::github::GraphQLClient;
use ethos_core::clients::kube::ensure_kube_client;
use ethos_core::types::config::{AppConfig, DynamicConfig};
use ethos_core::types::config::{ConfigValidationError, RepoConfig};
use ethos_core::types::errors::CoreError;
use ethos_core::types::project::ProjectConfig;
use ethos_core::types::repo::CloneRequest;

use crate::client::FriendshipperClient;
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
}

async fn get_config<T>(State(state): State<AppState<T>>) -> Result<Json<AppConfig>, CoreError>
where
    T: EngineProvider,
{
    let mut config = state.app_config.read().clone();

    // get github PAT from keyring
    if let Ok(pat) = keyring::Entry::new(APP_NAME, KEYRING_USER)?.get_password() {
        config.github_pat = Some(pat.into());
    }

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

#[derive(Debug, Deserialize)]
struct UpdateConfigParams {
    token: Option<String>,
    new_project: bool,
}

#[instrument(skip(state), err)]
async fn update_config<T>(
    State(state): State<AppState<T>>,
    Query(params): Query<UpdateConfigParams>,
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

    // if the server url changed, check its health endpoint
    if payload.server_url != current_config.server_url {
        let friendshipper_client = FriendshipperClient::new(payload.server_url.clone())?;
        friendshipper_client.check_health().await?;
    }

    // if we didn't have a server url, and we now do, and we don't have any okta configuration, set the okta configuration
    if current_config.server_url.is_empty()
        && !payload.server_url.is_empty()
        && payload.okta_config.is_none()
    {
        let friendshipper_client = FriendshipperClient::new(payload.server_url.clone())?;
        let okta_config = friendshipper_client.get_okta_config().await?;

        payload.okta_config = Some(okta_config);
    }

    // find selected artifact project in the map and set the root repo_path and repo_url to the project's
    if let Some(project_name) = payload.clone().selected_artifact_project {
        if let Some(project) = payload.projects.get(&project_name) {
            payload.repo_path = project.repo_path.clone();
            payload.repo_url = project.repo_url.clone();
        }
    }

    // if our playtest region has changed, we need to replace the aws client
    if payload.playtest_region != current_config.playtest_region {
        if let Some(token) = params.token {
            let friendshipper_client = FriendshipperClient::new(payload.server_url.clone())?;
            let credentials = friendshipper_client.get_aws_credentials(&token).await?;
            let friendshipper_config = friendshipper_client.get_config(&token).await?;
            state
                .replace_aws_client(
                    AWSClient::from_static_creds(
                        &credentials.access_key_id,
                        &credentials.secret_access_key,
                        credentials.session_token.as_deref(),
                        credentials.expiration,
                        friendshipper_config.artifact_bucket_name.clone(),
                    )
                    .await,
                    payload.playtest_region.clone(),
                    &payload.user_display_name.clone(),
                )
                .await?;
        } else {
            return Err(anyhow!(ConfigValidationError(
                "Token is required to update the AWS client.".to_string()
            ))
            .into());
        }
    }

    if !payload.repo_path.is_empty() {
        let git_dir = PathBuf::from(payload.repo_path.clone()).join(".git");
        // If the config hasn't been initialized, the user hasn't finished the welcome flow. We should allow
        // the user to save their repo path regardless of whether or not it's a git dir because a repo clone
        // may not have been started yet.
        if !git_dir.exists() && current_config.initialized {
            let parent_repo_path = PathBuf::from(&payload.repo_path)
                .parent()
                .and_then(|p| p.to_str())
                .unwrap_or(".")
                .to_string();
            let req = CloneRequest {
                url: payload.repo_url.clone(),
                path: parent_repo_path,
            };

            // get the name of the repo from the url
            let repo_name = req
                .url
                .split('/')
                .next_back()
                .unwrap()
                .trim_end_matches(".git");
            let full_repo_path = PathBuf::from(&req.path.clone()).join(repo_name);

            // hold on to the current repo path
            let current_repo_path = state.app_config.read().repo_path.clone();

            {
                // update state to point git at new directory
                // we need to short circuit updating the state's repo_path because state.git() relies
                // on it.
                let mut state = state.app_config.write();
                state.repo_path = full_repo_path
                    .clone()
                    .to_str()
                    .unwrap()
                    .replace("\\", "/")
                    .to_string();
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

                    return Err(
                        anyhow!(ConfigValidationError(format!("Error cloning repo: {e}"))).into(),
                    );
                }
            }

            payload.repo_path = full_repo_path
                .clone()
                .to_str()
                .unwrap()
                .replace("\\", "/")
                .to_string();

            // TODO(sylviacx): guard this because we don't have a way to test cloning a new project AND setting up dlls and engine stuff
            if !params.new_project {
                // call the DLL download handler
                let _ = download_dlls_handler(State(state.clone())).await?;

                // call the engine update handler
                update_engine_handler(State(state.clone())).await?;
            }
        }

        match payload.github_pat.clone() {
            Some(pat) => {
                match GraphQLClient::new(pat.clone().to_string()).await {
                    Ok(client) => {
                        state.github_client.write().replace(client);
                    }
                    Err(e) => {
                        return Err(anyhow!(ConfigValidationError(format!(
                            "Error creating GitHub client: {e}"
                        )))
                        .into());
                    }
                }

                // store pat in keyring
                let entry = keyring::Entry::new(APP_NAME, KEYRING_USER)?;
                entry.set_password(&pat.to_string())?;
            }
            None => {
                // Only worry about this if we don't already have a Github Client
                if state.github_client.read().clone().is_none() {
                    return Err(anyhow!(ConfigValidationError(
                        "GitHub Personal Access Token cannot be empty.".to_string()
                    ))
                    .into());
                }
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

fn save_config_to_file<T>(state: AppState<T>, log_msg: &str) -> Result<(), CoreError>
where
    T: EngineProvider,
{
    let file = fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&state.config_file)?;

    let mut config = state.app_config.read().clone();
    let repo_config = config.initialize_repo_config()?;

    // Initialize branch defaults if not set
    let branches_updated = config.initialize_branch_defaults(&repo_config);

    // Validate that configured branches exist in repo target branches
    if let Err(e) = config.validate_configured_branches(&repo_config) {
        return Err(CoreError::Internal(anyhow!(
            "Branch configuration error: {}. Please check your friendshipper.yaml target branches or update your branch configuration.", 
            e
        )));
    }

    // Get rid of the PAT
    config.github_pat = None;

    {
        let mut lock = state.repo_config.write();
        *lock = repo_config;
    }

    // If we updated branch defaults, also update the in-memory app config
    if branches_updated {
        let mut app_config_lock = state.app_config.write();
        app_config_lock.primary_branch = config.primary_branch.clone();
        app_config_lock.content_branch = config.content_branch.clone();
    }

    serde_yaml::to_writer(file, &config)?;

    info!("{}", log_msg);

    Ok(())
}
