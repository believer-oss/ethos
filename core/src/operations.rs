use crate::clients::git;
use crate::clients::git::Opts;
use crate::types::commits::Commit;
use crate::types::config::RepoConfig;
use crate::types::locks::{ForceUnlock, LockOperation, LockResponse};
use crate::types::repo::{LockRequest, RepoStatusRef};
use crate::worker::Task;
use anyhow::bail;
use async_trait::async_trait;
use chrono::DateTime;
use reqwest::StatusCode;
use std::env;
use std::path::PathBuf;
use tracing::{debug, error, info, instrument, warn};

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
    async fn execute(&self) -> anyhow::Result<()> {
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
    async fn execute(&self) -> anyhow::Result<()> {
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
            .lines()
            .map(|line| {
                let parts = line.split('|').collect::<Vec<_>>();

                let timestamp = DateTime::parse_from_rfc3339(parts[3]).unwrap();

                Commit {
                    sha: parts[0][..8].to_string(),
                    message: Some(parts[1].to_string()),
                    author: Some(parts[2].to_string()),
                    timestamp: Some(timestamp.with_timezone(&chrono::Local).to_string()),
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
    async fn execute(&self) -> anyhow::Result<()> {
        let mut args = vec!["add"];

        for file in &self.files {
            args.push(file);
        }

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
    async fn execute(&self) -> anyhow::Result<()> {
        let mut args = vec!["restore", "--staged"];

        for file in &self.files {
            args.push(file);
        }

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
    async fn execute(&self) -> anyhow::Result<()> {
        let remote_branch = self.repo_status.read().remote_branch.clone();
        let args: Vec<&str> = vec!["rebase", &remote_branch];

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
    pub force: bool,
}

#[async_trait]
impl Task for LockOp {
    #[instrument(name = "LockOp::execute", skip(self))]
    async fn execute(&self) -> anyhow::Result<()> {
        let _ = self.run().await?;

        Ok(())
    }

    fn get_name(&self) -> String {
        String::from("LfsLock")
    }
}

impl LockOp {
    #[instrument(name = "LockOp::run", skip(self))]
    pub async fn run(&self) -> anyhow::Result<LockResponse> {
        if self.github_pat.is_empty() {
            bail!("You must set your Github PAT in Preferences to interact with locks.");
        }

        let repo_path = self.git_client.repo_path.to_str().unwrap().to_string();
        let lfs_config = RepoConfig::read_lfs_config(&repo_path)?;

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
        let response = client
            .post(format!("{}/{}", server_url, endpoint))
            .bearer_auth(&self.github_pat)
            .json(&LockRequest {
                paths: self.paths.clone(),
                force: self.force,
            })
            .send()
            .await?;

        let status = response.status();
        if status.is_success() {
            let lock_response = response.json::<LockResponse>().await?;

            // update file readonly flag for requested paths as appropriate
            // See the GIT_LFS_SET_LOCKABLE_READONLY section at https://www.mankier.com/5/git-lfs-config#List_of_Options-Other_settings
            let mut should_set_read_flag = true; // this flag defaults to true if it is left unspecified

            match env::var("GIT_LFS_SET_LOCKABLE_READONLY") {
                Ok(env_str) => {
                    if let Ok(env_bool) = git::parse_bool_string(&env_str) {
                        should_set_read_flag = env_bool;
                    }
                }
                Err(_) => {
                    let output = self
                        .git_client
                        .run_and_collect_output(
                            &["config", "--get", "lfs.setlockablereadonly"],
                            Opts::default(),
                        )
                        .await;

                    if let Ok(output) = output {
                        if let Ok(bool_str) = git::parse_bool_string(&output) {
                            should_set_read_flag = bool_str;
                        }
                    }
                }
            }

            for failure in lock_response.batch.failures.iter() {
                warn!("Failed to lock path {}: {}", failure.path, failure.reason);
            }

            if should_set_read_flag {
                let set_readonly = self.op != LockOperation::Lock;
                let operation_str = if set_readonly { "set" } else { "clear" };

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

        Ok(LockResponse::default())
    }
    pub fn lock(git_client: git::Git, paths: Vec<String>, github_pat: String) -> LockOp {
        LockOp {
            git_client,
            paths,
            github_pat,
            op: LockOperation::Lock,
            force: false,
        }
    }

    pub fn unlock(
        git_client: git::Git,
        paths: Vec<String>,
        github_pat: String,
        force: ForceUnlock,
    ) -> LockOp {
        LockOp {
            git_client,
            paths,
            github_pat,
            op: LockOperation::Unlock,
            force: force == ForceUnlock::True,
        }
    }
}
