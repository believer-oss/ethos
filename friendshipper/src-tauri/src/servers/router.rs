use std::fs;
use std::fs::OpenOptions;
use std::io::Write;
use std::sync::Arc;

use anyhow::anyhow;
use axum::extract::{Path, Query, State};
use axum::routing::{delete, get, post};
use axum::{debug_handler, Json, Router};
use directories_next::ProjectDirs;
use serde::{Deserialize, Serialize};
use tracing::{error, info};

use ethos_core::clients::kube::ensure_kube_client;
use ethos_core::types::errors::CoreError;
use ethos_core::types::gameserver::{GameServerResults, LaunchRequest};
use ethos_core::{KUBE_DISPLAY_NAME_LABEL_KEY, KUBE_SHA_LABEL_KEY};

use crate::state::AppState;

pub fn router(shared_state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(get_servers).post(launch_server))
        .route("/open-logs", post(open_logs_folder))
        .route("/:name", delete(terminate_server).get(get_server))
        .route("/:name/logs", post(download_logs))
        .route("/:name/logs/tail", post(tail_logs))
        .route("/logs/stop", post(stop_tail))
        .with_state(shared_state)
}

#[derive(Debug, Deserialize, Serialize)]
struct GetServersParams {
    commit: Option<String>,
}

#[debug_handler]
async fn get_servers(
    State(state): State<Arc<AppState>>,
    params: Query<GetServersParams>,
) -> Result<Json<Vec<GameServerResults>>, CoreError> {
    let kube_client = ensure_kube_client(state.kube_client.read().clone())?;

    let commit = params.commit.clone();
    let servers = kube_client.list_gameservers(commit).await;

    match servers {
        Ok(servers) => {
            // Don't pass back the server until it's been assigned an IP
            let servers = servers
                .into_iter()
                .filter(|server| server.ip.is_some())
                .collect::<Vec<_>>();
            Ok(Json(servers))
        }
        Err(e) => {
            error!("Error getting servers: {:?}", e);
            Err(CoreError::from(anyhow!("Error getting servers: {:?}", e)))
        }
    }
}

#[debug_handler]
async fn launch_server(
    State(state): State<Arc<AppState>>,
    Json(request): Json<LaunchRequest>,
) -> Result<Json<String>, CoreError> {
    let kube_client = ensure_kube_client(state.kube_client.read().clone())?;

    info!("Launching server at version {}", request.commit);

    let commit = request.commit.clone();
    kube_client
        .create_gameserver_for_sha(
            commit,
            request.check_for_existing,
            request.display_name.as_str(),
            request.map,
        )
        .await?;

    Ok(Json(String::from("ok")))
}

#[derive(Debug, Deserialize, Serialize)]
pub struct DownloadLogsResponse {
    pub name: String,
    pub path: String,
}

async fn download_logs(
    Path(name): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Result<Json<DownloadLogsResponse>, CoreError> {
    let kube_client = ensure_kube_client(state.kube_client.read().clone())?;

    info!("Downloading logs for {}", name);

    let logs = kube_client.get_logs_for_gameserver(&name, false).await?;

    // Optimistically try to get previous logs
    let previous_logs = kube_client.get_logs_for_gameserver(&name, true).await?;

    if let Some(proj_dirs) = ProjectDirs::from("", "", crate::APP_NAME) {
        let mut log_path = proj_dirs.data_dir().to_path_buf();
        log_path.push(format!("server_logs/{}.log", name));

        let mut previous_log_path = proj_dirs.data_dir().to_path_buf();
        previous_log_path.push(format!("server_logs/{}_previous.log", name));

        // Create the directory if needed
        if let Some(p) = log_path.parent() {
            fs::create_dir_all(p)?
        }

        if let Some(logs) = logs {
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(log_path.clone())?;

            file.write_all(logs.as_bytes())?;
            file.sync_all()?;
        } else {
            return Err(CoreError::from(anyhow!(
                "Unable to find logs for server {}",
                name
            )));
        }

        if let Some(previous_logs) = previous_logs {
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(previous_log_path.clone())?;

            file.write_all(previous_logs.as_bytes())?;
            file.sync_all()?;
        }

        return Ok(Json(DownloadLogsResponse {
            name: name.clone(),
            path: log_path.to_str().unwrap().to_string(),
        }));
    }

    Err(CoreError::from(anyhow!(
        "Unable to find project directories"
    )))
}

async fn get_server(
    Path(name): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Result<Json<GameServerResults>, CoreError> {
    let kube_client = ensure_kube_client(state.kube_client.read().clone())?;

    info!("Fetching server {}", name);

    match kube_client.get_gameserver(&name).await {
        Ok(server) => {
            let display_name: String;
            let version: String;
            match server.metadata.labels {
                Some(labels) => {
                    display_name = match labels.get(KUBE_DISPLAY_NAME_LABEL_KEY) {
                        Some(name) => name.clone(),
                        None => server.metadata.name.clone().unwrap(),
                    };

                    version = match labels.get(KUBE_SHA_LABEL_KEY) {
                        Some(name) => name.clone(),
                        None => "".to_string(),
                    };
                }
                None => {
                    display_name = server.metadata.name.clone().unwrap();
                    version = "".to_string();
                }
            };

            match server.status {
                Some(status) => Ok(Json(GameServerResults {
                    display_name,
                    name: server.metadata.name.unwrap(),
                    ip: status.ip,
                    port: status.port,
                    netimgui_port: status.netimgui_port,
                    version: version.to_string(),
                })),
                None => Err(CoreError::from(anyhow!("Server is not ready yet"))),
            }
        }
        Err(e) => Err(CoreError::from(anyhow!("Error getting server: {:?}", e))),
    }
}

async fn terminate_server(
    Path(name): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Result<Json<String>, CoreError> {
    let kube_client = ensure_kube_client(state.kube_client.read().clone())?;

    info!("Terminating server {}", name);

    kube_client.delete_gameserver(&name).await?;

    Ok(Json(String::from("ok")))
}

async fn tail_logs(
    Path(name): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Result<(), CoreError> {
    let kube_client = ensure_kube_client(state.kube_client.read().clone())?;

    info!("Tailing logs for {}", name);
    kube_client.tail_logs_for_gameserver(&name).await?;

    Ok(())
}

async fn stop_tail(State(state): State<Arc<AppState>>) -> Result<(), CoreError> {
    let kube_client = ensure_kube_client(state.kube_client.read().clone())?;

    info!("Stopping tail");
    kube_client.stop_tail().await;

    Ok(())
}

async fn open_logs_folder() -> Result<(), CoreError> {
    if let Some(proj_dirs) = ProjectDirs::from("", "", crate::APP_NAME) {
        let mut log_path = proj_dirs.data_dir().to_path_buf();

        log_path.push("server_logs");

        if !log_path.exists() {
            match fs::create_dir_all(&log_path) {
                Ok(_) => {}
                Err(e) => {
                    error!("Failed to create logs folder: {:?}", e);
                    return Err(CoreError(anyhow!("Failed to create logs folder: {:?}", e)));
                }
            }
        }

        if let Err(e) = open::that(log_path) {
            error!("Failed to open logs folder: {:?}", e);
            return Err(CoreError(anyhow!("Failed to create logs folder: {:?}", e)));
        }
    }

    Ok(())
}
