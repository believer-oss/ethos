use axum::extract::State;
use axum::{async_trait, Json};

use crate::engine::EngineProvider;
use ethos_core::clients::git;
use ethos_core::types::errors::CoreError;
use ethos_core::worker::{Task, TaskSequence};

use crate::state::AppState;

pub const CONTENT_BRANCH_NAME: &str = "content-main";

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
    // Block on any other fetch-like operations in the queue
    let (tx, rx) = tokio::sync::oneshot::channel::<Option<CoreError>>();
    let mut sequence = TaskSequence::new().with_completion_tx(tx);

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
    let _ = rx.await;

    Ok(Json(String::from("OK")))
}

pub async fn checkout_content_branch_handler<T>(
    State(state): State<AppState<T>>,
) -> Result<Json<String>, CoreError>
where
    T: EngineProvider,
{
    // Block on any other fetch-like operations in the queue
    let (tx, rx) = tokio::sync::oneshot::channel::<Option<CoreError>>();
    let mut sequence = TaskSequence::new().with_completion_tx(tx);

    let op = {
        let app_config = state.app_config.read().clone();

        CheckoutOp {
            repo_path: app_config.repo_path.clone(),
            branch: CONTENT_BRANCH_NAME.to_string(),
            git_client: state.git(),
        }
    };

    sequence.push(Box::new(op));

    let _ = state.operation_tx.send(sequence).await;
    let _ = rx.await;

    Ok(Json(String::from("OK")))
}
