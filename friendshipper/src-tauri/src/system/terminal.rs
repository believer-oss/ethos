use std::process::Command;

use axum::extract::State;
use tracing::error;

use crate::engine::EngineProvider;
use crate::state::AppState;

#[cfg(any(target_os = "windows", target_os = "linux"))]
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
        error!("Error opening terminal to repo: {:?}", e);
    }
}
