use std::fs;
use std::fs::OpenOptions;
use std::io::Write;

use anyhow::anyhow;
use axum::extract::{Path, Query, State};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use directories_next::ProjectDirs;
use serde::{Deserialize, Serialize};
use tracing::{error, info};

use crate::engine::EngineProvider;
use ethos_core::clients::kube::ensure_kube_client;
use ethos_core::types::errors::CoreError;
use ethos_core::types::gameserver::{GameServerResults, LaunchRequest};

use crate::state::AppState;

pub fn router<T>() -> Router<AppState<T>>
where
    T: EngineProvider,
{
    Router::new()
        .route("/", get(get_servers).post(launch_server))
        .route("/open-logs", post(open_logs_folder))
        .route("/:name", delete(terminate_server).get(get_server))
        .route("/:name/logs", post(download_logs))
        .route("/:name/logs/tail", post(tail_logs))
        .route("/logs/stop", post(stop_tail))
}

#[derive(Debug, Deserialize, Serialize)]
struct GetServersParams {
    commit: Option<String>,
}

async fn get_servers<T>(
    State(state): State<AppState<T>>,
    params: Query<GetServersParams>,
) -> Result<Json<Vec<GameServerResults>>, CoreError>
where
    T: EngineProvider,
{
    let kube_client = ensure_kube_client(state.kube_client.read().clone())?;

    let commit = params.commit.clone();
    let servers = kube_client.list_gameservers(commit).await;

    match servers {
        Ok(servers) => {
            // Don't pass back the server until it's been assigned an IP
            let mut servers = servers
                .into_iter()
                .filter(|server| server.ip.is_some())
                .collect::<Vec<_>>();

            // Sort by creation timestamp, newest first
            servers.sort_by(|a, b| b.creation_timestamp.cmp(&a.creation_timestamp));

            Ok(Json(servers))
        }
        Err(e) => {
            error!("Error getting servers: {:?}", e);
            Err(CoreError::from(anyhow!("Error getting servers: {:?}", e)))
        }
    }
}

async fn launch_server<T>(
    State(state): State<AppState<T>>,
    Json(request): Json<LaunchRequest>,
) -> Result<Json<String>, CoreError>
where
    T: EngineProvider,
{
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

async fn download_logs<T>(
    Path(name): Path<String>,
    State(state): State<AppState<T>>,
) -> Result<Json<DownloadLogsResponse>, CoreError>
where
    T: EngineProvider,
{
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

async fn get_server<T>(
    Path(name): Path<String>,
    State(state): State<AppState<T>>,
) -> Result<Json<GameServerResults>, CoreError>
where
    T: EngineProvider,
{
    let kube_client = ensure_kube_client(state.kube_client.read().clone())?;

    info!("Fetching server {}", name);

    match kube_client.get_gameserver(&name).await {
        Ok(server) => match server.status {
            Some(status) => Ok(Json(GameServerResults {
                display_name: match server.spec.display_name.clone() {
                    Some(name) => name,
                    None => server.metadata.name.clone().unwrap(),
                },
                name: server.metadata.name.unwrap(),
                ip: status.ip,
                port: status.port,
                netimgui_port: status.netimgui_port,
                version: server.spec.version.clone(),
                creation_timestamp: server.metadata.creation_timestamp.unwrap(),
            })),
            None => Err(CoreError::from(anyhow!("Server is not ready yet"))),
        },
        Err(e) => Err(CoreError::from(anyhow!("Error getting server: {:?}", e))),
    }
}

async fn terminate_server<T>(
    Path(name): Path<String>,
    State(state): State<AppState<T>>,
) -> Result<Json<String>, CoreError>
where
    T: EngineProvider,
{
    let kube_client = ensure_kube_client(state.kube_client.read().clone())?;

    info!("Terminating server {}", name);

    kube_client.delete_gameserver(&name).await?;

    Ok(Json(String::from("ok")))
}

async fn tail_logs<T>(
    Path(name): Path<String>,
    State(state): State<AppState<T>>,
) -> Result<(), CoreError>
where
    T: EngineProvider,
{
    let kube_client = ensure_kube_client(state.kube_client.read().clone())?;

    info!("Tailing logs for {}", name);
    kube_client.tail_logs_for_gameserver(&name).await?;

    Ok(())
}

async fn stop_tail<T>(State(state): State<AppState<T>>) -> Result<(), CoreError>
where
    T: EngineProvider,
{
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
