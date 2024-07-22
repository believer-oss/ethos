use anyhow::anyhow;
use axum::extract::State;
use axum::{async_trait, Json};
use directories_next::ProjectDirs;
use octocrab::models::pulls::MergeableState;
use octocrab::Octocrab;
use std::path::PathBuf;
use tracing::{info, instrument, warn};

use crate::engine::EngineProvider;
use crate::APP_NAME;
use ethos_core::clients::aws::ensure_aws_client;
use ethos_core::clients::git;
use ethos_core::clients::github;
use ethos_core::operations::{AddOp, CommitOp, RestoreOp};
use ethos_core::storage::ArtifactStorage;
use ethos_core::types::config::AppConfigRef;
use ethos_core::types::config::RepoConfigRef;
use ethos_core::types::errors::CoreError;
use ethos_core::types::github::TokenNotFoundError;
use ethos_core::worker::{Task, TaskSequence};
use ethos_core::AWSClient;

use crate::repo::operations::{PushRequest, StatusOp};
use crate::repo::RepoStatusRef;
use crate::state::AppState;

#[derive(Clone)]
pub struct GitHubSubmitOp {
    pub head_branch: String,
    pub base_branch: String,
    pub commit_message: String,
    pub repo_status: RepoStatusRef,
    pub token: String,
    pub client: github::GraphQLClient,
}

#[derive(Clone)]
pub struct SubmitOp<T>
where
    T: EngineProvider,
{
    pub files: Vec<String>,
    pub commit_message: String,

    pub app_config: AppConfigRef,
    pub repo_config: RepoConfigRef,
    pub engine: T,
    pub aws_client: AWSClient,
    pub storage: ArtifactStorage,
    pub repo_status: RepoStatusRef,

    pub git_client: git::Git,
    pub token: String,
    pub github_client: github::GraphQLClient,
}

const SUBMIT_PREFIX: &str = "[quick submit]";

#[async_trait]
impl Task for GitHubSubmitOp {
    #[instrument(name = "GitHubSubmitOp::execute", skip(self))]
    async fn execute(&self) -> anyhow::Result<()> {
        let octocrab = Octocrab::builder()
            .personal_token(self.token.clone())
            .build()?;

        let truncated_message = if self.commit_message.len() > 50 {
            format!("{}...", &self.commit_message[..50])
        } else {
            self.commit_message.clone()
        };

        let owner: String;
        let repo: String;
        {
            let status = self.repo_status.read();
            owner = status.repo_owner.clone();
            repo = status.repo_name.clone();
        }

        let mut pr = octocrab
            .pulls(owner.clone(), repo.clone())
            .create(
                format!("{} {}", SUBMIT_PREFIX, truncated_message),
                self.head_branch.clone(),
                self.base_branch.clone(),
            )
            .send()
            .await?;

        while let Some(state) = pr.mergeable_state {
            match state {
                MergeableState::Blocked | MergeableState::Behind | MergeableState::Unknown => {
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    pr = octocrab
                        .pulls(owner.clone(), repo.clone())
                        .get(pr.number)
                        .await?;
                }
                MergeableState::Dirty => {
                    return Err(anyhow!(
                        "PR state is 'dirty'. It's likely a commit check has failed."
                    ));
                }
                _ => {
                    info!("mergeable state: {:?}", state);
                    break;
                }
            }
        }

        let id = self
            .client
            .get_pull_request_id(owner.clone(), repo.clone(), pr.number as i64)
            .await?;

        self.client.enqueue_pull_request(id).await?;

        Ok(())
    }

    fn get_name(&self) -> String {
        "GitHubSubmitOp".to_string()
    }
}

// Quick submit is a workflow that submits changes via GitHub Pull Requests, taking advantage of the GitHub merge queue to avoid having
// to sync latest first.
// When a commit goes through the merge queue, it becomes a different commit due to how the commit is merged into/
// rebased onto main. When making a successive change, GitHub isn't smart enough to detect that the previous commit is the same one is
// now in main, so it complains that there is a conflict, due to 2 "different" commits touching the same files, even though they have
// the exact same contents. To overcome this limitation, quick submits leverage the concept of git worktrees to resolve local changes
// with the latest changes in main.
// The general logic for quick submit pushes go like this:
// 1. User initiates quick submit
// 2. If the current branch has an existing quick submit change in the merge queue, cancel it. We'll just reuse the current branch.
//    We need to cancel the in-flight one since if it lands in main, it will conflict with what we try to put in the merge queue, so
//    instead we just resolve all the changes locally again, push them all up to the same branch, and resubmit to the merge queue.
// 3. Make a new f11r-<timestamp> branch to contain the changes if needed.
// 4. Commit new changes
// 5. If a scratch worktree folder doesn't exist, make one.
// 6. In the workree directory:
//    a. Make a branch called f11r-<timestamp>-wt and ensure it's up to date with exactly what's on f11r-<timestamp>.
//    b. Resolve local changes with latest main
//    c. Push changes to the remote
// 7. Trigger PR via github
#[async_trait]
impl<T> Task for SubmitOp<T>
where
    T: EngineProvider,
{
    #[instrument(name = "SubmitOp::execute", skip(self))]
    async fn execute(&self) -> anyhow::Result<()> {
        // abort if we are trying to submit any conflicted files
        {
            let conflicts = self.repo_status.read().conflicts.clone();
            let mut is_submitting_conflict = false;
            for file in self.files.iter() {
                if conflicts.iter().any(|x| x == file) {
                    is_submitting_conflict = true;
                    tracing::error!("Trying to submit conflicted file {}", file);
                }
            }
            if is_submitting_conflict {
                tracing::error!("Failing submit due to trying to submit conflicted files");
                anyhow::bail!("Submitting conflicted files is not allowed. Check the log for specific errors.");
            }
        }

        // save a snapshot before submitting
        // make sure we have a temp dir for copying our files
        let proj_dirs =
            ProjectDirs::from("", "", APP_NAME).ok_or(anyhow!("Failed to get project dirs"))?;

        let mut stash_path = proj_dirs.data_dir().to_path_buf();
        stash_path.push(".stash");

        // reset the stash path, delete and recreate
        if stash_path.exists() {
            std::fs::remove_dir_all(&stash_path)?;
        }

        std::fs::create_dir_all(&stash_path)?;

        let current_branch = self.git_client.current_branch().await?;

        // identify files that were deleted in the request
        let (stashed_files, deleted_files): (Vec<String>, Vec<String>) =
            self.files.iter().cloned().partition(|file| {
                // build path from repo path
                let path = self.git_client.repo_path.join(file);
                // check if the file exists
                path.exists()
            });

        // for each file, copy it to the stash
        for file in &stashed_files {
            let src = self.git_client.repo_path.join(file.clone());
            let dest = stash_path.join(file);

            std::fs::create_dir_all(dest.parent().unwrap())?;
            std::fs::copy(src, dest)?;
        }

        match self.execute_internal().await {
            Ok(_) => Ok(()),
            Err(e) => {
                // attempt to reset to original branch and restore snapshot
                // if this fails for any reason, we should simply log, then return the original error
                let _ = self
                    .recover(stash_path, &current_branch, stashed_files, deleted_files)
                    .await
                    .map_err(|err| {
                        warn!(
                            "Failed to recover from error during submit operation: {}",
                            err
                        );
                    });

                Err(e)
            }
        }
    }

    fn get_name(&self) -> String {
        "SubmitOp".to_string()
    }
}

impl<T> SubmitOp<T>
where
    T: EngineProvider,
{
    #[instrument(name = "SubmitOp::execute_internal", skip(self))]
    pub async fn execute_internal(&self) -> anyhow::Result<()> {
        let base_branch = self.repo_config.read().trunk_branch.clone();
        let prev_branch = self.repo_status.read().branch.clone();
        let mut f11r_branch = {
            let display_name = &self.app_config.read().user_display_name;
            let santized_display_name = display_name.replace(' ', "-");
            format!(
                "f11r-{}-{}",
                santized_display_name,
                chrono::Utc::now().timestamp()
            )
        };

        // If there's an inflight quicksubmit change, cancel it - we can be reasonably sure
        let mut needs_new_pr = false;

        let mut quicksubmit_pr_id: Option<String> = None;
        if is_quicksubmit_branch(&prev_branch) {
            let owner: String;
            let repo: String;
            {
                let status = self.repo_status.read();
                owner = status.repo_owner.clone();
                repo = status.repo_name.clone();
            }

            let merge_queue = self.github_client.get_merge_queue(&owner, &repo).await?;
            if let Some(entries) = merge_queue.entries {
                if let Some(nodes) = entries.nodes {
                    for node in nodes.into_iter().flatten() {
                        if let Some(commit) = node.head_commit {
                            if let Some(author) = commit.author {
                                if let Some(pr) = node.pull_request {
                                    if let Some(user) = author.user {
                                        if user.login == self.github_client.username
                                            && pr.title.starts_with(SUBMIT_PREFIX)
                                        {
                                            // There should only be one quicksubmit PR in merge queue at a time
                                            quicksubmit_pr_id = Some(pr.id);
                                            break;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            if let Some(id) = &quicksubmit_pr_id {
                // Silently absorb errors - the PR may have been already merged in this case
                let res = self
                    .github_client
                    .dequeue_pull_request(id.to_string())
                    .await;
                if let Err(e) = res {
                    warn!(
                        "Failed to cancel existing pull request {}. Reason: {}",
                        id, e
                    );
                    needs_new_pr = true;
                }
            }
        } else {
            needs_new_pr = true;
        }

        if needs_new_pr {
            self.git_client
                .run(&["checkout", "-b", &f11r_branch], Default::default())
                .await?;

            // Clean up the old f11r branch, if it was one
            if is_quicksubmit_branch(&prev_branch) {
                self.git_client
                    .delete_branch(&prev_branch, git::BranchType::Local)
                    .await?;
            }
        } else {
            f11r_branch = prev_branch;
        }

        let status_op = StatusOp {
            repo_status: self.repo_status.clone(),
            app_config: self.app_config.clone(),
            repo_config: self.repo_config.clone(),
            engine: self.engine.clone(),
            git_client: self.git_client.clone(),
            github_username: self.github_client.username.clone(),
            aws_client: self.aws_client.clone(),
            storage: self.storage.clone(),
            skip_fetch: true,
            skip_dll_check: true,
            allow_offline_communication: true,
        };

        // commit changes
        {
            // need to chunk the adds due to commandline length limitations
            for chunk in self.files.chunks(50) {
                let add_op = AddOp {
                    files: chunk.to_vec(),
                    git_client: self.git_client.clone(),
                };

                add_op.execute().await?;
            }

            // unstage any files that are staged but not in the request
            let mut staged_files = Vec::new();
            {
                let repo_status = self.repo_status.read();
                let modified = repo_status.modified_files.clone();
                for file in modified.into_iter() {
                    if !file.index_state.is_empty() {
                        staged_files.push(file.path.clone());
                    }
                }
            }

            let files_to_unstage: Vec<String> = staged_files
                .into_iter()
                .filter(|file| !self.files.contains(file))
                .collect();

            if !files_to_unstage.is_empty() {
                for chunk in files_to_unstage.chunks(50) {
                    let restore_op = RestoreOp {
                        files: chunk.to_vec(),
                        git_client: self.git_client.clone(),
                    };

                    restore_op.execute().await?;
                }
            }

            // We can skip the status check because we know for a fact that there are staged files
            let commit_op = CommitOp {
                message: self.commit_message.clone(),
                repo_status: self.repo_status.clone(),
                git_client: self.git_client.clone(),
                skip_status_check: true,
            };

            commit_op.execute().await?;

            // update status now that the files have been committed and there aren't any more
            // staged files
            status_op.execute().await?;
        }

        let worktree_path: PathBuf = 'path: {
            let repo_path = PathBuf::from(self.app_config.read().repo_path.clone());

            let worktrees = self.git_client.list_worktrees().await?;
            for tree in worktrees.iter() {
                if tree.directory != repo_path {
                    break 'path tree.directory.clone();
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
                .run(&["fetch", "origin", &base_branch], git_opts_lfs_stubs)
                .await?;
            git_client_worktree
                .run(
                    &["rebase", &format!("origin/{}", base_branch)],
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

        // If we already have a PR, requeue it. Otherwise just make a new PR and submit it to the merge queue
        match quicksubmit_pr_id {
            Some(pr_id) => {
                self.github_client.enqueue_pull_request(pr_id).await?;
            }
            None => {
                let gh_op = GitHubSubmitOp {
                    head_branch: worktree_branch.clone(),
                    base_branch: base_branch.clone(),
                    token: self.token.clone(),
                    commit_message: self.commit_message.clone(),
                    repo_status: self.repo_status.clone(),
                    client: self.github_client.clone(),
                };

                gh_op.execute().await?;
            }
        }

        // cleanup worktree branch
        _ = git_client_worktree
            .run(&["checkout", "--detach"], git_opts_lfs_stubs)
            .await;
        _ = git_client_worktree
            .delete_branch(&worktree_branch, git::BranchType::Local)
            .await;

        Ok(())
    }

    #[instrument(skip(self))]
    async fn recover(
        &self,
        stash_path: PathBuf,
        target_branch: &str,
        stashed_files: Vec<String>,
        deleted_files: Vec<String>,
    ) -> anyhow::Result<()> {
        self.git_client.hard_reset(target_branch).await?;

        // for any stashed files, copy them back
        for file in stashed_files {
            let src = stash_path.join(&file);
            let dest = self.git_client.repo_path.join(&file);

            std::fs::copy(src, dest)?;
        }

        // for any deleted files, we should ensure they are deleted
        for file in deleted_files {
            let path = self.git_client.repo_path.join(file);
            if path.exists() {
                std::fs::remove_file(path)?;
            }
        }

        Ok(())
    }
}

#[instrument(skip(state))]
pub async fn submit_handler<T>(
    State(state): State<AppState<T>>,
    Json(request): Json<PushRequest>,
) -> Result<Json<String>, CoreError>
where
    T: EngineProvider,
{
    let token = state.app_config.read().ensure_github_pat()?;

    let github_client = match state.github_client.read().clone() {
        Some(client) => client.clone(),
        None => return Err(CoreError(anyhow!(TokenNotFoundError))),
    };

    let aws_client = ensure_aws_client(state.aws_client.read().await.clone())?;

    let storage = match state.storage.read().clone() {
        Some(storage) => storage,
        None => {
            return Err(CoreError(anyhow!(
                "No storage configured for this app. AWS may still be initializing."
            )))
        }
    };

    let submit_op = SubmitOp {
        files: request.files.clone(),
        commit_message: request.commit_message.clone(),

        app_config: state.app_config.clone(),
        repo_config: state.repo_config.clone(),
        engine: state.engine.clone(),
        aws_client,
        storage,
        repo_status: state.repo_status.clone(),

        git_client: state.git(),
        token,
        github_client,
    };

    let (tx, rx) = tokio::sync::oneshot::channel::<Option<anyhow::Error>>();
    let mut sequence = TaskSequence::new().with_completion_tx(tx);
    sequence.push(Box::new(submit_op));

    state.operation_tx.send(sequence).await?;

    match rx.await {
        Ok(Some(e)) => {
            return Err(CoreError(e));
        }
        Ok(None) => {}
        Err(e) => return Err(e.into()),
    }

    Ok(Json("ok".to_string()))
}

pub fn is_quicksubmit_branch(branch: &str) -> bool {
    branch.starts_with("f11r")
}
