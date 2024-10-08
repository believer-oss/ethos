use anyhow::anyhow;
use axum::async_trait;
use axum::extract::Query;
use axum::extract::State;
use serde::Deserialize;
use std::{fs, path::PathBuf};
use tokio::sync::oneshot::error::RecvError;
use tracing::error;
use tracing::{debug, info};

use crate::engine::EngineProvider;
use ethos_core::types::errors::CoreError;
use ethos_core::worker::{Task, TaskSequence};

use crate::state::AppState;

#[cfg(not(target_os = "windows"))]
use std::os::unix::fs::PermissionsExt;

#[cfg(not(target_os = "windows"))]
use std::fs::Permissions;

#[derive(Default, Deserialize)]
pub struct InstallGitHooksParams {
    refresh: bool,
}

#[derive(Clone)]
pub struct InstallGitHooksOp {
    pub repo_path: String,
    pub git_hooks_path: String,
}

#[async_trait]
impl Task for InstallGitHooksOp {
    async fn execute(&self) -> Result<(), CoreError> {
        if self.repo_path.is_empty() {
            return Err(CoreError::Input(anyhow!(
                "Failed to install git hooks - repo_path is not set"
            )));
        }
        if self.git_hooks_path.is_empty() {
            return Err(CoreError::Input(anyhow!(
                "Failed to install git hooks - git_hooks_path is not set"
            )));
        }

        let git_hooks_path_source = {
            let mut buf = PathBuf::new();
            buf.push(&self.repo_path);
            buf.push(&self.git_hooks_path);
            buf
        };

        let git_hooks_path_dest = {
            let mut buf = PathBuf::new();
            buf.push(&self.repo_path);
            buf.push(".git/hooks");
            buf
        };

        info!(
            "Copying git hooks from dir {:?} to {:?}",
            git_hooks_path_source, git_hooks_path_dest
        );

        for entry in fs::read_dir(&git_hooks_path_source)?.flatten() {
            if let Ok(filetype) = entry.file_type() {
                if filetype.is_file() {
                    let mut destination = git_hooks_path_dest.clone();
                    destination.push(entry.file_name());

                    debug!("copying {:?} to {:?}", entry.path(), destination);
                    fs::copy(entry.path(), &destination)?;

                    #[cfg(not(target_os = "windows"))]
                    {
                        let perm = Permissions::from_mode(0o755);
                        fs::set_permissions(destination, perm).unwrap();
                    }
                }
            }
        }

        Ok(())
    }

    fn get_name(&self) -> String {
        String::from("InstallGitHooks")
    }
}

pub async fn install_git_hooks_handler<T>(
    State(state): State<AppState<T>>,
    params: Query<InstallGitHooksParams>,
) -> Result<(), CoreError>
where
    T: EngineProvider,
{
    if params.refresh {
        let config = state.app_config.read();
        let repo_config = config.initialize_repo_config()?;

        let mut lock = state.repo_config.write();
        *lock = repo_config.clone();
    }

    let git_hooks_path = state.repo_config.read().git_hooks_path.clone();
    if git_hooks_path.is_none() {
        return Err(CoreError::Internal(anyhow!(
            "Git hooks path is unset in friendshipper.yaml"
        )));
    }

    let op = InstallGitHooksOp {
        repo_path: state.app_config.read().repo_path.clone(),
        git_hooks_path: git_hooks_path.unwrap(),
    };

    let (tx, rx) = tokio::sync::oneshot::channel::<Option<CoreError>>();
    let mut sequence = TaskSequence::new().with_completion_tx(tx);
    sequence.push(Box::new(op));
    let _ = state.operation_tx.send(sequence).await;
    let res: Result<Option<CoreError>, RecvError> = rx.await;
    match res {
        Ok(operation_error) => {
            if let Some(e) = operation_error {
                error!("Failed to install git hook: {}", e);
            }
        }
        Err(e) => {
            error!("Failed to install git hook: {}", e);
        }
    }

    Ok(())
}
