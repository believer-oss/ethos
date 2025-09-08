use ethos_core::utils::junit::JunitOutput;
use friendshipper::engine::router::OpenUrlForPathRequest;
use tracing::error;

use ethos_core::storage::{ArtifactEntry, ArtifactList};
use ethos_core::tauri::command::{check_error, restart};
use ethos_core::tauri::error::TauriError;
use ethos_core::tauri::State;
use ethos_core::types::builds::SyncClientRequest;
use ethos_core::types::config::{AppConfig, DynamicConfig, UnrealVerSelDiagResponse};
use ethos_core::types::gameserver::{GameServerResults, LaunchRequest};
use ethos_core::types::github::merge_queue::get_merge_queue::GetMergeQueueRepositoryMergeQueue;
use ethos_core::types::github::pulls::get_pull_requests::GetPullRequestsSearchEdgesNodeOnPullRequest;
use ethos_core::types::playtests::{
    AssignUserRequest, CreatePlaytestRequest, Playtest, UnassignUserRequest, UpdatePlaytestRequest,
};
use ethos_core::types::project::ProjectConfig;
use ethos_core::types::repo::{ChangeSet, CommitFileInfo, PushRequest, RepoStatus, Snapshot};
use friendshipper::builds::router::GetWorkflowsResponse;
use friendshipper::repo::operations::{
    RestoreSnapshotRequest, SaveChangeSetRequest, SaveSnapshotRequest,
};

// Update the TauriError creation to include status_code
async fn create_tauri_error(res: reqwest::Response) -> TauriError {
    let status = res.status().as_u16();
    let message = res
        .text()
        .await
        .unwrap_or_else(|_| "Failed to read response body".to_string());
    TauriError {
        message,
        status_code: status,
    }
}

// Update the error checking function
fn is_error_status(status: reqwest::StatusCode) -> bool {
    status.is_client_error() || status.is_server_error()
}

#[tauri::command]
pub async fn get_unrealversionselector_status(
    state: tauri::State<'_, State>,
) -> Result<UnrealVerSelDiagResponse, TauriError> {
    let res = state
        .client
        .get(format!(
            "{}/system/diagnostics/unrealversionselector",
            state.server_url
        ))
        .send()
        .await?;

    if is_error_status(res.status()) {
        return Err(create_tauri_error(res).await);
    }

    Ok(res.json().await?)
}

// Config
#[tauri::command]
pub async fn get_dynamic_config(
    state: tauri::State<'_, State>,
) -> Result<DynamicConfig, TauriError> {
    let res = state
        .client
        .get(format!("{}/config/dynamic", state.server_url))
        .send()
        .await?;

    if is_error_status(res.status()) {
        return Err(create_tauri_error(res).await);
    }

    Ok(res.json().await?)
}

#[tauri::command]
pub async fn get_project_config(
    state: tauri::State<'_, State>,
) -> Result<Vec<ProjectConfig>, TauriError> {
    let res = state
        .client
        .get(format!("{}/config/projects", state.server_url))
        .send()
        .await?;

    if is_error_status(res.status()) {
        return Err(create_tauri_error(res).await);
    }

    Ok(res.json().await?)
}

// Commits
#[tauri::command]
pub async fn get_build(
    state: tauri::State<'_, State>,
    commit: String,
    project: Option<String>,
) -> Result<ArtifactEntry, TauriError> {
    let mut req = state.client.get(format!(
        "{}/builds/commit?commit={}",
        state.server_url, commit
    ));

    if let Some(project) = project {
        req = req.query(&[("project", project)]);
    }

    match req.send().await {
        Ok(res) => {
            if is_error_status(res.status()) {
                Err(create_tauri_error(res).await)
            } else {
                match res.json::<ArtifactEntry>().await {
                    Ok(res) => Ok(res),
                    Err(err) => Err(TauriError {
                        message: err.to_string(),
                        status_code: 0,
                    }),
                }
            }
        }
        Err(err) => Err(TauriError {
            message: err.to_string(),
            status_code: 0,
        }),
    }
}

#[tauri::command]
pub async fn get_builds(
    state: tauri::State<'_, State>,
    limit: Option<u32>,
    project: Option<String>,
) -> Result<ArtifactList, TauriError> {
    let mut req = state.client.get(format!("{}/builds", state.server_url));

    if let Some(limit) = limit {
        req = req.query(&[("limit", limit)]);
    }

    if let Some(project) = project {
        req = req.query(&[("project", project)]);
    }

    match req.send().await {
        Ok(res) => {
            if is_error_status(res.status()) {
                Err(create_tauri_error(res).await)
            } else {
                match res.json::<ArtifactList>().await {
                    Ok(res) => Ok(res),
                    Err(err) => Err(TauriError {
                        message: err.to_string(),
                        status_code: 0,
                    }),
                }
            }
        }
        Err(err) => Err(TauriError {
            message: err.to_string(),
            status_code: 0,
        }),
    }
}

#[tauri::command]
pub async fn show_commit_files(
    state: tauri::State<'_, State>,
    commit: String,
    stash: bool,
) -> Result<Vec<CommitFileInfo>, TauriError> {
    let res = state
        .client
        .get(format!(
            "{}/repo/show?commit={}&stash={}",
            state.server_url, commit, stash
        ))
        .send()
        .await?;

    if is_error_status(res.status()) {
        return Err(create_tauri_error(res).await);
    }

    Ok(res.json().await?)
}

#[tauri::command]
pub async fn verify_build(
    state: tauri::State<'_, State>,
    commit: String,
) -> Result<bool, TauriError> {
    let res = state
        .client
        .get(format!(
            "{}/builds/server/verify?commit={}",
            state.server_url, commit
        ))
        .send()
        .await?;

    if is_error_status(res.status()) {
        return Err(create_tauri_error(res).await);
    }

    Ok(res.json().await?)
}

#[tauri::command]
pub async fn update_app_config(
    state: tauri::State<'_, State>,
    config: AppConfig,
    token: Option<String>,
    new_project: bool,
) -> Result<String, TauriError> {
    let url = match token {
        Some(token) => format!(
            "{}/config?new_project={}&token={}",
            state.server_url, new_project, token
        ),
        None => format!("{}/config?new_project={}", state.server_url, new_project),
    };

    let res = state.client.post(url).json(&config.clone()).send().await?;

    if res.status().is_client_error() || res.status().is_server_error() {
        let status_code = res.status().as_u16();
        let body = res.text().await?;
        return Err(TauriError {
            message: body,
            status_code,
        });
    }

    Ok(res.text().await?)
}

#[tauri::command]
pub async fn sync_client(
    state: tauri::State<'_, State>,
    req: SyncClientRequest,
) -> Result<bool, TauriError> {
    let res = state
        .client
        .post(format!("{}/builds/client/sync", state.server_url))
        .json(&req)
        .send()
        .await?;

    if is_error_status(res.status()) {
        return Err(create_tauri_error(res).await);
    }

    Ok(res.json().await?)
}

#[tauri::command]
pub async fn cancel_download(state: tauri::State<'_, State>) -> Result<(), TauriError> {
    let res = state
        .client
        .post(format!("{}/builds/client/cancel", state.server_url))
        .send()
        .await?;

    if let Some(err) = check_error(res.status(), res.text().await?).await {
        error!("Error cancelling download: {}", err.message);
        return Err(err);
    }

    Ok(())
}

#[tauri::command]
pub async fn wipe_client_data(state: tauri::State<'_, State>) -> Result<(), TauriError> {
    let res = state
        .client
        .post(format!("{}/builds/client/wipe", state.server_url))
        .send()
        .await?;

    if let Some(err) = check_error(res.status(), res.text().await?).await {
        error!("Error wiping client data: {}", err.message);
        return Err(err);
    }

    Ok(())
}

#[tauri::command]
pub async fn reset_longtail(state: tauri::State<'_, State>) -> Result<(), TauriError> {
    let res = state
        .client
        .post(format!("{}/builds/longtail/reset", state.server_url))
        .send()
        .await?;

    if let Some(err) = check_error(res.status(), res.text().await?).await {
        error!("Error resetting longtail: {}", err.message);
        return Err(err);
    }

    restart(state).await?;

    Ok(())
}

#[tauri::command]
pub async fn reset_repo(state: tauri::State<'_, State>) -> Result<(), TauriError> {
    let res = state
        .client
        .post(format!("{}/repo/reset", state.server_url))
        .send()
        .await?;

    if let Some(err) = check_error(res.status(), res.text().await?).await {
        error!("Error resetting repo: {}", err.message);
        return Err(err);
    }

    Ok(())
}

#[tauri::command]
pub async fn refetch_repo(state: tauri::State<'_, State>) -> Result<(), TauriError> {
    let res = state
        .client
        .post(format!("{}/repo/refetch", state.server_url))
        .send()
        .await?;

    if let Some(err) = check_error(res.status(), res.text().await?).await {
        error!("Error refetching repo: {}", err.message);
        return Err(err);
    }

    Ok(())
}

#[tauri::command]
pub async fn reset_repo_to_commit(
    state: tauri::State<'_, State>,
    commit: String,
) -> Result<(), TauriError> {
    let res = state
        .client
        .post(format!("{}/repo/reset/{}", state.server_url, commit))
        .send()
        .await?;

    if let Some(err) = check_error(res.status(), res.text().await?).await {
        error!("Error resetting repo to commit {}: {}", commit, err.message);
        return Err(err);
    }

    Ok(())
}

// Argo
#[tauri::command]
pub async fn get_workflows(
    state: tauri::State<'_, State>,
    engine: bool,
) -> Result<GetWorkflowsResponse, TauriError> {
    let res = state
        .client
        .get(format!(
            "{}/builds/workflows?engine={}",
            state.server_url, engine
        ))
        .send()
        .await?;

    if is_error_status(res.status()) {
        return Err(create_tauri_error(res).await);
    }

    Ok(res.json().await?)
}

#[tauri::command]
pub async fn get_workflow_node_logs(
    state: tauri::State<'_, State>,
    uid: String,
    node_id: String,
) -> Result<String, TauriError> {
    let res = state
        .client
        .get(format!(
            "{}/builds/workflows/logs?uid={}&nodeId={}",
            state.server_url, uid, node_id
        ))
        .send()
        .await?;

    if is_error_status(res.status()) {
        return Err(create_tauri_error(res).await);
    }

    Ok(res.text().await?)
}

#[tauri::command]
pub async fn get_workflow_junit_artifact(
    state: tauri::State<'_, State>,
    uid: String,
    node_id: String,
) -> Result<Option<JunitOutput>, TauriError> {
    let res = state
        .client
        .get(format!(
            "{}/builds/workflows/junit?uid={}&nodeId={}",
            state.server_url, uid, node_id
        ))
        .send()
        .await?;

    if is_error_status(res.status()) {
        return Err(create_tauri_error(res).await);
    }

    Ok(res.json().await?)
}

#[tauri::command]
pub async fn stop_workflow(
    state: tauri::State<'_, State>,
    workflow: String,
) -> Result<String, TauriError> {
    let res = state
        .client
        .post(format!(
            "{}/builds/workflows/stop?workflow={}",
            state.server_url, workflow
        ))
        .send()
        .await?;

    if is_error_status(res.status()) {
        return Err(create_tauri_error(res).await);
    }

    Ok(res.text().await?)
}

#[tauri::command]
pub async fn create_promote_build_workflow(
    state: tauri::State<'_, State>,
    request: ethos_core::types::argo::workflow::CreatePromoteBuildWorkflowRequest,
) -> Result<ethos_core::types::argo::workflow::Workflow, TauriError> {
    let res = state
        .client
        .post(format!(
            "{}/builds/workflows/promote-build",
            state.server_url
        ))
        .json(&request)
        .send()
        .await?;

    if is_error_status(res.status()) {
        return Err(create_tauri_error(res).await);
    }

    Ok(res.json().await?)
}

// Repo
#[tauri::command]
pub async fn get_repo_status(
    state: tauri::State<'_, State>,
    skip_dll_check: bool,
    allow_offline_communication: bool,
) -> Result<RepoStatus, TauriError> {
    let res = state
        .client
        .get(format!(
            "{}/repo/status?&skipDllCheck={}&allowOfflineCommunication={}",
            state.server_url, skip_dll_check, allow_offline_communication
        ))
        .send()
        .await?;

    if is_error_status(res.status()) {
        return Err(create_tauri_error(res).await);
    }
    Ok(res.json().await?)
}

#[tauri::command]
pub async fn list_snapshots(state: tauri::State<'_, State>) -> Result<Vec<Snapshot>, TauriError> {
    let res = state
        .client
        .get(format!("{}/repo/snapshots", state.server_url))
        .send()
        .await?;

    if is_error_status(res.status()) {
        return Err(create_tauri_error(res).await);
    }

    Ok(res.json().await?)
}

#[tauri::command]
pub async fn restore_snapshot(
    state: tauri::State<'_, State>,
    commit: String,
) -> Result<(), TauriError> {
    let res = state
        .client
        .post(format!("{}/repo/snapshots/restore", state.server_url))
        .json(&RestoreSnapshotRequest { commit })
        .send()
        .await?;

    if let Some(err) = check_error(res.status(), res.text().await?).await {
        return Err(err);
    }

    Ok(())
}

#[tauri::command]
pub async fn save_snapshot(
    state: tauri::State<'_, State>,
    message: String,
    files: Vec<String>,
) -> Result<(), TauriError> {
    let res = state
        .client
        .post(format!("{}/repo/snapshots/save", state.server_url))
        .json(&SaveSnapshotRequest { message, files })
        .send()
        .await?;

    if let Some(err) = check_error(res.status(), res.text().await?).await {
        return Err(err);
    }
    Ok(())
}

#[tauri::command]
pub async fn delete_snapshot(
    state: tauri::State<'_, State>,
    commit: String,
) -> Result<(), TauriError> {
    let res = state
        .client
        .delete(format!(
            "{}/repo/snapshots?commit={}",
            state.server_url, commit
        ))
        .send()
        .await?;

    if let Some(err) = check_error(res.status(), res.text().await?).await {
        return Err(err);
    }
    Ok(())
}

#[tauri::command]
pub async fn save_changeset(
    state: tauri::State<'_, State>,
    change_sets: Vec<ChangeSet>,
) -> Result<(), TauriError> {
    let res = state
        .client
        .post(format!("{}/repo/changeset/save", state.server_url))
        .json(&SaveChangeSetRequest { change_sets })
        .send()
        .await?;

    if let Some(err) = check_error(res.status(), res.text().await?).await {
        return Err(err);
    }

    Ok(())
}

#[tauri::command]
pub async fn load_changeset(state: tauri::State<'_, State>) -> Result<Vec<ChangeSet>, TauriError> {
    let res = state
        .client
        .get(format!("{}/repo/changeset/load", state.server_url))
        .send()
        .await?;

    if is_error_status(res.status()) {
        return Err(create_tauri_error(res).await);
    }

    let body = res.text().await?;
    let json = serde_json::from_str(&body).map_err(|e| TauriError {
        message: e.to_string(),
        status_code: 0,
    })?;

    Ok(json)
}

#[tauri::command]
pub async fn quick_submit(
    state: tauri::State<'_, State>,
    req: PushRequest,
) -> Result<(), TauriError> {
    let res = state
        .client
        .post(format!("{}/repo/gh/submit", state.server_url))
        .json(&req)
        .send()
        .await?;

    if let Some(err) = check_error(res.status(), res.text().await?).await {
        return Err(err);
    }

    Ok(())
}

// GitHub
#[tauri::command]
pub async fn get_pull_request(
    state: tauri::State<'_, State>,
    id: u64,
) -> Result<serde_json::Value, TauriError> {
    let res = state
        .client
        .get(format!("{}/repo/gh/pulls/{}", state.server_url, id))
        .send()
        .await?;

    if is_error_status(res.status()) {
        return Err(create_tauri_error(res).await);
    }

    let body = res.text().await?;
    let json = serde_json::from_str(&body).map_err(|e| TauriError {
        message: e.to_string(),
        status_code: 0,
    })?;

    Ok(json)
}

#[tauri::command]
pub async fn get_pull_requests(
    state: tauri::State<'_, State>,
    limit: i64,
) -> Result<Vec<GetPullRequestsSearchEdgesNodeOnPullRequest>, TauriError> {
    let res = state
        .client
        .get(format!(
            "{}/repo/gh/pulls?limit={}",
            state.server_url, limit
        ))
        .send()
        .await?;

    if is_error_status(res.status()) {
        return Err(create_tauri_error(res).await);
    }

    Ok(res.json().await?)
}

#[tauri::command]
pub async fn get_merge_queue(
    state: tauri::State<'_, State>,
) -> Result<GetMergeQueueRepositoryMergeQueue, TauriError> {
    let res = state
        .client
        .get(format!("{}/repo/gh/queue", state.server_url))
        .send()
        .await?;

    if is_error_status(res.status()) {
        return Err(create_tauri_error(res).await);
    }

    Ok(res.json().await?)
}

// Servers
#[tauri::command]
pub async fn get_servers(
    state: tauri::State<'_, State>,
    commit: Option<String>,
) -> Result<Vec<GameServerResults>, TauriError> {
    let mut req = state.client.get(format!("{}/servers", state.server_url));

    if let Some(commit) = commit {
        req = req.query(&[("commit", commit)]);
    }

    match req.send().await {
        Ok(res) => {
            if is_error_status(res.status()) {
                Err(create_tauri_error(res).await)
            } else {
                match res.json::<Vec<GameServerResults>>().await {
                    Ok(res) => Ok(res),
                    Err(err) => Err(TauriError {
                        message: err.to_string(),
                        status_code: 0,
                    }),
                }
            }
        }
        Err(err) => Err(TauriError {
            message: err.to_string(),
            status_code: 0,
        }),
    }
}

#[tauri::command]
pub async fn get_server(
    state: tauri::State<'_, State>,
    name: &str,
) -> Result<GameServerResults, TauriError> {
    let res = state
        .client
        .get(format!("{}/servers/{}", state.server_url, name))
        .send()
        .await?;

    if is_error_status(res.status()) {
        return Err(create_tauri_error(res).await);
    }

    Ok(res.json().await?)
}

#[tauri::command]
pub async fn launch_server(
    state: tauri::State<'_, State>,
    req: LaunchRequest,
) -> Result<(), TauriError> {
    let res = state
        .client
        .post(format!("{}/servers", state.server_url))
        .json(&req)
        .send()
        .await?;

    if is_error_status(res.status()) {
        return Err(create_tauri_error(res).await);
    }

    Ok(())
}

#[tauri::command]
pub async fn terminate_server(
    state: tauri::State<'_, State>,
    name: String,
) -> Result<(), TauriError> {
    let res = state
        .client
        .delete(format!("{}/servers/{}", state.server_url, name))
        .send()
        .await?;

    if let Some(err) = check_error(res.status(), res.text().await?).await {
        return Err(err);
    }

    Ok(())
}

#[tauri::command]
pub async fn download_server_logs(
    state: tauri::State<'_, State>,
    name: String,
) -> Result<(), TauriError> {
    let res = state
        .client
        .post(format!("{}/servers/{}/logs", state.server_url, name))
        .send()
        .await?;

    if let Some(err) = check_error(res.status(), res.text().await?).await {
        return Err(err);
    }

    Ok(())
}

#[tauri::command]
pub async fn copy_profile_data_from_gameserver(
    state: tauri::State<'_, State>,
    name: String,
) -> Result<(), TauriError> {
    let res = state
        .client
        .post(format!("{}/servers/{}/profile", state.server_url, name))
        .send()
        .await?;

    if let Some(err) = check_error(res.status(), res.text().await?).await {
        return Err(err);
    }

    Ok(())
}

#[tauri::command]
pub async fn open_logs_folder(state: tauri::State<'_, State>) -> Result<(), TauriError> {
    let res = state
        .client
        .post(format!("{}/servers/open-logs", state.server_url))
        .send()
        .await?;

    if let Some(err) = check_error(res.status(), res.text().await?).await {
        return Err(err);
    }

    Ok(())
}

#[tauri::command]
pub async fn start_gameserver_log_tail(
    state: tauri::State<'_, State>,
    name: String,
) -> Result<(), TauriError> {
    state
        .client
        .post(format!("{}/servers/{}/logs/tail", state.server_url, name))
        .send()
        .await?;
    Ok(())
}

#[tauri::command]
pub async fn stop_gameserver_log_tail(state: tauri::State<'_, State>) -> Result<(), TauriError> {
    state
        .client
        .post(format!("{}/servers/logs/stop", state.server_url))
        .send()
        .await?;
    Ok(())
}

// Playtests
#[tauri::command]
pub async fn get_playtests(state: tauri::State<'_, State>) -> Result<Vec<Playtest>, TauriError> {
    let response = state
        .client
        .get(format!("{}/playtests", state.server_url))
        .send()
        .await?;

    let playtests: Vec<Playtest> = response.json().await?;
    Ok(playtests)
}

#[tauri::command]
pub async fn assign_user_to_group(
    state: tauri::State<'_, State>,
    req: AssignUserRequest,
) -> Result<(), TauriError> {
    let res = state
        .client
        .post(format!("{}/playtests/assign", state.server_url))
        .json(&req)
        .send()
        .await?;

    if let Some(err) = check_error(res.status(), res.text().await?).await {
        return Err(err);
    }

    Ok(())
}

#[tauri::command]
pub async fn unassign_user_from_playtest(
    state: tauri::State<'_, State>,
    req: UnassignUserRequest,
) -> Result<(), TauriError> {
    state
        .client
        .post(format!("{}/playtests/unassign", state.server_url))
        .json(&req)
        .send()
        .await?;
    Ok(())
}

#[tauri::command]
pub async fn create_playtest(
    state: tauri::State<'_, State>,
    req: CreatePlaytestRequest,
) -> Result<(), TauriError> {
    state
        .client
        .post(format!("{}/playtests", state.server_url))
        .json(&req)
        .send()
        .await?;
    Ok(())
}

#[tauri::command]
pub async fn update_playtest(
    state: tauri::State<'_, State>,
    playtest: String,
    req: UpdatePlaytestRequest,
) -> Result<(), TauriError> {
    state
        .client
        .put(format!("{}/playtests/{}", state.server_url, playtest))
        .json(&req)
        .send()
        .await?;
    Ok(())
}

#[tauri::command]
pub async fn delete_playtest(
    state: tauri::State<'_, State>,
    playtest: String,
) -> Result<(), TauriError> {
    state
        .client
        .delete(format!("{}/playtests/{}", state.server_url, playtest))
        .send()
        .await?;
    Ok(())
}

// Project
#[tauri::command]
pub async fn open_project(state: tauri::State<'_, State>) -> Result<(), TauriError> {
    state
        .client
        .post(format!("{}/project/open-project", state.server_url))
        .send()
        .await?;
    Ok(())
}

#[tauri::command]
pub async fn force_download_dlls(state: tauri::State<'_, State>) -> Result<(), TauriError> {
    let res = state
        .client
        .post(format!("{}/repo/download-dlls", state.server_url))
        .send()
        .await?;

    if is_error_status(res.status()) {
        return Err(create_tauri_error(res).await);
    }

    Ok(())
}

#[tauri::command]
pub async fn force_download_engine(state: tauri::State<'_, State>) -> Result<(), TauriError> {
    let res = state
        .client
        .post(format!("{}/repo/download-engine", state.server_url))
        .send()
        .await?;

    if is_error_status(res.status()) {
        return Err(create_tauri_error(res).await);
    }

    Ok(())
}

#[tauri::command]
pub async fn reset_engine(state: tauri::State<'_, State>) -> Result<(), TauriError> {
    let res = state
        .client
        .post(format!("{}/repo/reset-engine", state.server_url))
        .send()
        .await?;

    if let Some(err) = check_error(res.status(), res.text().await?).await {
        error!("Error reset engine: {}", err.message);
        return Err(err);
    }

    Ok(())
}

#[tauri::command]
pub async fn reinstall_git_hooks(state: tauri::State<'_, State>) -> Result<(), TauriError> {
    let res = state
        .client
        .post(format!(
            "{}/project/install-git-hooks?refresh=true",
            state.server_url
        ))
        .send()
        .await?;

    if is_error_status(res.status()) {
        return Err(create_tauri_error(res).await);
    }

    Ok(())
}

#[tauri::command]
pub async fn sync_engine_commit_with_uproject(
    state: tauri::State<'_, State>,
) -> Result<String, TauriError> {
    let res = state
        .client
        .post(format!(
            "{}/project/sync-engine-commit-with-uproject",
            state.server_url
        ))
        .send()
        .await?;

    if is_error_status(res.status()) {
        return Err(create_tauri_error(res).await);
    }

    let response_text = res.text().await?;
    Ok(response_text)
}

#[tauri::command]
pub async fn sync_uproject_commit_with_engine(
    state: tauri::State<'_, State>,
) -> Result<String, TauriError> {
    let res = state
        .client
        .post(format!(
            "{}/project/sync-uproject-commit-with-engine",
            state.server_url
        ))
        .send()
        .await?;

    if is_error_status(res.status()) {
        return Err(create_tauri_error(res).await);
    }

    let response_text = res.text().await?;
    Ok(response_text)
}

#[tauri::command]
pub async fn check_engine_ready(state: tauri::State<'_, State>) -> Result<bool, TauriError> {
    let res = state
        .client
        .post(format!("{}/engine/check-engine-ready", state.server_url))
        .send()
        .await?;

    if is_error_status(res.status()) {
        return Err(create_tauri_error(res).await);
    }

    let response_text = res.text().await?;
    Ok(response_text == "true")
}

async fn generate_and_open_sln(
    url: String,
    open: bool,
    generate: bool,
    client: reqwest::Client,
) -> Result<(), TauriError> {
    let endpoint: String = format!("{url}/project/sln?open={open}&generate={generate}");
    let res = client.post(endpoint).send().await?;

    if is_error_status(res.status()) {
        return Err(create_tauri_error(res).await);
    }

    Ok(())
}

pub async fn tray_open_sln(url: String, client: reqwest::Client) -> Result<(), TauriError> {
    let open = true;
    let generate = false;
    generate_and_open_sln(url, open, generate, client).await
}

pub async fn tray_generate_and_open_sln(
    url: String,
    client: reqwest::Client,
) -> Result<(), TauriError> {
    let open = true;
    let generate = true;
    generate_and_open_sln(url, open, generate, client).await
}

#[tauri::command]
pub async fn generate_sln(state: tauri::State<'_, State>) -> Result<(), TauriError> {
    let open = false;
    let generate = true;
    generate_and_open_sln(
        state.server_url.clone(),
        open,
        generate,
        state.client.clone(),
    )
    .await
}

#[tauri::command]
pub async fn open_sln(state: tauri::State<'_, State>) -> Result<(), TauriError> {
    let open = true;
    let generate = false;
    generate_and_open_sln(
        state.server_url.clone(),
        open,
        generate,
        state.client.clone(),
    )
    .await
}

// engine
#[tauri::command]
pub async fn open_url_for_path(
    state: tauri::State<'_, State>,
    path: String,
) -> Result<(), TauriError> {
    let res = state
        .client
        .post(format!("{}/engine/open-url", state.server_url))
        .json(&OpenUrlForPathRequest { path })
        .send()
        .await?;

    if is_error_status(res.status()) {
        return Err(create_tauri_error(res).await);
    }

    Ok(())
}

// logout
#[tauri::command]
pub async fn logout(state: tauri::State<'_, State>) -> Result<(), TauriError> {
    let res = state
        .client
        .post(format!("{}/auth/logout", state.server_url))
        .send()
        .await?;

    if is_error_status(res.status()) {
        return Err(create_tauri_error(res).await);
    }

    Ok(())
}
