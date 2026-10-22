use axum::extract::{Query, State};
use axum::Json;
use serde::Deserialize;
use tracing::{debug, warn};

use crate::engine::EngineProvider;
use crate::repo::operations::gh::submit::is_quicksubmit_branch;
use crate::state::AppState;
use ethos_core::clients::git;
use ethos_core::types::errors::CoreError;
use ethos_core::types::repo::{is_feature_branch, BranchInfo};

#[derive(Debug, Deserialize)]
pub struct ListBranchesParams {
    #[serde(default)]
    pub all: bool,
}

pub async fn list_branches_handler<T>(
    State(state): State<AppState<T>>,
    Query(params): Query<ListBranchesParams>,
) -> Result<Json<Vec<BranchInfo>>, CoreError>
where
    T: EngineProvider,
{
    let git = state.git();

    // Remote refs go stale when the background fetch loop is off.
    let background_disabled = state.app_config.read().disable_background_git_operations;
    if background_disabled {
        if let Err(e) = git
            .fetch(git::ShouldPrune::Yes, git::Opts::new_without_logs())
            .await
        {
            warn!("Failed to fetch before listing branches: {e}");
        }
    }

    let mut branches = git.list_remote_branches().await?;
    // Quick Submit branches are transient and never switch targets.
    branches.retain(|b| !is_quicksubmit_branch(&b.name));

    let current = state.repo_status.read().branch.clone();
    {
        let repo_config = state.repo_config.read();
        for b in branches.iter_mut() {
            b.is_feature_branch = is_feature_branch(&b.name);
            b.is_protected = repo_config.is_protected_branch(&b.name);
            b.is_current = b.name == current;
        }
    }

    if !params.all {
        branches.retain(|b| b.is_feature_branch);
    }

    debug!("Listed {} branches (all={})", branches.len(), params.all);

    Ok(Json(branches))
}
