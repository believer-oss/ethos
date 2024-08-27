use std::path::PathBuf;

use tracing::info;

use birdie::metadata::{
    DirectoryClass, DirectoryMetadata, UpdateMetadataClassRequest, UpdateMetadataRequest,
};
use birdie::repo::{DeleteFetchIncludeRequest, DownloadFilesRequest, File};
use ethos_core::tauri::command::check_client_error;
use ethos_core::tauri::error::TauriError;
use ethos_core::types::commits::Commit;
use ethos_core::types::locks::VerifyLocksResponse;
use ethos_core::types::repo::{CommitFileInfo, LockRequest, PushRequest, RepoStatus};

use crate::State;

#[tauri::command]
pub async fn show_commit_files(
    state: tauri::State<'_, State>,
    commit: String,
) -> Result<Vec<CommitFileInfo>, TauriError> {
    let res = state
        .client
        .get(format!("{}/repo/show?commit={}", state.server_url, commit))
        .send()
        .await?;

    if res.status().is_client_error() {
        let body = res.text().await?;
        return Err(TauriError { message: body });
    }

    Ok(res.json().await?)
}

#[tauri::command]
pub async fn get_file_history(
    state: tauri::State<'_, State>,
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
        let body = res.text().await?;
        return Err(TauriError { message: body });
    }

    Ok(res.json().await?)
}

// Repo
#[tauri::command]
pub async fn get_repo_status(state: tauri::State<'_, State>) -> Result<RepoStatus, TauriError> {
    let res = state
        .client
        .get(format!("{}/repo/status", state.server_url))
        .send()
        .await?;

    if res.status().is_client_error() {
        let body = res.text().await?;
        return Err(TauriError { message: body });
    }
    Ok(res.json().await?)
}

#[tauri::command]
pub async fn submit(state: tauri::State<'_, State>, req: PushRequest) -> Result<(), TauriError> {
    let res = state
        .client
        .post(format!("{}/repo/push", state.server_url))
        .json(&req)
        .send()
        .await?;

    if let Some(err) = check_client_error(res).await {
        return Err(err);
    }
    Ok(())
}
// LFS
#[tauri::command]
pub async fn download_lfs_files(
    state: tauri::State<'_, State>,
    files: Vec<String>,
) -> Result<(), TauriError> {
    let req = DownloadFilesRequest { files };
    let res = state
        .client
        .post(format!("{}/repo/lfs/download", state.server_url))
        .json(&req)
        .send()
        .await?;

    if let Some(err) = check_client_error(res).await {
        return Err(err);
    }

    Ok(())
}

#[tauri::command]
pub async fn lock_files(
    state: tauri::State<'_, State>,
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

    if let Some(err) = check_client_error(res).await {
        return Err(err);
    }

    Ok(())
}

#[tauri::command]
pub async fn unlock_files(
    state: tauri::State<'_, State>,
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

    if let Some(err) = check_client_error(res).await {
        return Err(err);
    }

    Ok(())
}

// Locks
#[tauri::command]
pub async fn verify_locks(
    state: tauri::State<'_, State>,
) -> Result<VerifyLocksResponse, TauriError> {
    let res = state
        .client
        .get(format!("{}/repo/lfs/locks/verify", state.server_url))
        .send()
        .await?;

    if res.status().is_client_error() {
        let body = res.text().await?;
        return Err(TauriError { message: body });
    }

    Ok(res.json().await?)
}

// Git Config
#[tauri::command]
pub async fn get_fetch_include(state: tauri::State<'_, State>) -> Result<Vec<String>, TauriError> {
    let res = state
        .client
        .get(format!("{}/repo/config/fetchinclude", state.server_url))
        .send()
        .await?;

    if res.status().is_client_error() {
        let body = res.text().await?;
        return Err(TauriError { message: body });
    }

    Ok(res.json().await?)
}

#[tauri::command]
pub async fn del_fetch_include(
    state: tauri::State<'_, State>,
    files: Vec<String>
) -> Result<(), TauriError> {
    let res = state
        .client
        .delete(format!("{}/repo/config/fetchinclude", state.server_url))
        .json(&DeleteFetchIncludeRequest { files })
        .send()
        .await?;

    if let Some(err) = check_client_error(res).await {
        return Err(err);
    }

    Ok(())
}

// Birdie commands
#[tauri::command]
pub async fn get_files(
    state: tauri::State<'_, State>,
    root: Option<String>,
) -> Result<Vec<File>, TauriError> {
    let mut req = state.client.get(format!("{}/repo/files", state.server_url));

    if let Some(root) = root {
        req = req.query(&[("root", root)]);
    }

    let res = req.send().await?;

    if res.status().is_client_error() {
        let body = res.text().await?;
        info!("get_files error: {}", body);
        return Err(TauriError { message: body });
    }

    Ok(res.json().await?)
}

#[tauri::command]
pub async fn get_all_files(state: tauri::State<'_, State>) -> Result<Vec<String>, TauriError> {
    let res = state
        .client
        .get(format!("{}/repo/files/all", state.server_url))
        .send()
        .await?;

    if res.status().is_client_error() {
        let body = res.text().await?;
        return Err(TauriError { message: body });
    }

    Ok(res.json().await?)
}

// Birdie metadata
#[tauri::command]
pub async fn get_directory_metadata(
    state: tauri::State<'_, State>,
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
        let body = res.text().await?;
        return Err(TauriError { message: body });
    }

    Ok(res.json().await?)
}
#[tauri::command]
pub async fn update_metadata_class(
    state: tauri::State<'_, State>,
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

    if let Some(err) = check_client_error(res).await {
        return Err(err);
    }

    Ok(())
}

#[tauri::command]
pub async fn update_metadata(
    state: tauri::State<'_, State>,
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
        let body = res.text().await?;
        return Err(TauriError { message: body });
    }

    Ok(res.json().await?)
}

#[tauri::command]
pub async fn sync_tools(state: tauri::State<'_, State>) -> Result<bool, TauriError> {
    let res = state
        .client
        .post(format!("{}/tools/sync", state.server_url))
        .send()
        .await?;

    if res.status().is_client_error() {
        let body = res.text().await?;
        return Err(TauriError { message: body });
    }

    let res_text = res.text().await;

    let did_sync: bool = matches!(res_text.unwrap().as_str(), "OK");
    Ok(did_sync)
}

#[tauri::command]
pub async fn run_set_env(state: tauri::State<'_, State>) -> Result<(), TauriError> {
    let res = state
        .client
        .post(format!("{}/tools/setenv", state.server_url))
        .send()
        .await?;

    if res.status().is_client_error() {
        let body = res.text().await?;
        return Err(TauriError { message: body });
    }

    Ok(())
}
