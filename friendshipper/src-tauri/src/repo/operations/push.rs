use std::sync::Arc;

use anyhow::Context;
use axum::{async_trait, debug_handler, extract::State, Json};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use ethos_core::clients::aws::ensure_aws_client;
use ethos_core::clients::git;
use ethos_core::operations::{AddOp, CommitOp, LockOp, RestoreOp};
use ethos_core::types::errors::CoreError;
use ethos_core::types::locks::ForceUnlock;
use ethos_core::types::repo::PushRequest;
use ethos_core::worker::{Task, TaskSequence};

use crate::{repo::operations::PullOp, state::AppState};

use super::{RepoStatusRef, StatusOp};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushResponse {
    #[serde(rename = "pushAttempted")]
    pub push_attempted: bool,

    pub conflicts: Option<Vec<String>>,
}

#[derive(Clone)]
pub struct PushOp {
    pub files: Vec<String>,
    pub git_client: git::Git,
    pub github_pat: String,
    pub trunk_branch: String,
    pub repo_status: RepoStatusRef,
}

#[async_trait]
impl Task for PushOp {
    async fn execute(&self) -> anyhow::Result<()> {
        // determine current branch
        let branch = self.git_client.current_branch().await?;

        // push
        self.git_client.push(&branch).await?;

        // release any locks if we're on main, otherwise assume the person making the change will be merging
        // via a PR, where the CI will automatically release the locks
        if branch == self.trunk_branch {
            let response = self.git_client.verify_locks().await?;

            let mut unlock_paths: Vec<String> = vec![];
            for lock in response.ours {
                if self.files.contains(&lock.path) {
                    unlock_paths.push(lock.path.clone());
                }
            }

            if !unlock_paths.is_empty() {
                let op = LockOp::unlock(
                    self.git_client.clone(),
                    unlock_paths,
                    self.github_pat.clone(),
                    ForceUnlock::False,
                );
                op.execute().await?;
            }
        } else {
            info!(
                "skipping locks release because we're on branch {}, not main",
                branch
            );
        }

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
) -> Result<Json<PushResponse>, CoreError> {
    let aws_client = ensure_aws_client(state.aws_client.read().await.clone())?;

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
            if !file.index_state.is_empty() {
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
    let status_op = {
        StatusOp {
            repo_status: state.repo_status.clone(),
            app_config: state.app_config.clone(),
            git_client: state.git(),
            aws_client: aws_client.clone(),
            storage: state.storage.read().clone().unwrap(),
            skip_fetch: false,
            skip_dll_check: true,
        }
    };

    // block on the status update - we need to check for conflicts
    // before we try to pull
    status_op.execute().await?;

    let mut sequence = TaskSequence::new();

    let mut pull_required = false;
    {
        let repo_status = state.repo_status.read();
        if repo_status.conflict_upstream {
            return Ok(Json(PushResponse {
                push_attempted: false,
                conflicts: Some(repo_status.conflicts.clone()),
            }));
        }

        // nothing to do
        if repo_status.commits_ahead == 0
            && !repo_status.has_staged_changes
            && !repo_status.has_local_changes
        {
            return Ok(Json(PushResponse {
                push_attempted: false,
                conflicts: None,
            }));
        }

        // queue up a pull if required
        if repo_status.commits_behind > 0 {
            pull_required = true;
        }
    }

    if pull_required {
        let storage = state
            .storage
            .read()
            .clone()
            .context("Storage not configured. AWS may still be initializing.")?;

        let task = PullOp {
            app_config: state.app_config.clone(),
            uproject_path_relative: state.repo_config.read().uproject_path.clone(),
            repo_status: state.repo_status.clone(),
            trunk_branch: state.repo_config.read().trunk_branch.clone(),
            longtail: state.longtail.clone(),
            longtail_tx: state.longtail_tx.clone(),
            aws_client: aws_client.clone(),
            storage,
            git_client: state.git(),
            github_client: state.github_client.read().clone(),
        };

        sequence.push(Box::new(task));
    }

    let commit_op = CommitOp {
        message: request.commit_message,
        repo_status: state.repo_status.clone(),
        git_client: state.git(),
    };

    sequence.push(Box::new(commit_op));

    let github_pat = match state.app_config.read().ensure_github_pat() {
        Ok(pat) => pat,
        Err(_) => {
            warn!("GitHub PAT is empty. Locking files will not work.");
            "".to_string()
        }
    };

    // queue up the push
    let push_op = PushOp {
        files: request.files.clone(),
        git_client: state.git(),
        repo_status: state.repo_status.clone(),
        trunk_branch: state.repo_config.read().trunk_branch.clone(),
        github_pat,
    };

    sequence.push(Box::new(push_op));

    // queue up another status update
    sequence.push(Box::new(status_op));

    let _ = state.operation_tx.send(sequence).await;

    Ok(Json(PushResponse {
        push_attempted: true,
        conflicts: None,
    }))
}
