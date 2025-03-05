use crate::engine::EngineProvider;
use crate::repo::operations::{PullOp, StatusOp};
use crate::state::AppState;
use anyhow::anyhow;
use axum::{async_trait, extract::State, Json};
use ethos_core::clients::aws::ensure_aws_client;
use ethos_core::clients::git;
use ethos_core::operations::{AddOp, CommitOp, RestoreOp};
use ethos_core::types::errors::CoreError;
use ethos_core::types::repo::PushRequest;
use ethos_core::worker::{Task, TaskSequence};
use serde::{Deserialize, Serialize};
use tracing::{info, instrument};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushResponse {
    #[serde(rename = "pushAttempted")]
    pub push_attempted: bool,
    pub conflicts: Option<Vec<String>>,
}

#[derive(Clone)]
pub struct PushOp {
    pub git_client: git::Git,
    pub branch: String,
}

#[async_trait]
impl Task for PushOp {
    #[instrument(name = "PushOp::execute", skip(self))]
    async fn execute(&self) -> Result<(), CoreError> {
        self.git_client.push(&self.branch).await?;

        Ok(())
    }

    fn get_name(&self) -> String {
        String::from("RepoPush")
    }
}

#[instrument(skip(state))]
pub async fn push_handler<T>(
    State(state): State<AppState<T>>,
    Json(request): Json<PushRequest>,
) -> Result<(), CoreError>
where
    T: EngineProvider,
{
    info!("push request: {:?}", request);

    // start by adding our files
    let add_op = AddOp {
        files: request.files.clone(),
        git_client: state.git(),
    };

    // block on add
    add_op.execute().await?;

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
        let restore_op = RestoreOp {
            files: files_to_unstage,
            git_client: state.git(),
        };

        // block on restore
        restore_op.execute().await?;
    }

    // force a status update
    let status_op = StatusOp {
        repo_status: state.repo_status.clone(),
        app_config: state.app_config.clone(),
        repo_config: state.repo_config.clone(),
        engine: state.engine.clone(),
        git_client: state.git().clone(),
        github_username: state.github_username().clone(),
        aws_client: None,
        storage: None,
        allow_offline_communication: false,
        skip_display_names: true,
        skip_engine_update: false,
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
        let aws_client = ensure_aws_client(state.aws_client.read().await.clone())?;

        let storage = match state.storage.read().clone() {
            Some(storage) => storage,
            None => {
                return Err(CoreError::Internal(anyhow!(
                    "Storage not configured. AWS may still be initializing."
                )));
            }
        };

        let pull_op = PullOp {
            app_config: state.app_config.clone(),
            repo_config: state.repo_config.clone(),
            repo_status: state.repo_status.clone(),
            longtail: state.longtail.clone(),
            longtail_tx: state.longtail_tx.clone(),
            aws_client: aws_client.clone(),
            storage,
            git_client: state.git(),
            github_client: state.github_client.read().clone(),
            engine: state.engine.clone(),
        };

        sequence.push(Box::new(pull_op));
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
        git_client: state.git(),
        branch: state.repo_status.read().branch.clone(),
    };

    sequence.push(Box::new(push_op));

    // queue up another status update
    sequence.push(Box::new(status_op));

    let _ = state.operation_tx.send(sequence).await;

    Ok(())
}
