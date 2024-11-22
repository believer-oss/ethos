use birdie::metadata::{
    DirectoryClass, DirectoryMetadata, UpdateMetadataClassRequest, UpdateMetadataRequest,
};
use birdie::repo::{DeleteFetchIncludeRequest, DownloadFilesRequest, File, SingleFileRequest};
use birdie::types::config::BirdieConfig;
use std::path::PathBuf;
use tracing::error;

use ethos_core::tauri::command::check_error;
use ethos_core::tauri::error::TauriError;
use ethos_core::types::commits::Commit;
use ethos_core::types::locks::VerifyLocksResponse;
use ethos_core::types::repo::{CommitFileInfo, LockRequest, PushRequest, RepoStatus};

use crate::TauriState;

// Add this function at the top of the file, after imports
async fn create_tauri_error(res: reqwest::Response) -> TauriError {
    let status = res.status();
    let body = res.text().await.unwrap_or_default();
    TauriError {
        message: body,
        status_code: status.as_u16(),
    }
}

#[tauri::command]
pub async fn get_config(state: tauri::State<'_, TauriState>) -> Result<BirdieConfig, TauriError> {
    let res = state
        .client
        .get(format!("{}/config", state.server_url))
        .send()
        .await?;

    if res.status().is_client_error() {
        return Err(create_tauri_error(res).await);
    }

    Ok(res.json().await?)
}

#[tauri::command]
pub async fn update_config(
    state: tauri::State<'_, TauriState>,
    config: BirdieConfig,
) -> Result<BirdieConfig, TauriError> {
    let res = state
        .client
        .post(format!("{}/config", state.server_url))
        .json(&config)
        .send()
        .await?;

    if res.status().is_client_error() {
        return Err(create_tauri_error(res).await);
    }

    Ok(res.json().await?)
}

#[tauri::command]
pub async fn show_commit_files(
    state: tauri::State<'_, TauriState>,
    commit: String,
) -> Result<Vec<CommitFileInfo>, TauriError> {
    let res = state
        .client
        .get(format!("{}/repo/show?commit={}", state.server_url, commit))
        .send()
        .await?;

    if res.status().is_client_error() {
        return Err(create_tauri_error(res).await);
    }

    Ok(res.json().await?)
}

#[tauri::command]
pub async fn get_file_history(
    state: tauri::State<'_, TauriState>,
    file: String,
) -> Result<Vec<Commit>, TauriError> {
    // url encode the file
    let file = urlencoding::encode(&file);

    let res = state
        .client
        .get(format!(
            "{}/repo/files/history?file={}",
            state.server_url, file
        ))
        .send()
        .await?;

    if res.status().is_client_error() {
        return Err(create_tauri_error(res).await);
    }

    Ok(res.json().await?)
}

// Repo
#[tauri::command]
pub async fn get_repo_status(
    state: tauri::State<'_, TauriState>,
) -> Result<RepoStatus, TauriError> {
    let res = state
        .client
        .get(format!("{}/repo/status", state.server_url))
        .send()
        .await?;

    if res.status().is_client_error() {
        return Err(create_tauri_error(res).await);
    }
    Ok(res.json().await?)
}

#[tauri::command]
pub async fn submit(
    state: tauri::State<'_, TauriState>,
    req: PushRequest,
) -> Result<(), TauriError> {
    let res = state
        .client
        .post(format!("{}/repo/push", state.server_url))
        .json(&req)
        .send()
        .await?;

    let status = res.status();
    let body = res.text().await?;

    if let Some(err) = check_error(status, body.clone()).await {
        return Err(err);
    }

    Ok(())
}
// LFS
#[tauri::command]
pub async fn download_lfs_files(
    state: tauri::State<'_, TauriState>,
    files: Vec<String>,
    include_wip: bool,
) -> Result<(), TauriError> {
    let res = state
        .client
        .post(format!("{}/repo/lfs/download", state.server_url))
        .json(&DownloadFilesRequest { files, include_wip })
        .send()
        .await?;

    if let Some(err) = check_error(res.status(), res.text().await?).await {
        return Err(err);
    }

    Ok(())
}

#[tauri::command]
pub async fn lock_files(
    state: tauri::State<'_, TauriState>,
    paths: Vec<String>,
) -> Result<(), TauriError> {
    let res = state
        .client
        .post(format!("{}/repo/lfs/locks/lock", state.server_url))
        .json(&LockRequest {
            paths,
            force: false,
        })
        .send()
        .await?;

    if let Some(err) = check_error(res.status(), res.text().await?).await {
        return Err(err);
    }

    Ok(())
}

#[tauri::command]
pub async fn unlock_files(
    state: tauri::State<'_, TauriState>,
    paths: Vec<String>,
    force: bool,
) -> Result<(), TauriError> {
    let res = state
        .client
        .post(format!(
            "{}/repo/lfs/locks/unlock?force={}",
            state.server_url, force
        ))
        .json(&LockRequest { paths, force })
        .send()
        .await?;

    if let Some(err) = check_error(res.status(), res.text().await?).await {
        return Err(err);
    }

    Ok(())
}

// Locks
#[tauri::command]
pub async fn verify_locks(
    state: tauri::State<'_, TauriState>,
) -> Result<VerifyLocksResponse, TauriError> {
    let res = state
        .client
        .get(format!("{}/repo/lfs/locks/verify", state.server_url))
        .send()
        .await?;

    if res.status().is_client_error() {
        return Err(create_tauri_error(res).await);
    }

    Ok(res.json().await?)
}

// Git Config
#[tauri::command]
pub async fn get_fetch_include(
    state: tauri::State<'_, TauriState>,
) -> Result<Vec<String>, TauriError> {
    let res = state
        .client
        .get(format!("{}/repo/config/fetchinclude", state.server_url))
        .send()
        .await?;

    if res.status().is_client_error() {
        return Err(create_tauri_error(res).await);
    }

    Ok(res.json().await?)
}

#[tauri::command]
pub async fn del_fetch_include(
    state: tauri::State<'_, TauriState>,
    files: Vec<String>,
) -> Result<(), TauriError> {
    let res = state
        .client
        .delete(format!("{}/repo/config/fetchinclude", state.server_url))
        .json(&DeleteFetchIncludeRequest { files })
        .send()
        .await?;

    if let Some(err) = check_error(res.status(), res.text().await?).await {
        return Err(err);
    }

    Ok(())
}

// Birdie commands
#[tauri::command]
pub async fn get_file(
    state: tauri::State<'_, TauriState>,
    path: String,
) -> Result<File, TauriError> {
    let res = state
        .client
        .get(format!("{}/repo/file", state.server_url))
        .json(&SingleFileRequest { path })
        .send()
        .await?;

    if res.status().is_client_error() {
        return Err(create_tauri_error(res).await);
    }

    Ok(res.json().await?)
}

#[tauri::command]
pub async fn get_files(
    state: tauri::State<'_, TauriState>,
    root: Option<String>,
) -> Result<Vec<File>, TauriError> {
    let mut req = state.client.get(format!("{}/repo/files", state.server_url));

    if let Some(root) = root {
        req = req.query(&[("root", root)]);
    }

    let res = req.send().await?;

    if res.status().is_client_error() {
        return Err(create_tauri_error(res).await);
    }

    Ok(res.json().await?)
}

#[tauri::command]
pub async fn get_all_files(state: tauri::State<'_, TauriState>) -> Result<Vec<String>, TauriError> {
    let res = state
        .client
        .get(format!("{}/repo/files/all", state.server_url))
        .send()
        .await?;

    if res.status().is_client_error() {
        return Err(create_tauri_error(res).await);
    }

    Ok(res.json().await?)
}

// Birdie metadata
#[tauri::command]
pub async fn get_directory_metadata(
    state: tauri::State<'_, TauriState>,
    path: PathBuf,
) -> Result<DirectoryMetadata, TauriError> {
    let res = state
        .client
        .get(format!(
            "{}/metadata?path={}",
            state.server_url,
            path.to_str().unwrap()
        ))
        .send()
        .await?;

    if res.status().is_client_error() {
        return Err(create_tauri_error(res).await);
    }

    Ok(res.json().await?)
}
#[tauri::command]
pub async fn update_metadata_class(
    state: tauri::State<'_, TauriState>,
    path: PathBuf,
    directory_class: String,
) -> Result<(), TauriError> {
    let res = state
        .client
        .post(format!("{}/metadata/class", state.server_url))
        .json(&UpdateMetadataClassRequest {
            path,
            directory_class: DirectoryClass::from_str(&directory_class),
        })
        .send()
        .await?;

    if let Some(err) = check_error(res.status(), res.text().await?).await {
        return Err(err);
    }

    Ok(())
}

#[tauri::command]
pub async fn update_metadata(
    state: tauri::State<'_, TauriState>,
    path: PathBuf,
    metadata: DirectoryMetadata,
) -> Result<DirectoryMetadata, TauriError> {
    let res = state
        .client
        .post(format!("{}/metadata", state.server_url))
        .json(&UpdateMetadataRequest { path, metadata })
        .send()
        .await?;

    if res.status().is_client_error() {
        return Err(create_tauri_error(res).await);
    }

    Ok(res.json().await?)
}

#[tauri::command]
pub async fn sync_tools(state: tauri::State<'_, TauriState>) -> Result<bool, TauriError> {
    let res = state
        .client
        .post(format!("{}/tools/sync", state.server_url))
        .send()
        .await?;

    if res.status().is_client_error() {
        return Err(create_tauri_error(res).await);
    }

    let res_text = res.text().await;

    let did_sync: bool = matches!(res_text.unwrap().as_str(), "OK");
    Ok(did_sync)
}

#[tauri::command]
pub async fn run_set_env(state: tauri::State<'_, TauriState>) -> Result<(), TauriError> {
    let res = state
        .client
        .post(format!("{}/tools/setenv", state.server_url))
        .send()
        .await?;

    if res.status().is_client_error() {
        return Err(create_tauri_error(res).await);
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
