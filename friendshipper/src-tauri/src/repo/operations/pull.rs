use std::path::PathBuf;
use std::sync::mpsc::Sender;

use anyhow::{anyhow, bail};
use axum::{async_trait, extract::State, Json};
use tokio::sync::oneshot::error::RecvError;
use tracing::{error, info, instrument};

use crate::engine::EngineProvider;
use ethos_core::clients::aws::ensure_aws_client;
use ethos_core::clients::git;
use ethos_core::clients::git::{PullStashStrategy, PullStrategy};
use ethos_core::clients::github::GraphQLClient;
use ethos_core::longtail::Longtail;
use ethos_core::msg::LongtailMsg;
use ethos_core::storage::{config::Project, ArtifactStorage};
use ethos_core::types::config::{AppConfigRef, RepoConfig, UProject};
use ethos_core::types::errors::CoreError;
use ethos_core::types::github::TokenNotFoundError;
use ethos_core::types::repo::PullResponse;
use ethos_core::worker::{Task, TaskSequence};
use ethos_core::AWSClient;

use crate::config::RepoConfigRef;
use crate::repo::operations::gh::submit::is_quicksubmit_branch;
use crate::repo::operations::UpdateEngineOp;
use crate::state::AppState;

use super::{DownloadDllsOp, RepoStatusRef, StatusOp};

#[derive(Clone)]
pub struct PullOp<T> {
    pub app_config: AppConfigRef,
    pub repo_config: RepoConfigRef,
    pub repo_status: RepoStatusRef,
    pub longtail: Longtail,
    pub longtail_tx: Sender<LongtailMsg>,
    pub aws_client: AWSClient,
    pub storage: ArtifactStorage,
    pub git_client: git::Git,
    pub github_client: Option<GraphQLClient>,
    pub engine: T,
}

#[async_trait]
impl<T> Task for PullOp<T>
where
    T: EngineProvider,
{
    #[instrument(name = "PullOp::execute", skip(self))]
    async fn execute(&self) -> anyhow::Result<()> {
        info!("Pulling repo");
        let github_username = self
            .github_client
            .clone()
            .map_or(String::default(), |x| x.username.clone());

        self.engine.check_ready_to_sync_repo().await?;

        {
            let status_op = StatusOp {
                repo_status: self.repo_status.clone(),
                app_config: self.app_config.clone(),
                repo_config: self.repo_config.clone(),
                engine: self.engine.clone(),
                git_client: self.git_client.clone(),
                github_username: github_username.clone(),
                aws_client: self.aws_client.clone(),
                storage: self.storage.clone(),
                skip_fetch: false,
                skip_dll_check: false,
                allow_offline_communication: false,
            };

            status_op.execute().await?;
        }

        // No need to hold a lock for this operation, but pass the ref directly to StatusOp so it can
        // make changes if necessary
        let app_config = self.app_config.read().clone();

        let branch: String;
        let owner: String;
        let repo_name: String;
        {
            let repo_status = self.repo_status.read();
            branch = repo_status.branch.clone();
            owner = repo_status.repo_owner.clone();
            repo_name = repo_status.repo_name.clone();
        }

        // The workflow for Quick Submit branches is that syncs switch back to main, preserving
        // whatever commits happened locally. This lets us cleanly resolve the commits made in the local
        // Quick Submit branch with the commits that have flowed through the merge queue, and avoids
        // potential conflicts when making another Quick Submit.
        let mut did_stash: bool = false;
        if is_quicksubmit_branch(&branch) {
            let github_client = match &self.github_client {
                Some(c) => c,
                None => return Err(anyhow!(TokenNotFoundError)),
            };
            let has_open_prs = github_client
                .is_branch_pr_open(&owner, &repo_name, &branch, 25)
                .await?;
            if has_open_prs {
                bail!("You may only sync when all Quick Submit changes have been merged.");
            }

            did_stash = self.git_client.stash(git::StashAction::Push).await?;
            let trunk_branch = self.repo_config.read().trunk_branch.clone();
            self.git_client.checkout(&trunk_branch).await?;

            // cleanup the old quicksubmit branch
            self.git_client
                .delete_branch(&branch, git::BranchType::Remote)
                .await?;
            self.git_client
                .delete_branch(&branch, git::BranchType::Local)
                .await?;

            // now that we're on main, make sure the status reflects this branch
            {
                let status_op = {
                    StatusOp {
                        repo_status: self.repo_status.clone(),
                        repo_config: self.repo_config.clone(),
                        engine: self.engine.clone(),
                        app_config: self.app_config.clone(),
                        git_client: self.git_client.clone(),
                        github_username: github_username.clone(),
                        aws_client: self.aws_client.clone(),
                        storage: self.storage.clone(),
                        skip_fetch: true,
                        skip_dll_check: false,
                        allow_offline_communication: false,
                    }
                };

                status_op.execute().await?;
            }
        } else {
            // If we're not on a Quick Submit branch, update the status and check for conflicts.
            let status_op = StatusOp {
                repo_status: self.repo_status.clone(),
                repo_config: self.repo_config.clone(),
                engine: self.engine.clone(),
                app_config: self.app_config.clone(),
                git_client: self.git_client.clone(),
                github_username: github_username.clone(),
                aws_client: self.aws_client.clone(),
                storage: self.storage.clone(),
                skip_fetch: true,
                skip_dll_check: false,
                allow_offline_communication: false,
            };

            status_op.execute().await?;

            let repo_status = self.repo_status.read();
            if !repo_status.conflicts.is_empty() {
                bail!("Conflicts detected, cannot pull. See Diagnostics.");
            }
        }

        // Check repo status to see if we need to pull at all.
        //
        // Note that we do NOT check to see if there are upstream conflicts if this is a Quick Submit branch. Typically
        // this shouldn't be an issue since most content creators will be using Quick Submit to submit changes, and checking for
        // conflicts after switching over from a Quick Submit branch will always yield false positives, as the commits from the
        // f11r branch will almost always have a different SHA since there will likely have been other changes that have gone in
        // since the submitter synced. Since we pull using a rebase, the local commits will be safely merged with the upstream ones
        // and essentially disappear.
        {
            let repo_status = self.repo_status.read();
            if repo_status.commits_behind == 0 {
                info!("no commits behind, skipping pull");

                return Ok(());
            }
        }

        let uproject_path_relative = self.repo_config.read().uproject_path.clone();
        let uproject_path = PathBuf::from(&app_config.repo_path).join(&uproject_path_relative);

        // save engine association before the .uproject potentially gets updated
        let old_uproject: Option<UProject> = match UProject::load(&uproject_path) {
            Err(e) => {
                error!(
                    "Failed to load uproject before sync, skipping engine update. Error: {}",
                    e
                );
                None
            }
            Ok(uproject) => Some(uproject),
        };

        self.git_client
            .pull(PullStrategy::Rebase, PullStashStrategy::Autostash)
            .await?;

        if did_stash {
            self.git_client.stash(git::StashAction::Pop).await?;
        }

        {
            let status_op = {
                StatusOp {
                    repo_status: self.repo_status.clone(),
                    app_config: self.app_config.clone(),
                    repo_config: self.repo_config.clone(),
                    engine: self.engine.clone(),
                    git_client: self.git_client.clone(),
                    github_username: github_username.clone(),
                    aws_client: self.aws_client.clone(),
                    storage: self.storage.clone(),
                    skip_fetch: true,
                    skip_dll_check: false,
                    allow_offline_communication: false,
                }
            };

            status_op.execute().await?;
        }

        let artifact_prefix = match app_config.selected_artifact_project.clone() {
            Some(project) => project,
            None => {
                return Err(anyhow!("No selected artifact project found in config."));
            }
        };

        if app_config.pull_dlls {
            match RepoConfig::get_project_name(&uproject_path_relative) {
                Some(project_name) => {
                    let download_op = DownloadDllsOp {
                        git_client: self.git_client.clone(),
                        project_name,
                        dll_commit: self.repo_status.read().dll_commit_remote.clone(),
                        download_symbols: self.app_config.read().editor_download_symbols,
                        storage: self.storage.clone(),
                        longtail: self.longtail.clone(),
                        tx: self.longtail_tx.clone(),
                        aws_client: self.aws_client.clone(),
                        artifact_prefix,
                        engine: self.engine.clone(),
                    };
                    download_op.execute().await?
                }
                None => {
                    error!("Unable to parse project name from uproject path {}. DLL download unavailable.", &uproject_path_relative);
                }
            }
        }

        let new_uproject: Option<UProject> = match UProject::load(&uproject_path) {
            Err(e) => {
                error!(
                    "Failed to load uproject after sync, skipping engine update. Error: {}",
                    e
                );
                None
            }
            Ok(uproject) => Some(uproject),
        };

        if new_uproject.is_some() && old_uproject.is_some() {
            let new_uproject = new_uproject.unwrap();
            let old_uproject = old_uproject.unwrap();

            info!(
                "Found engine association {} (previous was {}).",
                new_uproject.engine_association, old_uproject.engine_association
            );

            if new_uproject.engine_association != old_uproject.engine_association {
                let engine_path: PathBuf = app_config.get_engine_path(&new_uproject);

                let status = self.repo_status.read().clone();
                let project = if status.repo_owner.is_empty() || status.repo_name.is_empty() {
                    let (owner, repo) = match app_config.selected_artifact_project {
                        Some(ref project) => {
                            let (owner, repo) =
                                project.split_once('-').ok_or(anyhow!("Invalid project"))?;

                            (owner, repo)
                        }
                        None => {
                            return Err(anyhow!("No selected artifact project found in config."));
                        }
                    };

                    Project::new(owner, repo)
                } else {
                    Project::new(&status.repo_owner, &status.repo_name)
                };

                let update_engine_op = UpdateEngineOp {
                    engine_path,
                    old_uproject: Some(old_uproject.clone()),
                    new_uproject: new_uproject.clone(),
                    engine_type: app_config.engine_type,
                    longtail: self.longtail.clone(),
                    longtail_tx: self.longtail_tx.clone(),
                    aws_client: self.aws_client.clone(),
                    git_client: self.git_client.clone(),
                    download_symbols: app_config.engine_download_symbols,
                    storage: self.storage.clone(),
                    project,
                };
                update_engine_op.execute().await?;
            }
        }

        Ok(())
    }

    fn get_name(&self) -> String {
        String::from("RepoPull")
    }
}

#[instrument(skip(state))]
pub async fn pull_handler<T>(
    State(state): State<AppState<T>>,
) -> Result<Json<PullResponse>, CoreError>
where
    T: EngineProvider,
{
    let aws_client = ensure_aws_client(state.aws_client.read().await.clone())?;

    let storage = match state.storage.read().clone() {
        Some(storage) => storage,
        None => {
            return Err(CoreError(anyhow!(
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

    let (tx, rx) = tokio::sync::oneshot::channel::<Option<anyhow::Error>>();
    let mut sequence = TaskSequence::new().with_completion_tx(tx);
    sequence.push(Box::new(pull_op));
    let _ = state.operation_tx.send(sequence).await;

    info!("waiting for pull to complete");

    let res: Result<Option<anyhow::Error>, RecvError> = rx.await;
    if let Ok(Some(e)) = res {
        return Err(CoreError(e));
    }

    info!("pull completed");

    Ok(Json(PullResponse { conflicts: None }))
}
