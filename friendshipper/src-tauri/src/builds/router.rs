use std::collections::HashMap;
use std::fs;

use anyhow::Context;
use axum::extract::{Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Local, Utc};
use ethos_core::longtail::CacheControl;
use ethos_core::storage::{
    ArtifactBuildConfig, ArtifactConfig, ArtifactEntry, ArtifactKind, ArtifactList, Platform,
};
use ethos_core::utils::junit::JunitOutput;
use futures::StreamExt;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Time;
use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;
use tracing::{debug, error, info, instrument, warn};

use crate::engine::EngineProvider;
use ethos_core::clients::argo::{
    LogChunk, ARGO_WORKFLOW_COMMIT_LABEL_KEY, ARGO_WORKFLOW_COMPARE_ANNOTATION_KEY,
    ARGO_WORKFLOW_MESSAGE_ANNOTATION_KEY, ARGO_WORKFLOW_PUSHER_LABEL_KEY,
    ARGO_WORKFLOW_REF_LABEL_KEY,
};
use ethos_core::clients::aws::ensure_aws_client;
use ethos_core::clients::kube::ensure_kube_client;
use ethos_core::clients::obs;
use ethos_core::types::argo::workflow::{
    CreatePromoteBuildWorkflowRequest, Workflow, WorkflowStatus,
};
use ethos_core::types::builds::{LaunchMode, SyncClientRequest};
use ethos_core::types::config::PromoteBuildDestination;
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
        .route("/active", get(get_active_builds))
        .route("/commit", get(get_build))
        .route("/client/sync", post(sync_client))
        .route("/client/cancel", post(cancel_download))
        .route("/client/wipe", post(wipe_client_data))
        .route("/longtail/reset", post(reset_longtail))
        .route("/server/verify", get(verify_server_image))
        .route("/workflows", get(get_workflows))
        .route("/workflows/nodes", get(get_workflow_nodes))
        .route("/workflows/logs", get(get_logs_for_workflow_node))
        .route(
            "/workflows/:workflow_name/:node_id/logs/tail",
            post(start_workflow_log_tail),
        )
        .route("/workflows/logs/stop", post(stop_workflow_log_tail))
        .route("/workflows/junit", get(get_workflow_junit_artifact))
        .route("/workflows/stop", post(stop_workflow))
        .route(
            "/workflows/promote-build",
            post(create_promote_build_workflow),
        )
}

#[derive(Default, Deserialize)]
struct GetBuildParams {
    commit: String,
    project: Option<String>,
}

async fn get_build<T>(
    State(state): State<AppState<T>>,
    params: Query<GetBuildParams>,
) -> Result<Json<ArtifactEntry>, CoreError>
where
    T: EngineProvider,
{
    let aws_client = ensure_aws_client(state.aws_client.read().await.clone())?;
    aws_client.check_expiration().await?;

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

    let artifact_entry = storage
        .get_artifact_for_commit(artifact_config, &params.commit)
        .await?;
    Ok(Json(artifact_entry))
}

/// Outcome of reading one metadata object: the deployed sha and its last-modified
/// time, or the message from a failed read.
type MetadataReadResult = Result<(String, Option<DateTime<Utc>>), String>;

/// Metadata path -> read outcome. Keyed by path rather than by destination because
/// the path is what is actually fetched, and two destinations may share one.
type ResolvedMetadata = HashMap<String, MetadataReadResult>;

const NO_METADATA_PATH: &str = "no metadataObjectKey configured";
const NO_STEAM_BRANCHES: &str = "no steam branches configured";
const METADATA_READ_NOT_ATTEMPTED: &str = "metadata read was not attempted";
const NO_PROMOTED_BUCKET: &str =
    "no promoted artifact bucket configured; set promotedArtifactBucketName in dynamic config";

/// One row in the active builds modal: a launcher destination, or a single Steam
/// branch of a Steam destination.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveBuild {
    pub display_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub steam_branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deployed_at: Option<DateTime<Utc>>,
    pub status: ActiveBuildStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ActiveBuildStatus {
    Resolved,
    Tbd,
    Error,
}

impl ActiveBuild {
    fn new(display_name: &str, status: ActiveBuildStatus) -> Self {
        ActiveBuild {
            display_name: display_name.to_string(),
            steam_branch: None,
            sha: None,
            deployed_at: None,
            status,
            error: None,
        }
    }
}

/// Resolves the bucket holding promoted-build metadata objects. Pure.
///
/// Dynamic config wins over whatever the AWS client was built with. A blank value is
/// treated as unset so it cannot mask a working fallback. The result may still be
/// empty, which the caller reports as a configuration error.
fn resolve_promoted_bucket(from_dynamic_config: Option<&str>, fallback: &str) -> String {
    match from_dynamic_config.map(str::trim) {
        Some(bucket) if !bucket.is_empty() => bucket.to_string(),
        _ => fallback.trim().to_string(),
    }
}

/// Matches `PromoteBuildModal.svelte`, which lowercases before comparing.
fn is_steam_destination(destination: &PromoteBuildDestination) -> bool {
    destination
        .distribution
        .as_deref()
        .is_some_and(|distribution| distribution.eq_ignore_ascii_case("steam"))
}

/// Expands configured destinations into modal rows. Pure: no locks, no I/O, no async.
///
/// Every destination produces at least one row, including misconfigured ones. A
/// destination that silently vanishes is worse than one showing an error, because
/// the operator cannot tell "not deployed" from "not displayed".
fn build_rows(
    destinations: &[PromoteBuildDestination],
    resolved: &ResolvedMetadata,
) -> Vec<ActiveBuild> {
    let mut rows: Vec<ActiveBuild> = Vec::with_capacity(destinations.len());

    // Applies a resolved read to a row, or marks why it has no sha.
    let apply_read = |row: &mut ActiveBuild, key: Option<&str>| match key.map(|k| resolved.get(k)) {
        Some(Some(Ok((sha, deployed_at)))) => {
            row.status = ActiveBuildStatus::Resolved;
            row.sha = Some(sha.clone());
            row.deployed_at = *deployed_at;
        }
        Some(Some(Err(message))) => {
            row.status = ActiveBuildStatus::Error;
            row.error = Some(message.clone());
        }
        // Unreachable today; surfaced rather than hidden if that changes.
        Some(None) => {
            row.status = ActiveBuildStatus::Error;
            row.error = Some(METADATA_READ_NOT_ATTEMPTED.to_string());
        }
        None => {}
    };

    for destination in destinations {
        let key = destination.metadata_object_key.as_deref();

        // Steam is checked before the metadata key so a Steam destination that gains a
        // key still expands to one row per branch rather than collapsing into one.
        if is_steam_destination(destination) {
            let branches = destination.steam_branches.as_deref().unwrap_or_default();
            if branches.is_empty() {
                let mut row = ActiveBuild::new(&destination.display_name, ActiveBuildStatus::Tbd);
                row.error = Some(NO_STEAM_BRANCHES.to_string());
                apply_read(&mut row, key);
                rows.push(row);
            } else {
                for branch in branches {
                    let mut row =
                        ActiveBuild::new(&destination.display_name, ActiveBuildStatus::Tbd);
                    row.steam_branch = Some(branch.clone());
                    apply_read(&mut row, key);
                    rows.push(row);
                }
            }
            continue;
        }

        if key.is_some() {
            let mut row = ActiveBuild::new(&destination.display_name, ActiveBuildStatus::Error);
            apply_read(&mut row, key);
            rows.push(row);
            continue;
        }

        // Neither a metadata path nor Steam: still listed, so the gap is visible.
        let mut row = ActiveBuild::new(&destination.display_name, ActiveBuildStatus::Tbd);
        row.error = Some(NO_METADATA_PATH.to_string());
        rows.push(row);
    }

    rows
}

/// Lists every promotion destination with the commit currently deployed to it.
pub async fn get_active_builds<T>(
    State(state): State<AppState<T>>,
) -> Result<Json<Vec<ActiveBuild>>, CoreError>
where
    T: EngineProvider,
{
    // `state.dynamic_config`'s guard is !Send. Clone inside a scope so it drops
    // before any `.await` below, or the handler fails to compile.
    let (destinations, configured_bucket): (Vec<PromoteBuildDestination>, Option<String>) = {
        let dynamic_config = state.dynamic_config.read();
        (
            dynamic_config
                .promotable_build_destinations
                .clone()
                .unwrap_or_default(),
            dynamic_config.promoted_artifact_bucket_name.clone(),
        )
    };

    // Short-circuit before touching AWS: a config with no destinations must render an
    // empty modal, not a credentials error.
    if destinations.is_empty() {
        return Ok(Json(vec![]));
    }

    let aws_client = ensure_aws_client(state.aws_client.read().await.clone())?;
    aws_client.check_expiration().await?;

    // Dynamic config first, else whatever the client was built with. Both can be
    // empty; read_object_to_string reports that as a configuration error.
    let bucket = resolve_promoted_bucket(
        configured_bucket.as_deref(),
        &aws_client.get_promoted_artifacts_bucket(),
    );

    // Resolve the bucket once. Without this every metadata path would issue its own
    // doomed read and log its own error on each poll, all reporting the same thing.
    if bucket.is_empty() {
        let resolved: ResolvedMetadata = destinations
            .iter()
            .filter_map(|destination| destination.metadata_object_key.clone())
            .map(|path| (path, Err(NO_PROMOTED_BUCKET.to_string())))
            .collect();
        return Ok(Json(build_rows(&destinations, &resolved)));
    }

    // Distinct paths only: two destinations sharing a path must not cause two fetches.
    let mut metadata_paths: Vec<String> = destinations
        .iter()
        .filter_map(|destination| destination.metadata_object_key.clone())
        .collect();
    metadata_paths.sort();
    metadata_paths.dedup();

    let fetch_futures = metadata_paths.into_iter().map(|path| {
        let aws_client = aws_client.clone();
        let bucket = bucket.clone();
        async move {
            // The read result is carried inside the tuple rather than propagated: one
            // unreachable destination must not blank the whole modal.
            let result = aws_client
                .read_object_to_string(&bucket, &path)
                .await
                .map_err(|e| e.to_string());
            (path, result)
        }
    });

    let results: Vec<(String, MetadataReadResult)> = futures::stream::iter(fetch_futures)
        .buffered(8)
        .collect()
        .await;

    let resolved: ResolvedMetadata = results.into_iter().collect();

    Ok(Json(build_rows(&destinations, &resolved)))
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
    aws_client.check_expiration().await?;

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
) -> Result<Json<bool>, CoreError>
where
    T: EngineProvider,
{
    let aws_client = ensure_aws_client(state.aws_client.read().await.clone())?;

    let mut local_path = state.longtail.download_path.0.clone();
    let remote_path = payload
        .method_prefix
        .get_storage_url(&payload.artifact_entry);
    let tx = state.longtail_tx.clone();

    // make a client_cache dir if it doesn't exist
    let client_cache_dir = local_path.join("client_cache");
    if !client_cache_dir.exists() {
        fs::create_dir_all(client_cache_dir.clone())?;
    }

    if let Some(project) = state.app_config.read().clone().selected_artifact_project {
        local_path = local_path.join(project);
    }

    if let Some(sub_path) = payload.sub_path {
        local_path = local_path.join(sub_path);
    }

    local_path = local_path.join(payload.artifact_entry.base_name());

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

    let cache_control = CacheControl {
        path: client_cache_dir,
        max_size_bytes: state.app_config.read().max_client_cache_size_gb * 1024 * 1024 * 1024,
    };

    let local_path_clone = local_path.clone();
    match fs::create_dir_all(&local_path_clone) {
        Ok(_) => {
            let (cancel_tx, mut cancel_rx) = oneshot::channel();
            state.cancel_tx.write().await.replace(cancel_tx);

            info!("Starting download...");
            let longtail = state.longtail.clone();
            tokio::select! {
                cancel_result = &mut cancel_rx => {
                    info!("Cancel branch hit with result: {:?}", cancel_result);

                    let mut guard = longtail.child_process.lock();
                    if let Some(mut child) = guard.take() {
                        info!("Killing child process");
                        child.kill().unwrap();
                    }

                    return Ok(Json(false));
                }
                download_result = async move {
                    let credentials = aws_client.get_credentials().await;
                    tokio::task::spawn_blocking(move || {
                        info!("Starting actual download...");
                        state.longtail.get_archive(
                            &local_path_clone,
                            Some(cache_control),
                            &archive_urls,
                            tx,
                            credentials,
                        )
                    }).await
                } => {
                    info!("Download branch complete with result: {:?}", download_result);
                }
            }
        }
        Err(e) => return Err(CoreError::Internal(e.into())),
    }

    // reset cancel_tx to none
    state.cancel_tx.write().await.take();

    T::post_download(&local_path).await;

    if let Some(launch_options) = payload.launch_options {
        match launch_options.launch_mode {
            LaunchMode::WithServer => {
                if !launch_options.name.is_empty() {
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
                            ready: status.ready.unwrap_or(false),
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
            }
            LaunchMode::WithoutServer => {
                let empty_args: Vec<String> = Vec::new();
                let _child = match state.engine.launch(local_path, empty_args) {
                    Ok(_child) => _child,
                    Err(e) => {
                        error!("Failed to launch game client with error: {}", e);
                        return Err(CoreError::Internal(e));
                    }
                };
            }
        }
    }

    Ok(Json(true))
}

pub async fn cancel_download<T>(State(state): State<AppState<T>>) -> Result<(), CoreError>
where
    T: EngineProvider,
{
    if let Some(cancel_tx) = state.cancel_tx.write().await.take() {
        info!("Cancelling download");
        if let Err(e) = cancel_tx.send(()) {
            return Err(CoreError::Internal(anyhow::anyhow!(
                "Failed to cancel download: {:?}",
                e
            )));
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
    pub branch: Option<String>,
    pub workflows: Vec<Workflow>,
}

#[derive(Clone, Default, Deserialize, Serialize)]
pub struct GetWorkflowsParams {
    #[serde(default)]
    pub engine: bool,
    pub project: Option<String>,
}

#[derive(Clone, Default, Deserialize, Serialize)]
pub struct GetWorkflowsResponse {
    pub commits: Vec<CommitWorkflowInfo>,
}

fn resolve_workflow_project(
    params_project: Option<String>,
    selected_artifact_project: Option<String>,
    engine: bool,
    engine_repo_url: &str,
) -> anyhow::Result<String> {
    let mut project = if let Some(project) = params_project {
        project
    } else {
        selected_artifact_project
            .context("Project not configured. Repo may still be initializing.")?
    };

    if engine && !engine_repo_url.is_empty() {
        // `engine_repo_url` is free-form config, so walk from the right and skip empty segments
        // rather than indexing: a trailing slash still resolves, and a value with no '/' falls
        // through to the param/config project instead of panicking on an underflowed index.
        let mut segments = engine_repo_url.rsplit('/').filter(|s| !s.is_empty());

        let repo_name = segments
            .next()
            .map(|name| name.trim_end_matches(".git"))
            .filter(|name| !name.is_empty());
        let repo_owner = segments.next();

        if let (Some(owner), Some(name)) = (repo_owner, repo_name) {
            let owner = owner.to_lowercase();
            let name = name.to_lowercase();
            project = format!("{owner}-{name}");
        }
    }

    Ok(project)
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

    let selected_artifact_project = resolve_workflow_project(
        params.project.clone(),
        config.selected_artifact_project.clone(),
        params.engine,
        &config.engine_repo_url,
    )?;

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
        let branch = argolabels.get(ARGO_WORKFLOW_REF_LABEL_KEY).cloned();
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
                branch,
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

pub async fn get_workflow_nodes<T>(
    State(state): State<AppState<T>>,
    params: Query<GetWorkflowNodesParams>,
) -> Result<Json<Workflow>, CoreError>
where
    T: EngineProvider,
{
    let kube_client = ensure_kube_client(state.kube_client.read().clone())?;
    let workflow = kube_client.get_workflow_with_nodes(&params.name).await?;
    Ok(Json(workflow))
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GetWorkflowNodesParams {
    pub name: String,
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GetWorkflowNodeLogsParams {
    pub workflow_name: String,
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
        .get_logs_for_workflow_node(&params.workflow_name, &params.node_id, None::<fn(LogChunk)>)
        .await?;
    Ok(logs)
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GetWorkflowJunitArtifactParams {
    pub uid: String,
    pub node_id: String,
}

pub async fn get_workflow_junit_artifact<T>(
    State(state): State<AppState<T>>,
    params: Query<GetWorkflowJunitArtifactParams>,
) -> Result<Json<Option<JunitOutput>>, CoreError>
where
    T: EngineProvider,
{
    let kube_client = ensure_kube_client(state.kube_client.read().clone())?;
    let junit_output = kube_client
        .get_junit_artifact_for_workflow_node(&params.uid, &params.node_id)
        .await?;
    Ok(Json(junit_output))
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

#[instrument(skip(state))]
pub async fn create_promote_build_workflow<T>(
    State(state): State<AppState<T>>,
    Json(mut payload): Json<CreatePromoteBuildWorkflowRequest>,
) -> Result<Json<Workflow>, CoreError>
where
    T: EngineProvider,
{
    let kube_client = ensure_kube_client(state.kube_client.read().clone())?;

    // Get pusher from github client or fallback to user display name
    if payload.pusher.is_none() {
        let pusher = if let Some(github_client) = state.github_client.read().clone() {
            Some(github_client.username.clone())
        } else {
            let app_config = state.app_config.read();
            if !app_config.user_display_name.is_empty() {
                Some(app_config.user_display_name.clone())
            } else {
                None
            }
        };
        payload.pusher = pusher;
    }

    info!(
        "Creating promote build workflow for commit: {}",
        payload.commit
    );

    let workflow = kube_client.create_promote_build_workflow(payload).await?;

    info!(
        "Successfully created promote build workflow: {:?}",
        workflow.metadata.name
    );
    Ok(Json(workflow))
}

pub async fn start_workflow_log_tail<T>(
    State(state): State<AppState<T>>,
    axum::extract::Path((workflow_name, node_id)): axum::extract::Path<(String, String)>,
) -> Result<(), CoreError>
where
    T: EngineProvider,
{
    let kube_client = ensure_kube_client(state.kube_client.read().clone())?;
    let workflow_log_tx = state.workflow_log_tx.clone();

    info!(
        "Starting workflow log tail for workflow: {}, node: {}",
        workflow_name, node_id
    );

    // Create cancellation channel
    let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel::<()>();

    // Store the cancel sender for the stop function to use
    {
        let mut workflow_log_cancel = state.workflow_log_cancel_tx.write().await;
        *workflow_log_cancel = Some(cancel_tx);
    }

    // Start streaming in background task
    let kube_client_clone = kube_client.clone();
    let workflow_name_clone = workflow_name.clone();
    let node_id_clone = node_id.clone();

    tokio::spawn(async move {
        let channel_fn = {
            let workflow_log_tx = workflow_log_tx.clone();
            move |chunk: ethos_core::clients::argo::LogChunk| {
                debug!(
                    "Received log chunk - data length: {}, finished: {}, error: {:?}",
                    chunk.data.len(),
                    chunk.finished,
                    chunk.error
                );
                let is_empty = chunk.data.is_empty();
                if !is_empty {
                    debug!(
                        "Log chunk content preview: {:?}",
                        &chunk.data.chars().take(100).collect::<String>()
                    );
                }

                // Send the entire LogChunk with finished/error status
                let chunk_json = serde_json::to_string(&chunk).unwrap_or_default();
                if let Err(e) = workflow_log_tx.send(chunk_json) {
                    error!("Failed to send log chunk to workflow log channel: {}", e);
                } else if !is_empty {
                    debug!("Successfully sent log chunk to channel");
                }
            }
        };

        tokio::select! {
            result = kube_client_clone.get_logs_for_workflow_node(&workflow_name_clone, &node_id_clone, Some(channel_fn)) => {
                match result {
                    Ok(_) => {
                        info!("Workflow log streaming completed for {}/{}", workflow_name_clone, node_id_clone);
                    }
                    Err(e) => {
                        error!("Workflow log streaming failed for {}/{}: {:?}", workflow_name_clone, node_id_clone, e);
                    }
                }
            }
            _ = cancel_rx => {
                info!("Workflow log streaming cancelled for {}/{}", workflow_name_clone, node_id_clone);
            }
        }
    });

    Ok(())
}

pub async fn stop_workflow_log_tail<T>(State(state): State<AppState<T>>) -> Result<(), CoreError>
where
    T: EngineProvider,
{
    info!("Stopping workflow log tail");

    // Take the cancel sender and trigger cancellation
    if let Some(cancel_tx) = state.workflow_log_cancel_tx.write().await.take() {
        if let Err(e) = cancel_tx.send(()) {
            warn!("Failed to send cancellation signal: {:?}", e);
        } else {
            info!("Successfully sent cancellation signal to workflow log streaming");
        }
    } else {
        info!("No active workflow log streaming to cancel");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn param_project_wins_over_config() {
        let result = resolve_workflow_project(
            Some("param-project".to_string()),
            Some("config-project".to_string()),
            false,
            "",
        )
        .unwrap();
        assert_eq!(result, "param-project");
    }

    #[test]
    fn falls_back_to_config_project() {
        let result =
            resolve_workflow_project(None, Some("config-project".to_string()), false, "").unwrap();
        assert_eq!(result, "config-project");
    }

    #[test]
    fn errors_when_no_project_available() {
        assert!(resolve_workflow_project(None, None, false, "").is_err());
    }

    #[test]
    fn engine_repo_overrides_everything() {
        let result = resolve_workflow_project(
            Some("param-project".to_string()),
            Some("config-project".to_string()),
            true,
            "https://github.com/BelieverCo/GamePrototypeMP.git",
        )
        .unwrap();
        assert_eq!(result, "believerco-gameprototypemp");
    }

    #[test]
    fn engine_flag_without_repo_url_does_not_override() {
        let result = resolve_workflow_project(
            Some("param-project".to_string()),
            Some("config-project".to_string()),
            true,
            "",
        )
        .unwrap();
        assert_eq!(result, "param-project");
    }

    #[test]
    fn engine_repo_url_without_any_slash_falls_through_instead_of_panicking() {
        let result = resolve_workflow_project(
            Some("param-project".to_string()),
            Some("config-project".to_string()),
            true,
            "GamePrototypeMP",
        )
        .unwrap();
        assert_eq!(result, "param-project");
    }

    #[test]
    fn engine_repo_url_with_trailing_slash_still_resolves() {
        let result = resolve_workflow_project(
            Some("param-project".to_string()),
            Some("config-project".to_string()),
            true,
            "https://github.com/BelieverCo/GamePrototypeMP/",
        )
        .unwrap();
        assert_eq!(result, "believerco-gameprototypemp");
    }

    // Fixture values are synthetic: this repo is public.

    const SHA_ONE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const SHA_TWO: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    /// A destination with every optional unset, so each fixture sets only the
    /// fields it actually cares about.
    fn destination(display_name: &str) -> PromoteBuildDestination {
        PromoteBuildDestination {
            display_name: display_name.to_string(),
            backend_environment: None,
            metadata_path: None,
            metadata_object_key: None,
            distribution: None,
            game_config: None,
            steam_branches: None,
            disable_backend_deploy: None,
        }
    }

    fn timestamp() -> DateTime<Utc> {
        DateTime::<Utc>::from_timestamp(1_700_000_000, 0).expect("valid timestamp")
    }

    /// No destination may ever silently disappear, so the row count can never be
    /// lower than the destination count.
    fn assert_no_destination_dropped(
        destinations: &[PromoteBuildDestination],
        rows: &[ActiveBuild],
    ) {
        assert!(
            rows.len() >= destinations.len(),
            "expected at least one row per destination, got {} rows for {} destinations",
            rows.len(),
            destinations.len()
        );
    }

    #[test]
    fn launcher_destination_with_successful_read_is_resolved() {
        let mut dest = destination("Destination One");
        dest.metadata_object_key = Some("meta/path-one".to_string());
        let destinations = [dest];

        let mut resolved = ResolvedMetadata::new();
        resolved.insert(
            "meta/path-one".to_string(),
            Ok((SHA_ONE.to_string(), Some(timestamp()))),
        );

        let rows = build_rows(&destinations, &resolved);

        assert_no_destination_dropped(&destinations, &rows);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].display_name, "Destination One");
        assert_eq!(rows[0].status, ActiveBuildStatus::Resolved);
        assert_eq!(rows[0].sha, Some(SHA_ONE.to_string()));
        assert_eq!(rows[0].deployed_at, Some(timestamp()));
        assert!(rows[0].error.is_none());
        assert!(rows[0].steam_branch.is_none());
    }

    #[test]
    fn launcher_destination_with_failed_read_becomes_an_error_row() {
        let mut dest = destination("Destination One");
        dest.metadata_object_key = Some("meta/path-one".to_string());
        let destinations = [dest];

        let mut resolved = ResolvedMetadata::new();
        resolved.insert("meta/path-one".to_string(), Err("read failed".to_string()));

        let rows = build_rows(&destinations, &resolved);

        assert_no_destination_dropped(&destinations, &rows);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, ActiveBuildStatus::Error);
        assert_eq!(rows[0].error, Some("read failed".to_string()));
        assert!(rows[0].sha.is_none());
    }

    #[test]
    fn launcher_destination_absent_from_resolved_map_becomes_an_error_row() {
        let mut dest = destination("Destination One");
        dest.metadata_object_key = Some("meta/path-one".to_string());
        let destinations = [dest];

        // Deliberately empty: guards the branch that should be unreachable.
        let resolved = ResolvedMetadata::new();

        let rows = build_rows(&destinations, &resolved);

        assert_no_destination_dropped(&destinations, &rows);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, ActiveBuildStatus::Error);
        assert!(rows[0].error.is_some());
        assert!(rows[0].sha.is_none());
    }

    #[test]
    fn steam_destination_expands_to_one_row_per_branch() {
        let mut dest = destination("Destination One");
        dest.distribution = Some("steam".to_string());
        dest.steam_branches = Some(vec![
            "branch-one".to_string(),
            "branch-two".to_string(),
            "branch-three".to_string(),
        ]);
        let destinations = [dest];

        let rows = build_rows(&destinations, &ResolvedMetadata::new());

        assert_no_destination_dropped(&destinations, &rows);
        assert_eq!(rows.len(), 3);
        let branches: Vec<Option<String>> = rows.iter().map(|r| r.steam_branch.clone()).collect();
        assert_eq!(
            branches,
            vec![
                Some("branch-one".to_string()),
                Some("branch-two".to_string()),
                Some("branch-three".to_string()),
            ]
        );
        for row in &rows {
            assert_eq!(row.status, ActiveBuildStatus::Tbd);
            assert_eq!(row.display_name, "Destination One");
            assert!(row.sha.is_none());
        }
    }

    /// Silent-disappearance guard: an empty branch list must not collapse the
    /// destination to zero rows.
    #[test]
    fn steam_destination_with_empty_branch_list_still_produces_a_row() {
        let mut dest = destination("Destination One");
        dest.distribution = Some("steam".to_string());
        dest.steam_branches = Some(vec![]);
        let destinations = [dest];

        let rows = build_rows(&destinations, &ResolvedMetadata::new());

        assert_no_destination_dropped(&destinations, &rows);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, ActiveBuildStatus::Tbd);
        assert!(rows[0].steam_branch.is_none());
        assert!(rows[0].error.is_some());
    }

    /// Silent-disappearance guard: an absent branch list must not collapse the
    /// destination to zero rows.
    #[test]
    fn steam_destination_with_no_branches_still_produces_a_row() {
        let mut dest = destination("Destination One");
        dest.distribution = Some("steam".to_string());
        let destinations = [dest];

        let rows = build_rows(&destinations, &ResolvedMetadata::new());

        assert_no_destination_dropped(&destinations, &rows);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, ActiveBuildStatus::Tbd);
        assert!(rows[0].steam_branch.is_none());
        assert!(rows[0].error.is_some());
    }

    /// Silent-disappearance guard: a destination configured with neither a
    /// metadata path nor Steam distribution must still be listed.
    #[test]
    fn destination_with_neither_metadata_path_nor_steam_still_produces_a_row() {
        let destinations = [destination("Destination One")];

        let rows = build_rows(&destinations, &ResolvedMetadata::new());

        assert_no_destination_dropped(&destinations, &rows);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, ActiveBuildStatus::Tbd);
        assert!(rows[0].error.is_some());
        assert!(rows[0].sha.is_none());
        assert!(rows[0].steam_branch.is_none());
    }

    #[test]
    fn empty_destination_list_produces_no_rows() {
        let rows = build_rows(&[], &ResolvedMetadata::new());
        assert!(rows.is_empty());
    }

    #[test]
    fn steam_distribution_match_is_case_insensitive() {
        let mut dest = destination("Destination One");
        dest.distribution = Some("StEaM".to_string());
        dest.steam_branches = Some(vec!["branch-one".to_string()]);
        let destinations = [dest];

        let rows = build_rows(&destinations, &ResolvedMetadata::new());

        assert_no_destination_dropped(&destinations, &rows);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, ActiveBuildStatus::Tbd);
        assert_eq!(rows[0].steam_branch, Some("branch-one".to_string()));
    }

    #[test]
    fn two_destinations_sharing_a_metadata_path_both_resolve() {
        let mut first = destination("Destination One");
        first.metadata_object_key = Some("meta/path-one".to_string());
        let mut second = destination("Destination Two");
        second.metadata_object_key = Some("meta/path-one".to_string());
        let destinations = [first, second];

        let mut resolved = ResolvedMetadata::new();
        resolved.insert(
            "meta/path-one".to_string(),
            Ok((SHA_ONE.to_string(), Some(timestamp()))),
        );

        let rows = build_rows(&destinations, &resolved);

        assert_no_destination_dropped(&destinations, &rows);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].display_name, "Destination One");
        assert_eq!(rows[1].display_name, "Destination Two");
        for row in &rows {
            assert_eq!(row.status, ActiveBuildStatus::Resolved);
            assert_eq!(row.sha, Some(SHA_ONE.to_string()));
        }
    }

    #[test]
    fn one_failing_destination_does_not_affect_the_others() {
        let mut first = destination("Destination One");
        first.metadata_object_key = Some("meta/path-one".to_string());
        let mut second = destination("Destination Two");
        second.metadata_object_key = Some("meta/path-two".to_string());
        let destinations = [first, second];

        let mut resolved = ResolvedMetadata::new();
        resolved.insert("meta/path-one".to_string(), Err("read failed".to_string()));
        resolved.insert("meta/path-two".to_string(), Ok((SHA_TWO.to_string(), None)));

        let rows = build_rows(&destinations, &resolved);

        assert_no_destination_dropped(&destinations, &rows);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].status, ActiveBuildStatus::Error);
        assert_eq!(rows[1].status, ActiveBuildStatus::Resolved);
        assert_eq!(rows[1].sha, Some(SHA_TWO.to_string()));
        assert!(rows[1].deployed_at.is_none());
    }

    #[test]
    fn steam_destination_with_a_metadata_key_keeps_one_row_per_branch() {
        let mut dest = destination("Destination One");
        dest.metadata_object_key = Some("meta/path-one".to_string());
        dest.distribution = Some("steam".to_string());
        dest.steam_branches = Some(vec!["branch-one".to_string(), "branch-two".to_string()]);
        let destinations = [dest];

        let mut resolved = ResolvedMetadata::new();
        resolved.insert(
            "meta/path-one".to_string(),
            Ok((SHA_ONE.to_string(), Some(timestamp()))),
        );

        let rows = build_rows(&destinations, &resolved);

        // Branches must not collapse into a single row once Steam gains a metadata key.
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].steam_branch.as_deref(), Some("branch-one"));
        assert_eq!(rows[1].steam_branch.as_deref(), Some("branch-two"));
        for row in &rows {
            assert_eq!(row.status, ActiveBuildStatus::Resolved);
            assert_eq!(row.sha.as_deref(), Some(SHA_ONE));
        }
    }

    #[test]
    fn active_build_status_serializes_lowercase() {
        assert_eq!(
            serde_json::to_string(&ActiveBuildStatus::Resolved).unwrap(),
            "\"resolved\""
        );
        assert_eq!(
            serde_json::to_string(&ActiveBuildStatus::Tbd).unwrap(),
            "\"tbd\""
        );
        assert_eq!(
            serde_json::to_string(&ActiveBuildStatus::Error).unwrap(),
            "\"error\""
        );
    }

    #[test]
    fn active_build_serializes_camel_case() {
        let mut row = ActiveBuild::new("Destination One", ActiveBuildStatus::Resolved);
        row.sha = Some(SHA_ONE.to_string());
        row.deployed_at = Some(timestamp());

        let json = serde_json::to_string(&row).unwrap();

        assert!(json.contains("\"displayName\""));
        assert!(json.contains("\"deployedAt\""));
        // Unset optionals are omitted, not null: types.ts declares them optional.
        assert!(!json.contains("\"steamBranch\""));
        assert!(!json.contains("null"));
    }

    #[test]
    fn dynamic_config_bucket_overrides_the_client_fallback() {
        assert_eq!(
            resolve_promoted_bucket(Some("bucket-from-config"), "bucket-from-client"),
            "bucket-from-config"
        );
    }

    #[test]
    fn client_bucket_is_used_when_dynamic_config_has_none() {
        assert_eq!(
            resolve_promoted_bucket(None, "bucket-from-client"),
            "bucket-from-client"
        );
    }

    /// A stray empty string in dynamic config must not mask a working fallback.
    #[test]
    fn empty_dynamic_config_bucket_falls_back_rather_than_overriding() {
        assert_eq!(
            resolve_promoted_bucket(Some(""), "bucket-from-client"),
            "bucket-from-client"
        );
        assert_eq!(
            resolve_promoted_bucket(Some("   "), "bucket-from-client"),
            "bucket-from-client"
        );
    }

    /// Nothing configured anywhere yields an empty bucket, which the caller must
    /// report rather than hand to the SDK.
    #[test]
    fn nothing_configured_anywhere_yields_empty() {
        assert!(resolve_promoted_bucket(None, "").is_empty());
        assert!(resolve_promoted_bucket(Some(""), "").is_empty());
    }

    #[test]
    fn bucket_values_are_trimmed() {
        assert_eq!(
            resolve_promoted_bucket(Some("  spaced-bucket  "), ""),
            "spaced-bucket"
        );
    }
}
