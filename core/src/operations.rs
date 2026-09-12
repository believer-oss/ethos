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
            .split("\x1e\n")
            .filter_map(|line| {
                let parts = line.split('\x1f').collect::<Vec<_>>();

                // Ensure we have at least 4 parts
                if parts.len() < 4 {
                    warn!(
                        "Invalid git log line format (expected at least 4 parts): {}",
                        line
                    );
                    return None;
                }

                // Safely parse timestamp
                let timestamp = match DateTime::parse_from_rfc3339(parts[3]) {
                    Ok(ts) => Some(ts.with_timezone(&chrono::Local).to_string()),
                    Err(e) => {
                        warn!(
                            "Failed to parse timestamp '{}' from git log: {}",
                            parts[3], e
                        );
                        None
                    }
                };

                // Safely extract SHA (ensure it has enough characters)
                let sha = if parts[0].len() >= 8 {
                    parts[0][..8].to_string()
                } else {
                    parts[0].to_string()
                };

                Some(Commit {
                    sha,
                    message: Some(parts[1].to_string()),
                    author: Some(parts[2].to_string()),
                    timestamp,
                    status: None,
                    merge_timestamp: None,
                })
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

#[derive(Clone)]
pub struct GcOp {
    pub git_client: git::Git,
}

#[async_trait]
impl Task for GcOp {
    #[instrument(name = "GcOp::execute", skip(self))]
    async fn execute(&self) -> Result<(), CoreError> {
        self.git_client.run_gc_preserve_unreachable().await?;
        Ok(())
    }

    fn get_name(&self) -> String {
        String::from("RepoGc")
    }
}

/// A fuller maintenance pass than `GcOp`, for developers who have disabled the periodic background
/// tasks and need to keep the repository packed themselves.
#[derive(Clone)]
pub struct MaintenanceOp {
    pub git_client: git::Git,
}

#[async_trait]
impl Task for MaintenanceOp {
    /// Order is load-bearing: reflog expiry must precede gc for newly-expired entries' objects to
    /// become prunable at all, and the commit-graph is written last so it is built over the
    /// repacked object store.
    ///
    /// Each step uses an existing `Git` method rather than a fresh git invocation, and that is a
    /// safety property, not a style preference:
    ///
    /// - `expire_reflog` drives its window through `-c gc.reflogExpire*` config instead of the
    ///   `--expire` CLI options, which is what keeps git's per-ref lookup active and therefore what
    ///   protects `refs/stash`. Friendshipper snapshots *are* stashes, so a CLI-driven window would
    ///   delete users' work.
    /// - `run_gc_preserve_unreachable` is plain `git gc`, leaving git's two-week grace for
    ///   unreachable objects. `run_gc` (`--prune=now`) must never be used here.
    #[instrument(name = "MaintenanceOp::execute", skip(self))]
    async fn execute(&self) -> Result<(), CoreError> {
        self.git_client.expire_reflog().await?;
        self.git_client.run_gc_preserve_unreachable().await?;
        self.git_client.rewrite_graph().await?;

        Ok(())
    }

    fn get_name(&self) -> String {
        String::from("RepoMaintenance")
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
    pub primary_branch: String,
    pub content_branch: Option<String>,
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
        // Only proceed if content branch is configured
        let content_branch = match &self.content_branch {
            Some(branch) => branch,
            None => return Ok(vec![]),
        };

        // Use git cherry to find commits that are truly new (not cherry-picked)
        let upstream = format!("origin/{content_branch}");
        let head = format!("origin/{}", self.primary_branch);

        debug!("Using git cherry to compare {} vs {}", upstream, head);
        let cherry_output = self.git_client.cherry(&upstream, &head).await?;

        // Parse cherry output to get only new commits (those with + prefix)
        let new_commit_shas: Vec<&str> = cherry_output
            .lines()
            .filter_map(|line| {
                if line.starts_with('+') {
                    // Extract SHA from "+ <sha> <subject>" format
                    line.split_whitespace().nth(1)
                } else {
                    None // Skip commits with - prefix (already cherry-picked)
                }
            })
            .collect();

        if new_commit_shas.is_empty() {
            debug!("No new commits found after cherry filtering");
            return Ok(vec![]);
        }

        // Get detailed information for the new commits
        let commit_range = format!("origin/{content_branch}..origin/{}", self.primary_branch);
        debug!(
            "Getting commit details for {} commits in range: {}",
            new_commit_shas.len(),
            commit_range
        );
        let output = self.git_client.log(self.limit, &commit_range).await?;
        let result = output
            .split("\x1e\n")
            .filter(|line| !line.trim().is_empty())
            .filter_map(|line| {
                let parts = line.split('\x1f').collect::<Vec<_>>();

                if parts.len() >= 4 {
                    let full_sha = parts[0];
                    // Only include commits that are not cherrypicked
                    if new_commit_shas
                        .iter()
                        .any(|&cherry_sha| full_sha.starts_with(cherry_sha))
                    {
                        let timestamp = DateTime::parse_from_rfc3339(parts[3])
                            .unwrap_or_else(|_| chrono::Utc::now().into());

                        Some(Commit {
                            sha: parts[0][..8.min(parts[0].len())].to_string(),
                            message: Some(parts[1].to_string()),
                            author: Some(parts[2].to_string()),
                            timestamp: Some(timestamp.with_timezone(&chrono::Local).to_string()),
                            status: None,
                            merge_timestamp: None,
                        })
                    } else {
                        None
                    }
                } else {
                    None
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::process::Command as StdCommand;
    use std::sync::mpsc;
    use tempfile::TempDir;

    /// Initialize a fresh git repo in a tempdir with one committed `seed.txt`, mirroring the
    /// `setup_repo` helper in `clients::git`'s test module.
    fn setup_repo() -> (git::Git, TempDir) {
        let dir = tempfile::tempdir().expect("create tempdir");
        let path = dir.path().to_path_buf();

        let config_steps: &[&[&str]] = &[
            &["init"],
            &["config", "user.email", "test@example.com"],
            &["config", "user.name", "test"],
            &["config", "commit.gpgsign", "false"],
            &["config", "core.autocrlf", "false"],
        ];
        for args in config_steps {
            run_git(&path, args);
        }

        std::fs::write(path.join("seed.txt"), "seed").unwrap();
        run_git(&path, &["add", "seed.txt"]);
        run_git(&path, &["commit", "-m", "seed"]);

        let (tx, _rx) = mpsc::channel();
        (git::Git::new(path, tx), dir)
    }

    fn run_git(path: &Path, args: &[&str]) -> String {
        let out = StdCommand::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .expect("run git");
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    fn stash_list(path: &Path) -> String {
        run_git(path, &["stash", "list"])
    }

    /// Rewrite every entry in `refs/stash`'s reflog to look ~60 days old.
    ///
    /// Without this, preservation tests are worthless: a stash created seconds ago falls inside any
    /// sane expiry window, so it survives an *unsafe* reflog invocation just as happily as a safe
    /// one. Backdating is what makes the window actually reach the entries, and therefore what
    /// makes these tests able to fail. Verified by mutation: with backdated entries the unsafe
    /// `--expire`/`--expire-unreachable` CLI form deletes every stash, while the config-driven form
    /// `expire_reflog` uses keeps all of them.
    ///
    /// Reflog line format: `<old> <new> Name <email> <unix_ts> <tz>\tmessage`.
    fn backdate_stash_reflog(path: &Path) {
        let log = path.join(".git/logs/refs/stash");
        let contents = std::fs::read_to_string(&log).expect("stash reflog exists");
        let old_ts = (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs())
            - 60 * 24 * 3600;

        let rewritten: String = contents
            .lines()
            .map(|line| {
                // replace the unix timestamp that precedes the timezone offset
                let (meta, msg) = line.split_once('\t').unwrap_or((line, ""));
                let mut fields: Vec<&str> = meta.rsplitn(3, ' ').collect();
                fields.reverse();
                // fields = [head..., <unix_ts>, <tz>]
                let head = fields[0];
                let tz = fields[2];
                let out = format!("{head} {old_ts} {tz}");
                if msg.is_empty() {
                    format!("{out}\n")
                } else {
                    format!("{out}\t{msg}\n")
                }
            })
            .collect();

        std::fs::write(&log, rewritten).expect("rewrite stash reflog");
    }

    /// Loose object count from `git count-objects -v`'s `count:` field.
    fn loose_count(path: &Path) -> u64 {
        let out = run_git(path, &["count-objects", "-v"]);
        out.lines()
            .find_map(|l| l.strip_prefix("count:"))
            .and_then(|v| v.trim().parse().ok())
            .expect("count: field present")
    }

    /// Add `n` loose blobs in a single `git add`, which is far faster than one
    /// `hash-object` process per object.
    fn add_loose_objects(path: &Path, n: usize) {
        let dir = path.join("many");
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..n {
            std::fs::write(dir.join(format!("f{i}.txt")), format!("blob{i}")).unwrap();
        }
        run_git(path, &["add", "many"]);
    }

    /// Manual, non-snapshot stashes must survive maintenance.
    ///
    /// Direct regression guard for a bug that already shipped in this repository: reflog expiry
    /// passed `--expire`/`--expire-unreachable` as CLI options, which makes git skip the per-ref
    /// config lookup that protects `refs/stash`, and users lost their stashes. See
    /// `expire_reflog`'s doc comment in `clients::git`.
    ///
    /// Two details are what give this test teeth, and both were established by mutating the
    /// implementation and confirming the test fails:
    ///
    /// - **Several stashes, not one.** Only entries *below* the stash tip are unreachable, so a
    ///   lone stash can survive an unsafe invocation on reachability alone.
    /// - **Backdated reflog entries.** A stash created seconds ago sits inside any sane expiry
    ///   window and survives regardless of which form is used.
    #[tokio::test]
    async fn test_maintenance_preserves_manual_stashes() {
        let (git_client, _dir) = setup_repo();
        let path = git_client.repo_path.clone();

        for i in 0..3 {
            std::fs::write(path.join("seed.txt"), format!("dirty {i}")).unwrap();
            run_git(&path, &["stash", "push", "-m", &format!("manual work {i}")]);
        }
        backdate_stash_reflog(&path);

        let before = stash_list(&path);
        assert_eq!(
            before.lines().count(),
            3,
            "precondition: expected 3 stashes, got {before:?}"
        );

        MaintenanceOp {
            git_client: git_client.clone(),
        }
        .execute()
        .await
        .expect("maintenance succeeds");

        assert_eq!(
            stash_list(&path),
            before,
            "maintenance must not disturb the stash stack"
        );
    }

    /// A Friendshipper snapshot must survive maintenance.
    ///
    /// Deliberately separate from the manual-stash test even though both live in `refs/stash`
    /// today: if a future change ever filters stashes by message, that would break snapshots while
    /// leaving the manual-stash test passing.
    #[tokio::test]
    async fn test_maintenance_preserves_snapshot() {
        let (git_client, _dir) = setup_repo();
        let path = git_client.repo_path.clone();

        // a plain stash underneath, so the snapshot is not the reachable tip
        std::fs::write(path.join("seed.txt"), "plain").unwrap();
        run_git(&path, &["stash", "push", "-m", "plain stash"]);

        std::fs::write(path.join("seed.txt"), "snapshot me").unwrap();
        git_client
            .save_snapshot_all("test snapshot")
            .await
            .expect("save snapshot");

        backdate_stash_reflog(&path);

        let before = git_client.list_snapshots().await.expect("list snapshots");
        assert_eq!(before.len(), 1, "precondition: snapshot was not created");

        MaintenanceOp {
            git_client: git_client.clone(),
        }
        .execute()
        .await
        .expect("maintenance succeeds");

        let after = git_client.list_snapshots().await.expect("list snapshots");
        assert_eq!(
            after.len(),
            before.len(),
            "maintenance must not delete snapshots"
        );
        assert_eq!(after[0].commit, before[0].commit);
    }

    /// Maintenance must actually pack objects. The preservation tests above would both pass
    /// against an implementation that did nothing at all.
    ///
    /// Seeds generously on purpose: git's incremental thresholds sample a single fanout directory,
    /// so a few hundred loose objects can legitimately fall below the bar and make this pass for
    /// the wrong reason.
    #[tokio::test]
    async fn test_maintenance_packs_loose_objects() {
        let (git_client, _dir) = setup_repo();
        let path = git_client.repo_path.clone();

        add_loose_objects(&path, 3000);
        let before = loose_count(&path);
        assert!(
            before >= 3000,
            "precondition: expected many loose objects, got {before}"
        );

        MaintenanceOp {
            git_client: git_client.clone(),
        }
        .execute()
        .await
        .expect("maintenance succeeds");

        let after = loose_count(&path);
        assert!(
            after < before,
            "maintenance must pack loose objects: {before} -> {after}"
        );
    }

    /// Unreachable objects must keep git's two-week grace period, so a dangling commit — work
    /// left behind by a reset or an abandoned rebase — stays recoverable after maintenance.
    ///
    /// Guards the second Decision 10 invariant: `MaintenanceOp` must use
    /// `run_gc_preserve_unreachable` (plain `git gc`), never `run_gc` (`git gc --prune=now`).
    /// Verified by mutation: swapping in `run_gc` makes this test fail.
    #[tokio::test]
    async fn test_maintenance_preserves_dangling_commit() {
        let (git_client, _dir) = setup_repo();
        let path = git_client.repo_path.clone();

        // create a commit, then orphan it
        std::fs::write(path.join("doomed.txt"), "doomed").unwrap();
        run_git(&path, &["add", "doomed.txt"]);
        run_git(&path, &["commit", "-m", "doomed"]);
        let doomed = run_git(&path, &["rev-parse", "HEAD"]).trim().to_string();
        run_git(&path, &["reset", "--hard", "HEAD~1"]);

        // drop the reflog entries that would otherwise keep it reachable, so the only thing
        // standing between this commit and deletion is gc's prune expiry
        run_git(
            &path,
            &[
                "reflog",
                "expire",
                "--expire=now",
                "--expire-unreachable=now",
                "--all",
            ],
        );

        MaintenanceOp {
            git_client: git_client.clone(),
        }
        .execute()
        .await
        .expect("maintenance succeeds");

        let out = run_git(&path, &["cat-file", "-t", &doomed]);
        assert_eq!(
            out.trim(),
            "commit",
            "dangling commit {doomed} must remain recoverable after maintenance"
        );
    }

    #[test]
    fn test_maintenance_op_name() {
        let (tx, _rx) = mpsc::channel();
        let op = MaintenanceOp {
            git_client: git::Git::new(PathBuf::from("."), tx),
        };
        assert_eq!(op.get_name(), "RepoMaintenance");
    }
}
