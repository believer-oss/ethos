use std::fs;
use std::fs::File;
use std::io::Write;
use std::sync::Arc;

use anyhow::anyhow;
use axum::{debug_handler, extract::State};
use octocrab::models::repos::Release;
use octocrab::Octocrab;
use tracing::{error, info};

use ethos_core::types::errors::CoreError;
use ethos_core::BIN_SUFFIX;

use crate::state::AppState;
use crate::APP_NAME;

static REPO_OWNER: &str = "BelieverCo";
static REPO_NAME: &str = "ethos";

#[debug_handler]
pub async fn get_latest_version(State(state): State<Arc<AppState>>) -> Result<String, CoreError> {
    let token = state.app_config.read().ensure_github_pat()?;
    let octocrab = Octocrab::builder().personal_token(token.clone()).build()?;

    let releases = octocrab
        .repos(REPO_OWNER, REPO_NAME)
        .releases()
        .list()
        .send()
        .await?;

    let latest: Option<String> = releases
        .into_iter()
        .filter_map(|release| {
            if release.draft || release.prerelease {
                return None;
            }

            let tag_name = release.tag_name.clone();
            if release.tag_name.starts_with(APP_NAME)
                && release
                    .assets
                    .iter()
                    .any(|asset| asset.name == format!("{}{}", APP_NAME, BIN_SUFFIX))
            {
                return Some(tag_name);
            }

            None
        })
        .next();

    match latest {
        Some(latest) => Ok(latest
            .strip_prefix(&format!("{}-v", APP_NAME))
            .unwrap()
            .to_string()),
        None => Err(anyhow!("No latest version found").into()),
    }
}

#[debug_handler]
pub async fn run_update(State(state): State<Arc<AppState>>) -> Result<(), CoreError> {
    info!("Running update");
    let token = state.app_config.read().ensure_github_pat()?;
    let octocrab = Octocrab::builder().personal_token(token.clone()).build()?;

    let releases = octocrab
        .repos(REPO_OWNER, REPO_NAME)
        .releases()
        .list()
        .send()
        .await?;

    let latest: Option<Release> = releases
        .into_iter()
        .filter_map(|release| {
            if release.draft || release.prerelease {
                return None;
            }

            if release.tag_name.starts_with(APP_NAME)
                && release
                    .assets
                    .iter()
                    .any(|asset| asset.name == format!("{}{}", APP_NAME, BIN_SUFFIX))
            {
                return Some(release);
            }

            None
        })
        .next();

    match latest {
        Some(latest) => {
            let asset = latest
                .assets
                .iter()
                .find(|asset| asset.name == format!("{}{}", APP_NAME, BIN_SUFFIX));

            match asset {
                Some(asset) => {
                    // download release
                    info!(
                        "https://api.github.com/repos/{}/{}/releases/assets/{}",
                        REPO_OWNER, REPO_NAME, asset.id
                    );

                    let client = reqwest::Client::new();
                    let mut response = client
                        .get(format!(
                            "https://api.github.com/repos/{}/{}/releases/assets/{}",
                            REPO_OWNER, REPO_NAME, asset.id
                        ))
                        .header("Authorization", format!("Bearer {}", token))
                        .header("Accept", "application/octet-stream")
                        .header("User-Agent", APP_NAME)
                        .send()
                        .await?;

                    if response.status().is_client_error() {
                        let text = response.text().await?;
                        error!("Error downloading asset: {:?}", text.clone());
                        return Err(CoreError(anyhow!("Error downloading asset: {:?}", text)));
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
        None => Err(anyhow!("No latest version found").into()),
    }
}
