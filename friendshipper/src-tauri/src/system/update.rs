use std::fs;
use std::fs::File;
use std::io::Write;

use anyhow::anyhow;
use axum::extract::State;
use octocrab::models::repos::Release;
use octocrab::Octocrab;
use tracing::{error, info};

use ethos_core::types::errors::CoreError;
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

    let releases = octocrab
        .repos(REPO_OWNER, REPO_NAME)
        .releases()
        .list()
        .send()
        .await?;

    let mut latest = releases
        .into_iter()
        .filter_map(|release| {
            if release.draft || release.prerelease {
                return None;
            }

            // if release doesn't match the format app_name-vX.Y.Z, skip it
            if !release.tag_name.starts_with(&format!("{}-v", app_name)) {
                return None;
            }

            // get semver
            let version = release
                .tag_name
                .strip_prefix(&format!("{}-v", app_name))
                .unwrap();
            if semver::Version::parse(version).is_err() {
                return None;
            }

            let tag_name = release.tag_name.clone();
            if release
                .assets
                .iter()
                .any(|asset| asset.name == format!("{}{}", &app_name, BIN_SUFFIX))
            {
                return Some(tag_name);
            }

            None
        })
        .collect::<Vec<String>>();

    // sort by semver
    latest.sort_by(|a, b| {
        // we can unwrap because we asserted this format earlier
        let a = a.strip_prefix(&format!("{}-v", &app_name)).unwrap();
        let b = b.strip_prefix(&format!("{}-v", &app_name)).unwrap();
        let a = semver::Version::parse(a).unwrap();
        let b = semver::Version::parse(b).unwrap();

        // reverse it
        b.cmp(&a)
    });

    match latest.first() {
        Some(latest) => Ok(latest
            .strip_prefix(&format!("{}-v", &app_name))
            .unwrap()
            .to_string()),
        None => Err(anyhow!("No latest version found").into()),
    }
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

            if release.tag_name.starts_with(&app_name)
                && release
                    .assets
                    .iter()
                    .any(|asset| asset.name == format!("{}{}", &app_name, BIN_SUFFIX))
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
                        .header("User-Agent", &app_name);
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
