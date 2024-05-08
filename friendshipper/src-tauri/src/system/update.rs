use std::fs::File;
use std::{
    fs::{self},
    io::Write,
    path::PathBuf,
    sync::Arc,
};

use anyhow::anyhow;
use aws_sdk_s3::Client;
use axum::{debug_handler, extract::State};
use directories_next::BaseDirs;
use tracing::{error, info};

use ethos_core::clients::aws::ensure_aws_client;
use ethos_core::types::errors::CoreError;

use crate::state::AppState;
use crate::APP_NAME;

#[debug_handler]
pub async fn get_latest_version(State(state): State<Arc<AppState>>) -> Result<String, CoreError> {
    let aws_client = ensure_aws_client(state.aws_client.read().await.clone())?;

    match aws_client.get_latest_object_key(APP_NAME).await? {
        Some(entry) => {
            let version = entry.get_semver().unwrap();
            info!("Found latest version: {}", version);
            Ok(version.to_string())
        }
        None => {
            info!("No latest version found");
            Err(CoreError(anyhow!("No latest version found")))
        }
    }
}

#[debug_handler]
pub async fn run_update(State(state): State<Arc<AppState>>) -> Result<(), CoreError> {
    let aws_client = ensure_aws_client(state.aws_client.read().await.clone())?;

    info!("Running update");
    if let Some(latest) = aws_client.get_latest_object_key(APP_NAME).await? {
        let sdk_config = aws_client.get_sdk_config().await;
        let client = Client::new(&sdk_config);
        let exe_path = match std::env::current_exe() {
            Ok(path) => path,
            Err(e) => {
                return Err(anyhow!("Error getting current exe path: {:?}", e).into());
            }
        };

        let tmp_path = format!("{}_tmp", exe_path.to_str().unwrap());

        let key = latest.key.to_string();

        info!("Downloading key: {}", key);

        let mut file = File::create(tmp_path.clone())?;
        let config = state.app_config.read().clone();
        let mut result = match client
            .get_object()
            .bucket(config.aws_config.unwrap().artifact_bucket_name)
            .key(key)
            .send()
            .await
        {
            Ok(result) => result,
            Err(e) => {
                error!("Error downloading exe: {:?}", e);
                return Err(anyhow!("Error downloading exe: {:?}", e).into());
            }
        };

        while let Some(bytes) = result.body.try_next().await? {
            file.write_all(&bytes)?;
        }

        return match self_replace::self_replace(&tmp_path) {
            Ok(_) => {
                info!("Removing file: {}", tmp_path);
                fs::remove_file(&tmp_path)?;

                info!("Update complete");
                Ok(())
            }
            Err(e) => {
                error!("Error replacing exe: {:?}", e);
                Err(anyhow!("Error replacing exe: {:?}", e).into())
            }
        };
    }

    Err(anyhow!("No latest version found").into())
}

pub fn get_data_dir() -> Option<PathBuf> {
    if let Some(base_dirs) = BaseDirs::new() {
        let data_dir = base_dirs.data_dir().join(APP_NAME).join("data");

        fs::create_dir_all(&data_dir).unwrap();

        Some(data_dir)
    } else {
        None
    }
}
