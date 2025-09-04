use std::path::PathBuf;
use std::sync::mpsc::Sender;

use anyhow::{anyhow, Context};
use axum::{async_trait, extract::State, Json};
use ethos_core::storage::config::Project;
use tokio::sync::oneshot::error::RecvError;
use tracing::{error, info, instrument};

use ethos_core::clients::aws::ensure_aws_client;
use ethos_core::clients::git;
use ethos_core::clients::git::{PullStashStrategy, PullStrategy};
use ethos_core::clients::github::GraphQLClient;
use ethos_core::longtail::Longtail;
use ethos_core::msg::LongtailMsg;
use ethos_core::storage::ArtifactStorage;
use ethos_core::types::config::{AppConfigRef, RepoConfig, UProject};
use ethos_core::types::errors::CoreError;
use ethos_core::types::repo::PullResponse;
use ethos_core::worker::{Task, TaskSequence};
use ethos_core::AWSClient;

use crate::config::RepoConfigRef;
use crate::engine::EngineProvider;
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
    async fn execute(&self) -> Result<(), CoreError> {
        // We stash changes when switching back to main to avoid cases where local changes may conflict
        // with changes on main. If the stash wasn't restored for whatever reason (e.g. early out due
        // to no changes, or an error)
        self.execute_internal().await
    }

    fn get_name(&self) -> String {
        String::from("RepoPull")
    }
}

impl<T> PullOp<T>
where
    T: EngineProvider,
{
    async fn execute_internal(&self) -> Result<(), CoreError> {
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
                aws_client: None,
                storage: None,
                allow_offline_communication: false,
                skip_display_names: true,
                skip_engine_update: false,
            };

            status_op.execute().await?;
        }

        // Check for conflicts with incoming changes before creating snapshot
        let current_branch = self.git_client.current_branch().await?;
        let branch_for_conflict_check = if is_quicksubmit_branch(&current_branch) {
            // For f11r branches, check conflicts against the target branch since that's what we'll actually pull from
            self.app_config.read().target_branch.clone()
        } else {
            current_branch.clone()
        };
        let sync_conflicts = self
            .git_client
            .check_sync_vs_untracked_file_conflicts(&branch_for_conflict_check)
            .await?;
        if !sync_conflicts.is_empty() {
            return Err(CoreError::Input(anyhow!(
                "Sync failed: The following untracked local files conflict with incoming committed changes: {}\n\
                Please rename or move these local files before syncing, or commit them to resolve the conflict.",
                sync_conflicts.join(", ")
            )));
        }

        // take a snapshot if we have any modified files
        // we need to do this before we check for quicksubmit branch so that the
        // stashes resolve inside out correctly
        let mut snapshot = None;
        let mut snapshot_modified_files = vec![];
        let repo_status = self.repo_status.read().clone();
        if !repo_status.modified_files.is_empty() || !repo_status.untracked_files.is_empty() {
            // Capture the modified files at snapshot time for restore_snapshot
            snapshot_modified_files = repo_status.modified_files.0.clone();
            snapshot = Some(
                self.git_client
                    .save_snapshot_all("pre-pull", git::SaveSnapshotIndexOption::DiscardIndex)
                    .await?,
            );
        }

        // No need to hold a lock for this operation, but pass the ref directly to StatusOp so it can
        // make changes if necessary
        let app_config = self.app_config.read().clone();

        let branch: String;
        {
            let repo_status = self.repo_status.read();
            branch = repo_status.branch.clone();
        }

        // The workflow for Quick Submit branches is that syncs switch back to main, preserving
        // whatever commits happened locally. This lets us cleanly resolve the commits made in the local
        // Quick Submit branch with the commits that have flowed through the merge queue, and avoids
        // potential conflicts when making another Quick Submit.
        if is_quicksubmit_branch(&branch) {
            let target_branch = self.app_config.read().target_branch.clone();
            self.git_client.checkout(&target_branch).await?;

            // cleanup the old quicksubmit branch
            if self.git_client.has_remote_branch(&branch).await? {
                self.git_client
                    .delete_branch(&branch, git::BranchType::Remote)
                    .await?;
            }
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
                        aws_client: Some(self.aws_client.clone()),
                        storage: Some(self.storage.clone()),
                        allow_offline_communication: false,
                        skip_display_names: true,
                        skip_engine_update: true,
                    }
                };

                status_op.execute().await?;
            }
        } else {
            // If we're not on a Quick Submit branch, just check for conflicts from the last status check
            //
            // Note that we do NOT check to see if there are upstream conflicts if this is a Quick Submit branch. Typically
            // this shouldn't be an issue since most content creators will be using Quick Submit to submit changes, and checking for
            // conflicts after switching over from a Quick Submit branch will always yield false positives, as the commits from the
            // f11r branch will almost always have a different SHA since there will likely have been other changes that have gone in
            // since the submitter synced. Since we pull using a rebase, the local commits will be safely merged with the upstream ones
            // and essentially disappear.

            let repo_status = self.repo_status.read();
            if !repo_status.conflicts.is_empty() {
                return Err(CoreError::Input(anyhow!(
                    "Conflicts detected, cannot pull. See Diagnostics."
                )));
            }
        }

        // Check repo status to see if we need to pull at all.
        // Skip this check if this is the first sync after startup (last_updated is Unix epoch),
        // because commits_behind may be 0 due to stale remote refs before any fetch occurs.
        {
            let repo_status = self.repo_status.read().clone();
            let is_first_sync = repo_status.last_updated == chrono::DateTime::UNIX_EPOCH;

            if !is_first_sync && repo_status.commits_behind == 0 {
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

        // run git pull but retry one time if it fails
        match self
            .git_client
            .pull(PullStrategy::Rebase, PullStashStrategy::Autostash)
            .await
        {
            Ok(_) => {}
            Err(e) => {
                error!("Failed to pull, retrying once. Error: {}", e);
                self.git_client
                    .pull(PullStrategy::Rebase, PullStashStrategy::Autostash)
                    .await?;
            }
        }

        // Collect any errors from the following operations, but continue where possible
        // This needs to remain serial because we don't have support for multiple concurrent
        // progress bars, and longtail already saturates most connections.
        let mut errors: Vec<Option<CoreError>> = Vec::new();

        // Attempt to restore any snapshots that were made above.
        if let Some(snapshot) = snapshot {
            errors.push(
                self.git_client
                    .restore_snapshot(&snapshot.commit, snapshot_modified_files, false)
                    .await
                    .map_err(|e| e.into())
                    .err(),
            );
        }

        // Refresh the repo status with new information from the pull
        {
            let status_op = {
                StatusOp {
                    repo_status: self.repo_status.clone(),
                    app_config: self.app_config.clone(),
                    repo_config: self.repo_config.clone(),
                    engine: self.engine.clone(),
                    git_client: self.git_client.clone(),
                    github_username: github_username.clone(),
                    aws_client: Some(self.aws_client.clone()),
                    storage: Some(self.storage.clone()),
                    allow_offline_communication: false,
                    skip_display_names: true,
                    skip_engine_update: false,
                }
            };

            errors.push(status_op.execute().await.err());
        }

        // Download DLL artifacts or engine updates from the new pull
        {
            let new_uproject: Option<UProject> = UProject::load(&uproject_path)
                .map_err(|e| {
                    errors.push(Some(
                        e.context(
                            "Failed to load uproject after sync, skipping dll and engine update. Error: {}",
                        )
                        .into(),
                    ))
                })
                .ok();

            // We should have a selected_artifact_project because how else did we pull?
            let project: Project = self
                .app_config
                .read()
                .selected_artifact_project
                .clone()
                .context("No selected artifact project found in config.")?
                .as_str()
                .into();

            if let Some(uproject) = new_uproject {
                if app_config.pull_dlls {
                    let engine_path = self.app_config.read().get_engine_path(&uproject);

                    let project = project.clone();

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
                                project,
                                engine: self.engine.clone(),
                                engine_path,
                            };
                            errors.push(download_op.execute().await.err())
                        }
                        None => {
                            let e = CoreError::Input(
                                anyhow!("Unable to parse project name from uproject path {}. DLL download unavailable.", &uproject_path_relative)
                            );
                            errors.push(Some(e))
                        }
                    }
                }

                if let Some(old_uproject) = old_uproject {
                    info!(
                        "Found engine association {} (previous was {}).",
                        uproject.engine_association, old_uproject.engine_association
                    );

                    if uproject.engine_association != old_uproject.engine_association {
                        let engine_path: PathBuf = app_config.get_engine_path(&uproject);

                        let update_engine_op = UpdateEngineOp {
                            engine_path,
                            old_uproject: Some(old_uproject.clone()),
                            new_uproject: uproject.clone(),
                            engine_type: app_config.engine_type,
                            longtail: self.longtail.clone(),
                            longtail_tx: self.longtail_tx.clone(),
                            aws_client: self.aws_client.clone(),
                            git_client: self.git_client.clone(),
                            download_symbols: app_config.engine_download_symbols,
                            storage: self.storage.clone(),
                            project,
                            engine: self.engine.clone(),
                        };
                        errors.push(update_engine_op.execute().await.err());
                    }
                }
            }
        }

        // Check if there are any actual errors (not just None values)
        let actual_errors: Vec<String> = errors.iter().flatten().map(|e| format!("{e}")).collect();
        if !actual_errors.is_empty() {
            error!("Failures during repo pull: {}", actual_errors.join("\n"));
            return Err(CoreError::Internal(anyhow!(
                "Failures during repo pull: {}",
                actual_errors.join("\n")
            )));
        }

        Ok(())
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
            return Err(CoreError::Internal(anyhow!(
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

    let (tx, rx) = tokio::sync::oneshot::channel::<Option<CoreError>>();
    let mut sequence = TaskSequence::new().with_completion_tx(tx);
    sequence.push(Box::new(pull_op));
    let _ = state.operation_tx.send(sequence).await;

    let res: Result<Option<CoreError>, RecvError> = rx.await;
    if let Ok(Some(e)) = res {
        return Err(e);
    }

    Ok(Json(PullResponse { conflicts: None }))
}
