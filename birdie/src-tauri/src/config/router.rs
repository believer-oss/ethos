use std::{fs, path::PathBuf, sync::Arc};

use anyhow::anyhow;
use axum::{debug_handler, extract::State, routing::get, Json, Router};
use tracing::info;

use ethos_core::clients::github::GraphQLClient;
use ethos_core::types::config::AppConfig;
use ethos_core::types::config::{ConfigValidationError, RepoConfig};
use ethos_core::types::errors::CoreError;

use crate::repo::clone_handler;

use ethos_core::types::repo::CloneRequest;
#[cfg(windows)]
use {crate::DEFAULT_DRIVE_MOUNT, ethos_core::utils, std::path::Path};

use crate::state::AppState;
use crate::{APP_NAME, KEYRING_USER};

pub fn router(shared_state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(get_config).post(update_config))
        .route("/repo", get(get_repo_config))
        .with_state(shared_state)
}

async fn get_config(State(state): State<Arc<AppState>>) -> Json<AppConfig> {
    let config = state.app_config.read().clone();
    Json(config)
}

async fn get_repo_config(
    State(state): State<Arc<AppState>>,
) -> Result<Json<RepoConfig>, CoreError> {
    let config = state.repo_config.read();

    if config.trunk_branch.is_empty() {
        return Err(anyhow!(ConfigValidationError(
            "Trunk branch is not configured in the repository. Check friendshipper.yaml in the root of your project.".to_string()
        ))
            .into());
    }

    Ok(Json(RepoConfig {
        uproject_path: config.uproject_path.clone(),
        trunk_branch: config.trunk_branch.clone(),
        git_hooks_path: config.git_hooks_path.clone(),
    }))
}

#[debug_handler]
async fn update_config(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<AppConfig>,
) -> Result<String, CoreError> {
    let current_config = state.app_config.read().clone();

    let mut payload = payload;

    // Make sure if repo_url changed repo_path also changed, and vice versa, but only validate
    // if either is empty.
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

    if !payload.repo_path.is_empty() {
        let git_dir = PathBuf::from(payload.repo_path.clone()).join(".git");

        {
            let mut lock_cache = state.lock_cache.write().await;
            lock_cache.set_repo_path(payload.repo_path.clone());
        }

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
        }

        #[cfg(windows)]
        if payload.repo_path != current_config.repo_path {
            if Path::new(DEFAULT_DRIVE_MOUNT).exists() {
                utils::windows::unmount_drive("Y:")?;
            }

            utils::windows::mount_drive(DEFAULT_DRIVE_MOUNT, &payload.repo_path)?;
        };

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

    save_config_to_file(state, "Preferences successfully saved!");

    Ok("ok".to_string())
}

fn save_config_to_file(state: Arc<AppState>, log_msg: &str) {
    let file = fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&state.config_file)
        .unwrap();

    let mut config = state.app_config.read().clone();
    let repo_config = config.initialize_repo_config();

    // Get rid of the PAT
    config.github_pat = None;

    {
        let mut lock = state.repo_config.write();
        *lock = repo_config;
    }

    serde_yaml::to_writer(file, &config).unwrap();

    info!("{}", log_msg);
}
