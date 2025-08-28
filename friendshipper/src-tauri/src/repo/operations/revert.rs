use anyhow::anyhow;
use axum::extract::State;
use axum::{async_trait, Json};
use std::fs;
use std::io::Write;
use tempfile::NamedTempFile;
use tokio::sync::oneshot::error::RecvError;
use tracing::info;
use tracing::instrument;

use crate::engine::EngineProvider;
use crate::state::AppState;
use ethos_core::clients::git;
use ethos_core::operations::LockOp;
use ethos_core::types::errors::CoreError;
use ethos_core::types::locks::LockOperation;
use ethos_core::types::repo::{File, RevertFilesRequest};
use ethos_core::worker::{Task, TaskSequence};

use super::RepoStatusRef;

#[derive(Clone)]
pub struct RevertFilesOp<T> {
    pub files: Vec<String>,
    pub git_client: git::Git,
    pub repo_status: RepoStatusRef,
    pub engine: Option<T>,
    pub take_snapshot: bool,
}

// Note: This is not "git revert", it's "git checkout -- <files>"
#[async_trait]
impl<T> Task for RevertFilesOp<T>
where
    T: EngineProvider,
{
    #[instrument(skip(self), name = "RevertOp::execute", fields(files = ?self.files))]
    async fn execute(&self) -> Result<(), CoreError> {
        if let Some(engine) = &self.engine {
            engine.check_ready_to_sync_repo().await?;
        }

        if self.files.is_empty() {
            return Err(CoreError::Input(anyhow!("no files provided")));
        }

        if self.take_snapshot {
            // Include all files being reverted (both modified and untracked)
            let _ = self
                .git_client
                .save_snapshot(
                    "pre-revert",
                    self.files.clone(),
                    git::SaveSnapshotIndexOption::DiscardIndex,
                )
                .await?;
        }

        let branch = self.repo_status.read().branch.clone();

        let mut temp_file = NamedTempFile::new()?;
        for file in &self.files {
            writeln!(temp_file, "{file}")?;
        }
        temp_file.flush()?;

        let args = vec![
            "checkout",
            &branch,
            "--pathspec-from-file",
            temp_file.path().to_str().unwrap(),
        ];

        self.git_client.run(&args, git::Opts::default()).await?;

        Ok(())
    }

    fn get_name(&self) -> String {
        "RevertFilesOp".to_string()
    }
}

#[instrument(skip(state))]
pub async fn revert_files_handler<T>(
    State(state): State<AppState<T>>,
    Json(request): Json<RevertFilesRequest>,
) -> Result<Json<String>, CoreError>
where
    T: EngineProvider,
{
    let (tx, rx) = tokio::sync::oneshot::channel::<Option<CoreError>>();
    let mut sequence = TaskSequence::new().with_completion_tx(tx);

    let repo_path = state.app_config.read().repo_path.clone();

    let modified_files = {
        let repo_status = state.repo_status.read();
        repo_status.modified_files.clone()
    };

    let untracked_files = {
        let repo_status = state.repo_status.read();
        repo_status.untracked_files.clone()
    };

    let modified: Vec<&File> = request
        .files
        .clone()
        .iter()
        .filter_map(|f| {
            if modified_files.contains(f) {
                modified_files.get(f)
            } else {
                None
            }
        })
        .collect();

    let added: Vec<&File> = request
        .files
        .clone()
        .iter()
        .filter_map(|f| {
            if untracked_files.contains(f) {
                untracked_files.get(f)
            } else {
                None
            }
        })
        .collect();

    info!("Added: {:?}, Modified: {:?}", added, modified);

    // Create prerevert snapshot including all files (both modified and untracked) before any operations
    if request.take_snapshot && (!added.is_empty() || !modified.is_empty()) {
        let all_files: Vec<String> = request.files.clone();
        let _ = state
            .git()
            .save_snapshot(
                "pre-revert",
                all_files,
                git::SaveSnapshotIndexOption::DiscardIndex,
            )
            .await?;
    }

    if !added.is_empty() {
        for file in &added {
            let path = repo_path.clone() + "/" + &file.path;
            fs::remove_file(&path)?;
        }
    }

    if !modified.is_empty() {
        let op = RevertFilesOp {
            git_client: state.git(),
            repo_status: state.repo_status.clone(),
            files: modified.iter().map(|f| f.path.clone()).collect(),
            engine: if request.skip_engine_check {
                None
            } else {
                Some(state.engine.clone())
            },
            take_snapshot: false, // Snapshot already taken above
        };

        sequence.push(Box::new(op));
    }

    // unlock reverted files
    if !request.files.is_empty() {
        let lock_paths = request.files.to_vec();
        let github_pat = state
            .app_config
            .read()
            .github_pat
            .clone()
            .ok_or(CoreError::Internal(anyhow!(
                "No github pat found. Please set a github pat in the config"
            )))?;
        let github_username = state.github_username();

        let lock_op = LockOp {
            git_client: state.git(),
            paths: lock_paths,
            op: LockOperation::Unlock,
            response_tx: None,
            github_pat: github_pat.to_string(),
            repo_status: state.repo_status.clone(),
            github_username,
            force: false,
        };

        sequence.push(Box::new(lock_op));
    }

    let _ = state.operation_tx.send(sequence).await;

    let res: Result<Option<CoreError>, RecvError> = rx.await;
    if let Ok(Some(e)) = res {
        return Err(e);
    }

    Ok(Json(String::from("OK")))
}
