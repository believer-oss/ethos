#[cfg(target_os = "windows")]
use std::process::Command;
use std::sync::Arc;

use axum::extract::State;
use tracing::error;

use crate::state::AppState;

#[cfg(target_os = "windows")]
pub async fn open_terminal_to_path(State(_state): State<Arc<AppState>>, path: String) {
    if let Err(e) = Command::new("cmd")
        .arg("/C")
        .arg("start")
        .arg("powershell")
        .arg("-NoExit")
        .arg("-Command")
        .arg(format!("cd {}", path))
        .spawn()
    {
        error!("Error opening terminal to path: {:?}", e);
    }
}

#[cfg(not(target_os = "windows"))]
pub async fn open_terminal_to_path(State(_state): State<Arc<AppState>>, _path: String) {
    error!("Open terminal not supported on linux");
}
