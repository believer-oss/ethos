use std::sync::Arc;

use anyhow::anyhow;
use axum::extract::State;
use axum::{async_trait, Json};

use ethos_core::clients::git;
use ethos_core::types::errors::CoreError;
use ethos_core::worker::Task;

use crate::state::AppState;

use super::RepoStatusRef;

type DiffResponse = Vec<String>;

#[derive(Clone)]
pub struct DiffOp {
    pub repo_path: String,
    pub repo_status: RepoStatusRef,
    pub git_client: git::Git,
}

#[async_trait]
impl Task for DiffOp {
    async fn execute(&self) -> anyhow::Result<()> {
        let _ = self.run().await?;

        Ok(())
    }

    fn get_name(&self) -> String {
        String::from("RepoDiff")
    }
}

impl DiffOp {
    pub async fn run(&self) -> anyhow::Result<DiffResponse> {
        let upstream_branch = self.repo_status.read().remote_branch.clone();

        let output = self.git_client.diff_filenames(&upstream_branch).await?;

        let mut result = output.lines().map(|s| s.to_string()).collect::<Vec<_>>();
        result.dedup();

        Ok(result)
    }
}

pub async fn diff_handler(
    State(state): State<Arc<AppState>>,
) -> Result<Json<DiffResponse>, CoreError> {
    let diff_op = DiffOp {
        repo_status: state.repo_status.clone(),
        repo_path: state.app_config.read().repo_path.clone(),
        git_client: state.git(),
    };

    match diff_op.run().await {
        Ok(output) => Ok(Json(output)),
        Err(e) => Err(CoreError(anyhow!(
            "Error executing diff: {}",
            e.to_string()
        ))),
    }
}
