use std::{path::PathBuf, sync::Arc};

use axum::{extract::State, Router};
use axum::routing::post;
use ethos_core::clients::git;
use ethos_core::clients::git::{PullStashStrategy, PullStrategy};
use ethos_core::types::config::AppConfig;
use ethos_core::types::errors::CoreError;

use crate::state::AppState;


pub fn router(shared_state: Arc<AppState>) -> Router {
    Router::new()
        .route("/sync", post(sync_tools))
        .with_state(shared_state)
}

async fn sync_tools(
    State(state): State<Arc<AppState>>,
) -> Result<String, CoreError>  {

    let config: AppConfig = state.app_config.read().clone();
    let tools_path = config.tools_path.clone();

    let mut did_sync: String = "Fail".to_string();
    if std::path::Path::new(&tools_path).exists() {
        let repo_buf = PathBuf::from(tools_path);
        let git_client: git::Git = git::Git::new(repo_buf, state.git_tx.clone());

        git_client
            .pull(PullStrategy::Rebase, PullStashStrategy::Autostash)
            .await?;
        did_sync = "OK".to_string();
    }

    Ok(did_sync)
}