use std::sync::mpsc::Sender;
use std::sync::Arc;

use anyhow::anyhow;
use axum::extract::State;
use axum::{async_trait, Json};
use tracing::info;

use super::switch::run_switch;
use super::{normalize_and_validate_slug, require_aws_and_storage, run_sequence};
use crate::engine::EngineProvider;
use crate::state::AppState;
use ethos_core::clients::git;
use ethos_core::clients::git::CreateRemoteBranchOutcome;
use ethos_core::types::config::RepoConfigRef;
use ethos_core::types::errors::CoreError;
use ethos_core::types::repo::{CreateBranchRequest, CreateBranchResponse, FEATURE_BRANCH_PREFIX};
use ethos_core::worker::Task;

#[derive(Clone)]
pub struct CreateBranchOp {
    /// Raw slug from the request; the op normalizes it.
    pub slug: String,
    pub repo_config: RepoConfigRef,
    pub git_client: git::Git,
    pub sync_phase_tx: Sender<String>,
    /// Set to the full branch name on success; the handler reads it.
    pub created: Arc<parking_lot::Mutex<Option<String>>>,
}

fn already_exists_error(name: &str) -> CoreError {
    CoreError::Input(anyhow!(
        "Branch '{name}' already exists. Choose a different name."
    ))
}

#[async_trait]
impl Task for CreateBranchOp {
    async fn execute(&self) -> Result<(), CoreError> {
        let slug = normalize_and_validate_slug(&self.slug)?;
        let name = format!("{FEATURE_BRANCH_PREFIX}{slug}");
        let trunk = self.repo_config.read().trunk_branch.clone();
        if trunk.is_empty() {
            return Err(CoreError::Input(anyhow!(
                "This repository's config does not define a trunk branch, so branches cannot be created."
            )));
        }

        let _ = self.sync_phase_tx.send(format!("Fetching {trunk}"));
        self.git_client
            .run(&["fetch", "origin", &trunk], git::Opts::default())
            .await?;

        if self.git_client.has_local_branch(&name).await? {
            return Err(CoreError::Input(anyhow!(
                "A local branch named '{name}' already exists. Choose a different name."
            )));
        }

        if self.git_client.has_remote_branch_exact(&name).await? {
            return Err(already_exists_error(&name));
        }

        let _ = self
            .sync_phase_tx
            .send(format!("Creating {name} on origin"));
        match self.git_client.create_remote_branch(&name, &trunk).await {
            Ok(CreateRemoteBranchOutcome::Created) => {}
            Ok(CreateRemoteBranchOutcome::AlreadyExists) => {
                return Err(already_exists_error(&name));
            }
            Err(e) => return Err(CoreError::Internal(e)),
        }

        info!("Created {name} on origin from origin/{trunk}");
        *self.created.lock() = Some(name);
        Ok(())
    }

    fn get_name(&self) -> String {
        String::from("RepoCreateBranch")
    }
}

/// Maps the switch onto a just-created branch to the response; only errors raised before
/// HEAD moved can leave the user off the new branch.
pub(crate) fn create_response(
    name: String,
    switch_result: Result<Option<String>, CoreError>,
) -> Result<CreateBranchResponse, CoreError> {
    match switch_result {
        Ok(message) => Ok(CreateBranchResponse {
            branch: name,
            switched: true,
            message,
        }),
        Err(CoreError::Input(e)) => {
            let message = format!(
                "Branch '{name}' was created on origin, but you were not switched to it: {e}"
            );
            Ok(CreateBranchResponse {
                branch: name,
                switched: false,
                message: Some(message),
            })
        }
        Err(e) => Err(CoreError::Internal(anyhow!(
            "Branch '{name}' was created, but switching to it failed: {e}"
        ))),
    }
}

pub async fn create_branch_handler<T>(
    State(state): State<AppState<T>>,
    Json(req): Json<CreateBranchRequest>,
) -> Result<Json<CreateBranchResponse>, CoreError>
where
    T: EngineProvider,
{
    // Checked up front so nothing is created when the switch could not run.
    let (aws_client, storage) = require_aws_and_storage(&state).await?;

    let created = Arc::new(parking_lot::Mutex::new(None));
    let op = CreateBranchOp {
        slug: req.slug,
        repo_config: state.repo_config.clone(),
        git_client: state.git(),
        sync_phase_tx: state.sync_phase_tx.clone(),
        created: created.clone(),
    };
    run_sequence(&state, Box::new(op)).await?;

    let name = created.lock().clone().ok_or_else(|| {
        CoreError::Internal(anyhow!(
            "Branch creation finished without reporting the branch name"
        ))
    })?;

    let switch_result = run_switch(&state, &name, aws_client, storage).await;
    create_response(name, switch_result).map(Json)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NAME: &str = "f11r-branch-x";

    #[test]
    fn create_response_switched() {
        let res = create_response(NAME.into(), Ok(None)).unwrap();
        assert!(res.switched);
        assert_eq!(res.message, None);

        let res = create_response(NAME.into(), Ok(Some("Switched, but".into()))).unwrap();
        assert!(res.switched);
        assert_eq!(res.message.as_deref(), Some("Switched, but"));
    }

    #[test]
    fn create_response_not_switched() {
        let res =
            create_response(NAME.into(), Err(CoreError::Input(anyhow!("Close Unreal")))).unwrap();
        assert!(!res.switched);
        assert_eq!(
            res.message.as_deref(),
            Some("Branch 'f11r-branch-x' was created on origin, but you were not switched to it: Close Unreal")
        );
    }

    #[test]
    fn create_response_internal_error() {
        match create_response(NAME.into(), Err(CoreError::Internal(anyhow!("boom")))) {
            Err(CoreError::Internal(e)) => assert_eq!(
                e.to_string(),
                "Branch 'f11r-branch-x' was created, but switching to it failed: boom"
            ),
            other => panic!("expected Internal error, got {other:?}"),
        }
    }
}
