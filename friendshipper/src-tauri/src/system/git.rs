use std::sync::Arc;

use axum::extract::State;
use axum::Json;
#[cfg(any(target_os = "windows", target_os = "macos"))]
use tokio::process::Command;

use ethos_core::clients::git;
use ethos_core::types::errors::CoreError;
use ethos_core::types::repo::ConfigureUserRequest;

#[cfg(windows)]
use ethos_core::CREATE_NO_WINDOW;

use crate::state::AppState;

pub async fn install(State(state): State<Arc<AppState>>) -> Result<(), CoreError> {
    #[cfg(target_os = "windows")]
    {
        let mut cmd = Command::new("winget");
        cmd.args(["install", "--id", "Git.Git", "-e", "--source", "winget"])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .await?;
    }

    #[cfg(target_os = "macos")]
    {
        Command::new("brew")
            .args(["install", "git"])
            .output()
            .await?;
        Command::new("brew")
            .args(["install", "git-lfs"])
            .output()
            .await?;
    }

    state.git().version().await?;

    Ok(())
}

pub async fn configure_user(request: Json<ConfigureUserRequest>) -> Result<(), CoreError> {
    git::configure_global("user.name", &request.name).await?;
    git::configure_global("user.email", &request.email).await?;
    git::configure_global("push.autoSetupRemote", "true").await?;

    Ok(())
}
