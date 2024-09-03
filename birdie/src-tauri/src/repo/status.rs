use std::str;
use std::sync::Arc;

use anyhow::anyhow;
use axum::extract::Query;
use axum::{async_trait, debug_handler, extract::State, Json};
use parking_lot::RwLock;
use serde::Deserialize;
use tracing::{info, warn};

use ethos_core::clients::git;
use ethos_core::types::errors::CoreError;
use ethos_core::types::repo::FileState;
use ethos_core::types::repo::SubmitStatus;
use ethos_core::types::repo::{File, RepoStatus};
use ethos_core::worker::{Task, TaskSequence};

use crate::state::AppState;

pub type RepoStatusRef = Arc<RwLock<RepoStatus>>;

/*
git status --porcelain=v2 --branch --ignored

# branch.oid 06a3b67b10cf37ed685fbb8df4a0dd31f0c7fb05
# branch.head ar/exe-party
# branch.upstream origin/ar/exe-party
# branch.ab +0 -0
1 M. N... 100644 100644 100644 232ff8b3124704a5cca2bfab1c065cef6e30418f 75adf399778e40238255a7d19b4f42918e63a40b friendshipper-svc/src/repo/operations.rs

First column:
    # = branch info
    1 = tracked file
    2 = renamed file
    u = unmerged file
    ? = untracked file
    ! = ignored file

If line is a file, second column:
    M = modified
    A = added
    D = deleted
    R = renamed
    C = copied

    . before means unstaged, . after means staged.

Third column is always N... unless it's a submodule.

From there: octal file mode at HEAD, octal file mode in index, octal file mode in worktree, object name in HEAD, object name in index, file path
*/

#[derive(Clone)]
pub struct StatusOp {
    pub git_client: git::Git,
    pub repo_status: RepoStatusRef,
    pub skip_fetch: bool,
    pub github_username: String,
}

#[async_trait]
impl Task for StatusOp {
    async fn execute(&self) -> Result<(), CoreError> {
        let _ = self.run().await?;

        Ok(())
    }

    fn get_name(&self) -> String {
        String::from("RepoStatus")
    }
}

impl StatusOp {
    pub(crate) async fn run(&self) -> anyhow::Result<RepoStatus> {
        if !self.skip_fetch {
            info!("Fetching latest for {:?}", self.git_client.repo_path);
            self.git_client
                .run(&["fetch", "--prune"], git::Opts::default())
                .await?;
        }

        let output = self.git_client.status(vec![]).await?;

        let mut status = RepoStatus::new();

        for line in output.lines() {
            if line.starts_with("##") {
                status.parse_branch_string(line);

                continue;
            }

            let file = File::from_status_line(line);

            if file.is_staged {
                status.has_staged_changes = true;
            }

            if file.state == FileState::Added {
                status.untracked_files.0.push(file);
            } else {
                status.modified_files.0.push(file);
            }
        }

        if status.detached_head {
            warn!(
                "Detached head state detected for {:?}",
                self.git_client.repo_path
            );
        }

        // check modified files in local commits
        let mut modified_committed: Vec<String> = vec![];
        if status.commits_ahead > 0 {
            let range = format!("HEAD~{}...HEAD", status.commits_ahead);

            modified_committed = self.git_client.diff_filenames(&range).await?;
        }

        {
            info!("StatusOp: getting locks");
            status.lock_user.clone_from(&self.github_username);
            let locks = self.git_client.verify_locks().await?;
            status.locks_ours = locks.ours;
            status.locks_theirs = locks.theirs;
        }

        status.commit_head_origin = self
            .git_client
            .head_commit(git::CommitFormat::Long, git::CommitHead::Remote)
            .await?;

        let remote_url = match self
            .git_client
            .run_and_collect_output(
                &["config", "--get", "remote.origin.url"],
                git::Opts::default(),
            )
            .await
        {
            Ok(url) => url,
            Err(_) => {
                return Err(anyhow!("Failed to get remote URL for repo"));
            }
        };

        // https://github.com/owner/repository.git
        let parts = remote_url.split('/');
        if parts.count() < 4 {
            status.repo_owner = "".to_string();
            status.repo_name = "".to_string();
        } else {
            status.repo_owner = remote_url.split('/').nth(3).unwrap().to_string();
            status.repo_name = remote_url
                .split('/')
                .nth(4)
                .unwrap()
                .trim_end()
                .trim_end_matches(".git")
                .to_string();
        }

        {
            info!("StatusOp: finding upstream modified files...");
            status.modified_upstream = self.get_modified_upstream(&status).await?;

            status.conflicts = self.get_upstream_conflicts(&modified_committed, &status);
            if !status.conflicts.is_empty() {
                status.conflict_upstream = true;
            }
        }

        {
            info!("Updating file submit status");

            let update_files_submit_status = |files: &mut [File]| {
                for file in files.iter_mut() {
                    if file.state == FileState::Unmerged {
                        file.submit_status = SubmitStatus::Unmerged;
                    } else if status.conflicts.iter().any(|x| *x == file.path) {
                        file.submit_status = SubmitStatus::Conflicted;
                    } else if let Some(lock) = status
                        .locks_theirs
                        .iter()
                        .find(|lock| lock.path == file.path)
                    {
                        file.submit_status = SubmitStatus::CheckedOutByOtherUser;
                        file.locked_by = lock.owner.clone().map(|x| x.name).unwrap_or_default();
                    } else if status.locks_ours.iter().any(|x| x.path == file.path) {
                        file.locked_by.clone_from(&self.github_username);
                    }
                }
            };

            update_files_submit_status(&mut status.untracked_files.0);
            update_files_submit_status(&mut status.modified_files.0);
        }

        let mut repo_status = self.repo_status.write();
        *repo_status = status.clone();

        Ok(status)
    }
    async fn get_modified_upstream(
        &self,
        status: &RepoStatus,
    ) -> Result<Vec<String>, anyhow::Error> {
        // no commits upstream, no changes
        if status.commits_behind == 0 {
            return Ok(vec![]);
        }

        // check files modified upstream
        let range = format!("HEAD...{}", status.remote_branch);
        let modified_upstream = self.git_client.diff_filenames(&range).await?;
        Ok(modified_upstream)
    }

    fn get_upstream_conflicts(
        &self,
        modified_committed: &[String],
        repo_status: &RepoStatus,
    ) -> Vec<String> {
        repo_status
            .modified_upstream
            .iter()
            .filter(|file| {
                modified_committed.contains(file)
                    || repo_status.modified_files.contains(file)
                    || repo_status.untracked_files.contains(file)
            })
            .cloned()
            .collect::<Vec<_>>()
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusParams {
    #[serde(default)]
    pub skip_fetch: bool,
    // This does not get read
    #[allow(dead_code)]
    #[serde(default)]
    pub skip_dll_check: bool,
}

#[debug_handler]
pub async fn status_handler(
    State(state): State<Arc<AppState>>,
    params: Query<StatusParams>,
) -> Result<Json<RepoStatus>, CoreError> {
    let status_op = {
        StatusOp {
            repo_status: state.repo_status.clone(),
            git_client: state.git(),
            skip_fetch: params.skip_fetch,
            github_username: state.github_username(),
        }
    };

    // make sure this status operation is executed behind any queued operations
    let (tx, rx) = tokio::sync::oneshot::channel::<Option<CoreError>>();

    let mut sequence = TaskSequence::new().with_completion_tx(tx);
    sequence.push(Box::new(status_op));

    state.operation_tx.send(sequence).await?;

    match rx.await {
        Ok(e) => {
            if let Some(e) = e {
                return Err(e);
            }

            let status = state.repo_status.read();

            Ok(Json(status.clone()))
        }
        Err(_) => Err(CoreError::Internal(anyhow!(
            "Error executing status operation"
        ))),
    }
}
