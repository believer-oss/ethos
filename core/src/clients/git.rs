use std::collections::HashMap;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;

use anyhow::{anyhow, bail};
use chrono::{DateTime, Utc};
use futures::StreamExt;
use lazy_static::lazy_static;
use parking_lot::RwLock;
use regex::Regex;
use sysinfo::{ProcessRefreshKind, System, UpdateKind};
use tempfile::{NamedTempFile, TempPath};
use tokio::io::AsyncBufReadExt;
use tokio::io::BufReader;
use tokio::process::Command;
use tokio::sync::Mutex;
use tracing::warn;
use tracing::{debug, error, info, instrument};

use crate::types::errors::CoreError;
use crate::types::locks::VerifyLocksResponse;
use crate::types::repo::File;
use crate::types::repo::FileState;
use crate::types::repo::Snapshot;

static SNAPSHOT_PREFIX: &str = "snapshot";

lazy_static! {
    static ref WORKTREE_DIR_REGEX: Regex = Regex::new(r"^worktree (.+)").unwrap();
    static ref WORKTREE_SHA_REGEX: Regex = Regex::new(r"^HEAD (.+)").unwrap();
    static ref WORKTREE_BRANCH_REGEX: Regex = Regex::new(r"^(branch|detached)\s*(.+)?").unwrap();
    static ref GIT_FETCH_LOCK: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));

    /// Process-wide serialization gate for *every* `git` subprocess this app
    /// spawns. Two `git` processes running concurrently against the same repo
    /// and credential store race destructively: Git Credential Manager stomps
    /// the stored credential (surfacing as spurious re-auth prompts), and the
    /// index / refs / packfiles can collide. Every subprocess spawn site holds
    /// this guard for the lifetime of its child, so the app never has two `git`
    /// processes in flight at once. External git processes (a user's terminal,
    /// the engine) are out of scope — `wait_for_lock` mitigates index.lock
    /// contention with those.
    ///
    /// INVARIANT — this lock is NOT re-entrant (tokio's Mutex deadlocks on a
    /// second acquire from the same task). Every acquisition site is a leaf
    /// that spawns exactly one subprocess and never calls another acquiring
    /// function while holding the guard. `GIT_FETCH_LOCK` is always taken
    /// *before* this lock (in `fetch`/`refetch`) and never after, so the two
    /// have a consistent order and cannot deadlock against each other.
    static ref GIT_PROCESS_LOCK: Arc<Mutex<()>> = Arc::new(Mutex::new(()));

    /// Who currently holds [`GIT_PROCESS_LOCK`], for observability only. Set when
    /// the guard is acquired and cleared when it drops, so a waiting caller can
    /// name the op it's queued behind (and how long that op has held the lock)
    /// in the logs — turning "the app feels stuck on git" into a line that says
    /// which command is holding things up. A plain (sync) parking_lot mutex: it's
    /// a leaf, taken only briefly and never across an `.await`, so it adds no
    /// ordering constraints against the locks above.
    static ref GIT_LOCK_HOLDER: parking_lot::Mutex<Option<GitLockHolder>> =
        parking_lot::Mutex::new(None);
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
    // When set, the git command runs with interactive credential prompts
    // disabled. Use for background/non-user-initiated operations so a stale
    // credential fails quietly instead of spawning a login window.
    pub skip_interactive_auth: bool,
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
            skip_interactive_auth: false,
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
            skip_interactive_auth: false,
        }
    }

    pub fn new_without_logs<'a>() -> Opts<'a> {
        Opts {
            ignored_errors: &[],
            should_log_stdout: false,
            return_complete_error: false,
            lfs_mode: LfsMode::Inflated,
            skip_notify_frontend: false,
            skip_interactive_auth: false,
        }
    }

    pub fn new_with_complete_error<'a>() -> Opts<'a> {
        Opts {
            ignored_errors: &[],
            should_log_stdout: true,
            return_complete_error: true,
            lfs_mode: LfsMode::Inflated,
            skip_notify_frontend: false,
            skip_interactive_auth: false,
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

    pub fn with_skip_interactive_auth(mut self) -> Self {
        self.skip_interactive_auth = true;
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

/// Idle (no-progress) backstop for a single `git` subprocess. Because every git
/// process now holds [`GIT_PROCESS_LOCK`] for its whole lifetime, a hung process
/// — a stuck credential prompt, or a dead connection that never makes progress —
/// would otherwise hold the lock forever and wedge *every* git operation in the
/// app. The runner kills such a process so the lock is released and callers get
/// an error.
///
/// This is deliberately an *idle* timeout, not a wall-clock one: a hung process
/// is distinguished from a slow-but-healthy one (a large clone / fetch / LFS
/// pull streaming progress over a slow link) by the *absence of output*, not by
/// total elapsed time. We reset the clock on every line the child writes, so a
/// command that keeps making progress is never killed no matter how long it
/// runs, while one that goes completely silent for this long is treated as hung.
/// Generous on purpose — only true silence should trip it; it can be lowered now
/// that progressing ops are safe.
const GIT_PROCESS_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30 * 60);

/// How often the idle watchdog wakes to compare now against the child's last
/// output. Bounds how long past [`GIT_PROCESS_IDLE_TIMEOUT`] a hung process can
/// linger before it's killed; small relative to the timeout, large enough to be
/// negligible overhead.
const GIT_PROCESS_IDLE_CHECK: std::time::Duration = std::time::Duration::from_secs(60);

/// A caller holding [`GIT_PROCESS_LOCK`], tracked purely for log diagnostics.
struct GitLockHolder {
    /// Short description of the git op (typically the command line).
    label: String,
    /// When the lock was acquired, so waiters can report how long it's been held.
    since: std::time::Instant,
}

/// If the lock is already held this long when a new caller arrives, that's a
/// stall worth a warning (a slow sync, or an op on its way to the idle-timeout
/// kill) rather than ordinary brief contention.
const GIT_LOCK_SLOW_HOLD: std::time::Duration = std::time::Duration::from_secs(60);

/// RAII guard for [`GIT_PROCESS_LOCK`] that also clears [`GIT_LOCK_HOLDER`] on
/// drop. Returned by [`acquire_git_process_lock`]; hold it for the lifetime of
/// a single git subprocess.
pub struct GitLockGuard {
    // Dropped after this struct's `Drop::drop` runs, so the holder is cleared
    // just before the mutex is released.
    _guard: tokio::sync::OwnedMutexGuard<()>,
}

impl Drop for GitLockGuard {
    fn drop(&mut self) {
        *GIT_LOCK_HOLDER.lock() = None;
    }
}

/// Acquire the process-wide git serialization guard. Hold the returned guard
/// for the full lifetime of a `git` subprocess (spawn through wait). See
/// [`GIT_PROCESS_LOCK`] for the invariant — never call this while already
/// holding the guard. This applies to external callers in other crates too:
/// hold it across a single spawn+wait and never call another git-spawning
/// function while holding it.
///
/// `label` names the op for diagnostics (use the git command line where you
/// have it). When the lock is contended, the wait is logged with the op that's
/// holding it, so a stuck or slow git operation is visible in the logs.
pub async fn acquire_git_process_lock(label: &str) -> GitLockGuard {
    let guard = match GIT_PROCESS_LOCK.clone().try_lock_owned() {
        Ok(g) => g,
        Err(_) => {
            // Contended — name who we're queued behind. Read + drop the holder
            // lock before awaiting (never hold a sync mutex across `.await`).
            let held = GIT_LOCK_HOLDER
                .lock()
                .as_ref()
                .map(|h| (h.label.clone(), h.since.elapsed()));
            match held {
                Some((holder, held_for)) if held_for >= GIT_LOCK_SLOW_HOLD => {
                    warn!(
                        "git op {:?} blocked: lock held by {:?} for {:?} (slow op or stall)",
                        label, holder, held_for
                    );
                }
                Some((holder, held_for)) => {
                    debug!(
                        "git op {:?} waiting on lock held by {:?} (held {:?})",
                        label, holder, held_for
                    );
                }
                None => {}
            }
            GIT_PROCESS_LOCK.clone().lock_owned().await
        }
    };

    *GIT_LOCK_HOLDER.lock() = Some(GitLockHolder {
        label: label.to_string(),
        since: std::time::Instant::now(),
    });

    GitLockGuard { _guard: guard }
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

        // Reading FETCH_HEAD/HEAD races a concurrent fetch rewriting the same
        // ref; serialize like every other git subprocess.
        let _git_lock = acquire_git_process_lock("git log -1 (head_commit)").await;
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

    pub async fn fetch(&self, prune: ShouldPrune, opts: Opts<'_>) -> anyhow::Result<()> {
        // In the event that a fetch is already running, we will ignore the prune flag and return
        // after the current fetch completes. This should not be a problem, as the next fetch that
        // is not under contention will respect the prune flag.
        if let Ok(mut running) = GIT_FETCH_LOCK.clone().try_lock_owned() {
            *running = true;
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
                .await?
            } else {
                self.run(
                    &["fetch", "--no-auto-maintenance", "--show-forced-updates"],
                    opts,
                )
                .await?
            }
            *running = false;
            Ok(())
        } else {
            // If we get a TryLockError, it means that a fetch is already running, so we should
            // just block until it's done.
            GIT_FETCH_LOCK.clone().lock_owned().await;
            Ok(())
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
                // Split only at the first occurrence so messages that themselves contain
                // the word "snapshot" (e.g. "Auto-snapshot before importing X.zip") are
                // preserved in full instead of being truncated at the embedded match.
                let message = match parts[1].split_once(SNAPSHOT_PREFIX) {
                    Some((_, m)) => m.trim(),
                    None => return None,
                };
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

    #[instrument(name = "save_snapshot_all", skip_all, fields(message))]
    pub async fn save_snapshot_all(&self, message: &str) -> anyhow::Result<Snapshot> {
        self.save_snapshot(message, vec![]).await
    }

    #[instrument(name = "save_snapshot", skip_all, fields(message))]
    pub async fn save_snapshot(
        &self,
        message: &str,
        paths: Vec<String>,
    ) -> anyhow::Result<Snapshot> {
        self.wait_for_lock().await;

        let stash_message = format!("{SNAPSHOT_PREFIX} {message}");
        let snapshot_commit = self.build_snapshot_commit(&paths, &stash_message).await?;

        self.run(
            &[
                "stash",
                "store",
                "--message",
                &stash_message,
                &snapshot_commit,
            ],
            Opts::default(),
        )
        .await?;

        // Build the Snapshot from data we already have rather than
        // searching for our just-stored entry in `list_snapshots`.
        // The lookup was platform-fragile — `git stash list` against the
        // reflog-subject formatting from `git stash store --message ...`
        // doesn't behave the same across git versions, and on some
        // Linux builds the entry doesn't pass our SNAPSHOT_PREFIX filter
        // even when the store itself succeeded. `git stash store` always
        // pushes to the top of refs/stash, so the just-stored snapshot
        // is at stash@{0} until anything else touches the stash list.
        let new_snapshot = Snapshot {
            commit: snapshot_commit,
            message: message.to_string(),
            timestamp: Utc::now(),
            stash_index: "stash@{0}".to_string(),
        };

        // Keep at most 25 snapshots — drop the oldest if we exceeded.
        // Best-effort: if list_snapshots hiccups for any reason we'd
        // rather skip pruning than fail the save itself.
        if let Ok(snapshots) = self.list_snapshots().await {
            if snapshots.len() > 25 {
                if let Some(snapshot) = snapshots.get(25) {
                    let _ = self
                        .run(&["stash", "drop", &snapshot.stash_index], Opts::default())
                        .await;
                }
            }
        }

        Ok(new_snapshot)
    }

    /// Build a stash-shaped commit that captures exactly the requested paths
    /// (or every dirty + untracked path when `paths` is empty). All staging
    /// happens against a temp index via `GIT_INDEX_FILE`, so the user's real
    /// index and working tree are not touched.
    ///
    /// The returned commit has two parents — `HEAD` and an "index" commit
    /// pointing at the same snapshot tree — which matches the shape `git stash
    /// create` produces. That lets `restore_snapshot_via_cherry_pick`'s
    /// `git cherry-pick -m1` keep working unchanged.
    async fn build_snapshot_commit(
        &self,
        paths: &[String],
        stash_message: &str,
    ) -> anyhow::Result<String> {
        // Resolve HEAD to a SHA once. We use the same SHA for `read-tree` (the
        // tree we diff against) and for `commit-tree -p` (the first parent),
        // so a concurrent HEAD-changing op can't leave us with a snapshot
        // tree built against an old HEAD but parented to a new one.
        let head_sha = self
            .run_and_collect_output(&["rev-parse", "HEAD"], Opts::default())
            .await?
            .trim()
            .to_string();

        // Use a temp DIR (not NamedTempFile) for GIT_INDEX_FILE. NamedTempFile
        // keeps an open handle on the file; on Windows that can race git's
        // lock-then-rename pattern when it writes the index and surface as
        // "could not write index". With a tempdir we just hand git a path and
        // let it create the file itself.
        let temp_dir = tempfile::tempdir()?;
        let temp_index_path = temp_dir.path().join("snapshot-index");

        // Seed the temp index with HEAD so subsequent `git add`/`git rm` calls
        // produce a tree diffed against HEAD.
        self.run_with_index_file(&["read-tree", &head_sha], &temp_index_path)
            .await?;

        if paths.is_empty() {
            // Full snapshot: a single `git add -A -- .` walks the working tree
            // once and stages additions, modifications, AND deletions into the
            // temp index. Doing this via `ls-files` + `git add --pathspec-from-
            // file` is functionally equivalent but walks the tree twice, which
            // is noticeable on a large Unreal repo.
            self.run_with_index_file(&["add", "-A", "--", "."], &temp_index_path)
                .await?;
        } else {
            // Selective snapshot: files still on disk get staged via `git add`;
            // files already deleted need `git rm --cached` because
            // `git add <path>` rejects paths whose working-tree file is gone.
            let (deleted_paths, existing_paths): (Vec<&String>, Vec<&String>) =
                paths.iter().partition(|p| !self.repo_path.join(p).exists());

            if !existing_paths.is_empty() {
                let add_path = Self::write_pathspec_file(&existing_paths)?;
                self.run_with_index_file(
                    &[
                        "add",
                        "--pathspec-from-file",
                        add_path.to_str().expect("temp file path is non-UTF-8"),
                    ],
                    &temp_index_path,
                )
                .await?;
            }

            if !deleted_paths.is_empty() {
                let rm_path = Self::write_pathspec_file(&deleted_paths)?;
                self.run_with_index_file(
                    &[
                        "rm",
                        "--cached",
                        "--ignore-unmatch",
                        "--pathspec-from-file",
                        rm_path.to_str().expect("temp file path is non-UTF-8"),
                    ],
                    &temp_index_path,
                )
                .await?;
            }
        }

        let tree_sha = self
            .run_with_index_file(&["write-tree"], &temp_index_path)
            .await?
            .trim()
            .to_string();

        // Mimic `git stash create`'s 2-parent shape: parent 1 = HEAD,
        // parent 2 = an "index" commit pointing at the same tree.
        let index_commit_message = format!("index on {stash_message}");
        let index_commit = self
            .run_and_collect_output(
                &["commit-tree", &tree_sha, "-m", &index_commit_message],
                Opts::default(),
            )
            .await?
            .trim()
            .to_string();

        let snapshot_commit = self
            .run_and_collect_output(
                &[
                    "commit-tree",
                    &tree_sha,
                    "-p",
                    &head_sha,
                    "-p",
                    &index_commit,
                    "-m",
                    stash_message,
                ],
                Opts::default(),
            )
            .await?
            .trim()
            .to_string();

        Ok(snapshot_commit)
    }

    /// Run a git command against a specific index file. Used by the snapshot
    /// builder to keep its staging out of the user's real index. Returns the
    /// command's stdout on success; errors carry stderr in the message.
    async fn run_with_index_file(
        &self,
        args: &[&str],
        index_file: &Path,
    ) -> anyhow::Result<String> {
        let mut cmd = Command::new("git");
        for arg in args {
            cmd.arg(arg);
        }
        cmd.env("GIT_CLONE_PROTECTION_ACTIVE", "false");
        cmd.env("GIT_INDEX_FILE", index_file);
        if !self.repo_path.as_os_str().is_empty() {
            cmd.current_dir(&self.repo_path.canonicalize()?);
        }
        #[cfg(windows)]
        cmd.creation_flags(CREATE_NO_WINDOW);

        info!(
            "Running with temp index: git {} (index={})",
            args.join(" "),
            index_file.display()
        );

        // Operates on a temp index but still writes loose objects into the real
        // .git/objects — serialize against maintenance/gc and other git procs.
        let _git_lock =
            acquire_git_process_lock(&format!("git {} (temp index)", args.join(" "))).await;
        let output = cmd.output().await?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("git {:?} failed: {}", args, stderr.trim());
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// Write one pathspec per line to a temp file for `--pathspec-from-file`.
    /// Each line gets the `:(literal)` magic prefix: pathspecs are globs by
    /// default, so a real filename containing `[`/`]` would otherwise act as
    /// a character class and silently match sibling files (`a[bc]d.txt` also
    /// matches `abd.txt` and `acd.txt`). The write handle is closed before
    /// returning so the spawned git process is the file's only opener; the
    /// returned `TempPath` must be kept alive until that process exits.
    fn write_pathspec_file<I, S>(paths: I) -> anyhow::Result<TempPath>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut temp_file = NamedTempFile::new()?;
        for path in paths {
            writeln!(temp_file, ":(literal){}", path.as_ref())?;
        }
        temp_file.flush()?;
        Ok(temp_file.into_temp_path())
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

    /// User-facing snapshot restore. Always extracts the snapshot's view of
    /// the selected paths via the selective code path — when `paths_filter`
    /// is `None`, every path the snapshot touches is restored, giving the
    /// "restore everything" call a single mechanism with the same conflict
    /// semantics as a per-path restore.
    ///
    /// Pull/submit do NOT go through here — they invoke
    /// `restore_snapshot_via_cherry_pick` directly because they need
    /// 3-way-merge replay against a HEAD that may already contain upstream
    /// changes from a rebase.
    pub async fn restore_snapshot(
        &self,
        commit: &str,
        currently_local_files: Vec<File>,
        overwrite_local: bool,
        paths_filter: Option<Vec<String>>,
    ) -> anyhow::Result<()> {
        // Fetch the snapshot's entries once here and pass the selected
        // subset down — `restore_snapshot_selective` no longer needs to
        // re-run `git diff --name-status` itself.
        let entries = self.get_snapshot_entries_with_state(commit).await?;
        let selected: Vec<(String, FileState)> = match paths_filter {
            Some(f) => {
                let filter: std::collections::HashSet<&str> =
                    f.iter().map(String::as_str).collect();
                entries
                    .into_iter()
                    .filter(|(p, _)| filter.contains(p.as_str()))
                    .collect()
            }
            None => entries,
        };
        self.restore_snapshot_selective(commit, &selected, &currently_local_files, overwrite_local)
            .await
    }

    /// Replay a snapshot's diff onto current HEAD via `git cherry-pick`.
    /// Used as the post-pull/post-submit safety net: the working tree may
    /// already contain upstream changes merged in by autostash (pull) or
    /// have just been reset to the pre-submit branch (submit), and we need
    /// to re-apply the captured local edits on top. Cherry-pick gives us a
    /// 3-way merge against the snapshot's parent, so upstream changes
    /// applied by autostash aren't blindly stomped by snapshot contents.
    ///
    /// Intended only for those two internal callers — anything user-facing
    /// should go through `restore_snapshot`, which routes through the
    /// selective path and gets the cleaner overwrite/bail semantics.
    ///
    /// Untracked-file collisions during the cherry-pick are handled with
    /// the long-standing `.localcopy` / `.snapshotcopy` dance. The
    /// "are these files different?" comparison runs through
    /// `hash_object_with_attrs` rather than raw bytes so git's
    /// EOL/text-normalization rules apply symmetrically — otherwise a
    /// user's CRLF file and the cherry-picked LF copy register as a
    /// conflict purely because of `autocrlf` / `.gitattributes` smudging.
    pub async fn restore_snapshot_via_cherry_pick(
        &self,
        commit: &str,
        currently_modified_files: Vec<File>,
    ) -> anyhow::Result<()> {
        self.wait_for_lock().await;

        let snapshot_files = self.get_files_in_snapshot(commit).await?;
        let untracked_files = self.get_untracked_files().await?;

        // Rename untracked and modified files to .localcopy to avoid conflicts during restoration
        let mut renamed_files: Vec<(String, PathBuf)> = Vec::new();

        for untracked_file in &untracked_files {
            let source_path = self.repo_path.join(untracked_file);
            if source_path.exists() {
                let localcopy_path = self.repo_path.join(format!("{}.localcopy", untracked_file));
                std::fs::rename(&source_path, &localcopy_path)?;
                renamed_files.push((untracked_file.clone(), localcopy_path));
            }
        }

        let mut paths_to_unstage: Vec<&str> = Vec::new();
        for modified_file in &currently_modified_files {
            let source_path = self.repo_path.join(&modified_file.path);
            if source_path.exists() {
                let localcopy_path = self
                    .repo_path
                    .join(format!("{}.localcopy", modified_file.path));
                std::fs::rename(&source_path, &localcopy_path)?;
                renamed_files.push((modified_file.path.clone(), localcopy_path));
                paths_to_unstage.push(&modified_file.path);
            }
        }

        // Unstage every renamed file with one batched reset. Each `git reset`
        // rewrites the full index regardless of how many paths it's given, so
        // per-file resets cost O(files × index size) — the dominant cost of
        // restore with hundreds of checked-out assets on an Unreal-sized
        // index. Best-effort: a failed unstage surfaces downstream as a
        // cherry-pick or reset error, with the user's content already safe
        // in `.localcopy` files.
        if !paths_to_unstage.is_empty() {
            let unstage_path = Self::write_pathspec_file(&paths_to_unstage)?;
            self.run(
                &[
                    "reset",
                    "HEAD",
                    "--pathspec-from-file",
                    unstage_path.to_str().expect("temp file path is non-UTF-8"),
                ],
                Opts::default(),
            )
            .await
            .ok();
        }

        let cherry_pick_result = self
            .run(
                &["cherry-pick", "-n", "-m1", "--rerere-autoupdate", commit],
                Opts::default(),
            )
            .await;

        // reset so everything is unstaged
        let reset_path = Self::write_pathspec_file(&snapshot_files)?;

        let reset_result = self
            .run(
                &[
                    "reset",
                    "--pathspec-from-file",
                    reset_path.to_str().expect("temp file path is non-UTF-8"),
                ],
                Opts::default(),
            )
            .await;

        // Compare every renamed local copy against its restored counterpart
        // before touching disk. The comparisons are independent, so they run
        // with bounded concurrency — serially they were a per-asset cost on
        // syncs with hundreds of checked-out files. `None` means the restore
        // produced no file at that path (no conflict possible). The futures
        // are collected eagerly rather than mapped lazily inside the stream:
        // a closure returning an async block that borrows its argument trips
        // rustc's "implementation of `FnOnce` is not general enough"
        // limitation once callers wrap this future in `#[instrument]`.
        let comparison_futures: Vec<_> = renamed_files
            .iter()
            .map(|(original_path, localcopy_path)| {
                let target_path = self.repo_path.join(original_path);
                async move {
                    if target_path.exists() {
                        Some(
                            self.restored_file_matches_local(
                                original_path,
                                localcopy_path,
                                &target_path,
                            )
                            .await,
                        )
                    } else {
                        None
                    }
                }
            })
            .collect();
        let comparisons: Vec<Option<bool>> = futures::stream::iter(comparison_futures)
            .buffered(8)
            .collect()
            .await;

        // Restore renamed files and track conflicts
        let mut conflicts = Vec::new();

        for ((original_path, localcopy_path), identical) in
            renamed_files.into_iter().zip(comparisons)
        {
            let target_path = self.repo_path.join(&original_path);

            match identical {
                Some(true) => {
                    std::fs::remove_file(&localcopy_path)?;
                }
                Some(false) => {
                    let snapshot_copy_name = format!("{}.snapshotcopy", original_path);
                    let snapshot_copy_path = self.repo_path.join(&snapshot_copy_name);
                    std::fs::rename(&target_path, &snapshot_copy_path)?;
                    std::fs::rename(&localcopy_path, &target_path)?;
                    conflicts.push((original_path.clone(), snapshot_copy_name));
                }
                None => {
                    // No conflict, restore the file to its original name
                    std::fs::rename(&localcopy_path, &target_path)?;
                }
            }
        }

        // Always report conflicts first, regardless of Git operation success
        if !conflicts.is_empty() {
            let conflict_details: Vec<String> = conflicts
                .iter()
                .map(|(original, snapshot_copy)| {
                    format!(
                        "  - {} (snapshot version saved as {})",
                        original, snapshot_copy
                    )
                })
                .collect();

            let conflict_message = format!(
                "{} untracked file conflicts were found during snapshot restore:\n{}\n\nYour local untracked files have been preserved. The snapshot versions have been saved with '.snapshotcopy' extensions. Please review and resolve these conflicts manually.",
                conflicts.len(),
                conflict_details.join("\n")
            );

            // If Git operations failed, include conflict info with the Git error
            if let Err(git_error) = cherry_pick_result.as_ref().or(reset_result.as_ref()) {
                let error_msg = format!(
                    "Snapshot restore failed: {}\n\nAdditionally, {}",
                    git_error, conflict_message
                );
                error!("Snapshot restore failed with conflicts: {}", error_msg);
                bail!(error_msg);
            } else {
                // Git operations succeeded but we have conflicts
                let error_msg = format!("Snapshot restored successfully, but {}", conflict_message);
                warn!("Snapshot restore completed with conflicts: {}", error_msg);
                bail!(error_msg);
            }
        }

        // Check Git operation results (only after handling conflicts)
        if let Err(git_error) = cherry_pick_result {
            let error_msg = format!("Snapshot restore failed on cherrypick : {}\n\n", git_error);
            error!("Cherry-pick failed during snapshot restore: {}", error_msg);
            bail!(error_msg);
        }
        if let Err(git_error) = reset_result {
            let error_msg = format!("Snapshot restore failed on reset : {}\n\n", git_error);
            error!("Reset failed during snapshot restore: {}", error_msg);
            bail!(error_msg);
        }
        Ok(())
    }

    /// Restore only the requested subset of paths from a snapshot. The
    /// snapshot's view of each selected path is either applied as a working-
    /// tree deletion (for entries the snapshot recorded as deleted) or
    /// extracted via `git checkout` against a temp index (otherwise). Using
    /// `checkout` means `.gitattributes` filters — most importantly the LFS
    /// smudge filter — run, so LFS-tracked assets come out as real content
    /// rather than pointer files. The user's real index is never touched.
    async fn restore_snapshot_selective(
        &self,
        commit: &str,
        entries: &[(String, FileState)],
        currently_local_files: &[File],
        overwrite_local: bool,
    ) -> anyhow::Result<()> {
        self.wait_for_lock().await;

        // Before touching disk, refuse to clobber uncommitted local changes
        // (modified OR untracked) unless the caller explicitly opted in. The
        // frontend gates the same check in the modal; this is a backstop so
        // a stale UI or scripted caller can't silently stomp work.
        if !overwrite_local {
            let local: std::collections::HashSet<&str> = currently_local_files
                .iter()
                .map(|f| f.path.as_str())
                .collect();
            let mut conflicts: Vec<&str> = entries
                .iter()
                .filter(|(p, _)| local.contains(p.as_str()))
                .map(|(p, _)| p.as_str())
                .collect();
            if !conflicts.is_empty() {
                conflicts.sort();
                bail!(
                    "Cannot restore: {} selected file(s) would overwrite uncommitted local changes:\n  - {}\n\nCheck \"Overwrite local changes\" to proceed, or deselect the conflicting files.",
                    conflicts.len(),
                    conflicts.join("\n  - ")
                );
            }
        }

        // Partition entries into "delete from working tree" vs "extract from snapshot".
        let mut to_delete: Vec<&str> = Vec::new();
        let mut to_extract: Vec<&str> = Vec::new();
        for (path, state) in entries {
            match state {
                FileState::Deleted => to_delete.push(path.as_str()),
                _ => to_extract.push(path.as_str()),
            }
        }

        // Apply deletions directly — no git invocation needed, and `fs::
        // remove_file` doesn't care whether the file was LFS-tracked. Collect
        // per-path failures and keep going so one stuck file (e.g. a handle
        // held by another process) doesn't abort the rest of the restore;
        // we'll surface the full list at the end.
        let mut errors: Vec<String> = Vec::new();
        for path in &to_delete {
            let abs = self.repo_path.join(path);
            if abs.exists() {
                if let Err(e) = std::fs::remove_file(&abs) {
                    errors.push(format!("failed to remove {}: {}", path, e));
                }
            }
        }

        // Apply extractions via a batched `git checkout <commit>
        // --pathspec-from-file <list>` against a temp index. Running through
        // checkout (rather than dumping raw blobs ourselves) ensures the LFS
        // smudge filter runs and assets come back smudged on disk. The temp
        // index keeps the user's real index untouched.
        if !to_extract.is_empty() {
            let temp_dir = tempfile::tempdir()?;
            let temp_index_path = temp_dir.path().join("restore-index");

            // Seed the temp index from HEAD (resolved to a SHA so a
            // concurrent ref move can't desync us). `git checkout` reads the
            // index file to know what to update; a populated, valid index is
            // the safest starting point.
            let head_sha = self
                .run_and_collect_output(&["rev-parse", "HEAD"], Opts::default())
                .await?
                .trim()
                .to_string();
            self.run_with_index_file(&["read-tree", &head_sha], &temp_index_path)
                .await?;

            let pathspec_path = Self::write_pathspec_file(&to_extract)?;

            if let Err(e) = self
                .run_with_index_file(
                    &[
                        "checkout",
                        commit,
                        "--pathspec-from-file",
                        pathspec_path.to_str().expect("temp file path is non-UTF-8"),
                    ],
                    &temp_index_path,
                )
                .await
            {
                errors.push(format!(
                    "failed to extract {} file(s): {}",
                    to_extract.len(),
                    e
                ));
            }
        }

        if !errors.is_empty() {
            bail!(
                "Selective restore completed with {} error(s):\n  - {}",
                errors.len(),
                errors.join("\n  - ")
            );
        }

        Ok(())
    }

    /// Hash a file the way `git add` would — applying the same clean
    /// filters (`autocrlf`, `text`, `eol=`, custom clean drivers, LFS)
    /// that staging would apply. `attr_path` is the repo-relative path
    /// used for `.gitattributes` lookup; `file` is the actual on-disk
    /// file to hash. Used by the cherry-pick restore path so a renamed
    /// `.localcopy` and the cherry-picked extraction compare equal when
    /// they're semantically identical modulo normalization, instead of
    /// raw byte-equal.
    async fn hash_object_with_attrs(&self, attr_path: &str, file: &Path) -> anyhow::Result<String> {
        let file_str = file
            .to_str()
            .ok_or_else(|| anyhow!("hash-object path is non-UTF-8"))?;
        let path_arg = format!("--path={attr_path}");
        let out = self
            .run_and_collect_output(
                &["hash-object", &path_arg, "--", file_str],
                Opts::new_without_logs(),
            )
            .await?;
        Ok(out.trim().to_string())
    }

    /// Decide whether a file restored from a snapshot matches the user's
    /// pre-restore `.localcopy` of the same path. Byte-equal files are
    /// identical under any clean filter, and after an uneventful restore the
    /// two are almost always byte-equal (LFS smudge reproduces the exact
    /// bytes the snapshot captured) — so a raw comparison short-circuits the
    /// common case without running the LFS clean filter, a full content
    /// hash, over each side. Only on a byte mismatch do we fall back to
    /// filter-aware hashing via `hash_object_with_attrs`, which keeps EOL
    /// normalization (autocrlf, eol=, custom drivers) from registering as a
    /// conflict. Any error on either side counts as "different" so the local
    /// copy is preserved for review rather than silently dropped.
    async fn restored_file_matches_local(
        &self,
        attr_path: &str,
        local: &Path,
        restored: &Path,
    ) -> bool {
        if let Ok(true) = Self::bytes_equal(local.to_path_buf(), restored.to_path_buf()).await {
            return true;
        }

        let (local_hash, restored_hash) = tokio::join!(
            self.hash_object_with_attrs(attr_path, local),
            self.hash_object_with_attrs(attr_path, restored)
        );
        match (local_hash, restored_hash) {
            (Ok(local), Ok(restored)) => local == restored,
            _ => false,
        }
    }

    /// Raw byte comparison of two files, run on a blocking thread since the
    /// assets involved can be hundreds of megabytes. Bails out on the first
    /// differing chunk (or immediately on a size mismatch).
    async fn bytes_equal(a: PathBuf, b: PathBuf) -> anyhow::Result<bool> {
        tokio::task::spawn_blocking(move || {
            use std::io::Read;

            let len = std::fs::metadata(&a)?.len();
            if len != std::fs::metadata(&b)?.len() {
                return Ok(false);
            }

            // 1MB chunks: assets run to hundreds of MB, so smaller chunks
            // turn a single compare into thousands of read syscalls per side.
            const CHUNK: usize = 1024 * 1024;
            let mut file_a = std::fs::File::open(&a)?;
            let mut file_b = std::fs::File::open(&b)?;
            let mut buf_a = vec![0u8; CHUNK];
            let mut buf_b = vec![0u8; CHUNK];
            let mut remaining = len;
            while remaining > 0 {
                let n = remaining.min(CHUNK as u64) as usize;
                file_a.read_exact(&mut buf_a[..n])?;
                file_b.read_exact(&mut buf_b[..n])?;
                if buf_a[..n] != buf_b[..n] {
                    return Ok(false);
                }
                remaining -= n as u64;
            }
            Ok(true)
        })
        .await?
    }

    pub async fn get_untracked_files(&self) -> anyhow::Result<Vec<String>> {
        let output = self.status(vec![]).await?;

        let untracked_files: Vec<String> = output
            .lines()
            .filter_map(|line| {
                // Parse porcelain format: first two chars are status, rest is
                // filename. Porcelain wraps paths containing spaces in double
                // quotes (even under core.quotepath=false), so strip them the
                // same way File::from_status_line does — a quoted path would
                // fail every exists()/rename() it feeds, which left
                // space-named untracked files in place during snapshot restore
                // and aborted the cherry-pick with "untracked working tree
                // files would be overwritten".
                if line.len() > 2 && line.starts_with("??") {
                    Some(line[3..].trim_matches('"').to_string())
                } else {
                    None
                }
            })
            .collect();

        Ok(untracked_files)
    }

    pub async fn get_files_in_snapshot(
        &self,
        snapshot_commit: &str,
    ) -> anyhow::Result<Vec<String>> {
        // Snapshots are stash commits, so we need to diff against the first parent
        // to see what files were changed when the snapshot was created
        let output = self
            .run_and_collect_output(
                &[
                    "diff",
                    "--name-only",
                    &format!("{}^1", snapshot_commit),
                    snapshot_commit,
                ],
                Opts::new_without_logs(),
            )
            .await?;

        let files: Vec<String> = output
            .lines()
            .filter(|line| !line.is_empty())
            .map(|line| line.to_string())
            .collect();

        Ok(files)
    }

    /// Like `get_files_in_snapshot`, but returns each path's diff state vs the
    /// snapshot's first parent (HEAD at snapshot time). Used by the
    /// restore-preview UI so we can show whether a file would be added,
    /// overwritten, or deleted on restore.
    pub async fn get_snapshot_entries_with_state(
        &self,
        snapshot_commit: &str,
    ) -> anyhow::Result<Vec<(String, FileState)>> {
        let output = self
            .run_and_collect_output(
                &[
                    "diff",
                    "--name-status",
                    "-z",
                    &format!("{}^1", snapshot_commit),
                    snapshot_commit,
                ],
                Opts::new_without_logs(),
            )
            .await?;

        // `-z` emits fields NUL-terminated, one field per record:
        //   STATUS\0PATH\0                         for A/M/D/T/U
        //   STATUS\0OLDPATH\0NEWPATH\0             for R/C
        // (Verified via `git diff --name-status -z … | od -c`.) Status and
        // path are SEPARATE tokens; there's no tab between them.
        let mut tokens = output.split('\0').filter(|s| !s.is_empty());
        let mut entries = Vec::new();
        while let Some(status) = tokens.next() {
            let state = match status.chars().next() {
                Some('A') => FileState::Added,
                Some('D') => FileState::Deleted,
                Some('U') => FileState::Unmerged,
                Some('M') | Some('T') | Some('R') | Some('C') => FileState::Modified,
                _ => FileState::Unknown,
            };
            let path = match status.chars().next() {
                // R/C: consume OLDPATH, use NEWPATH.
                Some('R') | Some('C') => {
                    let _old = tokens.next();
                    match tokens.next() {
                        Some(p) => p.to_string(),
                        None => continue,
                    }
                }
                _ => match tokens.next() {
                    Some(p) => p.to_string(),
                    None => continue,
                },
            };
            entries.push((path, state));
        }
        Ok(entries)
    }

    pub fn find_tracked_conflicts(
        snapshot_files: &[String],
        untracked_files: &[String],
    ) -> Vec<String> {
        let mut conflicts = Vec::new();

        for untracked_file in untracked_files {
            if snapshot_files.contains(untracked_file) {
                conflicts.push(untracked_file.clone());
            }
        }

        conflicts
    }

    /// Compute the files that would be incoming in a pull from `origin/<branch>`.
    ///
    /// Callers are responsible for ensuring remote refs are fresh (e.g. by
    /// running `fetch` beforehand). This function deliberately does not fetch
    /// so that callers who have already fetched don't pay for a redundant
    /// network round-trip.
    pub async fn get_incoming_files(&self, branch: &str) -> anyhow::Result<Vec<String>> {
        let remote_branch = format!("origin/{}", branch);
        let range = format!("HEAD..{}", remote_branch);

        let files = self.diff_filenames(&range).await?;
        Ok(files)
    }

    /// Check for conflicts between the caller's known untracked files and
    /// files that would come in from a pull of `branch`.
    ///
    /// The caller is expected to pass the already-known untracked set — for
    /// example, from a `StatusOp` that just ran — so we don't spawn another
    /// full `git status` here. On large worktrees this is the difference
    /// between "free" and a multi-second scan.
    pub async fn check_sync_vs_untracked_file_conflicts(
        &self,
        branch: &str,
        untracked_files: &[String],
    ) -> anyhow::Result<Vec<String>> {
        // If the caller has no untracked files, there is nothing a tracked
        // incoming file can collide with. Skip the network round-trip.
        if untracked_files.is_empty() {
            return Ok(vec![]);
        }

        // If the remote branch doesn't exist, there are no incoming files to conflict with.
        // This is expected for local-only dev branches (e.g. coho/feature-name).
        if !self.has_remote_branch(branch).await? {
            return Ok(vec![]);
        }

        // Get files that would be incoming in a pull/sync
        let incoming_files = self.get_incoming_files(branch).await?;

        // Find conflicts between untracked local files and incoming tracked files
        let conflicts = Self::find_tracked_conflicts(&incoming_files, untracked_files);

        Ok(conflicts)
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

    pub async fn has_local_branch(&self, branch: &str) -> anyhow::Result<bool> {
        let output = self
            .run_and_collect_output(&["branch", "--list", branch], Opts::default())
            .await?;

        Ok(!output.trim().is_empty())
    }

    pub async fn has_remote_branch(&self, branch: &str) -> anyhow::Result<bool> {
        let output = self
            .run_and_collect_output(&["ls-remote", "--heads", "origin", branch], Opts::default())
            .await?;

        Ok(!output.is_empty())
    }

    /// Check whether `refs/remotes/origin/<branch>` exists locally.
    ///
    /// This is purely local — no network round-trip. Use this when you have
    /// just fetched and want to know whether the remote advertised the branch,
    /// as a cheap replacement for `has_remote_branch`. Prefer `has_remote_branch`
    /// when you need source-of-truth from the remote right now (e.g. deciding
    /// whether to `push -d` an old branch).
    pub async fn has_local_remote_tracking_branch(&self, branch: &str) -> anyhow::Result<bool> {
        let refname = format!("refs/remotes/origin/{branch}");
        let output = self
            .run_and_collect_output(
                &["for-each-ref", "--format=%(refname)", &refname],
                Opts::new_without_logs(),
            )
            .await?;

        Ok(!output.trim().is_empty())
    }

    pub async fn verify_locks(&self) -> anyhow::Result<VerifyLocksResponse> {
        // Runs `git lfs locks`, which contacts the LFS server and so can invoke
        // the credential helper. This is only ever called from the ambient
        // status refresh (see StatusOp), never from a deliberate user sync, so
        // it must not launch an interactive credential prompt: a stale
        // credential should fail quietly here (the caller degrades to the last
        // known lock state) and re-auth happens on the next user-initiated
        // pull/push. Without this, a background status refresh pops a GCM
        // login window every few seconds once the stored credential expires.
        let output = match self
            .run_and_collect_output(
                &["lfs", "locks", "--verify", "--json"],
                Opts::new_without_logs()
                    .with_complete_error()
                    .with_skip_interactive_auth(),
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
                    Opts::new_without_logs().with_skip_interactive_auth(),
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
                &format!("-{limit}"),
                "--pretty=format:%H%x1f%B%x1f%an%x1f%aI%x1f%x1e",
                git_ref,
            ],
            Opts::new_without_logs(),
        )
        .await
    }

    pub async fn cherry(&self, upstream: &str, head: &str) -> anyhow::Result<String> {
        self.run_and_collect_output(
            &["--no-pager", "cherry", upstream, head],
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

    // this looks at two refs and identifies commits that are likely the
    // same commit, but have different shas due to a rebase or cherry-pick
    // the file paths that are shared are returned
    pub async fn get_shared_changed_files(
        &self,
        from_ref: &str,
        to_ref: &str,
    ) -> anyhow::Result<Vec<String>> {
        let range = format!("{from_ref}...origin/{to_ref}");
        let output = self
            .run_and_collect_output(&["log", "--pretty=format:%H|%s", &range], Opts::default())
            .await?;

        // commit messages that are both local and upstream will appear twice
        // we can confirm they have the same changed files
        // if they have the same message and modified files, add those files to the list
        let mut shared_files = vec![];
        let mut commit_map: HashMap<String, String> = HashMap::new();
        for line in output.lines() {
            let parts = line.split('|').collect::<Vec<_>>();
            if parts.len() < 2 {
                continue;
            }
            let commit = parts[0].to_string();
            let message = parts[1].to_string();

            if commit_map.contains_key(&message) {
                // get the files for the stored commit and the current commit and compare
                let stored_commit = commit_map.get(&message).unwrap();

                let output = self
                    .run_and_collect_output(
                        &["show", "--name-only", "--oneline", stored_commit],
                        Opts::default(),
                    )
                    .await?;

                // drop the first line, which is the commit message
                let changed_files = output.lines().skip(1).collect::<Vec<_>>();

                let output = self
                    .run_and_collect_output(
                        &["show", "--name-only", "--oneline", &commit],
                        Opts::default(),
                    )
                    .await?;

                // drop the first line, which is the commit message
                let changed_files2 = output.lines().skip(1).collect::<Vec<_>>();

                // if the commits share files, add them to the list
                for file in changed_files {
                    if changed_files2.contains(&file) && !shared_files.contains(&file.to_string()) {
                        shared_files.push(file.to_string());
                    }
                }

                continue;
            }

            commit_map.insert(message.clone(), commit.clone());
        }

        Ok(shared_files)
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
                    &format!("{from_ref}...{to_ref}"),
                ],
                Opts::default(),
            )
            .await?;
        let behind_count = output.lines().next().unwrap_or("0").parse::<u32>()?;

        let output = self
            .run_and_collect_output(
                &[
                    "rev-list",
                    "--count",
                    "--right-only",
                    &format!("{from_ref}...{to_ref}"),
                ],
                Opts::default(),
            )
            .await?;
        let ahead_count = output.lines().next().unwrap_or("0").parse::<u32>()?;

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
        // Runs on the background maintenance loop and may prefetch over the
        // network, so it must not trigger an interactive credential prompt.
        self.run(
            &["maintenance", "run", "--auto"],
            Opts::default().with_skip_interactive_auth(),
        )
        .await
    }

    pub async fn run_gc(&self) -> anyhow::Result<()> {
        self.run(&["gc", "--prune=now"], Opts::default()).await
    }

    pub async fn run_gc_preserve_unreachable(&self) -> anyhow::Result<()> {
        self.run(&["gc"], Opts::default()).await
    }

    pub async fn count_objects(&self) -> anyhow::Result<String> {
        self.run_and_collect_output(&["count-objects", "-v"], Opts::new_without_logs())
            .await
    }

    /// Expire old reflog entries so unreachable objects can eventually be pruned.
    ///
    /// The expiry window is supplied through `-c gc.reflogExpire*` config rather
    /// than the `--expire`/`--expire-unreachable` command-line flags, and this
    /// distinction is the whole point. When *both* windows are given as CLI
    /// options, git skips its per-ref config lookup entirely — and that lookup is
    /// what protects `refs/stash`. The stash stack lives in refs/stash's reflog and
    /// every entry below the tip is unreachable (stash commits aren't ancestors of
    /// each other and sit on no branch), so a CLI-driven window sweeps away the
    /// user's stashes — including manual, non-snapshot ones — on every boot.
    /// Driving the window through config keeps git's per-ref path active (git's
    /// built-in default already protects refs/stash), and we additionally pin
    /// `gc."refs/stash".reflog*Expire=never` so stashes are safe regardless of the
    /// user's own config.
    ///
    /// We let `--all` enumerate the refs that actually have a reflog instead of
    /// listing refs ourselves: `git reflog expire <ref>` errors out
    /// (`<ref> points nowhere!`) on any ref with no reflog — tags, freshly-cloned
    /// remote-tracking refs, etc. — and a single bad ref would fail the whole run.
    /// `--all` only ever touches refs that have a reflog, so it sidesteps that
    /// entirely (and needs no command-line-length batching).
    pub async fn expire_reflog(&self) -> anyhow::Result<()> {
        self.run(
            &[
                "-c",
                "gc.reflogExpire=30.days",
                "-c",
                "gc.reflogExpireUnreachable=30.days",
                "-c",
                "gc.refs/stash.reflogExpire=never",
                "-c",
                "gc.refs/stash.reflogExpireUnreachable=never",
                "reflog",
                "expire",
                "--all",
            ],
            Opts::default(),
        )
        .await
    }

    pub async fn refetch(&self) -> anyhow::Result<()> {
        let mut fetch_running = GIT_FETCH_LOCK.clone().lock_owned().await;
        *fetch_running = true;
        self.run(&["fetch", "--refetch"], Opts::default()).await?;
        *fetch_running = false;
        Ok(())
    }

    pub async fn rewrite_graph(&self) -> anyhow::Result<()> {
        let result = self
            .run(
                &["commit-graph", "write", "--reachable"],
                Opts::default().with_complete_error(),
            )
            .await;

        if let Err(e) = &result {
            if e.to_string().contains("commit-graph.lock") {
                warn!("Removing commit-graph.lock");
                let graph_lock =
                    Path::new(&self.repo_path).join(".git/objects/info/commit-graph.lock");
                if let Err(e) = std::fs::remove_file(graph_lock) {
                    bail!("Failed to remove commit-graph.lock: {}", e);
                }

                return self
                    .run(&["commit-graph", "write", "--reachable"], Opts::default())
                    .await;
            }
        }

        Ok(())
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
                    skip_interactive_auth: false,
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

    /// Build a raw `git <args…>` command wired with this client's repo cwd and
    /// the standard env/creation flags, bypassing [`Self::run`]. For use by
    /// code that already holds the git process lock (`run` would re-acquire it
    /// and deadlock); the caller spawns the child and must hold the lock for
    /// its lifetime.
    fn build_raw_command(&self, args: &[&str]) -> anyhow::Result<Command> {
        let mut cmd = Command::new("git");
        cmd.args(args);
        cmd.env("GIT_CLONE_PROTECTION_ACTIVE", "false");
        if !self.repo_path.as_os_str().is_empty() {
            cmd.current_dir(self.repo_path.canonicalize()?);
        }
        #[cfg(windows)]
        cmd.creation_flags(CREATE_NO_WINDOW);
        Ok(cmd)
    }

    /// Build a `git credential <action>` command. Shared by the check, reject,
    /// and approve steps of [`Self::store_credential`].
    fn build_credential_command(&self, action: &str) -> anyhow::Result<Command> {
        self.build_raw_command(&["credential", action])
    }

    /// Read a single value from the repo's **local** git config; `None` when
    /// the key is unset (or unreadable). Caller must hold the git process lock.
    async fn local_config_get(&self, key: &str) -> Option<String> {
        let out = self
            .build_raw_command(&["config", "--local", "--get", key])
            .ok()?
            .output()
            .await
            .ok()?;
        if !out.status.success() {
            return None;
        }
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    }

    /// Write a single value into the repo's local git config. Caller must hold
    /// the git process lock.
    async fn local_config_set(&self, key: &str, value: &str) -> anyhow::Result<()> {
        let out = self
            .build_raw_command(&["config", "--local", key, value])?
            .output()
            .await?;
        if !out.status.success() {
            bail!(
                "git config {key} failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(())
    }

    /// Remove a key from the repo's local git config; succeeds when the key is
    /// already absent. Caller must hold the git process lock.
    async fn local_config_unset(&self, key: &str) -> anyhow::Result<()> {
        let out = self
            .build_raw_command(&["config", "--local", "--unset", key])?
            .output()
            .await?;
        // Exit code 5 = "key was not set" — already the state we want.
        if !out.status.success() && out.status.code() != Some(5) {
            bail!(
                "git config --unset {key} failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(())
    }

    /// `credential.https://<host>.username` — the standard git config key that
    /// makes git attach a username to every credential request for that URL.
    fn credential_pin_key(host: &str) -> String {
        format!("credential.https://{host}.username")
    }

    /// Local-config key recording the username we last seeded for `host`.
    /// Presence marks the pin as ours (vs. one the user set themselves); the
    /// value lets a username change evict the previously seeded entry.
    fn seeded_username_key(host: &str) -> String {
        format!("friendshipper.https://{host}.seededusername")
    }

    /// Local-config key preserving a user-set pin value we displaced, so
    /// [`Self::remove_credential_pin`] can restore it.
    fn original_username_key(host: &str) -> String {
        format!("friendshipper.https://{host}.originalusername")
    }

    /// Refresh git's configured credential helper with an HTTPS credential for
    /// `host`, so subsequent network git operations authenticate
    /// non-interactively instead of the helper launching a login window.
    ///
    /// First checks, non-interactively, whether the helper already serves this
    /// exact username+password. If it does, the store is left untouched and we
    /// only make sure the username pin (below) is in place. Otherwise we evict
    /// the entries we manage (`git credential reject` — the current username,
    /// plus the previously seeded username when it changed, e.g. the
    /// `x-access-token` fallback from an offline launch after the real login
    /// becomes known; never any other account the user has stored), then seed
    /// the new credential (`git credential approve`). The evict is required
    /// because a stale entry (e.g. an expired token) otherwise shadows what we
    /// store, so a `git fetch` keeps failing even though the seed appears to
    /// land.
    ///
    /// Only after a successful approve do we pin
    /// `credential.https://<host>.username` in the repo's local config. Git
    /// injects that username into every credential request made from this repo
    /// (background fetch/pull/push), so those requests target exactly the
    /// entry we seed rather than whatever same-host entry the helper answers a
    /// host-only lookup with — the wrong account, or a GCM account picker,
    /// when the user also stores a personal login for the same host. Pinning
    /// only after approve — and rolling the pin back if approve fails — means
    /// the pin never points at a username with no stored credential. A
    /// pre-existing pin the user set themselves is remembered and restored by
    /// [`Self::remove_credential_pin`] when seeding is turned off.
    ///
    /// The credential is fed over **stdin** — never on the command line — so the
    /// secret never lands in process arguments, our git-output channel, or the
    /// logs. Whatever helper the user has configured (e.g. Git Credential
    /// Manager) performs the actual storage.
    ///
    /// This is how a non-expiring PAT held in the app keyring becomes *git's*
    /// credential (the in-app PAT otherwise only feeds the GitHub HTTP API,
    /// never git). `username` should be the GitHub login when known; for a
    /// classic PAT the value is ignored by GitHub, so an empty string falls
    /// back to the conventional `x-access-token` rather than leaving git with
    /// an incomplete credential it would prompt to fill.
    ///
    /// Best-effort: if no credential helper is configured, the underlying git
    /// commands are no-ops and this returns `Ok`.
    pub async fn store_credential(
        &self,
        host: &str,
        username: &str,
        password: &str,
    ) -> anyhow::Result<()> {
        let username = if username.is_empty() {
            "x-access-token"
        } else {
            username
        };

        // Hold the process lock across the check and any replacement, like
        // every other spawn site.
        let _git_lock = acquire_git_process_lock("git credential (check + refresh)").await;

        // Fast path: the helper already serves exactly this credential for
        // this username. Leave the store alone — just make sure the pin is in
        // place (it can be missing after an upgrade from a build that seeded
        // without pinning).
        if self
            .current_credential_password(host, username)
            .await
            .as_deref()
            == Some(password)
        {
            debug!("git credential for {host} already current; leaving it untouched");
            if let Err(e) = self.ensure_credential_username_pin(host, username).await {
                warn!("Failed to pin credential username for {host}: {e}");
            }
            return Ok(());
        }

        // Narrate the action + effect so the logs explain why a stored
        // credential changed. Host + username only — never the password.
        info!(
            "Refreshing git credential for {host} from the saved PAT (username: {username}): \
             evicting our stale entry, then re-seeding the credential helper so background \
             fetch/pull/push/lfs authenticate without an interactive login."
        );

        // Step 1 — evict the entries we manage. Target specific usernames so
        // we never clear another account the user has stored: the current
        // username, plus the one we seeded previously (recorded in local
        // config) when it differs — otherwise a username flip (x-access-token
        // fallback ↔ real login) strands the old PAT-bearing entry in the
        // store forever. Best-effort: `reject` is a no-op when nothing
        // matches, and a hiccup here must not block re-seeding, so its result
        // is intentionally ignored. No secret involved (protocol + host +
        // username only).
        let mut evict = vec![username.to_string()];
        if let Some(prev) = self
            .local_config_get(&Self::seeded_username_key(host))
            .await
        {
            if prev != username && !prev.is_empty() {
                evict.push(prev);
            }
        }
        for user in &evict {
            if let Ok(reject_cmd) = self.build_credential_command("reject") {
                let reject_input = format!("protocol=https\nhost={host}\nusername={user}\n\n");
                let _ =
                    crate::utils::process::run_with_stdin(reject_cmd, reject_input.into_bytes())
                        .await;
            }
        }

        // Step 2 — seed. git's credential protocol: key=value lines terminated
        // by a blank line. The password line carries the secret; `input` is
        // sensitive (fed only via stdin, never logged). `run_with_stdin` does
        // not log stdin.
        let input =
            format!("protocol=https\nhost={host}\nusername={username}\npassword={password}\n\n");
        let approve_cmd = self.build_credential_command("approve")?;
        let approve_result =
            match crate::utils::process::run_with_stdin(approve_cmd, input.into_bytes()).await {
                Ok(out) if out.status.success() => Ok(()),
                Ok(out) => Err(anyhow::anyhow!(
                    "git credential approve failed: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                )),
                Err(e) => Err(e),
            };
        if let Err(e) = approve_result {
            // The eviction above may have removed the only entry the pin
            // points at; roll the pin back so git isn't left steering every
            // request at a username with no stored credential.
            if let Err(unpin_err) = self.remove_credential_pin_locked(host).await {
                warn!("Failed to roll back credential pin for {host}: {unpin_err}");
            }
            return Err(e);
        }

        // Step 3 — pin the username, now that a matching credential is known
        // to be stored. Best-effort: with the pin missing, behavior degrades
        // to the pre-pin host-only lookup, which still works for the common
        // single-account store.
        if let Err(e) = self.ensure_credential_username_pin(host, username).await {
            warn!("Failed to pin credential username for {host}: {e}");
        }

        info!("Stored git credential for {host} refreshed (username: {username})");
        Ok(())
    }

    /// Pin `credential.https://<host>.username` in the repo's local config so
    /// git attaches `username` to every credential request it makes from this
    /// repo (fetch/pull/push and `git credential fill` alike). This is what
    /// disambiguates our seeded entry from any other account the user has
    /// stored for the same host. Standard git config — honored by every
    /// credential helper, not just GCM.
    ///
    /// Alongside the pin, the seeded username is recorded under a
    /// `friendshipper.*` key so later calls can tell our pin from one the user
    /// set themselves: a
    /// foreign value is preserved (once) under a second key and restored by
    /// [`Self::remove_credential_pin`], and a username change lets
    /// [`Self::store_credential`] evict the previously seeded entry.
    ///
    /// Read-compares before writing so repeat calls don't rewrite .git/config.
    /// No-op when this client has no repo path (nowhere to pin). Caller must
    /// hold the git process lock; runs raw commands rather than [`Self::run`]
    /// because `run` would re-acquire that lock and deadlock.
    async fn ensure_credential_username_pin(
        &self,
        host: &str,
        username: &str,
    ) -> anyhow::Result<()> {
        if self.repo_path.as_os_str().is_empty() {
            return Ok(());
        }
        let pin_key = Self::credential_pin_key(host);
        let marker_key = Self::seeded_username_key(host);

        let current_pin = self.local_config_get(&pin_key).await;
        let marker = self.local_config_get(&marker_key).await;

        if current_pin.as_deref() == Some(username) {
            // Pin already correct; just make sure ownership is recorded.
            if marker.as_deref() != Some(username) {
                self.local_config_set(&marker_key, username).await?;
            }
            return Ok(());
        }

        if let Some(existing) = current_pin {
            // A pin that isn't the one we last wrote is the user's own config
            // (e.g. their multi-account disambiguation). Remember it — once —
            // so remove_credential_pin can put it back.
            if marker.as_deref() != Some(existing.as_str())
                && self
                    .local_config_get(&Self::original_username_key(host))
                    .await
                    .is_none()
            {
                self.local_config_set(&Self::original_username_key(host), &existing)
                    .await?;
            }
        }

        self.local_config_set(&pin_key, username).await?;
        self.local_config_set(&marker_key, username).await?;
        info!("Pinned {pin_key}={username} in local git config");
        Ok(())
    }

    /// Undo [`Self::store_credential`]'s username pin: if the current pin is
    /// the one we wrote, restore the value the user had before we first
    /// overwrote it (or drop the key when there was none), then clear our
    /// bookkeeping. A pin the user has since changed themselves is left alone.
    /// The seeded credential itself stays in the helper's store — from here on
    /// git's credential machinery is simply not touched. Safe to call when
    /// nothing was ever pinned. Used when the user turns credential seeding
    /// off.
    pub async fn remove_credential_pin(&self, host: &str) -> anyhow::Result<()> {
        if self.repo_path.as_os_str().is_empty() {
            return Ok(());
        }
        let _git_lock = acquire_git_process_lock("git config (remove credential pin)").await;
        self.remove_credential_pin_locked(host).await
    }

    /// Body of [`Self::remove_credential_pin`] for callers that already hold
    /// the git process lock (also used by [`Self::store_credential`] to roll
    /// back after a failed approve).
    async fn remove_credential_pin_locked(&self, host: &str) -> anyhow::Result<()> {
        if self.repo_path.as_os_str().is_empty() {
            return Ok(());
        }
        let marker_key = Self::seeded_username_key(host);
        let Some(marker) = self.local_config_get(&marker_key).await else {
            // We never pinned this repo — nothing to undo.
            return Ok(());
        };
        let pin_key = Self::credential_pin_key(host);
        let original_key = Self::original_username_key(host);

        if self.local_config_get(&pin_key).await.as_deref() == Some(marker.as_str()) {
            match self.local_config_get(&original_key).await {
                Some(original) => self.local_config_set(&pin_key, &original).await?,
                None => self.local_config_unset(&pin_key).await?,
            }
        }
        // A pin that no longer matches the marker was changed by the user
        // after we wrote it — theirs to keep. Either way our bookkeeping goes.
        self.local_config_unset(&marker_key).await?;
        self.local_config_unset(&original_key).await?;
        info!("Removed seeded credential username pin for {host}");
        Ok(())
    }

    /// Password the configured credential helper currently serves for
    /// `username` at `https://<host>`, or `None` if nothing is stored. The
    /// username is passed explicitly rather than relying on the config pin, so
    /// the check is meaningful even before the pin lands (the pin is written
    /// only after a successful seed). Runs non-interactively, so the check
    /// itself can never launch a credential prompt. Used by
    /// [`Self::store_credential`] to skip replacing a credential that already
    /// matches.
    async fn current_credential_password(&self, host: &str, username: &str) -> Option<String> {
        let mut cmd = self.build_credential_command("fill").ok()?;
        // Never prompt just to read the current value.
        cmd.env("GIT_TERMINAL_PROMPT", "false");
        cmd.env("GCM_INTERACTIVE", "never");
        let input = format!("protocol=https\nhost={host}\nusername={username}\n\n");
        let output = crate::utils::process::run_with_stdin(cmd, input.into_bytes())
            .await
            .ok()?;
        if !output.status.success() {
            return None;
        }
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .find_map(|l| l.strip_prefix("password=").map(str::to_string))
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

    pub async fn has_partial_clone_filter(&self) -> anyhow::Result<bool> {
        let result = self
            .run_and_collect_output(
                &["config", "--get", "remote.origin.partialclonefilter"],
                Opts::new_without_logs(),
            )
            .await;

        match result {
            Ok(filter) => {
                let has_filter = !filter.trim().is_empty();
                if has_filter {
                    info!("Found partial clone filter: {}", filter.trim());
                }
                Ok(has_filter)
            }
            Err(_) => {
                // Config key doesn't exist, so no partial clone filter
                Ok(false)
            }
        }
    }

    pub async fn remove_partial_clone_filter_and_refetch(&self) -> anyhow::Result<()> {
        info!("Removing partial clone filter and refetching repository");

        // Remove the partial clone filter
        self.run(
            &["config", "--unset", "remote.origin.partialclonefilter"],
            Opts::default(),
        )
        .await?;

        // Refetch all objects
        self.refetch().await?;

        info!("Successfully removed partial clone filter and refetched repository");
        Ok(())
    }

    pub async fn run_and_collect_output(
        &self,
        args: &[&str],
        opts: Opts<'_>,
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

    pub async fn run_and_collect_output_into_lines(
        &self,
        args: &[&str],
        opts: Opts<'_>,
    ) -> anyhow::Result<Vec<String>> {
        // assert we have at least one arg
        if args.is_empty() {
            bail!("No arguments provided to git command");
        }

        let output = self.run_and_collect_output(args, opts).await?;
        let lines = output.lines().map(|s| s.to_string()).collect::<Vec<_>>();
        Ok(lines)
    }

    pub async fn run(&self, args: &[&str], opts: Opts<'_>) -> anyhow::Result<()> {
        // assert we have at least one arg
        if args.is_empty() {
            bail!("No arguments provided to git command");
        }

        let res = self
            .run_and_collect_output_internal(args, opts, &mut None)
            .await;
        match res {
            Err(e) => {
                bail!("git {:?} failed with error: {}", args, e);
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

        // For background/non-user-initiated commands (e.g. the maintenance fetch
        // loop), never allow an interactive credential prompt. If the cached
        // credential is stale, the command fails quietly and the next
        // user-initiated operation handles re-authentication, instead of a
        // background loop spawning a GCM login window every few seconds.
        if opts.skip_interactive_auth {
            cmd.env("GIT_TERMINAL_PROMPT", "false");
            cmd.env("GCM_INTERACTIVE", "never");
        }

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

        // Serialize this subprocess against every other git process the app
        // spawns. Held until the child exits and its stdout/stderr readers are
        // joined below, guaranteeing no two app git processes overlap (which
        // is what stomps GCM credentials and collides on the index/refs).
        let _git_lock = acquire_git_process_lock(&git_cmd_str).await;

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

        // Tracks the last time the child produced any output, so the idle
        // watchdog below can tell a hung process from a slow-but-progressing
        // one. Both reader tasks bump it on every line.
        let last_activity: Arc<RwLock<std::time::Instant>> =
            Arc::new(RwLock::new(std::time::Instant::now()));

        let out_lines_thread = out_lines.clone();
        let out_activity = last_activity.clone();
        let is_collecting_out_lines = output.is_some();
        let should_log_stdout = opts.should_log_stdout;
        let out_handle = tokio::spawn(async move {
            while let Some(line) = out_reader.next_line().await.unwrap() {
                *out_activity.write() = std::time::Instant::now();

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
        let err_activity = last_activity.clone();
        let err_handle = tokio::spawn(async move {
            while let Some(line) = err_reader.next_line().await.unwrap() {
                *err_activity.write() = std::time::Instant::now();
                err_lines_thread.write().push(line.clone());
                info!("{}", line);
            }
        });

        // Idle watchdog: wake periodically and kill the child only if it has
        // produced no output for GIT_PROCESS_IDLE_TIMEOUT. A command that keeps
        // streaming progress (a large clone/fetch/LFS pull) resets the clock on
        // every line and is never killed for simply taking a long time; only a
        // truly stalled process (stuck prompt, dead connection) trips it.
        let status = loop {
            match tokio::time::timeout(GIT_PROCESS_IDLE_CHECK, git_proc.wait()).await {
                Ok(wait_result) => break wait_result?,
                Err(_elapsed) => {
                    let idle = last_activity.read().elapsed();
                    if idle >= GIT_PROCESS_IDLE_TIMEOUT {
                        warn!(
                            "git command made no progress for {:?}; killing stalled process: {}",
                            idle, git_cmd_str
                        );
                        let _ = git_proc.kill().await;
                        let _ = out_handle.await;
                        let _ = err_handle.await;
                        bail!("Git command stalled with no progress. Check the log for details.");
                    }
                    // Still producing output recently — keep waiting.
                }
            }
        };

        // The child has exited and closed its pipes, but the spawned readers may
        // not have flushed their final lines into out_lines/err_lines yet. Join
        // them before reading the collected output below — otherwise a command
        // that succeeded (e.g. `commit-tree` printing a single SHA) can
        // intermittently yield empty captured output under load, which then
        // corrupts callers like build_snapshot_commit (`commit-tree -p ""`).
        let _ = out_handle.await;
        let _ = err_handle.await;

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

    // Rewrites ~/.gitconfig; a concurrent git reading global config can hit a
    // transient access error on Windows (cf. the .git/config startup-order
    // fix). Serialize against every other git subprocess.
    let _git_lock = acquire_git_process_lock(&format!("git config --global {key}")).await;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command as StdCommand;
    use std::sync::mpsc;
    use tempfile::TempDir;

    /// Initialize a fresh git repo in a tempdir with one committed `seed.txt`.
    /// The TempDir guard must be kept alive for the duration of the test.
    fn setup_repo() -> (Git, TempDir) {
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
            let out = StdCommand::new("git")
                .args(*args)
                .current_dir(&path)
                .output()
                .expect("run git");
            assert!(
                out.status.success(),
                "git {:?} failed: {}",
                args,
                String::from_utf8_lossy(&out.stderr)
            );
        }

        std::fs::write(path.join("seed.txt"), "seed").unwrap();
        let out = StdCommand::new("git")
            .args(["add", "seed.txt"])
            .current_dir(&path)
            .output()
            .unwrap();
        assert!(out.status.success());
        let out = StdCommand::new("git")
            .args(["commit", "-m", "seed"])
            .current_dir(&path)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "seed commit failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        let (tx, _rx) = mpsc::channel();
        let git = Git::new(path, tx);
        (git, dir)
    }

    /// Point the repo's credential.helper at a throwaway `store` file so
    /// credential tests never touch the developer's real helper (e.g. a global
    /// GCM): an empty `credential.helper` value resets the inherited helper
    /// list, so only the throwaway `store` helper added afterward runs for
    /// this repo. Without the reset, git consults the global helper too and
    /// both `approve` (touching the real store!) and `fill` (shadowing our
    /// seeded value with the real one) would leak across the test boundary.
    /// Returns the path of the credential file backing the `store` helper.
    fn isolate_credential_helper(git: &Git, dir: &std::path::Path) -> std::path::PathBuf {
        let cred_file = dir.join("creds");
        let helper = format!(
            "store --file={}",
            cred_file.to_string_lossy().replace('\\', "/")
        );
        for args in [
            &["config", "--local", "credential.helper", ""][..],
            &["config", "--local", "--add", "credential.helper", &helper][..],
        ] {
            let out = StdCommand::new("git")
                .args(args)
                .current_dir(&git.repo_path)
                .output()
                .expect("set credential.helper");
            assert!(out.status.success());
        }
        cred_file
    }

    /// Read a key from the repo's local git config; `None` when unset.
    fn local_config(git: &Git, key: &str) -> Option<String> {
        let out = StdCommand::new("git")
            .args(["config", "--local", "--get", key])
            .current_dir(&git.repo_path)
            .output()
            .expect("git config --get");
        if out.status.success() {
            Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
        } else {
            None
        }
    }

    /// Set a key in the repo's local git config.
    fn set_local_config(git: &Git, key: &str, value: &str) {
        let out = StdCommand::new("git")
            .args(["config", "--local", key, value])
            .current_dir(&git.repo_path)
            .output()
            .expect("git config set");
        assert!(out.status.success(), "git config {key} failed");
    }

    /// Run `git credential <action>` in the repo with `input` on stdin and
    /// return stdout. Asserts the command succeeds.
    fn run_credential(git: &Git, action: &str, input: &[u8]) -> String {
        use std::io::Write;
        use std::process::Stdio;

        let mut child = StdCommand::new("git")
            .args(["credential", action])
            .current_dir(&git.repo_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap_or_else(|e| panic!("spawn git credential {action}: {e}"));
        child.stdin.take().unwrap().write_all(input).unwrap();
        let out = child.wait_with_output().expect("credential output");
        assert!(out.status.success(), "git credential {action} failed");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    // store_credential must (a) evict any stale host-level credential and
    // (b) make the new one retrievable via git's own credential plumbing —
    // that's the whole point of seeding the PAT so network git stops prompting.
    // We point credential.helper at a throwaway file (never the user's real
    // store), pre-seed a STALE credential, call store_credential, then
    // `git credential fill` and assert the stale value is gone and the new
    // username/password come back. Also covers the empty-username fallback to
    // `x-access-token`, so git never gets an incomplete credential it would
    // prompt to complete.
    #[tokio::test]
    async fn test_store_credential_evicts_stale_then_seeds() {
        let (git, dir) = setup_repo();
        isolate_credential_helper(&git, dir.path());

        // Pre-seed a STALE credential under the same username we manage
        // (x-access-token, the empty-username fallback) but an old password, so
        // we can prove store_credential's targeted reject evicts it before
        // writing the new one rather than leaving the stale value shadowing ours.
        run_credential(
            &git,
            "approve",
            b"protocol=https\nhost=github.com\nusername=x-access-token\npassword=ghp_STALEvalue\n\n",
        );

        // Empty username must fall back to x-access-token.
        git.store_credential("github.com", "", "ghp_seededtoken")
            .await
            .expect("store_credential");

        let stdout = run_credential(&git, "fill", b"protocol=https\nhost=github.com\n\n");
        assert!(
            stdout.contains("password=ghp_seededtoken"),
            "fill did not return seeded password: {stdout}"
        );
        assert!(
            stdout.contains("username=x-access-token"),
            "fill did not return fallback username: {stdout}"
        );
        assert!(
            !stdout.contains("ghp_STALEvalue"),
            "stale credential was not evicted before seeding: {stdout}"
        );

        // Seeding the SAME credential again hits the "already current" fast
        // path (fill matches → no reject/approve) and must leave it intact.
        git.store_credential("github.com", "", "ghp_seededtoken")
            .await
            .expect("store_credential (idempotent)");
        let stdout = run_credential(&git, "fill", b"protocol=https\nhost=github.com\n\n");
        assert!(
            stdout.contains("password=ghp_seededtoken"),
            "credential not intact after idempotent re-seed: {stdout}"
        );
    }

    // Regression for the check/set key mismatch flagged in review of the
    // credential seeding: `approve` writes a username-qualified entry, but a
    // fetch (and the pre-check inside store_credential) ask the helper with
    // host only. With a SECOND account stored for the same host, the host-only
    // answer can be the other account — the pre-check then never matches
    // (reject/approve churn on every call) and a real fetch can authenticate
    // as the wrong user. store_credential now pins
    // `credential.https://<host>.username` in the repo's local config, making
    // git attach our username to every credential request from this repo.
    #[tokio::test]
    async fn test_store_credential_two_accounts_pins_username() {
        let (git, dir) = setup_repo();
        let cred_file = isolate_credential_helper(&git, dir.path());

        // The user's own account, stored FIRST: the `store` helper answers a
        // host-only lookup with the first file match, so without the username
        // pin this entry would shadow ours.
        run_credential(
            &git,
            "approve",
            b"protocol=https\nhost=github.com\nusername=personal-user\npassword=ghp_personalsecret\n\n",
        );

        git.store_credential("github.com", "f11r-bot", "ghp_botsecret")
            .await
            .expect("store_credential");

        // The pin must land in local config.
        let out = StdCommand::new("git")
            .args([
                "config",
                "--local",
                "--get",
                "credential.https://github.com.username",
            ])
            .current_dir(&git.repo_path)
            .output()
            .expect("git config --get");
        assert!(out.status.success(), "credential username was not pinned");
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "f11r-bot");

        // A host-only fill — exactly what git-remote-https issues during a
        // fetch — must resolve to OUR account via the pinned username, even
        // though the personal account sits first in the store.
        let stdout = run_credential(&git, "fill", b"protocol=https\nhost=github.com\n\n");
        assert!(
            stdout.contains("username=f11r-bot") && stdout.contains("password=ghp_botsecret"),
            "host-only fill did not resolve to the pinned account: {stdout}"
        );
        assert!(
            !stdout.contains("ghp_personalsecret"),
            "host-only fill leaked the other account's credential: {stdout}"
        );

        // The user's own account must survive untouched — the reject inside
        // store_credential targets only the username we manage.
        let stdout = run_credential(
            &git,
            "fill",
            b"protocol=https\nhost=github.com\nusername=personal-user\n\n",
        );
        assert!(
            stdout.contains("password=ghp_personalsecret"),
            "the user's own credential was disturbed: {stdout}"
        );

        // A repeat call must hit the "already current" fast path even with the
        // second account present — this is the churn the pin exists to stop.
        // No reject/approve ran iff the backing store file is byte-identical.
        let before = std::fs::read(&cred_file).expect("read creds before");
        git.store_credential("github.com", "f11r-bot", "ghp_botsecret")
            .await
            .expect("store_credential (repeat)");
        let after = std::fs::read(&cred_file).expect("read creds after");
        assert_eq!(
            before, after,
            "repeat store_credential churned the credential store instead of no-opping"
        );
    }

    // A seeded-username flip (the x-access-token fallback from an offline
    // launch → the real login once GitHub is reachable) must not strand the
    // previously seeded PAT-bearing entry in the store: store_credential
    // records the username it seeded in local config and evicts that entry
    // when the username changes.
    #[tokio::test]
    async fn test_store_credential_username_change_evicts_old_entry() {
        let (git, dir) = setup_repo();
        let cred_file = isolate_credential_helper(&git, dir.path());

        // Offline launch: empty username falls back to x-access-token.
        git.store_credential("github.com", "", "ghp_token")
            .await
            .expect("first seed");
        let creds = std::fs::read_to_string(&cred_file).expect("read creds");
        assert!(
            creds.contains("x-access-token"),
            "fallback entry missing: {creds}"
        );

        // Same PAT, real login now known.
        git.store_credential("github.com", "real-login", "ghp_token")
            .await
            .expect("second seed");

        let creds = std::fs::read_to_string(&cred_file).expect("read creds");
        assert!(creds.contains("real-login"), "new entry missing: {creds}");
        assert!(
            !creds.contains("x-access-token"),
            "old seeded entry was stranded in the store: {creds}"
        );

        // Pin follows the new username.
        let stdout = run_credential(&git, "fill", b"protocol=https\nhost=github.com\n\n");
        assert!(
            stdout.contains("username=real-login"),
            "pin did not move to the new username: {stdout}"
        );
    }

    // Turning seeding off must hand git auth back to the user:
    // remove_credential_pin restores a pin the user had set themselves before
    // we displaced it, drops the pin entirely when there was none, leaves a
    // pin the user changed after our seed alone, and touches nothing when we
    // never pinned.
    #[tokio::test]
    async fn test_remove_credential_pin_restores_user_config() {
        const PIN: &str = "credential.https://github.com.username";
        const MARKER: &str = "friendshipper.https://github.com.seededusername";
        const ORIGINAL: &str = "friendshipper.https://github.com.originalusername";

        // Case 1: the user had their own pin — seeding displaces it (recording
        // the original), unpinning restores it.
        {
            let (git, dir) = setup_repo();
            isolate_credential_helper(&git, dir.path());
            set_local_config(&git, PIN, "personal-user");
            git.store_credential("github.com", "f11r-bot", "ghp_tok")
                .await
                .expect("seed");
            assert_eq!(local_config(&git, PIN).as_deref(), Some("f11r-bot"));
            assert_eq!(
                local_config(&git, ORIGINAL).as_deref(),
                Some("personal-user")
            );
            git.remove_credential_pin("github.com")
                .await
                .expect("unpin");
            assert_eq!(
                local_config(&git, PIN).as_deref(),
                Some("personal-user"),
                "user's own pin was not restored"
            );
            assert!(
                local_config(&git, MARKER).is_none() && local_config(&git, ORIGINAL).is_none(),
                "seeded-username bookkeeping survived the unpin"
            );
        }

        // Case 2: no pre-existing pin — unpinning drops the key entirely.
        {
            let (git, dir) = setup_repo();
            isolate_credential_helper(&git, dir.path());
            git.store_credential("github.com", "f11r-bot", "ghp_tok")
                .await
                .expect("seed");
            assert_eq!(local_config(&git, PIN).as_deref(), Some("f11r-bot"));
            git.remove_credential_pin("github.com")
                .await
                .expect("unpin");
            assert!(local_config(&git, PIN).is_none(), "our pin was not removed");
        }

        // Case 3: the user changed the pin after our seed — theirs to keep.
        {
            let (git, dir) = setup_repo();
            isolate_credential_helper(&git, dir.path());
            git.store_credential("github.com", "f11r-bot", "ghp_tok")
                .await
                .expect("seed");
            set_local_config(&git, PIN, "changed-later");
            git.remove_credential_pin("github.com")
                .await
                .expect("unpin");
            assert_eq!(
                local_config(&git, PIN).as_deref(),
                Some("changed-later"),
                "unpin clobbered a pin the user changed after our seed"
            );
            assert!(
                local_config(&git, MARKER).is_none(),
                "seeded-username bookkeeping survived the unpin"
            );
        }

        // Case 4: we never pinned — a user-set pin must be left untouched.
        {
            let (git, dir) = setup_repo();
            isolate_credential_helper(&git, dir.path());
            set_local_config(&git, PIN, "personal-user");
            git.remove_credential_pin("github.com")
                .await
                .expect("unpin (no-op)");
            assert_eq!(
                local_config(&git, PIN).as_deref(),
                Some("personal-user"),
                "unpin touched a pin we never wrote"
            );
        }
    }

    // Regression: startup maintenance expired reflogs with the expiry window
    // passed as `--expire`/`--expire-unreachable` CLI options, which makes git
    // skip its per-ref config lookup — the very mechanism that protects
    // refs/stash. Stash entries are unreachable from the ref tip, so the window
    // swept away the user's stashes (including manual, non-snapshot ones) on
    // every boot. expire_reflog must preserve stashes while still pruning
    // ordinary reflog entries — the "prunes others" half guards against a fix
    // that protects stashes by quietly turning expiry into a no-op.
    #[tokio::test]
    async fn test_expire_reflog_preserves_stashes_but_prunes_others() {
        use std::time::{SystemTime, UNIX_EPOCH};

        let (git, _dir) = setup_repo();

        // A second commit so HEAD's reflog has an ordinary entry to prune.
        std::fs::write(git.repo_path.join("seed.txt"), "second").unwrap();
        for args in [vec!["add", "seed.txt"], vec!["commit", "-m", "second"]] {
            let out = StdCommand::new("git")
                .args(&args)
                .current_dir(&git.repo_path)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {:?} failed: {}",
                args,
                String::from_utf8_lossy(&out.stderr)
            );
        }

        // A real (non-snapshot) stash from a dirty tracked file.
        std::fs::write(git.repo_path.join("seed.txt"), "dirty").unwrap();
        let out = StdCommand::new("git")
            .args(["stash", "push", "-m", "manual work"])
            .current_dir(&git.repo_path)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git stash push failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        let stash_count = |g: &Git| {
            let out = StdCommand::new("git")
                .args(["stash", "list"])
                .current_dir(&g.repo_path)
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).lines().count()
        };
        assert_eq!(stash_count(&git), 1, "expected one stash before expiry");

        // Backdate every reflog entry (HEAD + stash) well past the 30-day window.
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let backdated = now - 60 * 24 * 60 * 60;
        let backdate = |path: &std::path::Path| {
            let content = std::fs::read_to_string(path).unwrap();
            let rewritten: Vec<String> = content
                .lines()
                .map(|line| {
                    let (left, msg) = line.split_once('\t').expect("reflog line has a message");
                    let mut tail = left.rsplitn(3, ' ');
                    let tz = tail.next().unwrap();
                    let _old_ts = tail.next().unwrap();
                    let head = tail.next().unwrap();
                    format!("{head} {backdated} {tz}\t{msg}")
                })
                .collect();
            std::fs::write(path, format!("{}\n", rewritten.join("\n"))).unwrap();
        };
        let head_log = git.repo_path.join(".git/logs/HEAD");
        let stash_log = git.repo_path.join(".git/logs/refs/stash");
        backdate(&head_log);
        backdate(&stash_log);

        let entries = |p: &std::path::Path| {
            std::fs::read_to_string(p)
                .map(|c| c.lines().count())
                .unwrap_or(0)
        };
        let head_before = entries(&head_log);
        assert!(
            head_before > 0,
            "expected HEAD reflog entries before expiry"
        );

        git.expire_reflog().await.expect("expire_reflog");

        // Stash survives: the expiry window must not reach refs/stash.
        assert_eq!(
            stash_count(&git),
            1,
            "expire_reflog dropped a user stash that was older than the expiry window"
        );

        // ...but ordinary reflog entries past the window are still pruned, so the
        // expiry isn't silently a no-op (which would let the stash survive for the
        // wrong reason).
        let head_after = entries(&head_log);
        assert!(
            head_after < head_before,
            "expire_reflog did not prune backdated HEAD reflog entries (before={head_before}, after={head_after})"
        );
    }

    // Pre-PR regression: full snapshots silently dropped untracked files. The
    // snapshot is now built via `git add -A` against a temp index, which
    // stages additions/mods/dels — including untracked paths.
    #[tokio::test]
    async fn test_save_snapshot_all_captures_untracked_files() {
        let (git, _dir) = setup_repo();

        std::fs::write(git.repo_path.join("new.txt"), "hello").unwrap();

        let snapshot = git
            .save_snapshot_all("test untracked")
            .await
            .expect("save_snapshot_all");

        let files = git
            .get_files_in_snapshot(&snapshot.commit)
            .await
            .expect("get_files_in_snapshot");

        assert!(
            files.iter().any(|f| f == "new.txt"),
            "expected new.txt in snapshot, got: {:?}",
            files
        );
    }

    // Pre-PR regression: selective snapshots could capture paths the caller
    // didn't ask for. The snapshot tree is now built against a temp index
    // seeded from HEAD with only the requested paths staged, so the diff
    // against HEAD covers exactly those paths.
    #[tokio::test]
    async fn test_save_snapshot_with_paths_only_captures_those_paths() {
        let (git, _dir) = setup_repo();

        std::fs::write(git.repo_path.join("a.txt"), "a-base").unwrap();
        std::fs::write(git.repo_path.join("b.txt"), "b-base").unwrap();
        let out = StdCommand::new("git")
            .args(["add", "a.txt", "b.txt"])
            .current_dir(&git.repo_path)
            .output()
            .unwrap();
        assert!(out.status.success());
        let out = StdCommand::new("git")
            .args(["commit", "-m", "ab"])
            .current_dir(&git.repo_path)
            .output()
            .unwrap();
        assert!(out.status.success());

        // Dirty both tracked files; we only want a.txt in the snapshot.
        std::fs::write(git.repo_path.join("a.txt"), "a-dirty").unwrap();
        std::fs::write(git.repo_path.join("b.txt"), "b-dirty").unwrap();

        let snapshot = git
            .save_snapshot("only a", vec!["a.txt".to_string()])
            .await
            .expect("save_snapshot");

        let files = git
            .get_files_in_snapshot(&snapshot.commit)
            .await
            .expect("get_files_in_snapshot");

        assert!(
            files.iter().any(|f| f == "a.txt"),
            "a.txt missing from selective snapshot: {:?}",
            files
        );
        assert!(
            !files.iter().any(|f| f == "b.txt"),
            "b.txt leaked into selective snapshot: {:?}",
            files
        );
    }

    // Every `--pathspec-from-file` call site feeds through
    // `write_pathspec_file`, which prefixes `:(literal)`. This pins the
    // benign direction: the literal magic must keep matching a file whose
    // name contains glob metacharacters (brackets are legal filenames on
    // every platform we ship on), so a selective snapshot of `a[bc]d.txt`
    // still captures it — and never its glob-siblings.
    #[tokio::test]
    async fn test_save_snapshot_bracketed_filename_does_not_glob_match() {
        let (git, _dir) = setup_repo();

        std::fs::write(git.repo_path.join("a[bc]d.txt"), "bracket-base").unwrap();
        std::fs::write(git.repo_path.join("abd.txt"), "abd-base").unwrap();
        let out = StdCommand::new("git")
            .args(["--literal-pathspecs", "add", "a[bc]d.txt", "abd.txt"])
            .current_dir(&git.repo_path)
            .output()
            .unwrap();
        assert!(out.status.success());
        let out = StdCommand::new("git")
            .args(["commit", "-m", "bracket files"])
            .current_dir(&git.repo_path)
            .output()
            .unwrap();
        assert!(out.status.success());

        // Dirty both; the snapshot asks for the bracketed file only.
        std::fs::write(git.repo_path.join("a[bc]d.txt"), "bracket-dirty").unwrap();
        std::fs::write(git.repo_path.join("abd.txt"), "abd-dirty").unwrap();

        let snapshot = git
            .save_snapshot("bracket only", vec!["a[bc]d.txt".to_string()])
            .await
            .expect("save_snapshot");

        let files = git
            .get_files_in_snapshot(&snapshot.commit)
            .await
            .expect("get_files_in_snapshot");

        assert!(
            files.iter().any(|f| f == "a[bc]d.txt"),
            "a[bc]d.txt missing from selective snapshot: {:?}",
            files
        );
        assert!(
            !files.iter().any(|f| f == "abd.txt"),
            "abd.txt glob-leaked into selective snapshot of a[bc]d.txt: {:?}",
            files
        );
    }

    // `git reset --pathspec-from-file` (unlike `git add`) wildmatch-expands
    // each line against the index, so without `:(literal)` an entry of
    // `a[bc]d.txt` also unstages `abd.txt` — `[bc]` acts as a character
    // class (reproduced on git 2.47 for Windows). This pins the helper +
    // reset combination used by the batched unstage in
    // `restore_snapshot_via_cherry_pick`.
    #[tokio::test]
    async fn test_pathspec_file_reset_does_not_glob_match_siblings() {
        let (git, _dir) = setup_repo();

        std::fs::write(git.repo_path.join("a[bc]d.txt"), "bracket-base").unwrap();
        std::fs::write(git.repo_path.join("abd.txt"), "abd-base").unwrap();
        let out = StdCommand::new("git")
            .args(["--literal-pathspecs", "add", "a[bc]d.txt", "abd.txt"])
            .current_dir(&git.repo_path)
            .output()
            .unwrap();
        assert!(out.status.success());
        let out = StdCommand::new("git")
            .args(["commit", "-m", "bracket files"])
            .current_dir(&git.repo_path)
            .output()
            .unwrap();
        assert!(out.status.success());

        // Modify and stage both, then unstage only the bracketed file
        // through the helper-driven reset.
        std::fs::write(git.repo_path.join("a[bc]d.txt"), "bracket-dirty").unwrap();
        std::fs::write(git.repo_path.join("abd.txt"), "abd-dirty").unwrap();
        let out = StdCommand::new("git")
            .args(["--literal-pathspecs", "add", "a[bc]d.txt", "abd.txt"])
            .current_dir(&git.repo_path)
            .output()
            .unwrap();
        assert!(out.status.success());

        let pathspec = Git::write_pathspec_file(["a[bc]d.txt"]).expect("write_pathspec_file");
        git.run(
            &[
                "reset",
                "HEAD",
                "--pathspec-from-file",
                pathspec.to_str().expect("temp file path is non-UTF-8"),
            ],
            Opts::default(),
        )
        .await
        .expect("reset");

        let out = StdCommand::new("git")
            .args(["diff", "--cached", "--name-only"])
            .current_dir(&git.repo_path)
            .output()
            .unwrap();
        let staged = String::from_utf8_lossy(&out.stdout);
        assert!(
            !staged.contains("a[bc]d.txt"),
            "bracketed file should have been unstaged; staged set: {staged}"
        );
        assert!(
            staged.contains("abd.txt"),
            "abd.txt was glob-unstaged by the bracketed pathspec entry; staged set: {staged}"
        );
    }

    // restore_snapshot with paths_filter=None must route through the
    // selective path now (no more legacy cherry-pick body). This test
    // proves the routing works end-to-end: take a snapshot of a modified
    // file, overwrite the file locally with different content, restore
    // with None + overwrite_local=true, and assert the snapshot's content
    // is what landed on disk.
    #[tokio::test]
    async fn test_restore_snapshot_none_filter_routes_through_selective() {
        let (git, _dir) = setup_repo();

        // Commit a tracked file we can later diverge from.
        std::fs::write(git.repo_path.join("a.txt"), "base").unwrap();
        let out = StdCommand::new("git")
            .args(["add", "a.txt"])
            .current_dir(&git.repo_path)
            .output()
            .unwrap();
        assert!(out.status.success());
        let out = StdCommand::new("git")
            .args(["commit", "-m", "add a"])
            .current_dir(&git.repo_path)
            .output()
            .unwrap();
        assert!(out.status.success());

        // Take a snapshot of "a.txt = snapshot-version".
        std::fs::write(git.repo_path.join("a.txt"), "snapshot-version").unwrap();
        let snapshot = git
            .save_snapshot_all("snapshot of a")
            .await
            .expect("save_snapshot_all");

        // Move on locally to a different content; restore should overwrite.
        std::fs::write(git.repo_path.join("a.txt"), "post-snapshot").unwrap();

        git.restore_snapshot(&snapshot.commit, vec![], true, None)
            .await
            .expect("restore_snapshot");

        let on_disk = std::fs::read_to_string(git.repo_path.join("a.txt")).unwrap();
        assert_eq!(
            on_disk, "snapshot-version",
            "restore_snapshot(None) should have restored the snapshot's content"
        );
    }

    // Regression: an untracked file round-tripped through the
    // cherry-pick restore could be reported as a conflict — with the
    // snapshot version dropped at `.snapshotcopy` — even though its
    // content was identical to the user's local file modulo line
    // endings. `.gitattributes`/`autocrlf` normalize EOLs into the
    // snapshot blob, so the cherry-picked file (post-smudge) and the
    // renamed `.localcopy` (raw user bytes) differed by `\r` while git
    // considered them the same content. The fix compares both sides
    // via `git hash-object --path=<path>` so the same clean filters
    // apply.
    #[tokio::test]
    async fn test_cherry_pick_restore_ignores_eol_normalization() {
        let (git, _dir) = setup_repo();

        // Force LF in the snapshot blob regardless of platform/autocrlf.
        std::fs::write(git.repo_path.join(".gitattributes"), "*.py text eol=lf\n").unwrap();
        let out = StdCommand::new("git")
            .args(["add", ".gitattributes"])
            .current_dir(&git.repo_path)
            .output()
            .unwrap();
        assert!(out.status.success());
        let out = StdCommand::new("git")
            .args(["commit", "-m", "gitattributes"])
            .current_dir(&git.repo_path)
            .output()
            .unwrap();
        assert!(out.status.success());

        // Untracked .py with CRLF line endings on disk.
        std::fs::write(git.repo_path.join("foo.py"), "hello\r\nworld\r\n").unwrap();

        let snapshot = git
            .save_snapshot_all("eol normalization")
            .await
            .expect("save_snapshot_all");

        // Cross the filesystem mtime tick before restoring. This test is flaky
        // on Linux CI (tmpfs, ~1s mtime resolution): the snapshot stamps a temp
        // index at time T, and if the restore's working-tree write lands in the
        // same tick, git's stat-cache fast path can mis-decide whether the file
        // needs (re)writing — producing a spurious EOL "conflict". Windows
        // doesn't hit it (coarser NTFS mtime + a less aggressive fast path), and
        // a >1s gap guarantees a distinct tick so the stat compare is
        // unambiguous. See test_cherry_pick_restore_preserves_local_on_real_conflict.
        //
        // TODO(linux): this only de-flakes the *test*. The underlying
        // restore paths (`restore_snapshot_via_cherry_pick` and the temp-index
        // `git checkout` in `restore_snapshot_selective`) remain stat-cache
        // racy on Linux and could surface the same spurious conflict for real
        // users there. Friendshipper ships Windows-only today, so we defer the
        // production fix (force the worktree write via `git restore
        // --source=<commit>` / `git checkout -f`). Revisit if Linux is ever a
        // supported target.
        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;

        // With the bug, this would Err with "Snapshot restored successfully,
        // but 1 untracked file conflicts were found ... foo.py.snapshotcopy".
        // With the fix, hash-object sees the two files as the same blob.
        let result = git
            .restore_snapshot_via_cherry_pick(&snapshot.commit, vec![])
            .await;

        assert!(
            result.is_ok(),
            "cherry-pick restore reported a spurious EOL conflict: {:?}",
            result.err()
        );
        assert!(
            !git.repo_path.join("foo.py.snapshotcopy").exists(),
            ".snapshotcopy was created despite content being EOL-equivalent"
        );
    }

    // Positive companion to the EOL test: when the local and snapshot
    // contents really do differ, the cherry-pick restore must still
    // preserve the local version, write the snapshot version to
    // `.snapshotcopy`, and surface the conflict in its Err. Guards
    // against a regression where the hash-object comparison gets stuck
    // on one side and silently treats every comparison as equal.
    #[tokio::test]
    async fn test_cherry_pick_restore_preserves_local_on_real_conflict() {
        let (git, _dir) = setup_repo();

        // Commit foo.py as a tracked baseline.
        std::fs::write(git.repo_path.join("foo.py"), "print('committed')\n").unwrap();
        let out = StdCommand::new("git")
            .args(["add", "foo.py"])
            .current_dir(&git.repo_path)
            .output()
            .unwrap();
        assert!(out.status.success());
        let out = StdCommand::new("git")
            .args(["commit", "-m", "add foo"])
            .current_dir(&git.repo_path)
            .output()
            .unwrap();
        assert!(out.status.success());

        // Snapshot a modified version (content A).
        std::fs::write(git.repo_path.join("foo.py"), "print('snapshot edit')\n").unwrap();
        let snapshot = git
            .save_snapshot_all("snapshot foo")
            .await
            .expect("save_snapshot_all");

        // Diverge locally to a different content (content B). The cherry-
        // pick path will rename this aside as .localcopy, cherry-pick
        // pulls in A, and the conflict-detection loop must see A != B.
        std::fs::write(
            git.repo_path.join("foo.py"),
            "print('different local edit')\n",
        )
        .unwrap();

        let modified = vec![File {
            path: "foo.py".to_string(),
            ..Default::default()
        }];

        let result = git
            .restore_snapshot_via_cherry_pick(&snapshot.commit, modified)
            .await;

        // The function reports conflicts by returning Err with a
        // descriptive message — local files are preserved on disk
        // regardless.
        let err = result.expect_err("expected conflict Err for genuinely different files");
        let msg = err.to_string();
        assert!(
            msg.contains("foo.py.snapshotcopy"),
            "error message should name the .snapshotcopy file; got: {msg}"
        );

        // foo.py on disk should hold the local edit (content B).
        let on_disk = std::fs::read_to_string(git.repo_path.join("foo.py")).unwrap();
        assert_eq!(
            on_disk, "print('different local edit')\n",
            "local edit must be preserved at the original path"
        );

        // foo.py.snapshotcopy should hold the snapshot's version (content A).
        let snapshotcopy =
            std::fs::read_to_string(git.repo_path.join("foo.py.snapshotcopy")).unwrap();
        assert_eq!(
            snapshotcopy, "print('snapshot edit')\n",
            "snapshotcopy must hold the snapshot's content"
        );
    }

    // Untracked filenames containing spaces come back from `git status
    // --porcelain` wrapped in double quotes (even with quotepath=false).
    // `get_untracked_files` must strip them: a quoted path fails the
    // exists() check in `restore_snapshot_via_cherry_pick`, the file never
    // gets renamed aside, and the cherry-pick aborts with "untracked
    // working tree files would be overwritten" — exactly what happened on
    // a real sync with `foo - Copy.uasset` files in the worktree.
    #[tokio::test]
    async fn test_cherry_pick_restore_handles_space_named_untracked_files() {
        let (git, _dir) = setup_repo();

        std::fs::write(
            git.repo_path.join("coho-test - Copy.txt"),
            "untracked content",
        )
        .unwrap();

        let snapshot = git
            .save_snapshot_all("space test")
            .await
            .expect("save_snapshot_all");

        // The file is still on disk untracked, as during a real pull. The
        // restore must rename it aside before cherry-picking the snapshot
        // (which contains it as an add) back on top.
        let result = git
            .restore_snapshot_via_cherry_pick(&snapshot.commit, vec![])
            .await;
        assert!(
            result.is_ok(),
            "cherry-pick restore failed on space-named untracked file: {:?}",
            result.err()
        );

        let content = std::fs::read_to_string(git.repo_path.join("coho-test - Copy.txt")).unwrap();
        assert_eq!(content, "untracked content");
        assert!(
            !git.repo_path
                .join("coho-test - Copy.txt.localcopy")
                .exists(),
            "localcopy was not cleaned up after identical restore"
        );
    }
}
