use std::sync::Arc;

use axum::extract::State;
use axum::{async_trait, debug_handler, Json};

use ethos_core::clients::git;
use ethos_core::types::errors::CoreError;
use ethos_core::worker::{Task, TaskSequence};

use crate::state::AppState;

#[derive(Clone)]
pub struct CheckoutOp {
    pub repo_path: String,
    pub branch: String,
    pub git_client: git::Git,
}

#[async_trait]
impl Task for CheckoutOp {
    async fn execute(&self) -> anyhow::Result<()> {
        self.git_client
            .run(&["checkout", &self.branch], Default::default())
            .await
    }

    fn get_name(&self) -> String {
        String::from("RepoCheckout")
    }
}

#[debug_handler]
pub async fn checkout_trunk_handler(
    State(state): State<Arc<AppState>>,
) -> Result<Json<String>, CoreError> {
    // Block on any other fetch-like operations in the queue
    let (tx, rx) = tokio::sync::oneshot::channel::<Option<anyhow::Error>>();
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
