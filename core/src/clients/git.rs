use std::collections::HashMap;
use std::io::Write;
use std::path::Path;
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
                let add_path = {
                    let mut add_temp = NamedTempFile::new()?;
                    for path in &existing_paths {
                        writeln!(add_temp, "{path}")?;
                    }
                    add_temp.flush()?;
                    add_temp.into_temp_path()
                };
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
                let rm_path = {
                    let mut rm_temp = NamedTempFile::new()?;
                    for path in &deleted_paths {
                        writeln!(rm_temp, "{path}")?;
                    }
                    rm_temp.flush()?;
                    rm_temp.into_temp_path()
                };
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

        let output = cmd.output().await?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("git {:?} failed: {}", args, stderr.trim());
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
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

        for modified_file in &currently_modified_files {
            let source_path = self.repo_path.join(&modified_file.path);
            if source_path.exists() {
                let localcopy_path = self
                    .repo_path
                    .join(format!("{}.localcopy", modified_file.path));
                std::fs::rename(&source_path, &localcopy_path)?;
                renamed_files.push((modified_file.path.clone(), localcopy_path));

                // Reset the file in Git to remove it from the index
                self.run(
                    &["reset", "HEAD", "--", &modified_file.path],
                    Opts::default(),
                )
                .await
                .ok();
            }
        }

        let cherry_pick_result = self
            .run(
                &["cherry-pick", "-n", "-m1", "--rerere-autoupdate", commit],
                Opts::default(),
            )
            .await;

        // reset so everything is unstaged
        let reset_path = {
            let mut temp_file = NamedTempFile::new()?;
            for path in &snapshot_files {
                writeln!(temp_file, "{path}")?;
            }
            temp_file.flush()?;
            temp_file.into_temp_path()
        };

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

        // Restore renamed files and track conflicts
        let mut conflicts = Vec::new();

        for (original_path, localcopy_path) in renamed_files {
            let target_path = self.repo_path.join(&original_path);

            if target_path.exists() {
                // Hash both files as if `git add` were staging them at
                // `original_path` so the same clean filters
                // (autocrlf, eol=, custom drivers, LFS) apply on both
                // sides. Raw byte comparison here is unsafe because the
                // cherry-pick already ran the smudge filter on its
                // output while the .localcopy is the user's untouched
                // pre-rename bytes — they can differ purely by EOL even
                // when git considers them the same content.
                let local_hash = self
                    .hash_object_with_attrs(&original_path, &localcopy_path)
                    .await;
                let restored_hash = self
                    .hash_object_with_attrs(&original_path, &target_path)
                    .await;
                let identical = match (local_hash, restored_hash) {
                    (Ok(local), Ok(restored)) => local == restored,
                    // If hash-object failed on either side we can't be
                    // sure — preserve the local copy as `.snapshotcopy`
                    // so the user can review rather than silently
                    // dropping it.
                    _ => false,
                };

                if !identical {
                    let snapshot_copy_name = format!("{}.snapshotcopy", original_path);
                    let snapshot_copy_path = self.repo_path.join(&snapshot_copy_name);
                    std::fs::rename(&target_path, &snapshot_copy_path)?;
                    std::fs::rename(&localcopy_path, &target_path)?;
                    conflicts.push((original_path.clone(), snapshot_copy_name));
                } else {
                    std::fs::remove_file(&localcopy_path)?;
                }
            } else {
                // No conflict, restore the file to its original name
                std::fs::rename(&localcopy_path, &target_path)?;
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

            let pathspec_path = {
                let mut pathspec = NamedTempFile::new()?;
                for path in &to_extract {
                    writeln!(pathspec, "{path}")?;
                }
                pathspec.flush()?;
                pathspec.into_temp_path()
            };

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

    pub async fn get_untracked_files(&self) -> anyhow::Result<Vec<String>> {
        let output = self.status(vec![]).await?;

        let untracked_files: Vec<String> = output
            .lines()
            .filter_map(|line| {
                // Parse porcelain format: first two chars are status, rest is filename
                if line.len() > 2 && line.starts_with("??") {
                    Some(line[3..].to_string())
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
        self.run(&["maintenance", "run", "--auto"], Opts::default())
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
}
