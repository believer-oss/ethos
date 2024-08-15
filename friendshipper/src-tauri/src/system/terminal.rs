#[cfg(target_os = "windows")]
use std::process::Command;

use axum::extract::State;
use tracing::error;

use crate::engine::EngineProvider;
use crate::state::AppState;

#[cfg(target_os = "windows")]
pub async fn open_terminal_to_path<T>(State(_state): State<AppState<T>>, path: String)
where
    T: EngineProvider,
{
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

#[cfg(target_os = "linux")]
pub async fn open_terminal_to_path<T>(State(_state): State<AppState<T>>, _path: String)
    where
        T: EngineProvider,
{
    error!("Open terminal not supported on linux");
}
