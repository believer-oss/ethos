use axum::routing::post;
use axum::{extract::State, Router};
use std::path::Path;
use std::process::Command;
use std::{path::PathBuf, sync::Arc};
use walkdir::WalkDir;

use ethos_core::clients::git;
use ethos_core::clients::git::{PullStashStrategy, PullStrategy};
use ethos_core::types::errors::CoreError;

use crate::state::AppState;
use crate::types::config::BirdieConfig;

pub fn router(shared_state: Arc<AppState>) -> Router {
    Router::new()
        .route("/sync", post(sync_tools))
        .route("/setenv", post(run_set_env_cmd))
        .with_state(shared_state)
}

async fn sync_tools(State(state): State<Arc<AppState>>) -> Result<String, CoreError> {
    let config: BirdieConfig = state.app_config.read().clone();
    let tools_path = config.tools_path.clone();
    let tools_url = config.tools_url.clone();
    let (_, git_name) = tools_url.rsplit_once('/').unwrap();
    let repo_name = git_name.replace(".git","");

    let sync_path = format!("{tools_path}/{repo_name}");

    let mut did_sync: String = "Fail".to_string();
    let dir_exists = Path::new(&sync_path).exists();

    let mut dir_empty = true;
    if dir_exists {
        dir_empty = Path::new(&sync_path).read_dir()?.next().is_none();
    }

    // If there's no directory or an empty directory we need to clone instead
    if dir_exists && !dir_empty {
        let repo_buf = PathBuf::from(sync_path);
        let git_client: git::Git = git::Git::new(repo_buf, state.git_tx.clone());

        git_client
            .pull(PullStrategy::Rebase, PullStashStrategy::Autostash)
            .await?;
        did_sync = "OK".to_string();
    }

    Ok(did_sync)
}

async fn run_set_env_cmd(State(state): State<Arc<AppState>>) -> Result<(), CoreError> {
    let config: BirdieConfig = state.app_config.read().clone();
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
