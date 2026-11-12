use axum::extract::State;
use axum::{async_trait, Json};
use tracing::error;

use crate::config::RepoConfigRef;
use crate::engine::EngineProvider;
use ethos_core::clients::git;
use ethos_core::types::config::AppConfigRef;
use ethos_core::types::errors::CoreError;
use ethos_core::worker::{Task, TaskSequence};

use super::{guard_op, guard_state, InstallGitHooksOp};
use crate::state::AppState;

#[derive(Clone)]
pub struct CheckoutOp {
    pub repo_path: String,
    pub branch: String,
    pub git_client: git::Git,
}

#[async_trait]
impl Task for CheckoutOp {
    async fn execute(&self) -> Result<(), CoreError> {
        self.git_client
            .run(&["checkout", &self.branch], Default::default())
            .await
            .map_err(CoreError::Internal)
    }

    fn get_name(&self) -> String {
        String::from("RepoCheckout")
    }
}

pub async fn checkout_trunk_handler<T>(
    State(state): State<AppState<T>>,
) -> Result<Json<String>, CoreError>
where
    T: EngineProvider,
{
    guard_state(&state).await?;

    // Block on any other fetch-like operations in the queue
    let (tx, rx) = tokio::sync::oneshot::channel::<Option<CoreError>>();
    let mut sequence = TaskSequence::new().with_completion_tx(tx);
    sequence.push(guard_op(&state));

    let op = {
        let app_config = state.app_config.read().clone();
        let repo_config = state.repo_config.read();

        CheckoutOp {
            repo_path: app_config.repo_path.clone(),
            branch: repo_config.trunk_branch.clone(),
            git_client: state.git(),
        }
    };

    sequence.push(Box::new(op));

    let _ = state.operation_tx.send(sequence).await;
    if let Ok(Some(e)) = rx.await {
        return Err(e);
    }

    Ok(Json(String::from("OK")))
}

pub async fn checkout_target_branch_handler<T>(
    State(state): State<AppState<T>>,
) -> Result<Json<String>, CoreError>
where
    T: EngineProvider,
{
    guard_state(&state).await?;

    // Block on any other fetch-like operations in the queue
    let (tx, rx) = tokio::sync::oneshot::channel::<Option<CoreError>>();
    let mut sequence = TaskSequence::new().with_completion_tx(tx);
    sequence.push(guard_op(&state));

    let op = {
        let app_config = state.app_config.read().clone();

        CheckoutOp {
            repo_path: app_config.repo_path.clone(),
            branch: app_config.target_branch.clone(),
            git_client: state.git(),
        }
    };

    sequence.push(Box::new(op));
    sequence.push(Box::new(ReloadRepoConfigOp {
        app_config: state.app_config.clone(),
        repo_config: state.repo_config.clone(),
    }));

    let _ = state.operation_tx.send(sequence).await;
    if let Ok(Some(e)) = rx.await {
        return Err(e);
    }

    Ok(Json(String::from("OK")))
}

/// Reloads friendshipper.yaml and the git hooks from the working tree, so a
/// new target branch takes effect without restarting the app.
#[derive(Clone)]
pub struct ReloadRepoConfigOp {
    pub app_config: AppConfigRef,
    pub repo_config: RepoConfigRef,
}

#[async_trait]
impl Task for ReloadRepoConfigOp {
    async fn execute(&self) -> Result<(), CoreError> {
        let (repo_path, repo_config, valid) = {
            let app_config = self.app_config.read();
            let repo_config = app_config.initialize_repo_config()?;
            let valid = app_config.validate_configured_branches(&repo_config);
            (app_config.repo_path.clone(), repo_config, valid)
        };

        if let Some(git_hooks_path) = repo_config.git_hooks_path.clone() {
            let op = InstallGitHooksOp {
                repo_path,
                git_hooks_path,
            };
            if let Err(e) = op.execute().await {
                error!("Failed to install git hooks: {}", e);
            }
        }

        *self.repo_config.write() = repo_config;

        // As at startup: the branch is checked out and its config loaded, but report a mismatch.
        valid.map_err(|e| {
            CoreError::Input(anyhow::anyhow!(
                "App configuration error: {e}. Please check your friendshipper.yaml target branches or update your branch configuration."
            ))
        })
    }

    fn get_name(&self) -> String {
        String::from("ReloadRepoConfig")
    }
}
