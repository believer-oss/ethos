use std::sync::Arc;

use anyhow::anyhow;
use axum::{async_trait, debug_handler, extract::State, Json};
use serde::{Deserialize, Serialize};
use tracing::info;

use ethos_core::clients::git;
use ethos_core::operations::{AddOp, CommitOp, RestoreOp};
use ethos_core::types::errors::CoreError;
use ethos_core::types::repo::{PushRequest, RepoStatusRef};
use ethos_core::worker::{Task, TaskSequence};

use crate::repo::pull::PullOp;
use crate::state::AppState;

use super::StatusOp;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushResponse {
    #[serde(rename = "pushAttempted")]
    pub push_attempted: bool,

    pub conflicts: Option<Vec<String>>,
}

#[derive(Clone)]
pub struct PushOp {
    // This does not get read
    #[allow(dead_code)]
    pub files: Vec<String>,
    pub git_client: git::Git,
    pub trunk_branch: String,
    // This does not get read
    #[allow(dead_code)]
    pub repo_status: RepoStatusRef,
}

#[async_trait]
impl Task for PushOp {
    async fn execute(&self) -> anyhow::Result<()> {
        self.git_client.push(&self.trunk_branch).await?;

        Ok(())
    }

    fn get_name(&self) -> String {
        String::from("RepoPush")
    }
}

#[debug_handler]
pub async fn push_handler(
    State(state): State<Arc<AppState>>,
    Json(request): Json<PushRequest>,
) -> Result<(), CoreError> {
    info!("push request: {:?}", request);

    // start by adding our files
    for chunk in request.files.chunks(50) {
        let add_op = AddOp {
            files: chunk.to_vec(),
            git_client: state.git(),
        };

        // block on add
        add_op.execute().await?;
    }

    // unstage any files that are staged but not in the request
    let mut staged_files = Vec::new();
    {
        let repo_status = state.repo_status.read();
        let modified = repo_status.modified_files.clone();
        for file in modified.into_iter() {
            if file.is_staged {
                staged_files.push(file.path.clone());
            }
        }
    }

    let files_to_unstage: Vec<String> = staged_files
        .into_iter()
        .filter(|file| !request.files.contains(file))
        .collect();

    if !files_to_unstage.is_empty() {
        for chunk in files_to_unstage.chunks(50) {
            let restore_op = RestoreOp {
                files: chunk.to_vec(),
                git_client: state.git(),
            };

            // block on restore
            restore_op.execute().await?;
        }
    }

    // force a status update
    let status_op = StatusOp {
        repo_status: state.repo_status.clone(),
        git_client: state.git(),
        skip_fetch: false,
        github_username: state.github_username(),
    };

    // block on the status update - we need to check for conflicts
    // before we try to pull
    status_op.execute().await?;

    let mut sequence = TaskSequence::new();

    let mut pull_required = false;
    {
        let repo_status = state.repo_status.read();
        if repo_status.conflict_upstream {
            return Err(CoreError::Internal(anyhow!(
                "Conflict detected. See Diagnostics."
            )));
        }

        // nothing to do
        if repo_status.commits_ahead == 0
            && !repo_status.has_staged_changes
            && !repo_status.has_local_changes
        {
            return Ok(());
        }

        // queue up a pull if required
        if repo_status.commits_behind > 0 {
            pull_required = true;
        }
    }

    if pull_required {
        let app_config = state.app_config.read().clone();
        let task = PullOp {
            app_config,
            repo_status: state.repo_status.clone(),
            git_client: state.git(),
            github_username: state.github_username(),
        };

        sequence.push(Box::new(task));
    }

    let commit_op = CommitOp {
        message: request.commit_message,
        repo_status: state.repo_status.clone(),
        git_client: state.git(),
        skip_status_check: false,
    };

    sequence.push(Box::new(commit_op));

    // queue up the push
    let push_op = PushOp {
        files: request.files.clone(),
        git_client: state.git(),
        repo_status: state.repo_status.clone(),
        trunk_branch: "main".to_string(),
    };

    sequence.push(Box::new(push_op));

    // queue up another status update
    sequence.push(Box::new(status_op));

    let _ = state.operation_tx.send(sequence).await;

    Ok(())
}
