use crate::tauri::error::TauriError;
use crate::tauri::State;
use crate::types::commits::Commit;
use crate::types::config::{AppConfig, RepoConfig};
use crate::types::logs::LogEntry;
use crate::types::repo::{
    CloneRequest, ConfigureUserRequest, LockRequest, PullResponse, RebaseStatusResponse,
    RevertFilesRequest,
};

use tauri::process::current_binary;
use tauri::Env;
use tracing::{error, info, Instrument};

pub async fn check_error(code: reqwest::StatusCode, body: String) -> Option<TauriError> {
    if code.is_client_error() || code.is_server_error() {
        Some(TauriError {
            message: body,
            status_code: code.as_u16(),
        })
    } else {
        None
    }
}

// System

#[tauri::command]
pub async fn get_system_status(state: tauri::State<'_, State>) -> Result<bool, TauriError> {
    let res = state
        .client
        .get(format!("{}/system/status", state.server_url.clone()))
        .timeout(std::time::Duration::from_secs(3))
        .send()
        .await;

    match res {
        Ok(_) => Ok(true),
        Err(e) => Err(TauriError {
            message: e.to_string(),
            status_code: 500, // Internal Server Error as a default
        }),
    }
}

#[tauri::command]
pub async fn get_log_path(state: tauri::State<'_, State>) -> Result<String, TauriError> {
    Ok(state.log_path.clone().to_str().unwrap().to_string())
}

#[tauri::command]
pub async fn shutdown_server(state: tauri::State<'_, State>) -> Result<(), TauriError> {
    while state.shutdown_tx.send(()).await.is_ok() {
        // keep sending until the channel is closed
        info!("Sent shutdown signal");

        // wait a second
        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
    }

    Ok(())
}

#[tauri::command]
pub async fn restart(state: tauri::State<'_, State>) -> Result<(), TauriError> {
    use std::process::Command;

    let span = tracing::info_span!("shutting_down");

    async move {
        while state.shutdown_tx.send(()).await.is_ok() {
            // keep sending until the channel is closed
            info!("Sent shutdown signal");

            // wait a second
            tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
        }
    }
    .instrument(span)
    .await;

    if let Ok(path) = current_binary(&Env::default()) {
        match Command::new(path).spawn() {
            Ok(_) => {
                info!("Spawned new process. Waiting to restart.");

                // wait a second
                tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;

                std::process::exit(0);
            }
            Err(e) => error!("Error restarting: {}", e),
        }
    }

    Ok(())
}

#[tauri::command]
pub async fn get_logs(state: tauri::State<'_, State>) -> Result<Vec<LogEntry>, TauriError> {
    let res = state
        .client
        .get(format!("{}/system/logs", state.server_url))
        .send()
        .await?;

    if res.status().is_client_error() || res.status().is_server_error() {
        let status_code = res.status().as_u16();
        let body = res.text().await?;
        return Err(TauriError {
            message: body,
            status_code,
        });
    }

    Ok(res.json().await?)
}

#[tauri::command]
pub async fn open_system_logs_folder(state: tauri::State<'_, State>) -> Result<(), TauriError> {
    let res = state
        .client
        .post(format!("{}/system/open-logs", state.server_url))
        .send()
        .await?;

    if let Some(err) = check_error(res.status(), res.text().await?).await {
        return Err(err);
    }

    Ok(())
}

#[tauri::command]
pub async fn open_terminal_to_path(
    state: tauri::State<'_, State>,
    path: String,
) -> Result<(), TauriError> {
    let res = state
        .client
        .post(format!("{}/system/terminal", state.server_url))
        .json(&path)
        .send()
        .await?;

    if let Some(err) = check_error(res.status(), res.text().await?).await {
        return Err(err);
    }

    Ok(())
}

// Auth
#[tauri::command]
pub async fn check_login_required(state: tauri::State<'_, State>) -> Result<bool, TauriError> {
    let res = state
        .client
        .get(format!("{}/auth/status", state.server_url))
        .send()
        .await?;

    if res.status().is_client_error() || res.status().is_server_error() {
        let status_code = res.status().as_u16();
        let body = res.text().await?;
        return Err(TauriError {
            message: body,
            status_code,
        });
    }

    Ok(res.json().await?)
}

#[tauri::command]
pub async fn refresh_login(
    state: tauri::State<'_, State>,
    token: Option<String>,
) -> Result<(), TauriError> {
    let url = match token {
        Some(token) => format!("{}/auth/refresh?token={}", state.server_url, token),
        None => format!("{}/auth/refresh", state.server_url),
    };

    let res = state.client.post(url).send().await?;

    if let Some(err) = check_error(res.status(), res.text().await?).await {
        return Err(err);
    }

    Ok(())
}

// Git
#[tauri::command]
pub async fn install_git(state: tauri::State<'_, State>) -> Result<(), TauriError> {
    let res = state
        .client
        .post(format!("{}/system/git/install", state.server_url))
        .send()
        .await;

    match res {
        Ok(_) => Ok(()),
        Err(e) => Err(TauriError {
            message: e.to_string(),
            status_code: 500, // Internal Server Error as a default
        }),
    }
}

#[tauri::command]
pub async fn configure_git_user(
    state: tauri::State<'_, State>,
    name: String,
    email: String,
) -> Result<(), TauriError> {
    let res = state
        .client
        .post(format!("{}/system/git/configure", state.server_url))
        .json(&ConfigureUserRequest { name, email })
        .send()
        .await;

    match res {
        Ok(_) => Ok(()),
        Err(e) => Err(TauriError {
            message: e.to_string(),
            status_code: 500, // Internal Server Error as a default
        }),
    }
}

#[tauri::command]
pub async fn get_repo_config(state: tauri::State<'_, State>) -> Result<RepoConfig, TauriError> {
    let res = state
        .client
        .get(format!("{}/config/repo", state.server_url))
        .send()
        .await?;

    if res.status().is_client_error() || res.status().is_server_error() {
        let status_code = res.status().as_u16();
        let body = res.text().await?;
        return Err(TauriError {
            message: body,
            status_code,
        });
    }

    Ok(res.json().await?)
}

#[tauri::command]
pub async fn get_commits(
    state: tauri::State<'_, State>,
    limit: Option<u32>,
    remote: Option<bool>,
) -> Result<Vec<Commit>, TauriError> {
    let mut req = state.client.get(format!("{}/repo/log", state.server_url));

    if let Some(limit) = limit {
        req = req.query(&[("limit", limit)]);
    }

    if let Some(remote) = remote {
        req = req.query(&[("use_remote", remote)]);
    }

    match req.send().await {
        Ok(res) => {
            let status = res.status();
            if status.is_client_error() || status.is_server_error() {
                let body = res.text().await?;
                Err(TauriError {
                    message: body,
                    status_code: status.as_u16(),
                })
            } else {
                match res.json::<Vec<Commit>>().await {
                    Ok(res) => Ok(res),
                    Err(err) => Err(TauriError {
                        message: err.to_string(),
                        status_code: 500, // Internal Server Error as a default
                    }),
                }
            }
        }
        Err(err) => Err(TauriError {
            message: err.to_string(),
            status_code: 500, // Internal Server Error as a default
        }),
    }
}

#[tauri::command]
pub async fn clone_repo(
    state: tauri::State<'_, State>,
    req: CloneRequest,
) -> Result<(), TauriError> {
    let res = state
        .client
        .post(format!("{}/repo/clone", state.server_url))
        .json(&req)
        .send()
        .await?;

    if let Some(err) = check_error(res.status(), res.text().await?).await {
        return Err(err);
    }
    Ok(())
}

#[tauri::command]
pub async fn checkout_trunk(state: tauri::State<'_, State>) -> Result<(), TauriError> {
    let res = state
        .client
        .post(format!("{}/repo/checkout/trunk", state.server_url))
        .send()
        .await?;

    if let Some(err) = check_error(res.status(), res.text().await?).await {
        return Err(err);
    }

    Ok(())
}

#[tauri::command]
pub async fn revert_files(
    state: tauri::State<'_, State>,
    req: RevertFilesRequest,
) -> Result<(), TauriError> {
    let res = state
        .client
        .post(format!("{}/repo/revert", state.server_url))
        .json(&req)
        .send()
        .await?;

    if let Some(err) = check_error(res.status(), res.text().await?).await {
        return Err(err);
    }

    Ok(())
}

#[tauri::command]
pub async fn get_rebase_status(
    state: tauri::State<'_, State>,
) -> Result<RebaseStatusResponse, TauriError> {
    let res = state
        .client
        .get(format!("{}/repo/diagnostics/rebase", state.server_url))
        .send()
        .await?;

    if res.status().is_client_error() || res.status().is_server_error() {
        let status_code = res.status().as_u16();
        let body = res.text().await?;
        return Err(TauriError {
            message: body,
            status_code,
        });
    }

    Ok(res.json().await?)
}

#[tauri::command]
pub async fn fix_rebase(state: tauri::State<'_, State>) -> Result<(), TauriError> {
    let res = state
        .client
        .post(format!("{}/repo/diagnostics/rebase/fix", state.server_url))
        .send()
        .await?;

    if let Some(err) = check_error(res.status(), res.text().await?).await {
        return Err(err);
    }

    Ok(())
}

#[tauri::command]
pub async fn rebase(state: tauri::State<'_, State>) -> Result<(), TauriError> {
    let res = state
        .client
        .post(format!("{}/repo/diagnostics/rebase", state.server_url))
        .send()
        .await?;

    if let Some(err) = check_error(res.status(), res.text().await?).await {
        return Err(err);
    }

    Ok(())
}

#[tauri::command]
pub async fn acquire_locks(
    state: tauri::State<'_, State>,
    paths: Vec<String>,
    force: bool,
) -> Result<(), TauriError> {
    let request_data = LockRequest { paths, force };

    let res = state
        .client
        .post(format!("{}/repo/locks/lock", state.server_url))
        .json(&request_data)
        .send()
        .await?;

    if res.status().is_client_error() || res.status().is_server_error() {
        let status_code = res.status().as_u16();
        let body = res.text().await?;
        return Err(TauriError {
            message: body,
            status_code,
        });
    }

    Ok(())
}

#[tauri::command]
pub async fn release_locks(
    state: tauri::State<'_, State>,
    paths: Vec<String>,
    force: bool,
) -> Result<(), TauriError> {
    let request_data = LockRequest { paths, force };
    let res = state
        .client
        .post(format!("{}/repo/locks/unlock", state.server_url))
        .json(&request_data)
        .send()
        .await?;

    if let Some(err) = check_error(res.status(), res.text().await?).await {
        return Err(err);
    }

    Ok(())
}

#[tauri::command]
pub async fn sync_latest(state: tauri::State<'_, State>) -> Result<(), TauriError> {
    let res = state
        .client
        .post(format!("{}/repo/pull", state.server_url))
        .send()
        .await?;

    let status = res.status();
    let body = res.text().await?;

    if status.is_client_error() || status.is_server_error() {
        return Err(TauriError {
            message: body,
            status_code: status.as_u16(),
        });
    }

    let response: PullResponse = match serde_json::from_str(&body) {
        Ok(r) => r,
        Err(e) => {
            return Err(TauriError {
                message: format!("Failed to parse pull response: {}.", e),
                status_code: 500, // Internal Server Error as a default
            });
        }
    };

    if let Some(conflicts) = response.conflicts {
        if !conflicts.is_empty() {
            return Err(TauriError {
                message: format!("Failed to pull due to file conflict: {}", conflicts[0]),
                status_code: 409, // Conflict
            });
        }
    }

    Ok(())
}

// Config
#[tauri::command]
pub async fn get_app_config(state: tauri::State<'_, State>) -> Result<AppConfig, TauriError> {
    let res = state
        .client
        .get(format!("{}/config", state.server_url))
        .send()
        .await?;

    if res.status().is_client_error() || res.status().is_server_error() {
        let status_code = res.status().as_u16();
        let body = res.text().await?;
        return Err(TauriError {
            message: body,
            status_code,
        });
    }

    Ok(res.json().await?)
}

#[tauri::command]
pub async fn reset_config(state: tauri::State<'_, State>) -> Result<(), TauriError> {
    let res = state
        .client
        .post(format!("{}/config/reset", state.server_url))
        .send()
        .await?;

    if let Some(err) = check_error(res.status(), res.text().await?).await {
        return Err(err);
    }

    restart(state).await?;

    Ok(())
}

// Utilities
#[tauri::command]
pub async fn open_url(url: String) -> Result<(), TauriError> {
    open::that(url).map_err(|e| TauriError {
        message: e.to_string(),
        status_code: 500, // Internal Server Error as a default
    })?;

    Ok(())
}
