use axum::routing::post;
use axum::{extract::State, Router};
use ethos_core::clients::git;
use ethos_core::clients::git::{PullStashStrategy, PullStrategy};
use ethos_core::types::config::AppConfig;
use ethos_core::types::errors::CoreError;
use std::path::Path;
use std::process::Command;
use std::{path::PathBuf, sync::Arc};
use walkdir::WalkDir;

use crate::state::AppState;

pub fn router(shared_state: Arc<AppState>) -> Router {
    Router::new()
        .route("/sync", post(sync_tools))
        .route("/setenv", post(run_set_env_cmd))
        .with_state(shared_state)
}

async fn sync_tools(State(state): State<Arc<AppState>>) -> Result<String, CoreError> {
    let config: AppConfig = state.app_config.read().clone();
    let tools_path = config.tools_path.clone();

    let mut did_sync: String = "Fail".to_string();
    let dir_exists = Path::new(&tools_path).exists();
    let dir_empty = Path::new(&tools_path).read_dir()?.next().is_none();
    if dir_exists && !dir_empty {
        let repo_buf = PathBuf::from(tools_path);
        let git_client: git::Git = git::Git::new(repo_buf, state.git_tx.clone());

        git_client
            .pull(PullStrategy::Rebase, PullStashStrategy::Autostash)
            .await?;
        did_sync = "OK".to_string();
    }

    Ok(did_sync)
}

async fn run_set_env_cmd(State(state): State<Arc<AppState>>) -> Result<(), CoreError> {
    let config: AppConfig = state.app_config.read().clone();
    let tools_path = config.tools_path.clone();
    const CMD_FILE_NAME: &str = "set_env_var";

    if cfg!(target_os = "windows") {
        for entry in WalkDir::new(tools_path).max_depth(2) {
            let entry = entry.unwrap();
            let entry_path = entry.path().to_str().unwrap();
            if entry_path.contains(CMD_FILE_NAME) {
                Command::new("cmd")
                    .args(["/C", entry_path])
                    .output()
                    .expect("Failed to execute");
                break;
            }
        }
    }

    Ok(())
}
