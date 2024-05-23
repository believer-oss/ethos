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
use tracing::{debug, error, info};

#[derive(Clone)]
pub struct CommitOp {
    pub message: String,
    pub repo_status: RepoStatusRef,
    pub git_client: git::Git,
}

#[async_trait]
impl Task for CommitOp {
    async fn execute(&self) -> anyhow::Result<()> {
        {
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
    async fn execute(&self) -> anyhow::Result<()> {
        let _ = self.run().await?;

        Ok(())
    }

    fn get_name(&self) -> String {
        String::from("RepoLog")
    }
}

impl LogOp {
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
    async fn execute(&self) -> anyhow::Result<()> {
        let remote_branch = self.repo_status.read().remote_branch.clone();
        let args: Vec<&str> = vec!["rebase", &remote_branch];

        self.git_client.run(&args, git::Opts::default()).await?;

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
    async fn execute(&self) -> anyhow::Result<()> {
        let _ = self.run().await?;

        Ok(())
    }

    fn get_name(&self) -> String {
        String::from("LfsLock")
    }
}

impl LockOp {
    pub async fn run(&self) -> anyhow::Result<LockResponse> {
        if self.github_pat.is_empty() {
            bail!("You must set your Github PAT in Preferences to interact with locks.");
        }

        let server_url = RepoConfig::read_lfs_url(
            self.git_client
                .repo_path
                .to_str()
                .expect("was the git client passed an invalid repo path?"),
        )?;
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

        // try falling back to git lfs if the batch endpoint isn't available for our LFS server
        let status = response.status();
        if status.is_success() {
            let lock_response = response.json::<LockResponse>().await?;
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
            self.git_client.run(&args, git::Opts::default()).await?;
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
