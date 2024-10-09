use std::fs;

use anyhow::Context;
use axum::extract::{Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Local, Utc};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Time;
use serde::{Deserialize, Serialize};
use tracing::{error, info, instrument, warn};

use crate::engine::EngineProvider;
use ethos_core::clients::argo::{
    ARGO_WORKFLOW_COMMIT_LABEL_KEY, ARGO_WORKFLOW_COMPARE_ANNOTATION_KEY,
    ARGO_WORKFLOW_MESSAGE_ANNOTATION_KEY, ARGO_WORKFLOW_PUSHER_LABEL_KEY,
};
use ethos_core::clients::aws::ensure_aws_client;
use ethos_core::clients::kube::ensure_kube_client;
use ethos_core::clients::obs;
use ethos_core::storage::{
    ArtifactBuildConfig, ArtifactConfig, ArtifactKind, ArtifactList, Platform,
};
use ethos_core::types::argo::workflow::{Workflow, WorkflowStatus};
use ethos_core::types::builds::SyncClientRequest;
use ethos_core::types::errors::CoreError;
use ethos_core::types::gameserver::GameServerResults;

use crate::state::AppState;

const UNKNOWN_PUSHER: &str = "unknown";

pub fn router<T>() -> Router<AppState<T>>
where
    T: EngineProvider,
{
    Router::new()
        .route("/", get(get_builds))
        .route("/client/sync", post(sync_client))
        .route("/client/wipe", post(wipe_client_data))
        .route("/longtail/reset", post(reset_longtail))
        .route("/server/verify", get(verify_server_image))
        .route("/workflows", get(get_workflows))
        .route("/workflows/logs", get(get_logs_for_workflow_node))
        .route("/workflows/stop", post(stop_workflow))
}

#[derive(Default, Deserialize)]
struct GetBuildsParams {
    #[serde(default = "get_default_limit")]
    limit: usize,
    project: Option<String>,
}

fn get_default_limit() -> usize {
    10
}

async fn get_builds<T>(
    State(state): State<AppState<T>>,
    params: Query<GetBuildsParams>,
) -> Result<Json<ArtifactList>, CoreError>
where
    T: EngineProvider,
{
    let aws_client = ensure_aws_client(state.aws_client.read().await.clone())?;
    aws_client.check_config().await?;

    let project_param = params.project.clone();

    let project = if let Some(project) = project_param {
        project
    } else {
        state
            .app_config
            .read()
            .clone()
            .selected_artifact_project
            .context("Project not configured. Repo may still be initializing.")?
    };

    let storage = state
        .storage
        .read()
        .clone()
        .context("Storage not configured. AWS may still be initializing.")?;

    let artifact_config = ArtifactConfig::new(
        project.as_str().into(),
        ArtifactKind::Client,
        ArtifactBuildConfig::Development,
        Platform::Win64,
    );

    let mut builds = storage.artifact_list(artifact_config).await;

    if builds.entries.len() > params.limit {
        builds.entries.truncate(params.limit);
    }

    Ok(Json(builds))
}

#[derive(Default, Deserialize)]
struct VerifyServerImageParams {
    commit: String,
}

async fn verify_server_image<T>(
    State(state): State<AppState<T>>,
    params: Query<VerifyServerImageParams>,
) -> Json<bool>
where
    T: EngineProvider,
{
    let aws_client = ensure_aws_client(state.aws_client.read().await.clone()).unwrap();
    Json(
        aws_client
            .verify_ecr_image_for_commit(params.commit.clone())
            .await,
    )
}

#[instrument(skip(state), ret)]
async fn sync_client<T>(
    State(state): State<AppState<T>>,
    Json(payload): Json<SyncClientRequest>,
) -> Result<(), CoreError>
where
    T: EngineProvider,
{
    let aws_client = ensure_aws_client(state.aws_client.read().await.clone())?;

    let local_path = state
        .longtail
        .download_path
        .0
        .join(payload.artifact_entry.base_name());
    let remote_path = payload
        .method_prefix
        .get_storage_url(&payload.artifact_entry);
    let tx = state.longtail_tx.clone();

    let mut archive_urls: Vec<String> = vec![remote_path];

    if state.app_config.read().game_client_download_symbols {
        let project = state
            .app_config
            .read()
            .clone()
            .selected_artifact_project
            .context("Project not configured. Repo may still be initializing.")?;

        let symbols_config = ArtifactConfig::new(
            project.as_str().into(),
            ArtifactKind::ClientSymbols,
            ArtifactBuildConfig::Development,
            Platform::Win64,
        );

        match state.storage.read().clone() {
            Some(storage) => {
                match payload
                    .artifact_entry
                    .clone()
                    .convert_to_config(&symbols_config, &storage)
                {
                    Err(e) => warn!("Failed to determine symbols archive URL. Symbols will be unavailable. Error: {}", e),
                    Ok(symbols_entry) => {
                        let url = payload.method_prefix.get_storage_url(&symbols_entry);
                        archive_urls.push(url);
                    }
                }
            }
            None => {
                warn!("Storage not configured. AWS may still be initializing.");
            }
        };
    }

    match fs::create_dir_all(&local_path) {
        Ok(_) => {
            state.longtail.get_archive(
                &local_path,
                None,
                &archive_urls,
                tx,
                aws_client.get_credentials().await,
            )?;
        }
        Err(e) => return Err(CoreError::Internal(e.into())),
    }

    T::post_download(&local_path).await;

    if let Some(launch_options) = payload.launch_options {
        let kube_client = ensure_kube_client(state.kube_client.read().clone())?;
        let game_server = kube_client.get_gameserver(&launch_options.name).await?;

        if let Some(status) = game_server.status {
            info!(
                "Launching game client with server host {:?}:{}",
                status.ip, status.port
            );

            // Assume this GameServerResults type will become an engine-specific type in the future.
            // Right now, we're asking the client to basically look up game servers, then send us back
            // the IP, port, and netimgui port, and that seems inefficient. We should be able to have the client
            // send us a unique identifier for the server, and then we can call a generic GameServer -> LaunchConfig
            // style method.
            let game_server_results = GameServerResults {
                // these fields don't matter
                name: "".to_string(),
                display_name: "".to_string(),
                version: "".to_string(),
                creation_timestamp: Time(Utc::now()),

                // these fields matter
                ip: status.ip,
                port: status.port,
                netimgui_port: status.netimgui_port,
            };

            let args = state.engine.create_launch_args(
                state.app_config.read().clone(),
                state.repo_config.read().clone(),
                game_server_results,
            );
            let child = match state.engine.launch(local_path, args) {
                Ok(child) => child,
                Err(e) => {
                    error!("Failed to launch game client with error: {}", e);
                    return Err(CoreError::Internal(e));
                }
            };

            if let Some(mut child) = child {
                if state.app_config.read().record_play {
                    let client = obs::Client::default();
                    match client.start_recording().await {
                        Ok(_) => {}
                        Err(e) => {
                            return Err(e);
                        }
                    };

                    tokio::spawn(async move {
                        match child.wait() {
                            Ok(_) => {}
                            Err(e) => {
                                error!("Error waiting for child: {}", e);
                            }
                        }
                        match client.stop_recording().await {
                            Ok(_) => {}
                            Err(_) => {
                                error!("Error stopping recording");
                            }
                        }
                    });
                }
            }
        }
    }

    Ok(())
}

pub async fn wipe_client_data<T>(State(state): State<AppState<T>>) -> Result<(), CoreError>
where
    T: EngineProvider,
{
    let local_path = state.longtail.download_path.0.clone();

    // delete all directories in the download path except "logs"
    let entries = fs::read_dir(local_path)
        .context("Failed to read path")?
        .filter_map(|e| {
            let e = e.ok()?;
            (e.file_type().unwrap().is_dir() && !e.path().to_str().unwrap().ends_with("logs"))
                .then_some(e)
        })
        .collect::<Vec<_>>();

    for entry in entries {
        fs::remove_dir_all(entry.path())?;
    }

    Ok(())
}

pub async fn reset_longtail<T>(State(state): State<AppState<T>>) -> Result<(), CoreError>
where
    T: EngineProvider,
{
    let longtail_path = state.longtail.exec_path.clone();

    if let Some(longtail_path) = longtail_path {
        fs::remove_file(longtail_path)?;
    }

    Ok(())
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommitWorkflowInfo {
    pub creation_timestamp: String,
    pub message: Option<String>,
    pub compare_url: Option<String>,
    pub commit: String,
    pub pusher: String,
    pub workflows: Vec<Workflow>,
}

#[derive(Clone, Default, Deserialize, Serialize)]
pub struct GetWorkflowsParams {
    #[serde(default)]
    pub engine: bool,
}

#[derive(Clone, Default, Deserialize, Serialize)]
pub struct GetWorkflowsResponse {
    pub commits: Vec<CommitWorkflowInfo>,
}

async fn get_workflows<T>(
    State(state): State<AppState<T>>,
    params: Query<GetWorkflowsParams>,
) -> Result<Json<GetWorkflowsResponse>, CoreError>
where
    T: EngineProvider,
{
    let kube_client = ensure_kube_client(state.kube_client.read().clone())?;

    let config = state.app_config.read().clone();
    let mut selected_artifact_project = config
        .selected_artifact_project
        .context("Project not configured. Repo may still be initializing.")?;

    if params.engine && !config.engine_repo_url.is_empty() {
        let parts = config.engine_repo_url.split('/').collect::<Vec<&str>>();
        let repo_owner = parts.get(parts.len() - 2).unwrap().to_lowercase();
        let repo_name = parts
            .last()
            .unwrap()
            .trim_end_matches(".git")
            .to_lowercase();

        selected_artifact_project = format!("{}-{}", repo_owner, repo_name);
    }

    let workflows = kube_client
        .get_workflows(&selected_artifact_project)
        .await?;

    // create map from commit to CommitWorkflowInfo
    let mut commit_map: std::collections::HashMap<String, CommitWorkflowInfo> =
        std::collections::HashMap::new();

    for mut workflow in workflows {
        let unknown_pusher = String::from(UNKNOWN_PUSHER);
        let argolabels = workflow.metadata.labels.as_ref().unwrap();
        let argoannotations = workflow.metadata.annotations.as_ref().unwrap();

        workflow
            .status
            .as_mut()
            .map(|s: &mut WorkflowStatus| -> &mut WorkflowStatus {
                s.started_at = s.started_at.as_mut().map(|started_at| {
                    let f: DateTime<Local> = DateTime::parse_from_rfc3339(started_at)
                        .unwrap_or(Local::now().into())
                        .into();
                    f.time().format("%r").to_string()
                });
                s.finished_at = s.finished_at.as_mut().map(|finished_at| {
                    let f: DateTime<Local> = DateTime::parse_from_rfc3339(finished_at)
                        .unwrap_or(Local::now().into())
                        .into();
                    f.time().format("%r").to_string()
                });
                s
            });

        let commit = argolabels.get(ARGO_WORKFLOW_COMMIT_LABEL_KEY).unwrap();
        let pusher = argolabels
            .get(ARGO_WORKFLOW_PUSHER_LABEL_KEY)
            .unwrap_or(&unknown_pusher);
        let message = argoannotations
            .get(ARGO_WORKFLOW_MESSAGE_ANNOTATION_KEY)
            .cloned();
        let compare_url = argoannotations
            .get(ARGO_WORKFLOW_COMPARE_ANNOTATION_KEY)
            .cloned();

        let creation_timestamp = workflow.metadata.creation_timestamp.clone();
        let commit_info = commit_map
            .entry(commit.clone())
            .or_insert(CommitWorkflowInfo {
                creation_timestamp: creation_timestamp.unwrap().0.to_rfc3339(),
                message,
                compare_url,
                commit: commit.clone(),
                pusher: pusher.clone(),
                workflows: Vec::new(),
            });
        commit_info.workflows.push(workflow);
    }

    // create a vector of CommitWorkflowInfo sorted by creation_timestamp
    let mut commits: Vec<CommitWorkflowInfo> = commit_map.into_values().collect();
    commits.sort_by_key(|c| c.creation_timestamp.clone());
    commits.reverse();

    Ok(Json(GetWorkflowsResponse { commits }))
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GetWorkflowNodeLogsParams {
    pub uid: String,
    pub node_id: String,
}

pub async fn get_logs_for_workflow_node<T>(
    State(state): State<AppState<T>>,
    params: Query<GetWorkflowNodeLogsParams>,
) -> Result<String, CoreError>
where
    T: EngineProvider,
{
    let kube_client = ensure_kube_client(state.kube_client.read().clone())?;
    let logs = kube_client
        .get_logs_for_workflow_node(&params.uid, &params.node_id)
        .await?;
    Ok(logs)
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StopWorkflowParams {
    pub workflow: String,
}

pub async fn stop_workflow<T>(
    State(state): State<AppState<T>>,
    params: Query<StopWorkflowParams>,
) -> Result<String, CoreError>
where
    T: EngineProvider,
{
    let kube_client = ensure_kube_client(state.kube_client.read().clone())?;
    let wf = kube_client.stop_workflow(&params.workflow).await?;
    Ok(wf)
}
