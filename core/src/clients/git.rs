use std::io::Write;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;

use anyhow::{anyhow, bail};
use chrono::{DateTime, Utc};
use lazy_static::lazy_static;
use parking_lot::RwLock;
use regex::Regex;
use sysinfo::{ProcessRefreshKind, System, UpdateKind};
use tempfile::NamedTempFile;
use tokio::io::AsyncBufReadExt;
use tokio::io::BufReader;
use tokio::process::Command;
use tracing::warn;
use tracing::{debug, error, info, instrument};

use crate::types::errors::CoreError;
use crate::types::locks::VerifyLocksResponse;
use crate::types::repo::{File, Snapshot};

static SNAPSHOT_PREFIX: &str = "snapshot";

lazy_static! {
    static ref WORKTREE_DIR_REGEX: Regex = Regex::new(r"^worktree (.+)").unwrap();
    static ref WORKTREE_SHA_REGEX: Regex = Regex::new(r"^HEAD (.+)").unwrap();
    static ref WORKTREE_BRANCH_REGEX: Regex = Regex::new(r"^(branch|detached)\s*(.+)?").unwrap();
}

#[cfg(windows)]
use crate::CREATE_NO_WINDOW;

#[derive(Eq, PartialEq)]
pub enum CommitHead {
    Local,
    Remote,
}

#[derive(Eq, PartialEq)]
pub enum CommitFormat {
    Short,
    Long,
}

#[derive(Eq, PartialEq)]
pub enum ShouldPrune {
    Yes,
    No,
}

#[derive(Eq, PartialEq)]
pub enum MergeType {
    FF,
    FFOnly,
    NoFF,
}

#[derive(Eq, PartialEq)]
pub enum StashAction {
    Push,
    Pop,
}

#[derive(Eq, PartialEq)]
pub enum BranchType {
    Local,
    Remote,
}

#[derive(Eq, PartialEq)]
pub enum SaveSnapshotIndexOption {
    KeepIndex,
    DiscardIndex,
}

#[derive(Eq, PartialEq)]
pub enum PullStrategy {
    Rebase,
    FFOnly,
}

#[derive(Eq, PartialEq)]
pub enum PullStashStrategy {
    Autostash,
    None,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LfsMode {
    Inflated,
    Stubs,
}

#[derive(Clone, Debug)]
pub struct Git {
    pub repo_path: PathBuf,
    pub tx: std::sync::mpsc::Sender<String>,
}

#[derive(Clone, Copy, Debug)]
pub struct Opts<'a> {
    pub ignored_errors: &'a [&'a str],
    pub should_log_stdout: bool,
    pub return_complete_error: bool,
    pub lfs_mode: LfsMode,
    pub skip_notify_frontend: bool,
}

#[derive(Clone, Debug, Default)]
pub struct WorktreeInfo {
    pub directory: PathBuf,
    pub sha: String,
    pub branch: Option<String>, // if None, it is detached
}

impl Default for Opts<'_> {
    fn default() -> Self {
        Opts {
            ignored_errors: &[],
            should_log_stdout: true,
            return_complete_error: false,
            lfs_mode: LfsMode::Inflated,
            skip_notify_frontend: false,
        }
    }
}

impl Opts<'_> {
    pub fn new_with_ignored<'a>(ignored_errors: &'a [&'a str]) -> Opts<'a> {
        Opts {
            ignored_errors,
            should_log_stdout: true,
            return_complete_error: false,
            lfs_mode: LfsMode::Inflated,
            skip_notify_frontend: false,
        }
    }

    pub fn new_without_logs<'a>() -> Opts<'a> {
        Opts {
            ignored_errors: &[],
            should_log_stdout: false,
            return_complete_error: false,
            lfs_mode: LfsMode::Inflated,
            skip_notify_frontend: false,
        }
    }

    pub fn new_with_complete_error<'a>() -> Opts<'a> {
        Opts {
            ignored_errors: &[],
            should_log_stdout: true,
            return_complete_error: true,
            lfs_mode: LfsMode::Inflated,
            skip_notify_frontend: false,
        }
    }

    pub fn with_complete_error(mut self) -> Self {
        self.return_complete_error = true;
        self
    }

    pub fn with_lfs_stubs(mut self) -> Self {
        self.lfs_mode = LfsMode::Stubs;
        self
    }

    pub fn with_skip_notify_frontend(mut self) -> Self {
        self.skip_notify_frontend = true;
        self
    }
}

pub fn parse_bool_string(bool_str: &str) -> anyhow::Result<bool> {
    if bool_str == "true" || bool_str == "yes" || bool_str == "1" {
        return Ok(true);
    } else if bool_str == "false" || bool_str == "no" || bool_str == "0" {
        return Ok(false);
    }

    bail!("Unable to parse string")
}

impl Git {
    pub fn new(repo_path: PathBuf, tx: std::sync::mpsc::Sender<String>) -> Git {
        Git { repo_path, tx }
    }

    pub async fn head_commit(
        &self,
        format: CommitFormat,
        commit_head: CommitHead,
    ) -> anyhow::Result<String> {
        let mut log = Command::new("git");
        log.arg("log");
        log.arg("-1");
        log.arg("--format=\"%H\"");
        if commit_head == CommitHead::Remote {
            log.arg("FETCH_HEAD");
        }
        log.current_dir(&self.repo_path.canonicalize()?);

        #[cfg(windows)]
        log.creation_flags(CREATE_NO_WINDOW);

        let output = log.output().await?;

        let stdout = std::str::from_utf8(&output.stdout)?;
        let commit = stdout.lines().take(1).next().unwrap_or_default();
        let commit = commit.strip_prefix('"').unwrap_or(commit);
        let commit = commit.strip_suffix('"').unwrap_or(commit);
        let commit = if format == CommitFormat::Short {
            commit.get(..8).unwrap_or(commit)
        } else {
            commit
        };
        Ok(commit.to_string())
    }

    pub async fn fetch<'a>(&self, prune: ShouldPrune, opts: Opts<'a>) -> anyhow::Result<()> {
        if prune == ShouldPrune::Yes {
            self.run(
                &[
                    "fetch",
                    "--prune",
                    "--no-auto-maintenance",
                    "--show-forced-updates",
                ],
                opts,
            )
            .await
        } else {
            self.run(
                &["fetch", "--no-auto-maintenance", "--show-forced-updates"],
                opts,
            )
            .await
        }
    }

    pub async fn commit(&self, message: &str) -> anyhow::Result<()> {
        self.run(&["commit", "-m", message], Opts::default()).await
    }

    pub async fn pull(
        &self,
        pull_strategy: PullStrategy,
        stash_strategy: PullStashStrategy,
    ) -> anyhow::Result<()> {
        let current_branch = self.current_branch().await?;

        // TODO: handle alternative remotes
        let mut args = vec!["pull", "origin", &current_branch];

        match pull_strategy {
            PullStrategy::Rebase => args.push("--rebase"),
            PullStrategy::FFOnly => {
                args.push("--ff-only");
                args.push("--no-rebase");
            }
        }

        match stash_strategy {
            PullStashStrategy::Autostash => args.push("--autostash"),
            PullStashStrategy::None => {}
        }

        self.run(&args, Opts::default()).await
    }

    pub async fn push(&self, branch: &str) -> anyhow::Result<()> {
        // We'll deal with alternative remotes later
        self.run(&["push", "origin", branch], Opts::default()).await
    }

    pub async fn wait_for_lock(&self) {
        let index_lock_path = self.repo_path.join(".git/index.lock");
        while index_lock_path.exists() {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }

    pub async fn hard_reset(&self, branch: &str) -> anyhow::Result<()> {
        // if .git/index.lock exists, wait for it to be gone
        self.wait_for_lock().await;

        self.run(&["reset", "--hard"], Opts::default()).await?;
        self.run(&["clean", "-fd"], Opts::default()).await?;
        self.run(&["checkout", branch], Opts::default()).await
    }

    pub async fn checkout(&self, branch_or_commit: &str) -> anyhow::Result<()> {
        self.run(&["checkout", branch_or_commit], Opts::default())
            .await
    }

    pub async fn merge(&self, git_ref: &str, merge_type: MergeType) -> anyhow::Result<()> {
        let merge_type = match merge_type {
            MergeType::FF => "--ff",
            MergeType::FFOnly => "--ff-only",
            MergeType::NoFF => "--no-ff",
        };
        self.run(&["merge", merge_type, git_ref], Opts::default())
            .await
    }

    pub async fn stash(&self, action: StashAction) -> anyhow::Result<bool> {
        let action = if action == StashAction::Push {
            "save"
        } else {
            "pop"
        };
        let output = self
            .run_and_collect_output(&["stash", action], Opts::default())
            .await?;
        Ok(!output.contains("No local changes to save"))
    }

    pub async fn list_snapshots(&self) -> anyhow::Result<Vec<Snapshot>> {
        let output = self
            .run_and_collect_output(
                &["stash", "list", "--pretty=format:%gd|%gs|%H|%aI"],
                Opts::new_without_logs(),
            )
            .await?;

        let snapshots = output
            .lines()
            .filter_map(|line| {
                if !line.contains(SNAPSHOT_PREFIX) {
                    return None;
                }

                let parts = line.split('|').collect::<Vec<_>>();
                if parts.len() < 4 {
                    debug!("Skipping line due to bad parse: {}", line);
                    return None;
                }

                let stash_index = parts[0].trim();
                let message = parts[1].split(SNAPSHOT_PREFIX).collect::<Vec<_>>()[1].trim();
                let commit = parts[2].trim();
                let date = parts[3].trim();

                match DateTime::parse_from_rfc3339(date) {
                    Ok(date) => Some(Snapshot {
                        commit: commit.to_string(),
                        message: message.to_string(),
                        timestamp: date.with_timezone(&Utc),
                        stash_index: stash_index.to_string(),
                    }),
                    Err(e) => {
                        info!("Failed to parse date: {}", e);
                        None
                    }
                }
            })
            .collect();

        Ok(snapshots)
    }

    #[instrument(name = "save_snapshot_all", skip_all, fields(message, keep_index))]
    pub async fn save_snapshot_all(
        &self,
        message: &str,
        keep_index: SaveSnapshotIndexOption,
    ) -> anyhow::Result<Snapshot> {
        self.save_snapshot(message, vec![], keep_index).await
    }

    #[instrument(name = "save_snapshot", skip_all, fields(message, keep_index))]
    pub async fn save_snapshot(
        &self,
        message: &str,
        paths: Vec<String>,
        keep_index: SaveSnapshotIndexOption,
    ) -> anyhow::Result<Snapshot> {
        self.wait_for_lock().await;

        if paths.is_empty() {
            self.run(&["add", "--", "."], Opts::default()).await?;
        } else {
            let mut temp_file = NamedTempFile::new()?;
            for path in &paths {
                writeln!(temp_file, "{}", path)?;
            }
            temp_file.flush()?;

            self.run(
                &[
                    "add",
                    "--pathspec-from-file",
                    temp_file.path().to_str().unwrap(),
                ],
                Opts::default(),
            )
            .await?;
        }

        let stash_message = format!("{} {}", SNAPSHOT_PREFIX, message);
        let mut stash_create_args = vec!["stash", "create"];
        if keep_index == SaveSnapshotIndexOption::KeepIndex {
            stash_create_args.push("--keep-index");
        }

        // if paths is empty, stash everything
        let mut temp_file = NamedTempFile::new()?;
        if paths.is_empty() {
            stash_create_args.push(".");
        } else {
            // set up a temp file
            for path in &paths {
                writeln!(temp_file, "{}", path)?;
            }
            temp_file.flush()?;

            stash_create_args.push("--pathspec-from-file");
            stash_create_args.push(temp_file.path().to_str().unwrap());
        }

        // We use the stash create and store commands because stash push modifies the working
        // tree, and we need to avoid doing that when the engine is running, as it could cause
        // the command to fail and put the repo into a bad state. The create/store commands
        // create a stash commit and store it into the stash list, respectively, without
        // messing with the working tree at all.
        let stash_sha = self
            .run_and_collect_output(&stash_create_args, Opts::default())
            .await?;

        let stash_store_args = &["stash", "store", "--message", &stash_message, &stash_sha];
        self.run(stash_store_args, Opts::default()).await?;

        let snapshots = self.list_snapshots().await?;

        let first = snapshots.first();
        assert!(first.is_some(), "Failed to get snapshot");

        // if we have more than 25 snapshots, drop the oldest one
        if snapshots.len() > 25 {
            if let Some(snapshot) = snapshots.get(25) {
                self.run(&["stash", "drop", &snapshot.stash_index], Opts::default())
                    .await?;
            }
        }

        let mut temp_file = NamedTempFile::new()?;
        for path in &paths {
            writeln!(temp_file, "{}", path)?;
        }
        temp_file.flush()?;

        let mut args = vec!["reset"];
        args.push("--pathspec-from-file");
        args.push(temp_file.path().to_str().unwrap());

        self.run(&args, Opts::default()).await?;

        // We can unwrap because we asserted earlier
        Ok(first.unwrap().clone())
    }

    pub async fn delete_snapshot(&self, commit: &str) -> anyhow::Result<()> {
        let snapshots = self.list_snapshots().await?;

        let snapshot = snapshots.iter().find(|s| s.commit == commit);

        if let Some(snapshot) = snapshot {
            self.run(&["stash", "drop", &snapshot.stash_index], Opts::default())
                .await
        } else {
            Ok(())
        }
    }

    pub async fn restore_snapshot(
        &self,
        commit: &str,
        currently_modified_files: Vec<File>,
    ) -> anyhow::Result<()> {
        self.wait_for_lock().await;

        // get list of files in commit
        let files = self
            .run_and_collect_output(&["stash", "show", "--name-only", commit], Opts::default())
            .await?;

        // bail if there are any conflicting files in the stash
        if files
            .lines()
            .any(|f| currently_modified_files.iter().any(|cf| cf.path == *f))
        {
            bail!("Cannot restore snapshot due to conflicting files");
        }

        // check out the files from the stash
        let apply_args = vec!["stash", "apply", commit];
        self.run(&apply_args, Opts::default()).await?;

        // reset so everything is unstaged
        let mut temp_file = NamedTempFile::new()?;
        for path in files.lines() {
            writeln!(temp_file, "{}", path)?;
        }
        temp_file.flush()?;

        self.run(
            &[
                "reset",
                "--pathspec-from-file",
                temp_file.path().to_str().unwrap(),
            ],
            Opts::default(),
        )
        .await
        .map_err(|e| anyhow!("Failed to reset: {}", e))
    }

    pub async fn delete_branch(&self, branch: &str, branch_type: BranchType) -> anyhow::Result<()> {
        let args = match branch_type {
            BranchType::Local => vec!["branch", "-D", branch],
            BranchType::Remote => vec!["push", "-d", "origin", branch],
        };
        // Ignore "remote ref does not exist" error - if that's the case, then the remote branch that we want to delete
        // doesn't exist anyway, so it's effectively deleted
        self.run(
            &args,
            Opts::new_with_ignored(&["remote ref does not exist"]),
        )
        .await
    }

    pub async fn has_remote_branch(&self, branch: &str) -> anyhow::Result<bool> {
        let output = self
            .run_and_collect_output(&["ls-remote", "--heads", "origin", branch], Opts::default())
            .await?;

        Ok(!output.is_empty())
    }

    pub async fn verify_locks(&self) -> anyhow::Result<VerifyLocksResponse> {
        let output = match self
            .run_and_collect_output(
                &["lfs", "locks", "--verify", "--json"],
                Opts::new_without_logs().with_complete_error(),
            )
            .await
        {
            Ok(output) => output,
            Err(e) => {
                // if this is a problem with the lock cache, delete the lock cache file
                if e.to_string().contains("lockcache.db") {
                    let lock_cache_path = self.repo_path.join(".git/lfs/lockcache.db");
                    if lock_cache_path.exists() {
                        std::fs::remove_file(lock_cache_path)?;
                    }
                }

                // then try again
                self.run_and_collect_output(
                    &["lfs", "locks", "--verify", "--json"],
                    Opts::new_without_logs(),
                )
                .await?
            }
        };

        let response: VerifyLocksResponse = serde_json::from_str(&output)?;

        Ok(response)
    }

    pub async fn log(&self, limit: usize, git_ref: &str) -> anyhow::Result<String> {
        self.run_and_collect_output(
            &[
                "--no-pager",
                "log",
                &format!("-{}", limit),
                "--pretty=format:%H|%s|%an|%aI",
                git_ref,
            ],
            Opts::new_without_logs(),
        )
        .await
    }

    pub async fn version(&self) -> anyhow::Result<String> {
        self.run_and_collect_output(&["version"], Opts::default())
            .await
    }

    pub async fn status(&self, paths: Vec<String>) -> anyhow::Result<String> {
        let mut args = vec!["status", "--porcelain", "-uall", "--branch"];

        if !paths.is_empty() {
            args.push("--");

            paths.iter().for_each(|path| {
                args.push(path);
            });
        }

        self.run_and_collect_output(&args, Opts::new_without_logs())
            .await
    }

    pub async fn current_branch(&self) -> anyhow::Result<String> {
        let output = self
            .run_and_collect_output(&["branch", "--show-current"], Opts::default())
            .await?;

        Ok(output
            .lines()
            .take(1)
            .next()
            .unwrap_or_default()
            .to_string())
    }

    pub async fn get_ahead_behind(
        &self,
        from_ref: &str,
        to_ref: &str,
    ) -> anyhow::Result<(u32, u32)> {
        let output = self
            .run_and_collect_output(
                &[
                    "rev-list",
                    "--count",
                    "--left-only",
                    &format!("{}...{}", from_ref, to_ref),
                ],
                Opts::default(),
            )
            .await?;
        let behind_count = output.lines().next().unwrap_or("0").parse::<u32>().unwrap();

        let output = self
            .run_and_collect_output(
                &[
                    "rev-list",
                    "--count",
                    "--right-only",
                    &format!("{}...{}", from_ref, to_ref),
                ],
                Opts::default(),
            )
            .await?;
        let ahead_count = output.lines().next().unwrap_or("0").parse::<u32>().unwrap();

        Ok((ahead_count, behind_count))
    }

    pub async fn diff_filenames(&self, range: &str) -> anyhow::Result<Vec<String>> {
        let output = self
            .run_and_collect_output(&["diff", "--name-only", range], Opts::new_without_logs())
            .await?;
        let mut result = output.lines().map(|s| s.to_string()).collect::<Vec<_>>();
        result.dedup();
        Ok(result)
    }

    pub async fn abort_rebase(&self) -> anyhow::Result<()> {
        self.run(&["rebase", "--abort"], Opts::default()).await
    }

    pub async fn quit_rebase(&self) -> anyhow::Result<()> {
        self.run(&["rebase", "--quit"], Opts::default()).await
    }

    pub async fn run_maintenance(&self) -> anyhow::Result<()> {
        self.run(&["maintenance", "run", "--auto"], Opts::default())
            .await
    }

    pub async fn run_gc(&self) -> anyhow::Result<()> {
        self.run(&["gc", "--prune=now"], Opts::default()).await
    }

    pub async fn expire_reflog(&self) -> anyhow::Result<()> {
        self.run(
            &[
                "reflog",
                "expire",
                "--expire-unreachable=30.days",
                "--expire=30.days",
                "--all",
            ],
            Opts::default(),
        )
        .await
    }

    // sample output of: git worktree list --porcelain
    // worktree D:/repos/fellowship
    // HEAD 6ca1438e074b664470df54319cd6272a4d4d565d
    // branch refs/heads/f11r-rjd-test3
    //
    // worktree D:/repos/fellowship-wt
    // HEAD b09777a82eaa707f722bcf0d2566b6104f43bc11
    // detached
    pub async fn list_worktrees(&self) -> anyhow::Result<Vec<WorktreeInfo>> {
        let output = self
            .run_and_collect_output(
                &["worktree", "list", "--porcelain"],
                Opts {
                    ignored_errors: &[],
                    should_log_stdout: false,
                    return_complete_error: true,
                    lfs_mode: LfsMode::Stubs,
                    skip_notify_frontend: false,
                },
            )
            .await?;

        let mut entries: Vec<WorktreeInfo> = vec![];
        let mut info = WorktreeInfo::default();
        for line in output.lines() {
            if line.is_empty() {
                continue;
            }

            if let Some(caps) = WORKTREE_DIR_REGEX.captures(line) {
                info.directory = caps[1].to_string().into();
            } else if let Some(caps) = WORKTREE_SHA_REGEX.captures(line) {
                info.sha = caps[1].to_string();
            } else if let Some(caps) = WORKTREE_BRANCH_REGEX.captures(line) {
                info.branch = caps.get(2).map(|m| m.as_str().to_string());
                entries.push(info.clone());
                info = WorktreeInfo::default();
            }
        }

        Ok(entries)
    }

    pub async fn set_config(&self, key: &str, value: &str) -> anyhow::Result<()> {
        self.run(&["config", key, value], Opts::default()).await
    }

    #[instrument]
    pub async fn configure_untracked_cache(&self) -> anyhow::Result<()> {
        // get current setting for core.untrackedCache
        let current_setting = match self
            .run_and_collect_output(&["config", "core.untrackedCache"], Opts::default())
            .await
        {
            Ok(output) => output,
            Err(_) => "false".to_string(),
        };

        // if it's already true, do nothing
        if current_setting.trim() == "true" {
            return Ok(());
        }

        // we need to check if the system supports mtime first
        let status = self
            .run(&["update-index", "--test-untracked-cache"], Opts::default())
            .await;

        // if it does, enable untracked cache
        if status.is_ok() {
            self.run(&["update-index", "--untracked-cache"], Opts::default())
                .await?;
            self.set_config("core.untrackedCache", "true").await?;
        }

        Ok(())
    }

    pub async fn get_username(&self) -> anyhow::Result<String> {
        let username = self
            .run_and_collect_output(&["config", "user.name"], Opts::default())
            .await?;
        Ok(username)
    }

    pub async fn run_and_collect_output<'a>(
        &self,
        args: &[&str],
        opts: Opts<'a>,
    ) -> anyhow::Result<String> {
        // assert we have at least one arg
        if args.is_empty() {
            bail!("No arguments provided to git command");
        }

        let mut sys = System::new();
        sys.refresh_processes_specifics(
            ProcessRefreshKind::new().with_exe(UpdateKind::OnlyIfNotSet),
        );
        // Just bail if we have more than 3 git-credential-manager procs running, because it might be death spiraling
        if sys.processes_by_name("git-credential-manager").count() > 3 {
            bail!("User may need to authenticate with the git credential manager");
        }

        let mut output = Some(String::new());
        let res = self
            .run_and_collect_output_internal(args, opts, &mut output)
            .await;
        match res {
            Err(e) => {
                bail!("git {:?} failed with error: {}", args, e);
            }
            Ok(_) => Ok(output.unwrap()),
        }
    }

    pub async fn run_and_collect_output_into_lines<'a>(
        &self,
        args: &[&str],
        opts: Opts<'a>,
    ) -> anyhow::Result<Vec<String>> {
        // assert we have at least one arg
        if args.is_empty() {
            bail!("No arguments provided to git command");
        }

        let output = self.run_and_collect_output(args, opts).await?;
        let lines = output.lines().map(|s| s.to_string()).collect::<Vec<_>>();
        Ok(lines)
    }

    pub async fn run<'a>(&self, args: &[&str], opts: Opts<'a>) -> anyhow::Result<()> {
        // assert we have at least one arg
        if args.is_empty() {
            bail!("No arguments provided to git command");
        }

        let res = self
            .run_and_collect_output_internal(args, opts, &mut None)
            .await;
        match res {
            Err(e) => {
                error!("git {:?} failed with error: {}", args, e);
                bail!("Git command failed. Check the log for details.");
            }
            Ok(_) => Ok(()),
        }
    }

    #[instrument(fields(otel.name = format!("git {}", args[0]).as_str()))]
    async fn run_and_collect_output_internal<'a>(
        &self,
        args: &[&str],
        opts: Opts<'a>,
        output: &mut Option<String>,
    ) -> anyhow::Result<()> {
        let message = if args.len() <= 8 {
            format!("Running 'git {}'", args.join(" "))
        } else {
            format!(
                "Running 'git {} (+ {} more)'",
                args[..8].join(" "),
                args.len() - 8
            )
        };

        if !opts.skip_notify_frontend {
            if let Err(e) = self.tx.send(message) {
                warn!("Failed to send git command message: {}", e);
            }
        }

        let mut cmd = Command::new("git");
        for arg in args {
            cmd.arg(arg);
        }

        // disable clone protection
        cmd.env("GIT_CLONE_PROTECTION_ACTIVE", "false");

        if opts.lfs_mode == LfsMode::Stubs {
            cmd.env("GIT_LFS_SKIP_SMUDGE", "1");
        }

        if !&self.repo_path.as_os_str().is_empty() {
            // if the first arg is clone, set current dir to the parent, then canonicalize
            if args[0] == "clone" {
                cmd.current_dir(&self.repo_path.parent().unwrap().canonicalize()?);
            } else {
                cmd.current_dir(&self.repo_path.canonicalize()?);
            }
        }

        #[cfg(windows)]
        cmd.creation_flags(CREATE_NO_WINDOW);

        let mut git_cmd_str: String = "git".to_string();
        for arg in args {
            git_cmd_str.push(' ');
            git_cmd_str.push_str(arg);
        }
        info!("Running: {}", git_cmd_str);

        let out_pipe = Stdio::piped();
        let err_pipe = Stdio::piped();

        let mut git_proc = match cmd.stdout(out_pipe).stderr(err_pipe).spawn() {
            Ok(p) => p,
            Err(e) => {
                error!("Failed to run: {}. Reason: {}", git_cmd_str, e);
                bail!("Failed to run git command. Check the log for details.");
            }
        };

        let stdout = git_proc.stdout.take().expect("Failed to get stdout");
        let stderr = git_proc.stderr.take().expect("Failed to get stderr");

        let mut out_reader = BufReader::new(stdout).lines();
        let mut err_reader = BufReader::new(stderr).lines();

        let out_lines: Arc<RwLock<Vec<String>>> = Arc::new(RwLock::new(vec![]));
        let err_lines: Arc<RwLock<Vec<String>>> = Arc::new(RwLock::new(vec![]));

        let out_lines_thread = out_lines.clone();
        let is_collecting_out_lines = output.is_some();
        let should_log_stdout = opts.should_log_stdout;
        tokio::spawn(async move {
            while let Some(line) = out_reader.next_line().await.unwrap() {
                // Shorten any line starting with "Updating files". Git currently sends us a huge
                // wall of text with all the individual percentage updates, instead of one line
                // per update.
                let mut line = line;

                if line.contains("Updating files") {
                    line = "Updating files...".to_string();
                }

                if is_collecting_out_lines {
                    out_lines_thread.write().push(line.clone());
                }
                if should_log_stdout {
                    info!("{}", line);
                }
            }
        });

        let err_lines_thread = err_lines.clone();
        tokio::spawn(async move {
            while let Some(line) = err_reader.next_line().await.unwrap() {
                err_lines_thread.write().push(line.clone());
                info!("{}", line);
            }
        });

        let status = git_proc.wait().await?;

        if !status.success() {
            // git config --get <blah> has empty output with a bad exit code if the variable is
            // unset, so just handle that case gracefully here to avoid spamming logging with errors
            let should_skip_error_logging =
                args.len() >= 2 && args[0] == "config" && args[1] == "--get";

            if !should_skip_error_logging {
                let mut failed = true;
                let locked_lines = err_lines.read();
                for line in &*locked_lines {
                    for err in opts.ignored_errors {
                        if line.contains(*err) {
                            failed = false;
                            break;
                        }
                    }
                    if !failed {
                        break;
                    }
                }

                if failed {
                    let locked_lines = err_lines.read();
                    let err_output: String = locked_lines.join("\n");
                    error!("Failed to run: {}.\n{}", git_cmd_str, err_output);

                    if opts.return_complete_error {
                        bail!("Git command failed: {}", err_output);
                    }

                    bail!("Git command failed. Check the log for details.");
                }
            }
        }

        if output.is_some() {
            let locked_lines = out_lines.read();
            *output = Some(locked_lines.join("\n"));
        }

        Ok(())
    }
}

pub async fn configure_global(key: &str, value: &str) -> Result<(), CoreError> {
    let mut cmd = Command::new("git");
    cmd.arg("config").arg("--global").arg(key).arg(value);

    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);

    let cmd_output = cmd.output().await?;
    if !cmd_output.status.success() {
        let err_output = String::from_utf8(cmd_output.stderr).unwrap_or_default();
        error!(
            "Failed to run: git config --global {} {}.\n{}",
            key, value, err_output
        );

        return Err(CoreError::Internal(anyhow!(
            "Git config failed: {}",
            err_output
        )));
    }

    Ok(())
}
