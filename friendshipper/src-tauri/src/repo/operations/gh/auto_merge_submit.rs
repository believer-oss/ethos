use crate::engine::{CommunicationType, EngineProvider};
use crate::repo::operations::gh::submit::is_quicksubmit_branch;
use crate::repo::operations::{GitHubSubmitOp, StatusOp};
use crate::repo::RepoStatusRef;
use crate::state::AppState;
use anyhow::anyhow;
use async_trait::async_trait;
use axum::extract::State;
use axum::Json;
use ethos_core::clients::git::SaveSnapshotIndexOption;
use ethos_core::clients::{git, github};
use ethos_core::operations::{AddOp, CommitOp, LockOp, RestoreOp};
use ethos_core::storage::ArtifactStorage;
use ethos_core::types::config::{AppConfigRef, RepoConfigRef};
use ethos_core::types::errors::CoreError;
use ethos_core::types::github::TokenNotFoundError;
use ethos_core::types::locks::LockOperation;
use ethos_core::types::repo::{File, PushRequest, SubmitStatus};
use ethos_core::worker::{Task, TaskSequence};
use ethos_core::AWSClient;
use std::path::PathBuf;
use tracing::{instrument, warn};

#[derive(Clone)]
pub struct AutoMergeSubmitOp<T>
where
    T: EngineProvider,
{
    pub files: Vec<String>,
    pub commit_message: String,

    pub app_config: AppConfigRef,
    pub repo_config: RepoConfigRef,
    pub engine: T,
    pub aws_client: Option<AWSClient>,
    pub storage: Option<ArtifactStorage>,
    pub repo_status: RepoStatusRef,

    pub git_client: git::Git,
    pub token: String,
    pub github_client: github::GraphQLClient,
}

#[async_trait]
impl<T> Task for AutoMergeSubmitOp<T>
where
    T: EngineProvider,
{
    #[instrument(name = "AutoMergeSubmitOp::execute", skip(self))]
    async fn execute(&self) -> Result<(), CoreError> {
        // abort if there are no files to submit
        if self.files.is_empty() {
            return Err(CoreError::Input(anyhow!("No files to submit")));
        }

        // abort if we are trying to submit any conflicted files, or files that should be locked, but aren't
        {
            let repo_status = self.repo_status.read().clone();
            let mut unsubmittable_files: Vec<File> = vec![];

            for file in self.files.iter() {
                let mut all_modified_iter = repo_status
                    .modified_files
                    .0
                    .iter()
                    .chain(repo_status.untracked_files.0.iter());
                if let Some(file) = all_modified_iter.find(|x| x.path == *file) {
                    match file.submit_status {
                        SubmitStatus::Ok => {}
                        _ => unsubmittable_files.push(file.clone()),
                    }
                }
            }

            if !unsubmittable_files.is_empty() {
                let engine_path = self
                    .app_config
                    .read()
                    .load_engine_path_from_repo(&self.repo_config.read())
                    .unwrap_or_default();
                let unsubmittable_file_paths: Vec<String> =
                    unsubmittable_files.iter().map(|x| x.path.clone()).collect();

                let unsubmittable_display_names = self
                    .engine
                    .get_asset_display_names(
                        CommunicationType::None,
                        &engine_path,
                        &unsubmittable_file_paths,
                    )
                    .await;

                for (file, display_name) in unsubmittable_files
                    .iter()
                    .zip(unsubmittable_display_names.iter())
                {
                    let name_formatted: String = if display_name.is_empty() {
                        file.path.clone()
                    } else {
                        format!("{} ({})", display_name, file.path)
                    };
                    let reason = match file.submit_status {
                        SubmitStatus::Ok => panic!("should have been filtered out by above code"),
                        SubmitStatus::CheckoutRequired => "This file is an asset and must be checked out (locked) before submitting",
                        SubmitStatus::CheckedOutByOtherUser => "This file is an asset and must be checked out (locked) before submitting, but it is locked by another user",
                        SubmitStatus::Unmerged => "This file is unmerged and must be reverted to continue",
                        SubmitStatus::Conflicted => "A newer version of this file exists; this file must be reverted to continue",
                    };
                    tracing::error!("{}: {}", reason, name_formatted);
                }
                return Err(CoreError::Input(anyhow!("Some files are not allowed to be submitted. Check the log for specific errors.")));
            }
        }

        // save a snapshot before submitting with all modified/added files
        // make sure we have a temp dir for copying our files
        let status = self.repo_status.read().clone();
        let modified_files = status.modified_files.0.clone();
        let untracked_files = status.untracked_files.0.clone();
        let all_files = modified_files
            .into_iter()
            .chain(untracked_files.into_iter())
            .map(|file| file.path.clone())
            .collect();
        let snapshot = self
            .git_client
            .save_snapshot("pre-submit", all_files, SaveSnapshotIndexOption::KeepIndex)
            .await?;

        match self.execute_internal().await {
            Ok(_) => Ok(()),
            Err(e) => {
                // can't touch the working tree unless the engine isn't running
                if self.engine.check_ready_to_sync_repo().await.is_ok() {
                    // attempt to reset to original branch and restore snapshot
                    // if this fails for any reason, we should simply log, then return the original error
                    let branch = self.repo_status.read().branch.clone();
                    let conflict_strategy = self.app_config.read().conflict_strategy.clone();
                    self.git_client.hard_reset(&branch).await?;

                    match self
                        .git_client
                        .restore_snapshot(&snapshot.commit, vec![], conflict_strategy)
                        .await
                    {
                        Ok(_) => {}
                        Err(e) => {
                            // log the error, but don't return it
                            warn!("Failed to restore snapshot after failed submit: {}", e);
                        }
                    }
                } else {
                    warn!("Unable to automatically restore pre-submit state due to editor running.")
                }
                Err(e)
            }
        }
    }

    fn get_name(&self) -> String {
        "AutoMergeSubmitOp".to_string()
    }
}

impl<T> AutoMergeSubmitOp<T>
where
    T: EngineProvider,
{
    #[instrument(name = "AutoMergeSubmitOp::execute_internal", skip(self))]
    pub async fn execute_internal(&self) -> Result<(), CoreError> {
        let target_branch = self.app_config.read().target_branch.clone();
        let prev_branch = self.repo_status.read().branch.clone();

        let f11r_branch = {
            let display_name = &self.app_config.read().user_display_name;
            let santized_display_name = display_name.replace(' ', "-");
            format!(
                "f11r-{}-{}-{}",
                target_branch,
                santized_display_name,
                chrono::Utc::now().timestamp()
            )
        };

        self.git_client
            .run(&["checkout", "-b", &f11r_branch], Default::default())
            .await?;

        // Clean up the old f11r branch, if it was one
        if is_quicksubmit_branch(&prev_branch) {
            self.git_client
                .delete_branch(&prev_branch, git::BranchType::Local)
                .await?;
        }

        let status_op = StatusOp {
            repo_status: self.repo_status.clone(),
            app_config: self.app_config.clone(),
            repo_config: self.repo_config.clone(),
            engine: self.engine.clone(),
            git_client: self.git_client.clone(),
            github_username: self.github_client.username.clone(),
            aws_client: None,
            storage: None,
            allow_offline_communication: false,
            skip_display_names: true,
            skip_engine_update: false,
        };

        // commit changes
        {
            let add_op = AddOp {
                files: self.files.clone(),
                git_client: self.git_client.clone(),
            };

            add_op.execute().await?;

            // unstage any files that are staged but not in the request
            let mut staged_files = Vec::new();
            {
                let repo_status = self.repo_status.read();
                let modified = repo_status.modified_files.clone();
                for file in modified.into_iter() {
                    if file.is_staged {
                        staged_files.push(file.path.clone());
                    }
                }
            }

            let files_to_unstage: Vec<String> = staged_files
                .into_iter()
                .filter(|file| !self.files.contains(file))
                .collect();

            if !files_to_unstage.is_empty() {
                let restore_op = RestoreOp {
                    files: files_to_unstage,
                    git_client: self.git_client.clone(),
                };

                restore_op.execute().await?;
            }

            // We can skip the status check because we know for a fact that there are staged files
            let commit_op = CommitOp {
                message: self.commit_message.clone(),
                repo_status: self.repo_status.clone(),
                git_client: self.git_client.clone(),
                skip_status_check: true,
            };

            commit_op.execute().await?;

            // push up the branch - this way the commit's files are saved to the remote
            self.git_client.push(&f11r_branch).await?;

            // update status now that the files have been committed and there aren't any more
            // staged files
            status_op.execute().await?;
        }

        if is_quicksubmit_branch(&prev_branch) {
            let worktree_path: PathBuf = 'path: {
                let repo_path = PathBuf::from(self.app_config.read().repo_path.clone());

                let worktrees = self.git_client.list_worktrees().await?;
                for tree in worktrees.iter() {
                    if tree.directory != repo_path {
                        // if the directory exists on disk, break
                        if tree.directory.exists() {
                            break 'path tree.directory.clone();
                        }

                        // if the directory doesn't exist, remove the worktree
                        self.git_client
                            .run(
                                &[
                                    "worktree",
                                    "remove",
                                    tree.directory.to_string_lossy().as_ref(),
                                ],
                                Default::default(),
                            )
                            .await?;
                    }
                }

                let repo_folder_name: String = repo_path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();

                // create worktree if it doesn't exist yet
                let mut worktree_path = repo_path.clone();
                worktree_path.pop();
                worktree_path.push(format!(".{}-wt", repo_folder_name));

                self.git_client
                    .run(
                        &[
                            "worktree",
                            "add",
                            "--detach",
                            &worktree_path.to_string_lossy(),
                        ],
                        git::Opts::default().with_lfs_stubs(),
                    )
                    .await?;

                worktree_path.clone()
            };

            let worktree_branch = format!("{}-wt", f11r_branch);

            let mut git_client_worktree = self.git_client.clone();
            git_client_worktree.repo_path.clone_from(&worktree_path);

            // To make the worktree as cheap as possible, we need to make sure no LFS files are checked out and
            // they remain stubs
            let git_opts_lfs_stubs = git::Opts::default().with_lfs_stubs();

            // resolve changes with latest main and push up to the remote
            {
                let worktree_prev_branch = git_client_worktree.current_branch().await?;

                // delete the worktree branch if it exists - we need to make one that matches the state of
                // f11r_branch exactly, and the old worktree branch will likely have changes from main mixed
                // up into it.
                if worktree_branch == worktree_prev_branch {
                    _ = git_client_worktree
                        .run(&["checkout", "--detach"], git_opts_lfs_stubs)
                        .await;
                    _ = git_client_worktree
                        .delete_branch(&worktree_branch, git::BranchType::Local)
                        .await;
                }

                // Checkout a new branch for the worktree in the same state as the f11r branch
                self.git_client
                    .run(&["branch", &worktree_branch], git::Opts::default())
                    .await?;

                // now we can resolve any new changes in main with the current changes and push up to the remote
                git_client_worktree
                    .run(&["checkout", &worktree_branch], git_opts_lfs_stubs)
                    .await?;
                git_client_worktree
                    .run(&["fetch", "origin", &*target_branch], git_opts_lfs_stubs)
                    .await?;
                git_client_worktree
                    .run(
                        &["rebase", &format!("origin/{}", target_branch)],
                        git_opts_lfs_stubs,
                    )
                    .await?;

                // force is needed when pushing changes because we may be reusing a remote branch
                git_client_worktree
                    .run(
                        &["push", "-f", "origin", &worktree_branch],
                        git::Opts::default(),
                    )
                    .await?;

                // cleanup old branch
                if worktree_branch != worktree_prev_branch
                    && is_quicksubmit_branch(&worktree_prev_branch)
                {
                    git_client_worktree
                        .delete_branch(&worktree_prev_branch, git::BranchType::Local)
                        .await?;
                }
            }

            // Submit the PR to the merge queue
            let gh_op = GitHubSubmitOp {
                head_branch: worktree_branch.clone(),
                base_branch: target_branch.to_string(),
                token: self.token.clone(),
                commit_message: self.commit_message.clone(),
                repo_status: self.repo_status.clone(),
                client: self.github_client.clone(),
                enable_auto_merge: true,
            };

            gh_op.execute().await?;

            // cleanup worktree branch
            _ = git_client_worktree
                .run(&["checkout", "--detach"], git_opts_lfs_stubs)
                .await;
            _ = git_client_worktree
                .delete_branch(&worktree_branch, git::BranchType::Local)
                .await;

            return Ok(());
        }

        let gh_op = GitHubSubmitOp {
            head_branch: f11r_branch.clone(),
            base_branch: target_branch.to_string(),
            token: self.token.clone(),
            commit_message: self.commit_message.clone(),
            repo_status: self.repo_status.clone(),
            client: self.github_client.clone(),
            enable_auto_merge: true,
        };

        gh_op.execute().await?;
        let owner = self.repo_status.read().repo_owner.clone();
        let repo_name = self.repo_status.read().repo_name.clone();

        let start = std::time::Instant::now();
        let mut has_open_prs = true;
        while has_open_prs && start.elapsed().as_secs() < 10 {
            has_open_prs = self
                .github_client
                .is_branch_pr_open(&owner, &repo_name, &f11r_branch, 25)
                .await?;
            if has_open_prs {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
        }

        // unlock all files submitted
        let github_username = self.github_client.username.clone();
        let lock_op = LockOp {
            git_client: self.git_client.clone(),
            paths: self.files.clone(),
            op: LockOperation::Unlock,
            response_tx: None,
            github_pat: self.token.clone(),
            repo_status: self.repo_status.clone(),
            github_username,
            force: false,
        };
        lock_op.execute().await?;

        Ok(())
    }
}

#[instrument(skip(state))]
pub async fn auto_merge_submit_handler<T>(
    State(state): State<AppState<T>>,
    Json(request): Json<PushRequest>,
) -> Result<Json<String>, CoreError>
where
    T: EngineProvider,
{
    let token = state
        .app_config
        .read()
        .github_pat
        .clone()
        .ok_or(CoreError::Input(anyhow!(
            "GitHub PAT is not configured. Please configure it in the settings."
        )))?;

    if request.files.is_empty() {
        return Err(CoreError::Input(anyhow!("No files to submit")));
    }

    let github_client = match state.github_client.read().clone() {
        Some(client) => client.clone(),
        None => return Err(CoreError::Internal(anyhow!(TokenNotFoundError))),
    };

    let auto_merge_submit_op = AutoMergeSubmitOp {
        files: request.files.clone(),
        commit_message: request.commit_message.clone(),

        app_config: state.app_config.clone(),
        repo_config: state.repo_config.clone(),
        engine: state.engine.clone(),
        aws_client: None,
        storage: None,
        repo_status: state.repo_status.clone(),

        git_client: state.git(),
        token: token.to_string(),
        github_client,
    };

    let (tx, rx) = tokio::sync::oneshot::channel::<Option<CoreError>>();
    let mut sequence = TaskSequence::new().with_completion_tx(tx);
    sequence.push(Box::new(auto_merge_submit_op));

    state.operation_tx.send(sequence).await?;

    match rx.await {
        Ok(Some(e)) => {
            return Err(e);
        }
        Ok(None) => {}
        Err(e) => return Err(e.into()),
    }

    Ok(Json("ok".to_string()))
}
