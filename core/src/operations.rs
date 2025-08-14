use crate::clients::git;
use crate::clients::git::Opts;
use crate::types::commits::Commit;
use crate::types::config::RepoConfig;
use crate::types::errors::CoreError;
use crate::types::locks::Lock;
use crate::types::locks::OwnerInfo;
use crate::types::locks::{LockOperation, LockResponse};
use crate::types::repo::{LockRequest, RepoStatusRef};
use crate::worker::Task;
use anyhow::bail;
use async_trait::async_trait;
use chrono::DateTime;
use reqwest::StatusCode;
use std::env;
use std::io::Write;
use std::path::PathBuf;
use tempfile::NamedTempFile;
use tracing::{debug, error, info, instrument, warn, Instrument};

#[derive(Clone)]
pub struct CommitOp {
    pub message: String,
    pub repo_status: RepoStatusRef,
    pub git_client: git::Git,
    pub skip_status_check: bool,
}

#[async_trait]
impl Task for CommitOp {
    #[instrument(name = "CommitOp::execute", skip(self))]
    async fn execute(&self) -> Result<(), CoreError> {
        if !self.skip_status_check {
            let repo_status = self.repo_status.read();
            if !repo_status.has_staged_changes {
                return Ok(());
            }
        }

        self.git_client.commit(&self.message).await?;

        Ok(())
    }

    fn get_name(&self) -> String {
        String::from("RepoCommit")
    }
}

pub type LogResponse = Vec<Commit>;

#[derive(Clone)]
pub struct LogOp {
    pub limit: usize,
    pub use_remote: bool,
    pub repo_path: String,
    pub repo_status: RepoStatusRef,
    pub git_client: git::Git,
}

#[async_trait]
impl Task for LogOp {
    #[instrument(name = "LogOp::execute", skip(self))]
    async fn execute(&self) -> Result<(), CoreError> {
        let _ = self.run().await?;

        Ok(())
    }

    fn get_name(&self) -> String {
        String::from("RepoLog")
    }
}

impl LogOp {
    #[instrument(name = "LogOp::run", skip(self))]
    pub async fn run(&self) -> anyhow::Result<LogResponse> {
        let git_ref = match self.use_remote {
            true => self.repo_status.read().remote_branch.clone(),
            false => self.repo_status.read().branch.clone(),
        };

        // Sometimes we ask for the remote branch's log, but the branch may be deleted as part
        // of the pull request process. In this case, we should return an empty log.
        if git_ref.is_empty() {
            debug!("Branch is empty, returning empty log");
            return Ok(vec![]);
        }

        let output = self.git_client.log(self.limit, &git_ref).await?;
        let result = output
            .split("END\n")
            .map(|line| {
                let parts = line.split('|').collect::<Vec<_>>();

                let timestamp = DateTime::parse_from_rfc3339(parts[3]).unwrap();

                Commit {
                    sha: parts[0][..8].to_string(),
                    message: Some(parts[1].to_string()),
                    author: Some(parts[2].to_string()),
                    timestamp: Some(timestamp.with_timezone(&chrono::Local).to_string()),
                    status: None,
                }
            })
            .collect::<Vec<_>>();

        Ok(result)
    }
}

#[derive(Clone)]
pub struct AddOp {
    pub files: Vec<String>,
    pub git_client: git::Git,
}

#[async_trait]
impl Task for AddOp {
    #[instrument(name = "AddOp::execute", skip(self))]
    async fn execute(&self) -> Result<(), CoreError> {
        let mut args = vec!["add"];

        let mut temp_file = NamedTempFile::new()?;
        for file in &self.files {
            writeln!(temp_file, "{file}")?;
        }
        temp_file.flush()?;

        args.push("--pathspec-from-file");
        args.push(temp_file.path().to_str().unwrap());

        self.git_client
            .run(args.as_slice(), Opts::new_without_logs())
            .await?;

        Ok(())
    }

    fn get_name(&self) -> String {
        String::from("RepoAdd")
    }
}

#[derive(Clone)]
pub struct RestoreOp {
    pub files: Vec<String>,
    pub git_client: git::Git,
}

#[async_trait]
impl Task for RestoreOp {
    #[instrument(name = "RestoreOp::execute", skip(self))]
    async fn execute(&self) -> Result<(), CoreError> {
        let mut args = vec!["restore", "--staged"];

        let mut temp_file = NamedTempFile::new()?;
        for file in &self.files {
            writeln!(temp_file, "{file}")?;
        }
        temp_file.flush()?;

        args.push("--pathspec-from-file");
        args.push(temp_file.path().to_str().unwrap());

        self.git_client
            .run(args.as_slice(), Default::default())
            .await?;

        Ok(())
    }

    fn get_name(&self) -> String {
        String::from("RepoRestore")
    }
}

#[derive(Clone)]
pub struct RebaseOp {
    pub git_client: git::Git,
    pub repo_status: RepoStatusRef,
}

#[async_trait]
impl Task for RebaseOp {
    #[instrument(name = "RebaseOp::execute", skip(self))]
    async fn execute(&self) -> Result<(), CoreError> {
        let remote_branch = self.repo_status.read().remote_branch.clone();
        let args: Vec<&str> = vec!["rebase", "--autostash", &remote_branch];

        self.git_client.run(&args, Opts::default()).await?;

        Ok(())
    }

    fn get_name(&self) -> String {
        String::from("RepoRebase")
    }
}

#[derive(Debug, Clone)]
pub struct LockOp {
    pub git_client: git::Git,
    pub paths: Vec<String>,
    pub github_pat: String,
    pub op: LockOperation,
    pub repo_status: RepoStatusRef,
    pub github_username: String,
    pub force: bool,
    pub response_tx: Option<tokio::sync::mpsc::Sender<LockResponse>>,
}

#[async_trait]
impl Task for LockOp {
    #[instrument(name = "LockOp::execute", skip(self))]
    async fn execute(&self) -> Result<(), CoreError> {
        let _ = self.run().await?;

        Ok(())
    }

    fn get_name(&self) -> String {
        format!("LockOp ({:?})", self.op)
    }
}

#[derive(Clone)]
pub struct BranchCompareOp {
    pub limit: usize,
    pub repo_path: String,
    pub repo_status: RepoStatusRef,
    pub git_client: git::Git,
    pub target_branches: Vec<crate::types::config::TargetBranchConfig>,
}

#[async_trait]
impl Task for BranchCompareOp {
    #[instrument(name = "BranchCompareOp::execute", skip(self))]
    async fn execute(&self) -> Result<(), CoreError> {
        let _ = self.run().await?;
        Ok(())
    }

    fn get_name(&self) -> String {
        String::from("BranchCompare")
    }
}

impl BranchCompareOp {
    #[instrument(name = "BranchCompareOp::run", skip(self))]
    pub async fn run(&self) -> anyhow::Result<LogResponse> {
        // Only proceed if there are exactly 2 target branches
        if self.target_branches.len() != 2 {
            return Ok(vec![]);
        }

        // Find the branch with merge queue and the one without
        let merge_queue_branch = self
            .target_branches
            .iter()
            .find(|branch| branch.uses_merge_queue);
        let non_merge_queue_branch = self
            .target_branches
            .iter()
            .find(|branch| !branch.uses_merge_queue);

        if merge_queue_branch.is_none() || non_merge_queue_branch.is_none() {
            return Ok(vec![]);
        }

        let merge_queue_branch_name = &merge_queue_branch.unwrap().name;
        let non_merge_queue_branch_name = &non_merge_queue_branch.unwrap().name;

        // Get commits in merge_queue_branch that are not in non_merge_queue_branch
        let commit_range =
            format!("origin/{non_merge_queue_branch_name}..origin/{merge_queue_branch_name}");

        debug!("Getting commits in range: {}", commit_range);

        let output = self.git_client.log(self.limit, &commit_range).await?;
        let result = output
            .split("END\n")
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                let parts = line.split('|').collect::<Vec<_>>();

                if parts.len() >= 4 {
                    let timestamp = DateTime::parse_from_rfc3339(parts[3])
                        .unwrap_or_else(|_| chrono::Utc::now().into());

                    Commit {
                        sha: parts[0][..8.min(parts[0].len())].to_string(),
                        message: Some(parts[1].to_string()),
                        author: Some(parts[2].to_string()),
                        timestamp: Some(timestamp.with_timezone(&chrono::Local).to_string()),
                        status: None,
                    }
                } else {
                    // Handle malformed lines gracefully
                    Commit {
                        sha: "unknown".to_string(),
                        message: Some("Malformed commit data".to_string()),
                        author: Some("Unknown".to_string()),
                        timestamp: Some(chrono::Local::now().to_string()),
                        status: None,
                    }
                }
            })
            .collect::<Vec<_>>();

        Ok(result)
    }
}

impl LockOp {
    #[instrument(name = "LockOp::run", skip(self))]
    pub async fn run(&self) -> anyhow::Result<LockResponse> {
        if self.github_pat.is_empty() {
            bail!("You must set your Github PAT in Preferences to interact with locks.");
        }

        let repo_path = self.git_client.repo_path.to_str().unwrap().to_string();
        if let Ok(lfs_config) = RepoConfig::read_lfs_config(&repo_path) {
            let server_url = match lfs_config.lfs.url {
                Some(url) => url,
                None => bail!(".lfsconfig is not configured with a url"),
            };

            let endpoint = match self.op {
                LockOperation::Lock => "locks/batch/lock",
                LockOperation::Unlock => "locks/batch/unlock",
            };

            let client = reqwest::ClientBuilder::new()
                .connection_verbose(true)
                .build()
                .unwrap();
            let mut unique_paths = {
                let mut unique = self.paths.clone();
                unique.sort();
                unique.dedup();
                unique
            };

            // if we're locking, filter out files that are already in locks.ours, otherwise
            // filter out files that are not
            let repo_status = self.repo_status.read().clone();
            unique_paths = unique_paths
                .into_iter()
                .filter(|path| {
                    if self.op == LockOperation::Lock {
                        !repo_status.locks_ours.iter().any(|lock| lock.path == *path)
                    } else if !self.force {
                        repo_status.locks_ours.iter().any(|lock| lock.path == *path)
                    } else {
                        true
                    }
                })
                .collect::<Vec<_>>();

            let span = tracing::info_span!("lfs_batch_request");
            let request_url = format!("{}/{}", server_url.clone(), endpoint);
            let response = async move {
                client
                    .post(request_url)
                    .bearer_auth(&self.github_pat)
                    .json(&LockRequest {
                        paths: unique_paths,
                        force: self.force,
                    })
                    .send()
                    .await
            }
            .instrument(span)
            .await?;

            let status = response.status();
            if status.is_success() {
                let lock_response = response.json::<LockResponse>().await?;

                // update file readonly flag for requested paths as appropriate
                // See the GIT_LFS_SET_LOCKABLE_READONLY section at https://www.mankier.com/5/git-lfs-config#List_of_Options-Other_settings
                let mut should_set_read_flag = false; // this flag defaults to false if it is left unspecified

                if let Ok(env_str) = env::var("GIT_LFS_SET_LOCKABLE_READONLY") {
                    if let Ok(env_bool) = git::parse_bool_string(&env_str) {
                        should_set_read_flag = env_bool;
                    }
                }

                for failure in lock_response.batch.failures.iter() {
                    warn!("Failed to lock path {}: {}", failure.path, failure.reason);
                }

                // update repo status to ensure any status updates sent to the engine have the latest lock info
                {
                    let mut repo_status = self.repo_status.write();
                    if self.op == LockOperation::Lock {
                        let timestamp: String = chrono::Utc::now().to_rfc3339();
                        for lock in lock_response.batch.paths.iter() {
                            repo_status.locks_ours.push(Lock {
                                id: String::new(),
                                path: lock.clone(),
                                display_name: Some(String::new()),
                                locked_at: timestamp.clone(),
                                owner: Some(OwnerInfo {
                                    name: self.github_username.clone(),
                                }),
                            });
                        }
                    } else {
                        let filter_func =
                            |lock: &Lock| !lock_response.batch.paths.contains(&lock.path);

                        repo_status.locks_ours.retain(filter_func);

                        if self.force {
                            repo_status.locks_theirs.retain(filter_func)
                        }
                    }
                }

                // if we're honoring the GIT_LFS_SET_LOCKABLE_READONLY flag, we set the readonly flag for the locked files
                // if NOT, we ensure the readonly flag is not set
                let set_readonly = match should_set_read_flag {
                    true => self.op != LockOperation::Lock,
                    false => false,
                };
                let operation_str = if set_readonly { "set" } else { "clear" };

                let span = tracing::info_span!("set_readonly").entered();
                for path in &self.paths {
                    if !lock_response.batch.failures.iter().any(|x| x.path == *path) {
                        let mut absolute_path = PathBuf::from(&repo_path);
                        absolute_path.push(path);

                        match absolute_path.try_exists() {
                            Ok(exists) => {
                                if exists {
                                    // canonicalize path (this cleans up the path and ensures existence)
                                    match absolute_path.canonicalize() {
                                        Ok(canonical_path) => {
                                            // set readonly flag for the canonical path (not the original path
                                            match std::fs::metadata(&canonical_path) {
                                                Ok(metadata) => {
                                                    let mut perms = metadata.permissions().clone();
                                                    if perms.readonly() != set_readonly {
                                                        perms.set_readonly(set_readonly);

                                                        if let Err(e) = std::fs::set_permissions(
                                                            &canonical_path,
                                                            perms,
                                                        ) {
                                                            error!(
                                                    "Failed to {} readonly flag for file {:?}: {}",
                                                    operation_str, &canonical_path, e
                                                );
                                                        }
                                                    }
                                                }
                                                Err(e) => error!(
                                                    "Failed to {} readonly flag for file {:?}: {}",
                                                    operation_str, &canonical_path, e
                                                ),
                                            }
                                        }
                                        Err(e) => error!(
                                        "Failed to canonicalize path {:?} for readonly flag: {}",
                                        &absolute_path, e
                                    ),
                                    }
                                }
                            }
                            Err(e) => {
                                error!(
                                    "Failed to check existence of path {:?} for readonly flag: {}",
                                    &absolute_path, e
                                );
                            }
                        }
                    }
                }
                span.exit();

                if let Some(response_tx) = &self.response_tx {
                    response_tx.send(lock_response.clone()).await?;
                }
                return Ok(lock_response);
            } else if status == StatusCode::NOT_FOUND {
                info!(
                    "Batch lock API at {} unavailable, falling back to git lfs",
                    server_url
                );
            } else {
                let body = response.text().await?;
                error!("Failed lock request at {} with error {}", server_url, body);
                bail!("Failed lock request. Check log for details.");
            }
        }

        // try falling back to git lfs if the batch endpoint isn't available for our LFS server
        let lfs_op = match self.op {
            LockOperation::Lock => "lock",
            LockOperation::Unlock => "unlock",
        };

        // Run each op one at a time to avoid overloading the CLI - it can only take so many args
        for path in &self.paths {
            let mut args: Vec<&str> = vec![];
            args.push("lfs");
            args.push(lfs_op);
            if self.op == LockOperation::Unlock && self.force {
                args.push("--force");
            }
            args.push(path);
            self.git_client.run(&args, Opts::default()).await?;
        }

        if let Some(response_tx) = &self.response_tx {
            response_tx.send(LockResponse::default()).await?;
        }
        Ok(LockResponse::default())
    }
}
