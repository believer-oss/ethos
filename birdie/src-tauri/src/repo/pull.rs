use std::sync::Arc;

use anyhow::anyhow;
use axum::{async_trait, debug_handler, extract::State, Json};
use tokio::sync::oneshot::error::RecvError;
use tracing::{info, instrument};

use crate::types::config::BirdieConfig;
use ethos_core::clients::git;
use ethos_core::clients::git::{PullStashStrategy, PullStrategy};
use ethos_core::types::errors::CoreError;
use ethos_core::types::repo::{PullResponse, RepoStatusRef};
use ethos_core::worker::{Task, TaskSequence};

use crate::state::AppState;

use super::StatusOp;

#[derive(Clone)]
pub struct PullOp {
    // This does not get read
    #[allow(dead_code)]
    pub app_config: BirdieConfig,
    pub repo_status: RepoStatusRef,
    pub git_client: git::Git,
    pub github_username: String,
}

#[async_trait]
impl Task for PullOp {
    #[instrument(skip(self))]
    async fn execute(&self) -> Result<(), CoreError> {
        {
            let status_op = {
                StatusOp {
                    repo_status: self.repo_status.clone(),
                    git_client: self.git_client.clone(),
                    skip_fetch: false,
                    github_username: self.github_username.clone(),
                }
            };

            status_op.execute().await?;
        }

        // Check repo status to see if we need to pull at all.
        {
            let repo_status = self.repo_status.read();
            if repo_status.commits_behind == 0 {
                info!("no commits behind, skipping pull");

                return Ok(());
            }

            if !repo_status.conflicts.is_empty() {
                return Err(CoreError::Input(anyhow!(
                    "Conflicts detected, cannot pull. See Diagnostics."
                )));
            }
        }

        self.git_client
            .pull(PullStrategy::FFOnly, PullStashStrategy::None)
            .await?;

        {
            let status_op = {
                StatusOp {
                    repo_status: self.repo_status.clone(),
                    git_client: self.git_client.clone(),
                    skip_fetch: true,
                    github_username: self.github_username.clone(),
                }
            };

            status_op.execute().await?;
        }

        Ok(())
    }

    fn get_name(&self) -> String {
        String::from("RepoPull")
    }
}

#[debug_handler]
pub async fn pull_handler(
    State(state): State<Arc<AppState>>,
) -> Result<Json<PullResponse>, CoreError> {
    let config = state.app_config.read().clone();

    let pull_op = PullOp {
        app_config: config,
        repo_status: state.repo_status.clone(),
        git_client: state.git(),
        github_username: state.github_username(),
    };

    let (tx, rx) = tokio::sync::oneshot::channel::<Option<CoreError>>();
    let mut sequence = TaskSequence::new().with_completion_tx(tx);
    sequence.push(Box::new(pull_op));
    let _ = state.operation_tx.send(sequence).await;

    let res: Result<Option<CoreError>, RecvError> = rx.await;
    if let Ok(Some(e)) = res {
        return Err(e);
    }

    Ok(Json(PullResponse { conflicts: None }))
}
