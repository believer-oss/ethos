use anyhow::anyhow;
use axum::extract::State;
use axum::{async_trait, Json};

use crate::engine::EngineProvider;
use ethos_core::clients::git;
use ethos_core::types::errors::CoreError;
use ethos_core::worker::Task;

use crate::state::AppState;

type DiffResponse = Vec<String>;

#[derive(Clone)]
pub struct DiffOp {
    pub trunk_branch: String,
    pub git_client: git::Git,
}

#[async_trait]
impl Task for DiffOp {
    async fn execute(&self) -> Result<(), CoreError> {
        let _ = self.run().await?;

        Ok(())
    }

    fn get_name(&self) -> String {
        String::from("RepoDiff")
    }
}

impl DiffOp {
    pub async fn run(&self) -> anyhow::Result<DiffResponse> {
        let upstream_branch = format!("origin/{}", self.trunk_branch);

        let result = self.git_client.diff_filenames(&upstream_branch).await?;
        Ok(result)
    }
}

pub async fn diff_handler<T>(
    State(state): State<AppState<T>>,
) -> Result<Json<DiffResponse>, CoreError>
where
    T: EngineProvider,
{
    let diff_op = DiffOp {
        trunk_branch: state.repo_config.read().trunk_branch.clone(),
        git_client: state.git(),
    };

    match diff_op.run().await {
        Ok(output) => Ok(Json(output)),
        Err(e) => Err(CoreError::Internal(anyhow!("Error executing diff: {}", e))),
    }
}
