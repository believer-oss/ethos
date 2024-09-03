use std::fs;
use std::fs::File;
use std::io::Write;

use anyhow::anyhow;
use axum::extract::State;
use octocrab::Octocrab;
use tracing::{error, info};

use ethos_core::types::errors::CoreError;
use ethos_core::utils::update;
use ethos_core::BIN_SUFFIX;

use crate::engine::EngineProvider;
use crate::state::AppState;
use crate::APP_NAME;

#[cfg(target_os = "macos")]
use crate::VERSION;

#[cfg(target_os = "macos")]
pub async fn get_latest_version() -> Result<String, CoreError> {
    Ok(VERSION.to_string())
}

static REPO_OWNER: &str = "believer-oss";
static REPO_NAME: &str = "ethos";

#[cfg(not(target_os = "macos"))]
pub async fn get_latest_version<T>(State(state): State<AppState<T>>) -> Result<String, CoreError>
where
    T: EngineProvider,
{
    let token = state.app_config.read().github_pat.clone();
    let octocrab = if let Some(token) = token {
        Octocrab::builder().personal_token(token).build()?
    } else {
        Octocrab::builder().build()?
    };

    let app_name = APP_NAME.to_lowercase();
    let latest =
        update::get_latest_github_release(&octocrab, &app_name, REPO_OWNER, REPO_NAME).await?;

    Ok(latest
        .tag_name
        .strip_prefix(&format!("{}-v", APP_NAME.to_lowercase()))
        .unwrap()
        .to_string())
}

pub async fn run_update<T>(State(state): State<AppState<T>>) -> Result<(), CoreError>
where
    T: EngineProvider,
{
    info!("Running update");
    let token = state.app_config.read().github_pat.clone();
    let octocrab = if let Some(ref token) = token {
        Octocrab::builder().personal_token(token.clone()).build()?
    } else {
        Octocrab::builder().build()?
    };

    let app_name = APP_NAME.to_lowercase();

    let latest =
        update::get_latest_github_release(&octocrab, &app_name, REPO_OWNER, REPO_NAME).await?;
    let app_name = APP_NAME.to_lowercase();

    let asset = latest
        .assets
        .iter()
        .find(|asset| asset.name == format!("{}{}", &app_name, BIN_SUFFIX));

    match asset {
        Some(asset) => {
            // download release
            info!(
                "https://api.github.com/repos/{}/{}/releases/assets/{}",
                REPO_OWNER, REPO_NAME, asset.id
            );

            let client = reqwest::Client::new();
            let response = client
                .get(format!(
                    "https://api.github.com/repos/{}/{}/releases/assets/{}",
                    REPO_OWNER, REPO_NAME, asset.id
                ))
                .header("Accept", "application/octet-stream")
                .header("User-Agent", app_name);
            let mut response = if let Some(token) = token {
                response
                    .header("Authorization", format!("Bearer {}", token))
                    .send()
                    .await?
            } else {
                response.send().await?
            };

            if response.status().is_client_error() {
                let text = response.text().await?;
                error!("Error downloading asset: {:?}", text.clone());
                return Err(CoreError::Internal(anyhow!(
                    "Error downloading asset: {:?}",
                    text
                )));
            }

            let exe_path = match std::env::current_exe() {
                Ok(path) => path,
                Err(e) => {
                    return Err(anyhow!("Error getting current exe path: {:?}", e).into());
                }
            };

            let tmp_path = format!("{}_tmp", exe_path.to_str().unwrap());
            info!("Downloading to: {}", tmp_path);

            let mut file = File::create(tmp_path.clone())?;
            while let Some(chunk) = response.chunk().await? {
                file.write_all(&chunk)?;
            }

            match self_replace::self_replace(&tmp_path) {
                Ok(_) => {
                    info!("Updated exe");
                    fs::remove_file(&tmp_path)?;
                    Ok(())
                }
                Err(e) => {
                    error!("Error replacing exe: {:?}", e);
                    Err(anyhow!("Error replacing exe: {:?}", e).into())
                }
            }
        }
        None => Err(anyhow!("No asset found").into()),
    }
}
