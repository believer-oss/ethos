use std::sync::Arc;

use anyhow::anyhow;
use axum::extract::State;
use axum::{async_trait, debug_handler, Json};
use octocrab::models::pulls::MergeableState;
use octocrab::Octocrab;
use tracing::info;

use ethos_core::clients::aws::ensure_aws_client;
use ethos_core::clients::github;
use ethos_core::operations::{AddOp, CommitOp, RestoreOp};
use ethos_core::types::errors::CoreError;
use ethos_core::types::github::TokenNotFoundError;
use ethos_core::worker::{Task, TaskSequence};

use crate::repo::operations::PushOp;
use crate::repo::operations::{PushRequest, StatusOp};
use crate::repo::RepoStatusRef;
use crate::state::AppState;

#[derive(Clone)]
pub struct GitHubSubmitOp {
    pub head_branch: String,
    pub base_branch: String,
    pub commit_message: String,
    pub repo_status: RepoStatusRef,
    pub token: String,
    pub client: github::GraphQLClient,
}

#[async_trait]
impl Task for GitHubSubmitOp {
    async fn execute(&self) -> anyhow::Result<()> {
        let octocrab = Octocrab::builder()
            .personal_token(self.token.clone())
            .build()?;

        let truncated_message = if self.commit_message.len() > 50 {
            format!("{}...", &self.commit_message[..50])
        } else {
            self.commit_message.clone()
        };

        let owner: String;
        let repo: String;
        {
            let status = self.repo_status.read();
            owner = status.repo_owner.clone();
            repo = status.repo_name.clone();
        }

        let mut pr = octocrab
            .pulls(owner.clone(), repo.clone())
            .create(
                format!("[quick submit] {}", truncated_message),
                self.head_branch.clone(),
                self.base_branch.clone(),
            )
            .send()
            .await?;

        while let Some(state) = pr.mergeable_state {
            match state {
                MergeableState::Blocked | MergeableState::Behind | MergeableState::Unknown => {
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    pr = octocrab
                        .pulls(owner.clone(), repo.clone())
                        .get(pr.number)
                        .await?;
                }
                MergeableState::Dirty => {
                    return Err(anyhow!(
                        "PR state is 'dirty'. It's likely a commit check has failed."
                    ));
                }
                _ => {
                    info!("mergeable state: {:?}", state);
                    break;
                }
            }
        }

        let id = self
            .client
            .get_pull_request_id(owner.clone(), repo.clone(), pr.number as i64)
            .await?;

        self.client.enqueue_pull_request(id).await?;

        Ok(())
    }

    fn get_name(&self) -> String {
        "GitHubSubmitOp".to_string()
    }
}

#[debug_handler]
pub async fn submit_handler(
    State(state): State<Arc<AppState>>,
    Json(request): Json<PushRequest>,
) -> Result<Json<String>, CoreError> {
    let aws_client = ensure_aws_client(state.aws_client.read().await.clone())?;

    {
        let branch = state.repo_status.read().branch.clone();
        if is_quicksubmit_branch(&branch) {
            return Err(CoreError(
                anyhow!("Submitting from a QuickSubmit branch is not allowed. You must sync to submit again."
            )));
        }
    }

    let github_client = match state.github_client.read().clone() {
        Some(client) => client.clone(),
        None => return Err(CoreError(anyhow!(TokenNotFoundError))),
    };

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
            repo_config: state.repo_config.clone(),
            ofpa_cache: state.ofpa_cache.clone(),
            git_client: state.git(),
            aws_client: aws_client.clone(),
            storage: state.storage.read().clone().unwrap(),
            skip_fetch: false,
            skip_dll_check: true,
            skip_ofpa_translation: false,
        }
    };

    // block on the status update - we need to check for conflicts
    // before we try to pull
    status_op.execute().await?;

    let branch_name = {
        let display_name = &state.app_config.read().user_display_name;
        let santized_display_name = display_name.replace(' ', "-");
        format!(
            "f11r-{}-{}",
            santized_display_name,
            chrono::Utc::now().timestamp()
        )
    };

    state
        .git()
        .run(&["checkout", "-b", &branch_name], Default::default())
        .await?;

    // commit
    let (tx, rx) = tokio::sync::oneshot::channel::<Option<anyhow::Error>>();
    let mut sequence = TaskSequence::new().with_completion_tx(tx);

    let commit_op = CommitOp {
        message: request.commit_message.clone(),
        repo_status: state.repo_status.clone(),
        git_client: state.git(),
    };

    sequence.push(Box::new(commit_op));

    let github_pat = state.app_config.read().ensure_github_pat()?;

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

    let token = state.app_config.read().ensure_github_pat()?;

    let gh_op = GitHubSubmitOp {
        head_branch: branch_name.clone(),
        base_branch: state.repo_config.read().trunk_branch.clone(),
        token,
        commit_message: request.commit_message.clone(),
        repo_status: state.repo_status.clone(),
        client: github_client,
    };

    sequence.push(Box::new(gh_op));

    state.operation_tx.send(sequence).await?;

    match rx.await {
        Ok(Some(e)) => return Err(CoreError(e)),
        Ok(None) => {}
        Err(e) => return Err(e.into()),
    }

    Ok(Json("ok".to_string()))
}

pub fn is_quicksubmit_branch(branch: &str) -> bool {
    branch.starts_with("f11r")
}
