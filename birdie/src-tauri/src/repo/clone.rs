use std::path::PathBuf;
use std::sync::Arc;

use anyhow::anyhow;
use axum::extract::State;
use axum::Json;
use fs_extra::dir::get_size;
use tracing::info;

use ethos_core::types::errors::CoreError;
use ethos_core::types::repo::CloneRequest;

use crate::state::AppState;

pub async fn clone_handler(
    State(state): State<Arc<AppState>>,
    Json(request): Json<CloneRequest>,
) -> Result<(), CoreError> {
    // If the repo exists, let's check if it's a git repo
    let mut repo_path = PathBuf::from(&request.path);
    let project_name = request
        .url
        .split('/')
        .last()
        .unwrap_or_default()
        .trim_end_matches(".git");

    repo_path.push(project_name);

    if repo_path.exists() {
        let mut git_path = repo_path.clone();
        git_path.push(".git");

        if git_path.exists() {
            // Let's assume it's the right repo and bail out with a success.
            return Ok(());
        }

        return Err(CoreError(anyhow!(
            "The folder {} already exists but is not a git repository.",
            repo_path.to_str().unwrap_or_default()
        )));
    }

    info!("git clone: {}", request.url);

    let repo_name = request
        .url
        .split('/')
        .last()
        .unwrap_or_default()
        .trim_end_matches(".git");
    let repo_path = PathBuf::from(&request.path).join(repo_name);

    let tx = state.git_tx.clone();
    let status_handle = tokio::spawn(async move {
        loop {
            let mut size = get_size(&repo_path).unwrap_or_default() as f64 / 1024.0 / 1024.0;

            if size > 1024.0 {
                size /= 1024.0;
                tx.send(format!("Downloaded: {:.2} GB", size)).unwrap();
            } else {
                tx.send(format!("Downloaded: {:.2} MB", size)).unwrap();
            }

            tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
        }
    });

    state
        .git()
        .run(
            &["clone", "--filter=tree:0", "--progress", &request.url],
            Default::default(),
        )
        .await?;

    status_handle.abort();

    {
        // We need to force a read of the in-repo configuration file.
        let config = state.app_config.read();
        let repo_config = config.initialize_repo_config();

        let mut lock = state.repo_config.write();
        *lock = repo_config.clone();
    }

    Ok(())
}
