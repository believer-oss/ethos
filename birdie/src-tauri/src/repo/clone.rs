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
        .next_back()
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

        return Err(CoreError::Internal(anyhow!(
            "The folder {} already exists but is not a git repository.",
            repo_path.to_str().unwrap_or_default()
        )));
    }

    info!("git clone: {}", request.url);

    let repo_name = request
        .url
        .split('/')
        .next_back()
        .unwrap_or_default()
        .trim_end_matches(".git");
    let repo_path = PathBuf::from(&request.path).join(repo_name);
    let repo_path_str = repo_path.to_str().unwrap_or_default();

    let tx = state.git_tx.clone();
    let size_check_path = repo_path.clone();
    let status_handle = tokio::spawn(async move {
        loop {
            let mut size = get_size(&size_check_path).unwrap_or_default() as f64 / 1024.0 / 1024.0;

            if size > 1024.0 {
                size /= 1024.0;
                tx.send(format!("Downloaded: {size:.2} GB")).unwrap();
            } else {
                tx.send(format!("Downloaded: {size:.2} MB")).unwrap();
            }

            tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
        }
    });

    state
        .git()
        .run(
            &[
                "clone",
                "--filter=tree:0",
                "--progress",
                &request.url,
                repo_path_str,
            ],
            Default::default(),
        )
        .await?;

    status_handle.abort();

    Ok(())
}
