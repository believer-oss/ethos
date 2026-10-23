use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::OnceLock;

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
use crate::types::repo::BranchInfo;
use crate::types::repo::File;
use crate::types::repo::FileState;
use crate::types::repo::FileTouch;
use crate::types::repo::Snapshot;

static SNAPSHOT_PREFIX: &str = "snapshot";

lazy_static! {
    static ref WORKTREE_DIR_REGEX: Regex = Regex::new(r"^worktree (.+)").unwrap();
    static ref WORKTREE_SHA_REGEX: Regex = Regex::new(r"^HEAD (.+)").unwrap();
    static ref WORKTREE_BRANCH_REGEX: Regex = Regex::new(r"^(branch|detached)\s*(.+)?").unwrap();
    static ref GIT_VERSION_REGEX: Regex = Regex::new(r"^git version (\d+)\.(\d+)").unwrap();
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreateRemoteBranchOutcome {
    Created,
    AlreadyExists,
}

/// Oldest git with `merge-tree --write-tree`.
pub const MIN_BRANCH_GIT: (u32, u32) = (2, 38);

/// Version string of a git already found to be supported; unsupported results are never cached.
static SUPPORTED_GIT: OnceLock<String> = OnceLock::new();

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GitVersionCheck {
    pub version: String,
    pub supported: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MergeTreeOutcome {
    Clean,
    Conflicts(Vec<String>),
}

/// Parses `(major, minor)` from the first line of `git version` output.
pub fn parse_git_version(output: &str) -> Option<(u32, u32)> {
    let first_line = output.trim().lines().next()?;
    let caps = GIT_VERSION_REGEX.captures(first_line)?;
    let major = caps[1].parse().ok()?;
    let minor = caps[2].parse().ok()?;
    Some((major, minor))
}

/// `version` is the token after `git version `, or the trimmed raw output when unparseable.
fn version_check_from_output(raw: &str) -> GitVersionCheck {
    let trimmed = raw.trim();
    let Some(parsed) = parse_git_version(trimmed) else {
        return GitVersionCheck {
            version: trimmed.to_string(),
            supported: false,
        };
    };
    let version = trimmed
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("git version "))
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap_or(trimmed)
        .to_string();
    GitVersionCheck {
        version,
        supported: parsed >= MIN_BRANCH_GIT,
    }
}

fn is_tree_oid(record: &[u8]) -> bool {
    (record.len() == 40 || record.len() == 64)
        && record
            .iter()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
}

/// Interprets `git merge-tree --write-tree --name-only --no-messages -z` output. Exit 1 alone is
/// not a conflict: a bad ref also exits 1, so a conflict requires a leading tree OID and paths.
pub fn parse_merge_tree_output(
    exit_code: Option<i32>,
    stdout: &[u8],
    stderr: &str,
) -> anyhow::Result<MergeTreeOutcome> {
    let code = exit_code.map_or_else(|| "signal".to_string(), |c| c.to_string());
    let stderr = stderr.trim();

    let mut records: Vec<&[u8]> = stdout.split(|b| *b == b'\0').collect();
    if records.last().is_some_and(|r| r.is_empty()) {
        records.pop();
    }

    if !records.first().is_some_and(|r| is_tree_oid(r)) {
        bail!("git merge-tree did not return a tree OID (exit {code}): {stderr}");
    }

    match exit_code {
        Some(0) => Ok(MergeTreeOutcome::Clean),
        Some(1) => {
            let paths: Vec<String> = records[1..]
                .iter()
                .take_while(|r| !r.is_empty())
                .map(|r| String::from_utf8_lossy(r).into_owned())
                .collect();
            if paths.is_empty() {
                bail!("git merge-tree reported conflicts but listed no paths");
            }
            Ok(MergeTreeOutcome::Conflicts(paths))
        }
        _ => bail!("git merge-tree failed (exit {code}): {stderr}"),
    }
}

/// Parses `log --name-only --format=%x1e%H%x1f%an%x1f%aI` output into each path's newest touch.
pub fn parse_last_touch(output: &str) -> HashMap<String, FileTouch> {
    let mut touches: HashMap<String, FileTouch> = HashMap::new();
    for record in output.split('\x1e') {
        if record.trim().is_empty() {
            continue;
        }
        let mut lines = record.lines();
        let Some(header) = lines.next() else {
            continue;
        };
        let fields: Vec<&str> = header.split('\x1f').collect();
        if fields.len() < 3 {
            continue;
        }
        let sha: String = fields[0].chars().take(8).collect();
        for path in lines.filter(|l| !l.is_empty()) {
            touches
                .entry(path.to_string())
                .or_insert_with(|| FileTouch {
                    sha: sha.clone(),
                    author: fields[1].to_string(),
                    date: fields[2].to_string(),
                });
        }
    }
    touches
}

/// Files that git's autostash apply left conflicted after a pull, and how each was settled.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AutostashConflict {
    /// Text files left with git's conflict markers.
    pub marker_files: Vec<String>,
    /// Files whose local version replaced the pulled one.
    pub kept_local_files: Vec<String>,
    /// Files deleted locally whose pulled version was removed again.
    pub kept_deleted_files: Vec<String>,
    /// Files in a state the settling did not recognize, left as git wrote them.
    pub unresolved_files: Vec<String>,
    /// A pre-pull snapshot holds the local versions.
    pub snapshot_saved: bool,
    /// Git's `autostash` stash entry was dropped because the snapshot covers it.
    pub autostash_dropped: bool,
}

impl AutostashConflict {
    /// What the pull left to the user, starting lower-case after "Synced, but".
    pub fn details(&self) -> String {
        let mut out = "some files changed both locally and on origin.".to_string();
        if !self.marker_files.is_empty() {
            out.push_str(&format!(
                " Resolve the conflict markers in {} before submitting.",
                self.marker_files.join(", ")
            ));
        }
        let kept: Vec<String> = self
            .kept_local_files
            .iter()
            .cloned()
            .chain(
                self.kept_deleted_files
                    .iter()
                    .map(|p| format!("{p} (deleted)")),
            )
            .collect();
        if !kept.is_empty() {
            out.push_str(&format!(
                " Kept your local version of {}; submitting will overwrite origin's changes to them.",
                kept.join(", ")
            ));
        }
        if !self.unresolved_files.is_empty() {
            out.push_str(&format!(
                " Could not merge {}; check them before submitting.",
                self.unresolved_files.join(", ")
            ));
        }
        let saved_in = match (self.snapshot_saved, self.autostash_dropped) {
            (true, true) => "the 'pre-pull' snapshot",
            (true, false) => "the 'pre-pull' snapshot and in git's 'autostash' stash entry",
            (false, _) => "git's 'autostash' stash entry",
        };
        out.push_str(&format!(" Your local versions are saved in {saved_in}."));
        out
    }
}

impl std::fmt::Display for AutostashConflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Synced, but {}", self.details())
    }
}

impl std::error::Error for AutostashConflict {}

/// An unmerged index path with the blob id at stages 1-3 (base, ours, theirs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnmergedEntry {
    pub path: String,
    pub stages: [Option<String>; 3],
}

/// Parses `git ls-files -u -z` output into one entry per path, in output order.
fn parse_unmerged_entries(output: &str) -> Vec<UnmergedEntry> {
    let mut entries: Vec<UnmergedEntry> = Vec::new();
    for record in output.split('\0') {
        let record = record.trim_start_matches('\n');
        let Some((meta, path)) = record.split_once('\t') else {
            continue;
        };
        let mut fields = meta.split_whitespace();
        let (Some(_mode), Some(blob), Some(stage)) = (fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let Some(slot) = stage.parse::<usize>().ok().and_then(|s| s.checked_sub(1)) else {
            continue;
        };
        if slot > 2 {
            continue;
        }
        if entries.last().is_none_or(|e| e.path != path) {
            entries.push(UnmergedEntry {
                path: path.to_string(),
                stages: Default::default(),
            });
        }
        if let Some(entry) = entries.last_mut() {
            entry.stages[slot] = Some(blob.to_string());
        }
    }
    entries
}

enum UnmergedOutcome {
    Markers,
    KeptLocal,
    KeptDeletion,
    Unresolved,
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

/// Parses `for-each-ref` output written with `%1f` field and `%1e` record separators.
pub(crate) fn parse_branch_records(output: &str) -> Vec<BranchInfo> {
    output
        .split('\x1e')
        .map(|record| record.trim_start_matches('\n'))
        .filter(|record| !record.is_empty())
        .filter_map(|record| {
            let fields: Vec<&str> = record.split('\x1f').collect();
            let [name, sha, short_sha, commit_date, author, subject, contents] = fields[..] else {
                return None;
            };
            if name == "HEAD" {
                return None;
            }
            Some(BranchInfo {
                name: name.to_string(),
                sha: sha.to_string(),
                short_sha: short_sha.to_string(),
                commit_date: commit_date.to_string(),
                author: author.to_string(),
                subject: subject.to_string(),
                message: contents.trim_end().to_string(),
                is_feature_branch: false,
                is_protected: false,
                is_current: false,
            })
        })
        .collect()
}

/// Whether `git push --porcelain` output reports `refs/heads/<name>` as a newly created ref.
pub(crate) fn push_created_new_ref(porcelain_stdout: &str, name: &str) -> bool {
    let target = format!(":refs/heads/{name}\t");
    porcelain_stdout
        .lines()
        .any(|line| line.starts_with("*\t") && line.contains(&target))
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
                "{} local file conflicts were found during snapshot restore:\n{}\n\nYour local files have been preserved. The snapshot versions have been saved with '.snapshotcopy' extensions. Please review and resolve these conflicts manually.",
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

    /// The commit at the top of `refs/stash`, if any.
    pub async fn stash_top(&self) -> anyhow::Result<Option<String>> {
        let output = self
            .run_and_collect_output(
                &["stash", "list", "-n1", "--format=%H"],
                Opts::new_without_logs(),
            )
            .await?;
        Ok(output
            .lines()
            .map(str::trim)
            .find(|s| !s.is_empty())
            .map(str::to_string))
    }

    /// Runs a `-z` name-only command and returns its paths.
    async fn collect_nul_paths(&self, args: &[&str]) -> anyhow::Result<Vec<String>> {
        let output = self
            .run_and_collect_output(args, Opts::new_without_logs())
            .await?;
        Ok(output
            .split('\0')
            .map(|s| s.trim_matches('\n'))
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect())
    }

    /// Unmerged index entries, read from the index only.
    pub async fn unmerged_entries(&self) -> anyhow::Result<Vec<UnmergedEntry>> {
        let output = self
            .run_and_collect_output(&["ls-files", "-u", "-z"], Opts::new_without_logs())
            .await?;
        Ok(parse_unmerged_entries(&output))
    }

    /// Paths with unmerged index entries.
    pub async fn unmerged_paths(&self) -> anyhow::Result<Vec<String>> {
        Ok(self
            .unmerged_entries()
            .await?
            .into_iter()
            .map(|entry| entry.path)
            .collect())
    }

    /// Blob id of the working-tree file at `path` after clean filters, or `None` if it is
    /// missing.
    async fn worktree_blob(&self, path: &str) -> anyhow::Result<Option<String>> {
        if std::fs::symlink_metadata(self.repo_path.join(path)).is_err() {
            return Ok(None);
        }
        let output = self
            .run_and_collect_output(&["hash-object", "--", path], Opts::new_without_logs())
            .await?;
        Ok(Some(output.trim().to_string()))
    }

    fn has_conflict_markers(&self, path: &str) -> bool {
        std::fs::read(self.repo_path.join(path)).is_ok_and(|bytes| {
            bytes.starts_with(b"<<<<<<<") || bytes.windows(8).any(|w| w == b"\n<<<<<<<")
        })
    }

    async fn is_lfs_file(&self, path: &str) -> anyhow::Result<bool> {
        let output = self
            .run_and_collect_output(
                &["check-attr", "-z", "filter", "--", path],
                Opts::new_without_logs(),
            )
            .await?;
        Ok(output.split('\0').nth(2) == Some("lfs"))
    }

    async fn checkout_theirs(&self, path: &str) -> UnmergedOutcome {
        let pathspec = format!(":(literal){path}");
        match self
            .run(&["checkout", "--theirs", "--", &pathspec], Opts::default())
            .await
        {
            Ok(()) => UnmergedOutcome::KeptLocal,
            Err(e) => {
                warn!(path, "Could not restore the local version: {e}");
                UnmergedOutcome::Unresolved
            }
        }
    }

    /// Settles one path that git's autostash apply left unmerged. Stage 2 is the pulled
    /// version and stage 3 the stashed local one. Where git could not merge textually, the
    /// local version or deletion is put back: binary files and modify/delete conflicts hold
    /// the pulled version, and LFS pointers hold a pointer with conflict markers.
    async fn settle_unmerged_entry(&self, entry: &UnmergedEntry) -> UnmergedOutcome {
        let path = entry.path.as_str();
        let ours = entry.stages[1].as_deref();
        let theirs = entry.stages[2].as_deref();
        if theirs.is_some() {
            match self.is_lfs_file(path).await {
                Ok(true) => return self.checkout_theirs(path).await,
                Ok(false) => {}
                Err(e) => {
                    warn!(path, "Could not read attributes of conflicted file: {e}");
                    return UnmergedOutcome::Unresolved;
                }
            }
        }
        let worktree = match self.worktree_blob(path).await {
            Ok(blob) => blob,
            Err(e) => {
                warn!(path, "Could not hash conflicted file: {e}");
                return UnmergedOutcome::Unresolved;
            }
        };
        match (theirs, worktree.as_deref()) {
            (Some(theirs), Some(w)) if w == theirs => UnmergedOutcome::KeptLocal,
            (Some(_), Some(w)) if Some(w) == ours => self.checkout_theirs(path).await,
            (Some(_), Some(_)) if self.has_conflict_markers(path) => UnmergedOutcome::Markers,
            (None, None) => UnmergedOutcome::KeptDeletion,
            (None, Some(w)) if Some(w) == ours => {
                match std::fs::remove_file(self.repo_path.join(path)) {
                    Ok(()) => UnmergedOutcome::KeptDeletion,
                    Err(e) => {
                        warn!(path, "Could not restore the local deletion: {e}");
                        UnmergedOutcome::Unresolved
                    }
                }
            }
            _ => UnmergedOutcome::Unresolved,
        }
    }

    /// Settles the conflicts git's autostash apply left after a pull, or returns `None` if
    /// there are none.
    ///
    /// Text conflicts keep git's markers. Binary and modify/delete conflicts get the local
    /// version or deletion back in the working tree. Conflicted and staged paths are then
    /// reset to `HEAD` so they show as plain local edits. Git's leftover `autostash` entry is
    /// dropped only when every path was settled and `snapshot_commit` holds the same content.
    pub async fn settle_autostash_conflicts(
        &self,
        snapshot_commit: Option<&str>,
        stash_before: Option<&str>,
    ) -> anyhow::Result<Option<AutostashConflict>> {
        let entries = self.unmerged_entries().await?;
        if entries.is_empty() {
            return Ok(None);
        }

        let paths: Vec<&str> = entries.iter().map(|e| e.path.as_str()).collect();
        warn!(?paths, "Autostash apply after pull left conflicts");

        let mut conflict = AutostashConflict {
            snapshot_saved: snapshot_commit.is_some(),
            ..Default::default()
        };
        for entry in &entries {
            let list = match self.settle_unmerged_entry(entry).await {
                UnmergedOutcome::Markers => &mut conflict.marker_files,
                UnmergedOutcome::KeptLocal => &mut conflict.kept_local_files,
                UnmergedOutcome::KeptDeletion => &mut conflict.kept_deleted_files,
                UnmergedOutcome::Unresolved => &mut conflict.unresolved_files,
            };
            list.push(entry.path.clone());
        }

        let mut reset: Vec<String> = self
            .collect_nul_paths(&["diff", "--cached", "--name-only", "-z"])
            .await?;
        reset.extend(entries.into_iter().map(|e| e.path));
        reset.sort();
        reset.dedup();
        let reset_path = Self::write_pathspec_file(&reset)?;
        self.run(
            &[
                "reset",
                "-q",
                "--pathspec-from-file",
                reset_path.to_str().expect("temp file path is non-UTF-8"),
            ],
            Opts::default(),
        )
        .await?;

        conflict.autostash_dropped = match snapshot_commit {
            Some(commit) if conflict.unresolved_files.is_empty() => {
                self.drop_autostash_covered_by(commit, stash_before).await
            }
            _ => false,
        };
        Ok(Some(conflict))
    }

    /// Finishes a `pull --autostash` that started with `stash_before` at the top of `refs/stash`.
    ///
    /// If git's autostash apply conflicted, the conflicts are settled as in
    /// `settle_autostash_conflicts` and `snapshot` is not applied on top. Otherwise, or if
    /// settling fails, `snapshot` is restored as usual, calling `on_restore` first.
    pub async fn finish_autostash_pull(
        &self,
        snapshot: Option<&Snapshot>,
        stash_before: Option<&str>,
        modified_files: Vec<File>,
        on_restore: impl FnOnce() + Send,
    ) -> anyhow::Result<Option<AutostashConflict>> {
        let snapshot_commit = snapshot.map(|s| s.commit.as_str());
        match self
            .settle_autostash_conflicts(snapshot_commit, stash_before)
            .await
        {
            Ok(Some(conflict)) => return Ok(Some(conflict)),
            Ok(None) => {}
            Err(e) if snapshot.is_some() => {
                warn!("Could not settle autostash conflicts, restoring the snapshot: {e:#}");
            }
            Err(e) => return Err(e),
        }
        if let Some(snapshot) = snapshot {
            on_restore();
            self.restore_snapshot_via_cherry_pick(&snapshot.commit, modified_files)
                .await?;
        }
        Ok(None)
    }

    /// Drops `stash@{0}` if it is the `autostash` entry pushed directly on top of `stash_before`
    /// and every path it changes, in its worktree and index trees, has the same content in
    /// `snapshot_commit`.
    async fn drop_autostash_covered_by(
        &self,
        snapshot_commit: &str,
        stash_before: Option<&str>,
    ) -> bool {
        let autostash = match self
            .autostash_covered_by(snapshot_commit, stash_before)
            .await
        {
            Ok(Some(autostash)) => autostash,
            Ok(None) => {
                warn!("Keeping the autostash entry: the snapshot does not provably cover it");
                return false;
            }
            Err(e) => {
                warn!("Keeping the autostash entry: {e}");
                return false;
            }
        };
        match self
            .run(&["stash", "drop", "-q", "stash@{0}"], Opts::default())
            .await
        {
            Ok(()) => {
                info!(%autostash, "Dropped the autostash entry covered by the snapshot");
                true
            }
            Err(e) => {
                warn!("Failed to drop autostash entry {autostash}: {e}");
                false
            }
        }
    }

    /// The autostash commit when `drop_autostash_covered_by` may drop it.
    async fn autostash_covered_by(
        &self,
        snapshot_commit: &str,
        stash_before: Option<&str>,
    ) -> anyhow::Result<Option<String>> {
        let output = self
            .run_and_collect_output(
                &["stash", "list", "-n2", "--format=%H%x1f%gs"],
                Opts::new_without_logs(),
            )
            .await?;
        let entries: Vec<(&str, &str)> = output
            .lines()
            .filter_map(|line| line.split_once('\x1f'))
            .collect();

        let Some(&(autostash, subject)) = entries.first() else {
            return Ok(None);
        };
        let below = entries.get(1).map(|(sha, _)| *sha);
        if subject.trim() != "autostash" || below != stash_before {
            return Ok(None);
        }

        for tree in [autostash.to_string(), format!("{autostash}^2")] {
            let base = format!("{autostash}^1");
            let changed = self
                .collect_nul_paths(&["diff", "--name-only", "-z", "--no-renames", &base, &tree])
                .await?;
            let differs: HashSet<String> = self
                .collect_nul_paths(&[
                    "diff",
                    "--name-only",
                    "-z",
                    "--no-renames",
                    snapshot_commit,
                    &tree,
                ])
                .await?
                .into_iter()
                .collect();
            if changed.iter().any(|path| differs.contains(path)) {
                return Ok(None);
            }
        }

        Ok(Some(autostash.to_string()))
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

    /// Lists `refs/remotes/origin/*`, newest commit first. Reads local refs only.
    pub async fn list_remote_branches(&self) -> anyhow::Result<Vec<BranchInfo>> {
        let output = self
            .run_and_collect_output(
                &[
                    "for-each-ref",
                    "--sort=-committerdate",
                    "refs/remotes/origin",
                    "--format=%(refname:lstrip=3)%1f%(objectname)%1f%(objectname:short)%1f%(committerdate:iso-strict)%1f%(authorname)%1f%(subject)%1f%(contents)%1e",
                ],
                Opts {
                    skip_notify_frontend: true,
                    should_log_stdout: false,
                    ..Default::default()
                },
            )
            .await?;

        Ok(parse_branch_records(&output))
    }

    /// Whether `refs/heads/<name>` exists on origin.
    ///
    /// The single fully qualified ls-remote check for branch existence. Use it instead of
    /// `has_remote_branch`, which tail-matches (`foo` also matches `refs/heads/team/foo`).
    pub async fn has_remote_branch_exact(&self, name: &str) -> anyhow::Result<bool> {
        let refname = format!("refs/heads/{name}");
        let output = self
            .run_and_collect_output(
                &["ls-remote", "--heads", "origin", &refname],
                Opts::default(),
            )
            .await?;

        Ok(output
            .lines()
            .any(|line| line.split_whitespace().nth(1) == Some(refname.as_str())))
    }

    /// Creates `name` on origin at `refs/remotes/origin/<base>`, failing if it already exists.
    ///
    /// The empty lease makes the push atomic: git rejects it if the ref exists at another SHA,
    /// and reports "up to date" (no `*` line) if it exists at the same SHA.
    pub async fn create_remote_branch(
        &self,
        name: &str,
        base: &str,
    ) -> anyhow::Result<CreateRemoteBranchOutcome> {
        let lease = format!("--force-with-lease=refs/heads/{name}:");
        let refspec = format!("refs/remotes/origin/{base}:refs/heads/{name}");
        let result = self
            .run_and_collect_output(
                &["push", "--porcelain", &lease, "origin", &refspec],
                Opts::default().with_complete_error(),
            )
            .await;

        match result {
            Ok(stdout) => {
                if push_created_new_ref(&stdout, name) {
                    Ok(CreateRemoteBranchOutcome::Created)
                } else {
                    Ok(CreateRemoteBranchOutcome::AlreadyExists)
                }
            }
            Err(e) => match self.has_remote_branch_exact(name).await {
                Ok(true) => Ok(CreateRemoteBranchOutcome::AlreadyExists),
                Ok(false) | Err(_) => {
                    Err(anyhow!("Failed to create branch '{name}' on origin: {e}"))
                }
            },
        }
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

    /// Checks git against `MIN_BRANCH_GIT`. Only a supported result is cached, so an upgrade
    /// is picked up on the next check.
    pub async fn check_min_version(&self) -> anyhow::Result<GitVersionCheck> {
        if let Some(version) = SUPPORTED_GIT.get() {
            return Ok(GitVersionCheck {
                version: version.clone(),
                supported: true,
            });
        }

        let raw = self.version().await?;
        let check = version_check_from_output(&raw);
        if check.supported {
            let _ = SUPPORTED_GIT.set(check.version.clone());
        }
        Ok(check)
    }

    /// Predicts whether merging `head_ref` into `base_ref` conflicts, without touching the
    /// working tree, index or refs. Spawned directly because the exit code and NUL-separated
    /// stdout are both needed.
    pub async fn predict_merge_conflicts(
        &self,
        base_ref: &str,
        head_ref: &str,
    ) -> anyhow::Result<MergeTreeOutcome> {
        if base_ref.starts_with('-') || head_ref.starts_with('-') {
            bail!("Invalid ref for merge-tree: {base_ref} {head_ref}");
        }

        let args = [
            "merge-tree",
            "--write-tree",
            "--name-only",
            "--no-messages",
            "-z",
            base_ref,
            head_ref,
        ];
        let mut cmd = Command::new("git");
        cmd.args(args);
        cmd.env("GIT_CLONE_PROTECTION_ACTIVE", "false");
        cmd.env("LC_ALL", "C");
        if !self.repo_path.as_os_str().is_empty() {
            cmd.current_dir(self.repo_path.canonicalize()?);
        }
        #[cfg(windows)]
        cmd.creation_flags(CREATE_NO_WINDOW);

        info!("Running: git {}", args.join(" "));

        let output = cmd.output().await?;
        parse_merge_tree_output(
            output.status.code(),
            &output.stdout,
            &String::from_utf8_lossy(&output.stderr),
        )
    }

    /// Maps each path changed in `range` (newest `limit` commits) to its newest commit.
    pub async fn last_touch_by_path(
        &self,
        range: &str,
        limit: usize,
    ) -> anyhow::Result<HashMap<String, FileTouch>> {
        if range.starts_with('-') {
            bail!("Invalid log range: {range}");
        }

        let output = self
            .run_and_collect_output(
                &[
                    "-c",
                    "core.quotePath=false",
                    "log",
                    "--no-renames",
                    "--name-only",
                    "--format=%x1e%H%x1f%an%x1f%aI",
                    &format!("-{limit}"),
                    range,
                    "--",
                ],
                Opts::new_without_logs().with_skip_notify_frontend(),
            )
            .await?;
        Ok(parse_last_touch(&output))
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
        let out_handle = tokio::spawn(async move {
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
        let err_handle = tokio::spawn(async move {
            while let Some(line) = err_reader.next_line().await.unwrap() {
                err_lines_thread.write().push(line.clone());
                info!("{}", line);
            }
        });

        let status = git_proc.wait().await?;

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

        // Signal the frontend that the git command has completed by sending
        // an empty string, allowing the UI to clear the status message.
        if !opts.skip_notify_frontend {
            let _ = self.tx.send(String::new());
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

    // Unlocked lockable assets sit on disk read-only. Restore must overwrite
    // one and leave it writable — the same contract `import_zipped_changes`
    // holds to.
    #[tokio::test]
    async fn test_restore_snapshot_overwrites_readonly_file_and_leaves_it_writable() {
        let (git, _dir) = setup_repo();

        std::fs::write(git.repo_path.join("a.txt"), "base").unwrap();
        for args in [vec!["add", "a.txt"], vec!["commit", "-m", "add a"]] {
            let out = StdCommand::new("git")
                .args(&args)
                .current_dir(&git.repo_path)
                .output()
                .unwrap();
            assert!(out.status.success());
        }

        std::fs::write(git.repo_path.join("a.txt"), "snapshot-version").unwrap();
        let snapshot = git
            .save_snapshot_all("snapshot of a")
            .await
            .expect("save_snapshot_all");

        // Diverge locally, then mark the file read-only the way an unlocked
        // lockable asset would be.
        let path = git.repo_path.join("a.txt");
        std::fs::write(&path, "post-snapshot").unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_readonly(true);
        std::fs::set_permissions(&path, perms).unwrap();

        git.restore_snapshot(&snapshot.commit, vec![], true, None)
            .await
            .expect("restore over a read-only file");

        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "snapshot-version",
            "restore should have overwritten the read-only file"
        );
        assert!(
            !std::fs::metadata(&path).unwrap().permissions().readonly(),
            "restore should leave the file writable"
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
        assert!(
            msg.contains("1 local file conflicts") && !msg.contains("untracked"),
            "a tracked file must not be described as untracked; got: {msg}"
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

    /// Runs git in `dir`, asserts success and returns trimmed stdout.
    fn git_ok(dir: &std::path::Path, args: &[&str]) -> String {
        let out = StdCommand::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("run git");
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// `setup_repo()` plus a local bare repo as `origin` with `main` pushed and fetched.
    /// Returns (git, repo dir guard, origin dir guard).
    fn setup_repo_with_bare_origin() -> (Git, TempDir, TempDir) {
        let (git, dir) = setup_repo();
        let origin = tempfile::tempdir().expect("create origin tempdir");
        git_ok(origin.path(), &["init", "--bare"]);
        let origin_path = origin.path().to_string_lossy().to_string();
        git_ok(&git.repo_path, &["remote", "add", "origin", &origin_path]);
        git_ok(&git.repo_path, &["push", "origin", "HEAD:refs/heads/main"]);
        git_ok(&git.repo_path, &["fetch", "origin"]);
        (git, dir, origin)
    }

    fn remote_sha(origin: &std::path::Path, refname: &str) -> String {
        git_ok(origin, &["rev-parse", "--verify", refname])
    }

    #[test]
    fn test_parse_branch_records() {
        let output = concat!(
            "f11r-branch-new\x1faaaa1111\x1faaaa111\x1f2026-10-21T13:01:46-07:00\x1fAlice\x1fAdd thing\x1fAdd thing\n\nBody line one\nBody line two\n\n\x1e\n",
            "HEAD\x1fbbbb2222\x1fbbbb222\x1f2026-10-20T10:00:00-07:00\x1fBob\x1fHead\x1fHead\n\x1e\n",
            "broken\x1fcccc\x1fccc\x1f2026-10-19T10:00:00-07:00\x1fCarol\x1e\n",
            "main\x1fdddd4444\x1fdddd444\x1f2026-10-18T09:00:00-07:00\x1fDan\x1fOne-liner\x1fOne-liner\n\x1e"
        );

        let records = parse_branch_records(output);
        let names: Vec<&str> = records.iter().map(|b| b.name.as_str()).collect();
        assert_eq!(names, vec!["f11r-branch-new", "main"]);

        let first = &records[0];
        assert_eq!(first.sha, "aaaa1111");
        assert_eq!(first.short_sha, "aaaa111");
        assert_eq!(first.commit_date, "2026-10-21T13:01:46-07:00");
        assert_eq!(first.author, "Alice");
        assert_eq!(first.subject, "Add thing");
        assert_eq!(first.message, "Add thing\n\nBody line one\nBody line two");
        assert!(!first.is_feature_branch && !first.is_protected && !first.is_current);

        assert_eq!(records[1].message, "One-liner");
        assert!(parse_branch_records("").is_empty());
        assert!(parse_branch_records("\n").is_empty());
    }

    #[test]
    fn test_push_created_new_ref() {
        let name = "f11r-branch-x";
        assert!(push_created_new_ref(
            "To C:/origin\n*\trefs/remotes/origin/main:refs/heads/f11r-branch-x\t[new branch]\nDone",
            name
        ));
        assert!(!push_created_new_ref(
            "To C:/origin\n=\trefs/remotes/origin/main:refs/heads/f11r-branch-x\t[up to date]\nDone",
            name
        ));
        assert!(!push_created_new_ref(
            "!\trefs/remotes/origin/main:refs/heads/f11r-branch-x\t[rejected] (stale info)",
            name
        ));
        assert!(!push_created_new_ref("", name));
        assert!(!push_created_new_ref(
            "*\trefs/remotes/origin/main:refs/heads/f11r-branch-xy\t[new branch]",
            name
        ));
        assert!(!push_created_new_ref(
            "*\trefs/remotes/origin/main:refs/heads/team/f11r-branch-x\t[new branch]",
            name
        ));
    }

    #[tokio::test]
    async fn test_list_remote_branches_reads_origin_refs_and_skips_head() {
        let (git, _dir) = setup_repo();
        git_ok(
            &git.repo_path,
            &["update-ref", "refs/remotes/origin/a", "HEAD"],
        );
        git_ok(
            &git.repo_path,
            &["update-ref", "refs/remotes/origin/feature/b", "HEAD"],
        );
        git_ok(
            &git.repo_path,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/a",
            ],
        );

        let branches = git
            .list_remote_branches()
            .await
            .expect("list_remote_branches");
        let mut names: Vec<&str> = branches.iter().map(|b| b.name.as_str()).collect();
        names.sort();
        assert_eq!(names, vec!["a", "feature/b"]);

        let head_sha = git_ok(&git.repo_path, &["rev-parse", "HEAD"]);
        for b in &branches {
            assert_eq!(b.sha, head_sha);
            assert!(head_sha.starts_with(&b.short_sha));
            assert_eq!(b.subject, "seed");
            assert_eq!(b.message, "seed");
            assert_eq!(b.author, "test");
        }
    }

    #[tokio::test]
    async fn test_create_remote_branch_new_then_up_to_date() {
        let (git, _dir, origin) = setup_repo_with_bare_origin();
        let main_sha = git_ok(&git.repo_path, &["rev-parse", "refs/remotes/origin/main"]);

        let outcome = git
            .create_remote_branch("f11r-branch-x", "main")
            .await
            .expect("create_remote_branch");
        assert_eq!(outcome, CreateRemoteBranchOutcome::Created);
        assert_eq!(
            remote_sha(origin.path(), "refs/heads/f11r-branch-x"),
            main_sha
        );

        // Same name at the same SHA: git reports "up to date" and exits 0.
        let outcome = git
            .create_remote_branch("f11r-branch-x", "main")
            .await
            .expect("create_remote_branch again");
        assert_eq!(outcome, CreateRemoteBranchOutcome::AlreadyExists);
        assert_eq!(
            remote_sha(origin.path(), "refs/heads/f11r-branch-x"),
            main_sha
        );
    }

    #[tokio::test]
    async fn test_create_remote_branch_existing_at_different_sha() {
        let (git, _dir, origin) = setup_repo_with_bare_origin();
        let first_sha = git_ok(&git.repo_path, &["rev-parse", "HEAD"]);

        std::fs::write(git.repo_path.join("seed.txt"), "second").unwrap();
        git_ok(&git.repo_path, &["commit", "-am", "second"]);
        let second_sha = git_ok(&git.repo_path, &["rev-parse", "HEAD"]);
        git_ok(
            &git.repo_path,
            &["push", "origin", "HEAD:refs/heads/f11r-branch-y"],
        );
        git_ok(
            &git.repo_path,
            &["update-ref", "refs/remotes/origin/main", &first_sha],
        );

        let outcome = git
            .create_remote_branch("f11r-branch-y", "main")
            .await
            .expect("create_remote_branch");
        assert_eq!(outcome, CreateRemoteBranchOutcome::AlreadyExists);
        assert_eq!(
            remote_sha(origin.path(), "refs/heads/f11r-branch-y"),
            second_sha
        );
    }

    // A plain push would fast-forward an existing branch; the empty lease must refuse it.
    #[tokio::test]
    async fn test_create_remote_branch_does_not_fast_forward_existing() {
        let (git, _dir, origin) = setup_repo_with_bare_origin();
        let first_sha = git_ok(&git.repo_path, &["rev-parse", "HEAD"]);
        git_ok(
            &git.repo_path,
            &["push", "origin", "HEAD:refs/heads/f11r-branch-z"],
        );

        std::fs::write(git.repo_path.join("seed.txt"), "second").unwrap();
        git_ok(&git.repo_path, &["commit", "-am", "second"]);
        git_ok(&git.repo_path, &["push", "origin", "HEAD:refs/heads/main"]);
        git_ok(&git.repo_path, &["fetch", "origin"]);

        let outcome = git
            .create_remote_branch("f11r-branch-z", "main")
            .await
            .expect("create_remote_branch");
        assert_eq!(outcome, CreateRemoteBranchOutcome::AlreadyExists);
        assert_eq!(
            remote_sha(origin.path(), "refs/heads/f11r-branch-z"),
            first_sha
        );
    }

    #[tokio::test]
    async fn test_create_remote_branch_reports_push_failure() {
        let (git, _dir) = setup_repo();
        let missing = tempfile::tempdir().expect("create tempdir");
        let missing_origin = missing.path().join("does-not-exist");
        git_ok(
            &git.repo_path,
            &["remote", "add", "origin", &missing_origin.to_string_lossy()],
        );
        git_ok(
            &git.repo_path,
            &["update-ref", "refs/remotes/origin/main", "HEAD"],
        );

        let err = git
            .create_remote_branch("f11r-branch-x", "main")
            .await
            .expect_err("push to a missing origin must fail");
        let msg = err.to_string();
        // The existence re-check also fails here, so the original push error is returned.
        assert!(
            msg.contains("Git command failed:"),
            "unexpected error: {msg}"
        );
    }

    #[tokio::test]
    async fn test_create_remote_branch_wraps_error_when_branch_absent() {
        let (git, _dir, origin) = setup_repo_with_bare_origin();

        // Base ref missing locally: push fails, origin is reachable, branch absent.
        let err = git
            .create_remote_branch("f11r-branch-x", "no-such-base")
            .await
            .expect_err("push from a missing base must fail");
        let msg = err.to_string();
        assert!(
            msg.starts_with("Failed to create branch 'f11r-branch-x' on origin:"),
            "unexpected error: {msg}"
        );
        // git's stderr names the missing source ref.
        assert!(msg.contains("no-such-base"), "unexpected error: {msg}");
        assert!(
            StdCommand::new("git")
                .args(["rev-parse", "--verify", "refs/heads/f11r-branch-x"])
                .current_dir(origin.path())
                .output()
                .map(|o| !o.status.success())
                .unwrap(),
            "branch must not exist on origin"
        );
    }

    #[tokio::test]
    async fn test_has_remote_branch_exact() {
        let (git, _dir, _origin) = setup_repo_with_bare_origin();
        git_ok(
            &git.repo_path,
            &["push", "origin", "HEAD:refs/heads/f11r-branch-x"],
        );
        git_ok(
            &git.repo_path,
            &["push", "origin", "HEAD:refs/heads/team/f11r-branch-x"],
        );
        git_ok(
            &git.repo_path,
            &["push", "origin", "HEAD:refs/heads/team/f11r-branch-z"],
        );

        assert!(git.has_remote_branch_exact("f11r-branch-x").await.unwrap());
        assert!(git.has_remote_branch_exact("main").await.unwrap());
        assert!(!git.has_remote_branch_exact("branch-x").await.unwrap());
        assert!(!git
            .has_remote_branch_exact("f11r-branch-nope")
            .await
            .unwrap());
        // Only `team/f11r-branch-z` exists; the tail-matching helper would say yes.
        assert!(!git.has_remote_branch_exact("f11r-branch-z").await.unwrap());
        assert!(git.has_remote_branch("f11r-branch-z").await.unwrap());
    }

    /// Commits `file` with `content` and message `msg` in `dir`.
    fn commit_file(dir: &std::path::Path, file: &str, content: &str, msg: &str) {
        std::fs::write(dir.join(file), content).unwrap();
        git_ok(dir, &["add", file]);
        git_ok(dir, &["commit", "-m", msg]);
    }

    /// User repo on `feat` (one pushed commit F, upstream set) plus a collaborator clone
    /// that advances `main` and force-pushes `feat` rebased onto it, rewriting F.
    /// Returns (git, user dir guard, origin dir guard, collaborator dir guard).
    fn setup_remote_force_pushed_feat() -> (Git, TempDir, TempDir, TempDir) {
        let (git, dir, origin) = setup_repo_with_bare_origin();
        let repo = git.repo_path.clone();
        git_ok(&repo, &["switch", "-c", "feat"]);
        commit_file(&repo, "feat.txt", "feat", "F");
        git_ok(&repo, &["push", "-u", "origin", "feat"]);

        let collab = tempfile::tempdir().expect("create collaborator tempdir");
        let origin_path = origin.path().to_string_lossy().to_string();
        git_ok(
            collab.path().parent().unwrap(),
            &["clone", &origin_path, &collab.path().to_string_lossy()],
        );
        let c = collab.path();
        git_ok(c, &["config", "user.email", "collab@example.com"]);
        git_ok(c, &["config", "user.name", "collab"]);
        git_ok(c, &["config", "commit.gpgsign", "false"]);
        git_ok(c, &["config", "core.autocrlf", "false"]);

        git_ok(c, &["switch", "-c", "feat", "origin/feat"]);
        git_ok(c, &["switch", "-c", "trunk", "origin/main"]);
        commit_file(c, "main.txt", "main", "M");
        git_ok(c, &["push", "origin", "trunk:refs/heads/main"]);
        git_ok(c, &["switch", "feat"]);
        git_ok(c, &["rebase", "trunk"]);
        // Reword the patch too, so only the fork point (not patch-id matching) can drop F.
        std::fs::write(c.join("feat.txt"), "feat rewritten").unwrap();
        git_ok(c, &["commit", "-a", "--amend", "--no-edit"]);
        git_ok(c, &["push", "--force", "origin", "feat"]);

        // The user's origin/feat still points at F; the pull fetches the rewrite.
        (git, dir, origin, collab)
    }

    #[tokio::test]
    async fn test_pull_rebase_after_remote_force_push_drops_old_commits() {
        let (git, _dir, origin, _collab) = setup_remote_force_pushed_feat();
        let repo = &git.repo_path;

        git.pull(PullStrategy::Rebase, PullStashStrategy::Autostash)
            .await
            .expect("pull");

        assert_eq!(
            git_ok(repo, &["log", "--format=%s", "origin/feat..HEAD"]),
            ""
        );
        let head = git_ok(repo, &["rev-parse", "HEAD"]);
        assert_eq!(head, git_ok(repo, &["rev-parse", "origin/feat"]));
        assert_eq!(head, remote_sha(origin.path(), "refs/heads/feat"));
        assert_eq!(git_ok(repo, &["log", "--format=%s", "-3"]), "F\nM\nseed");
    }

    #[tokio::test]
    async fn test_pull_rebase_after_remote_force_push_keeps_unpushed_local_commit() {
        let (git, _dir, origin, _collab) = setup_remote_force_pushed_feat();
        let repo = &git.repo_path;
        commit_file(repo, "local.txt", "local", "L");
        std::fs::write(repo.join("seed.txt"), "seed\ndirty edit").unwrap();
        let stashes_before = git_ok(repo, &["stash", "list"]);

        git.pull(PullStrategy::Rebase, PullStashStrategy::Autostash)
            .await
            .expect("pull");

        assert_eq!(
            git_ok(repo, &["log", "--format=%s", "origin/feat..HEAD"]),
            "L"
        );
        assert_eq!(
            git_ok(repo, &["rev-parse", "origin/feat"]),
            remote_sha(origin.path(), "refs/heads/feat")
        );
        assert_eq!(
            std::fs::read(repo.join("seed.txt")).unwrap(),
            b"seed\ndirty edit"
        );
        assert_eq!(git_ok(repo, &["stash", "list"]), stashes_before);
        assert_eq!(git_ok(repo, &["log", "--format=%s", "-4"]), "L\nF\nM\nseed");
    }

    const DUMMY_BASE: &str = "line1\nline2\nline3\n";

    /// User repo on `feat` (pushed, upstream set) with `dummy.txt` and `other.txt`, plus a
    /// collaborator commit on `origin/feat` writing `collab_content` to `collab_file`.
    /// Returns (git, user dir guard, origin dir guard, collaborator dir guard).
    fn setup_feat_with_collab_commit(
        collab_file: &str,
        collab_content: &str,
    ) -> (Git, TempDir, TempDir, TempDir) {
        setup_feat_with_collab(&[], |c| {
            std::fs::write(c.join(collab_file), collab_content).unwrap();
        })
    }

    /// Turns off any globally configured `lfs` filter so LFS pointers stay plain blobs.
    fn disable_lfs_filter(dir: &std::path::Path) {
        for key in ["process", "clean", "smudge"] {
            git_ok(dir, &["config", &format!("filter.lfs.{key}"), ""]);
        }
        git_ok(dir, &["config", "filter.lfs.required", "false"]);
    }

    /// `setup_feat_with_collab_commit` with `seed` files in the base commit and a collaborator
    /// commit of whatever `collab_edit` does to its working tree.
    fn setup_feat_with_collab(
        seed: &[(&str, &[u8])],
        collab_edit: impl FnOnce(&std::path::Path),
    ) -> (Git, TempDir, TempDir, TempDir) {
        let (git, dir, origin) = setup_repo_with_bare_origin();
        let repo = git.repo_path.clone();
        disable_lfs_filter(&repo);
        git_ok(&repo, &["switch", "-c", "feat"]);
        std::fs::write(repo.join("other.txt"), "other\n").unwrap();
        git_ok(&repo, &["add", "other.txt"]);
        for (path, content) in seed {
            std::fs::write(repo.join(path), content).unwrap();
            git_ok(&repo, &["add", path]);
        }
        commit_file(&repo, "dummy.txt", DUMMY_BASE, "base");
        git_ok(&repo, &["push", "-u", "origin", "feat"]);

        let collab = tempfile::tempdir().expect("create collaborator tempdir");
        let origin_path = origin.path().to_string_lossy().to_string();
        git_ok(
            collab.path().parent().unwrap(),
            &["clone", &origin_path, &collab.path().to_string_lossy()],
        );
        let c = collab.path();
        git_ok(c, &["config", "user.email", "collab@example.com"]);
        git_ok(c, &["config", "user.name", "collab"]);
        git_ok(c, &["config", "commit.gpgsign", "false"]);
        git_ok(c, &["config", "core.autocrlf", "false"]);
        disable_lfs_filter(c);
        git_ok(c, &["switch", "-c", "feat", "origin/feat"]);
        collab_edit(c);
        git_ok(c, &["add", "-A"]);
        git_ok(c, &["commit", "-m", "collab"]);
        git_ok(c, &["push", "origin", "feat"]);

        (git, dir, origin, collab)
    }

    fn modified(paths: &[&str]) -> Vec<File> {
        paths
            .iter()
            .map(|path| File {
                path: path.to_string(),
                ..Default::default()
            })
            .collect()
    }

    /// The git half of `PullOp`: pre-pull snapshot, `pull --rebase --autostash`, then finish.
    async fn snapshot_pull_finish(
        git: &Git,
        modified_paths: &[&str],
        untracked_paths: &[&str],
    ) -> (Snapshot, anyhow::Result<Option<AutostashConflict>>) {
        let snapshot_paths = modified_paths
            .iter()
            .chain(untracked_paths)
            .map(|p| p.to_string())
            .collect();
        let snapshot = git
            .save_snapshot("pre-pull", snapshot_paths)
            .await
            .expect("save_snapshot");
        let stash_before = git.stash_top().await.expect("stash_top");
        assert_eq!(stash_before.as_deref(), Some(snapshot.commit.as_str()));
        git.pull(PullStrategy::Rebase, PullStashStrategy::Autostash)
            .await
            .expect("pull");
        let result = git
            .finish_autostash_pull(
                Some(&snapshot),
                stash_before.as_deref(),
                modified(modified_paths),
                || {},
            )
            .await;
        (snapshot, result)
    }

    fn stash_entries(repo: &std::path::Path) -> Vec<String> {
        git_ok(repo, &["stash", "list", "--format=%H %gs"])
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn assert_no_restore_leftovers(repo: &std::path::Path) {
        for name in [
            "dummy.txt.snapshotcopy",
            "dummy.txt.localcopy",
            "other.txt.snapshotcopy",
            "other.txt.localcopy",
            "new.txt.localcopy",
        ] {
            assert!(!repo.join(name).exists(), "{name} must not exist");
        }
    }

    #[tokio::test]
    async fn test_finish_autostash_pull_clean_restores_snapshot() {
        let (git, _dir, _origin, _collab) =
            setup_feat_with_collab_commit("other.txt", "collab other\n");
        let repo = git.repo_path.clone();
        std::fs::write(repo.join("dummy.txt"), "line1\nmine\nline3\n").unwrap();
        std::fs::write(repo.join("new.txt"), "new\n").unwrap();

        let (snapshot, result) = snapshot_pull_finish(&git, &["dummy.txt"], &["new.txt"]).await;

        assert_eq!(result.expect("finish"), None);
        assert_eq!(
            std::fs::read_to_string(repo.join("dummy.txt")).unwrap(),
            "line1\nmine\nline3\n"
        );
        assert_eq!(
            std::fs::read_to_string(repo.join("other.txt")).unwrap(),
            "collab other\n"
        );
        assert_eq!(
            std::fs::read_to_string(repo.join("new.txt")).unwrap(),
            "new\n"
        );
        assert_no_restore_leftovers(&repo);
        assert_eq!(git_ok(&repo, &["diff", "--cached", "--name-only"]), "");
        assert_eq!(
            stash_entries(&repo),
            vec![format!("{} snapshot pre-pull", snapshot.commit)]
        );
    }

    async fn assert_overlapping_edit_leaves_markers_only(local_commit: bool) {
        let (git, _dir, _origin, _collab) =
            setup_feat_with_collab_commit("dummy.txt", "line1\ncollab\nline3\n");
        let repo = git.repo_path.clone();
        if local_commit {
            commit_file(&repo, "local.txt", "local\n", "local");
        }
        std::fs::write(repo.join("dummy.txt"), "line1\nmine\nline3\n").unwrap();
        std::fs::write(repo.join("other.txt"), "my other\n").unwrap();
        std::fs::write(repo.join("new.txt"), "new\n").unwrap();

        let (snapshot, result) =
            snapshot_pull_finish(&git, &["dummy.txt", "other.txt"], &["new.txt"]).await;

        let conflict = result.expect("finish").expect("autostash conflict");
        assert_eq!(
            conflict,
            AutostashConflict {
                marker_files: vec!["dummy.txt".to_string()],
                snapshot_saved: true,
                autostash_dropped: true,
                ..Default::default()
            }
        );

        let dummy = std::fs::read_to_string(repo.join("dummy.txt")).unwrap();
        assert_eq!(dummy.matches("<<<<<<<").count(), 1, "{dummy}");
        assert!(
            dummy.contains("collab") && dummy.contains("mine"),
            "{dummy}"
        );
        assert_eq!(
            std::fs::read_to_string(repo.join("other.txt")).unwrap(),
            "my other\n"
        );
        assert_eq!(
            std::fs::read_to_string(repo.join("new.txt")).unwrap(),
            "new\n"
        );
        assert_no_restore_leftovers(&repo);

        assert!(git.unmerged_paths().await.unwrap().is_empty());
        assert_eq!(git_ok(&repo, &["diff", "--cached", "--name-only"]), "");
        assert_eq!(
            git_ok(&repo, &["show", "HEAD:dummy.txt"]),
            "line1\ncollab\nline3"
        );
        assert_eq!(
            stash_entries(&repo),
            vec![format!("{} snapshot pre-pull", snapshot.commit)]
        );
        assert_eq!(
            git_ok(&repo, &["show", &format!("{}:dummy.txt", snapshot.commit)]),
            "line1\nmine\nline3"
        );
    }

    #[tokio::test]
    async fn test_finish_autostash_pull_fast_forward_conflict_leaves_markers_only() {
        assert_overlapping_edit_leaves_markers_only(false).await;
    }

    #[tokio::test]
    async fn test_finish_autostash_pull_rebase_conflict_leaves_markers_only() {
        assert_overlapping_edit_leaves_markers_only(true).await;
    }

    #[tokio::test]
    async fn test_finish_autostash_pull_keeps_autostash_not_in_snapshot() {
        let (git, _dir, _origin, _collab) =
            setup_feat_with_collab_commit("dummy.txt", "line1\ncollab\nline3\n");
        let repo = git.repo_path.clone();
        // The staged version exists only in git's autostash index commit.
        std::fs::write(repo.join("dummy.txt"), "line1\nstaged\nline3\n").unwrap();
        git_ok(&repo, &["add", "dummy.txt"]);
        std::fs::write(repo.join("dummy.txt"), "line1\nmine\nline3\n").unwrap();

        let (snapshot, result) = snapshot_pull_finish(&git, &["dummy.txt"], &[]).await;

        let conflict = result.expect("finish").expect("autostash conflict");
        assert!(conflict.snapshot_saved && !conflict.autostash_dropped);
        assert!(conflict.to_string().contains("'autostash' stash entry"));
        let entries = stash_entries(&repo);
        assert_eq!(entries.len(), 2, "{entries:?}");
        assert!(entries[0].ends_with(" autostash"), "{entries:?}");
        assert_eq!(entries[1], format!("{} snapshot pre-pull", snapshot.commit));
        assert!(!repo.join("dummy.txt.snapshotcopy").exists());
    }

    const LFS_ATTRS: &str = "*.uasset filter=lfs diff=lfs merge=lfs -text\n";

    fn lfs_pointer(oid: &str) -> String {
        format!("version test-lfs\noid sha256:{oid}\nsize 10\n")
    }

    /// Seeds a binary file, an LFS pointer, `ud.txt` and `du.txt`; the collaborator changes the
    /// binary, the pointer and `ud.txt`, deletes `du.txt` and changes `dummy.txt` when `text`.
    fn setup_feat_with_mixed_collab(text: bool) -> (Git, TempDir, TempDir, TempDir) {
        let base_pointer = lfs_pointer("base");
        setup_feat_with_collab(
            &[
                (".gitattributes", LFS_ATTRS.as_bytes()),
                ("a.bin", b"base\0bin"),
                ("a.uasset", base_pointer.as_bytes()),
                ("ud.txt", b"ud\n"),
                ("du.txt", b"du\n"),
            ],
            |c| {
                std::fs::write(c.join("a.bin"), b"remote\0bin").unwrap();
                std::fs::write(c.join("a.uasset"), lfs_pointer("remote")).unwrap();
                std::fs::write(c.join("ud.txt"), "ud remote\n").unwrap();
                std::fs::remove_file(c.join("du.txt")).unwrap();
                if text {
                    std::fs::write(c.join("dummy.txt"), "line1\ncollab\nline3\n").unwrap();
                }
            },
        )
    }

    /// Local edits that conflict with every change of `setup_feat_with_mixed_collab`.
    fn write_mixed_local_edits(repo: &std::path::Path, text: bool) {
        std::fs::write(repo.join("a.bin"), b"local\0bin").unwrap();
        std::fs::write(repo.join("a.uasset"), lfs_pointer("local")).unwrap();
        std::fs::remove_file(repo.join("ud.txt")).unwrap();
        std::fs::write(repo.join("du.txt"), "du local\n").unwrap();
        if text {
            std::fs::write(repo.join("dummy.txt"), "line1\nmine\nline3\n").unwrap();
        }
    }

    fn assert_mixed_local_edits_kept(repo: &std::path::Path) {
        assert_eq!(std::fs::read(repo.join("a.bin")).unwrap(), b"local\0bin");
        assert_eq!(
            std::fs::read_to_string(repo.join("a.uasset")).unwrap(),
            lfs_pointer("local")
        );
        assert!(!repo.join("ud.txt").exists(), "ud.txt must stay deleted");
        assert_eq!(
            std::fs::read_to_string(repo.join("du.txt")).unwrap(),
            "du local\n"
        );
        assert_eq!(git_ok(repo, &["show", "HEAD:a.bin"]), "remote\0bin");
        assert_eq!(git_ok(repo, &["show", "HEAD:ud.txt"]), "ud remote");
        assert_eq!(git_ok(repo, &["ls-files", "-u"]), "");
        assert_eq!(git_ok(repo, &["diff", "--cached", "--name-only"]), "");
        let status = git_ok(
            repo,
            &[
                "status",
                "--porcelain",
                "--",
                "a.bin",
                "a.uasset",
                "ud.txt",
                "du.txt",
            ],
        );
        // `git_ok` trims the leading space of the first line, so compare trimmed lines.
        let mut status: Vec<&str> = status.lines().map(str::trim).collect();
        status.sort();
        assert_eq!(
            status,
            vec!["?? du.txt", "D ud.txt", "M a.bin", "M a.uasset"]
        );
    }

    async fn assert_binary_and_delete_conflicts_keep_local(local_commit: bool) {
        let (git, _dir, _origin, _collab) = setup_feat_with_mixed_collab(false);
        let repo = git.repo_path.clone();
        if local_commit {
            commit_file(&repo, "local.txt", "local\n", "local");
        }
        write_mixed_local_edits(&repo, false);

        let (snapshot, result) =
            snapshot_pull_finish(&git, &["a.bin", "a.uasset", "ud.txt", "du.txt"], &[]).await;

        let conflict = result.expect("finish").expect("autostash conflict");
        assert_eq!(
            conflict,
            AutostashConflict {
                kept_local_files: vec![
                    "a.bin".to_string(),
                    "a.uasset".to_string(),
                    "du.txt".to_string()
                ],
                kept_deleted_files: vec!["ud.txt".to_string()],
                snapshot_saved: true,
                autostash_dropped: true,
                ..Default::default()
            }
        );
        assert_mixed_local_edits_kept(&repo);
        assert_eq!(
            stash_entries(&repo),
            vec![format!("{} snapshot pre-pull", snapshot.commit)]
        );
        assert_eq!(
            git_ok(&repo, &["show", &format!("{}:a.bin", snapshot.commit)]),
            "local\0bin"
        );
    }

    #[tokio::test]
    async fn test_finish_autostash_pull_fast_forward_binary_and_delete_conflicts_keep_local() {
        assert_binary_and_delete_conflicts_keep_local(false).await;
    }

    #[tokio::test]
    async fn test_finish_autostash_pull_rebase_binary_and_delete_conflicts_keep_local() {
        assert_binary_and_delete_conflicts_keep_local(true).await;
    }

    #[tokio::test]
    async fn test_finish_autostash_pull_mixed_conflicts() {
        let (git, _dir, _origin, _collab) = setup_feat_with_mixed_collab(true);
        let repo = git.repo_path.clone();
        commit_file(&repo, "local.txt", "local\n", "local");
        write_mixed_local_edits(&repo, true);

        let (snapshot, result) = snapshot_pull_finish(
            &git,
            &["a.bin", "a.uasset", "ud.txt", "du.txt", "dummy.txt"],
            &[],
        )
        .await;

        let conflict = result.expect("finish").expect("autostash conflict");
        assert_eq!(
            conflict,
            AutostashConflict {
                marker_files: vec!["dummy.txt".to_string()],
                kept_local_files: vec![
                    "a.bin".to_string(),
                    "a.uasset".to_string(),
                    "du.txt".to_string()
                ],
                kept_deleted_files: vec!["ud.txt".to_string()],
                unresolved_files: vec![],
                snapshot_saved: true,
                autostash_dropped: true,
            }
        );
        assert_mixed_local_edits_kept(&repo);
        let dummy = std::fs::read_to_string(repo.join("dummy.txt")).unwrap();
        assert!(
            dummy.contains("<<<<<<<") && dummy.contains("collab") && dummy.contains("mine"),
            "{dummy}"
        );
        assert_eq!(
            stash_entries(&repo),
            vec![format!("{} snapshot pre-pull", snapshot.commit)]
        );
        assert_eq!(
            conflict.to_string(),
            "Synced, but some files changed both locally and on origin. Resolve the conflict markers in dummy.txt before submitting. Kept your local version of a.bin, a.uasset, du.txt, ud.txt (deleted); submitting will overwrite origin's changes to them. Your local versions are saved in the 'pre-pull' snapshot."
        );
    }

    #[tokio::test]
    async fn test_settle_autostash_conflicts_keeps_autostash_when_unresolved() {
        let (git, _dir, _origin, _collab) =
            setup_feat_with_collab_commit("dummy.txt", "line1\ncollab\nline3\n");
        let repo = git.repo_path.clone();
        std::fs::write(repo.join("dummy.txt"), "line1\nmine\nline3\n").unwrap();
        let snapshot = git
            .save_snapshot("pre-pull", vec!["dummy.txt".to_string()])
            .await
            .unwrap();
        let stash_before = git.stash_top().await.unwrap();
        git.pull(PullStrategy::Rebase, PullStashStrategy::Autostash)
            .await
            .unwrap();
        // Neither side's version and no markers: not a state settling recognizes.
        std::fs::write(repo.join("dummy.txt"), "edited\n").unwrap();

        let conflict = git
            .settle_autostash_conflicts(Some(&snapshot.commit), stash_before.as_deref())
            .await
            .unwrap()
            .expect("autostash conflict");

        assert_eq!(conflict.unresolved_files, vec!["dummy.txt".to_string()]);
        assert!(!conflict.autostash_dropped);
        assert!(conflict.to_string().contains("Could not merge dummy.txt"));
        let entries = stash_entries(&repo);
        assert_eq!(entries.len(), 2, "{entries:?}");
        assert!(entries[0].ends_with(" autostash"), "{entries:?}");
        assert_eq!(
            std::fs::read_to_string(repo.join("dummy.txt")).unwrap(),
            "edited\n"
        );
    }

    #[tokio::test]
    async fn test_finish_autostash_pull_falls_back_to_snapshot_restore() {
        let (git, _dir, _origin, _collab) =
            setup_feat_with_collab_commit("dummy.txt", "line1\ncollab\nline3\n");
        let repo = git.repo_path.clone();
        std::fs::write(repo.join("dummy.txt"), "line1\nmine\nline3\n").unwrap();
        let snapshot = git
            .save_snapshot("pre-pull", vec!["dummy.txt".to_string()])
            .await
            .unwrap();
        let stash_before = git.stash_top().await.unwrap();
        git.pull(PullStrategy::Rebase, PullStashStrategy::Autostash)
            .await
            .unwrap();

        // A held index lock makes the reset fail.
        let lock = repo.join(".git").join("index.lock");
        std::fs::write(&lock, "").unwrap();
        let settled = git
            .settle_autostash_conflicts(Some(&snapshot.commit), stash_before.as_deref())
            .await;
        assert!(settled.is_err(), "{settled:?}");

        let mut no_snapshot_restore = false;
        let result = git
            .finish_autostash_pull(None, stash_before.as_deref(), vec![], || {
                no_snapshot_restore = true
            })
            .await;
        assert!(!no_snapshot_restore);
        assert!(result.is_err(), "{result:?}");

        // The restore waits for the index lock, so release it when the restore starts.
        let mut restore_started = false;
        let result = git
            .finish_autostash_pull(
                Some(&snapshot),
                stash_before.as_deref(),
                modified(&["dummy.txt"]),
                || {
                    restore_started = true;
                    std::fs::remove_file(&lock).unwrap();
                },
            )
            .await;
        assert!(restore_started, "the snapshot restore must run: {result:?}");
        assert!(!lock.exists());
    }

    #[test]
    fn test_parse_unmerged_entries() {
        let output = concat!(
            "100644 aaaa 1\ta b.txt\x00100644 bbbb 2\ta b.txt\x00100644 cccc 3\ta b.txt\x00",
            "100644 dddd 1\tud.txt\x00100644 eeee 2\tud.txt\x00",
            "100644 ffff 1\tdu.txt\x00100644 9999 3\tdu.txt\x00"
        );
        let entries = parse_unmerged_entries(output);
        let s = |v: &str| Some(v.to_string());
        assert_eq!(
            entries,
            vec![
                UnmergedEntry {
                    path: "a b.txt".to_string(),
                    stages: [s("aaaa"), s("bbbb"), s("cccc")],
                },
                UnmergedEntry {
                    path: "ud.txt".to_string(),
                    stages: [s("dddd"), s("eeee"), None],
                },
                UnmergedEntry {
                    path: "du.txt".to_string(),
                    stages: [s("ffff"), None, s("9999")],
                },
            ]
        );
        assert!(parse_unmerged_entries("").is_empty());
    }

    #[test]
    fn test_autostash_conflict_message() {
        let conflict = AutostashConflict {
            marker_files: vec!["a.txt".to_string(), "b.txt".to_string()],
            snapshot_saved: true,
            autostash_dropped: true,
            ..Default::default()
        };
        assert_eq!(
            conflict.to_string(),
            "Synced, but some files changed both locally and on origin. Resolve the conflict markers in a.txt, b.txt before submitting. Your local versions are saved in the 'pre-pull' snapshot."
        );
        let conflict = AutostashConflict {
            kept_local_files: vec!["a.uasset".to_string()],
            kept_deleted_files: vec!["b.txt".to_string()],
            unresolved_files: vec!["c.txt".to_string()],
            ..Default::default()
        };
        assert_eq!(
            conflict.details(),
            "some files changed both locally and on origin. Kept your local version of a.uasset, b.txt (deleted); submitting will overwrite origin's changes to them. Could not merge c.txt; check them before submitting. Your local versions are saved in git's 'autostash' stash entry."
        );
    }

    const OID40: &str = "0de5581443506c8d04fe12dacf3f30ebce63da91";

    fn merge_tree_stdout(records: &[&str]) -> Vec<u8> {
        let mut out = Vec::new();
        for r in records {
            out.extend_from_slice(r.as_bytes());
            out.push(b'\0');
        }
        out
    }

    #[test]
    fn test_merge_tree_clean() {
        let stdout = merge_tree_stdout(&[OID40]);
        assert_eq!(
            parse_merge_tree_output(Some(0), &stdout, "").unwrap(),
            MergeTreeOutcome::Clean
        );
    }

    #[test]
    fn test_merge_tree_conflicts_in_order() {
        let stdout = merge_tree_stdout(&[OID40, "dir/a b.uasset", "Content/Ünï.umap"]);
        assert_eq!(
            parse_merge_tree_output(Some(1), &stdout, "").unwrap(),
            MergeTreeOutcome::Conflicts(vec![
                "dir/a b.uasset".to_string(),
                "Content/Ünï.umap".to_string()
            ])
        );
    }

    #[test]
    fn test_merge_tree_missing_ref_is_error() {
        let stderr = "merge-tree: origin/x - not something we can merge";
        let err = parse_merge_tree_output(Some(1), b"", stderr).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("did not return a tree OID"), "{msg}");
        assert!(msg.contains(stderr), "{msg}");
    }

    #[test]
    fn test_merge_tree_rejects_bad_output() {
        // exit 1 with non-OID text
        assert!(parse_merge_tree_output(Some(1), b"fatal: something\0", "").is_err());
        // exit 1 with an OID but no paths
        let err = parse_merge_tree_output(Some(1), &merge_tree_stdout(&[OID40]), "").unwrap_err();
        assert!(err.to_string().contains("listed no paths"));
        // exit 128 with an OID
        let err =
            parse_merge_tree_output(Some(128), &merge_tree_stdout(&[OID40]), "boom").unwrap_err();
        assert!(err.to_string().contains("exit 128"));
        // exit 0 without an OID
        assert!(parse_merge_tree_output(Some(0), b"", "").is_err());
        // killed by a signal
        let err =
            parse_merge_tree_output(None, &merge_tree_stdout(&[OID40, "a.txt"]), "").unwrap_err();
        assert!(err.to_string().contains("exit signal"));
        // uppercase hex is not an OID
        let upper = OID40.to_uppercase();
        assert!(parse_merge_tree_output(Some(0), &merge_tree_stdout(&[&upper]), "").is_err());
    }

    #[test]
    fn test_merge_tree_oid_forms() {
        let oid64 = "a".repeat(64);
        assert_eq!(
            parse_merge_tree_output(Some(0), &merge_tree_stdout(&[&oid64]), "").unwrap(),
            MergeTreeOutcome::Clean
        );
        // no trailing NUL
        assert_eq!(
            parse_merge_tree_output(Some(1), format!("{OID40}\0a.txt").as_bytes(), "").unwrap(),
            MergeTreeOutcome::Conflicts(vec!["a.txt".to_string()])
        );
        // paths stop at the first empty record
        assert_eq!(
            parse_merge_tree_output(Some(1), format!("{OID40}\0a.txt\0\0b.txt\0").as_bytes(), "")
                .unwrap(),
            MergeTreeOutcome::Conflicts(vec!["a.txt".to_string()])
        );
    }

    #[test]
    fn test_parse_git_version() {
        assert_eq!(
            parse_git_version("git version 2.47.1.windows.1\n"),
            Some((2, 47))
        );
        assert_eq!(
            parse_git_version("git version 2.39.3 (Apple Git-146)"),
            Some((2, 39))
        );
        assert_eq!(parse_git_version("garbage"), None);
        assert_eq!(parse_git_version("git version abc"), None);
        assert_eq!(parse_git_version(""), None);
    }

    #[test]
    fn test_git_version_check_from_output() {
        assert!(version_check_from_output("git version 2.38.0").supported);
        assert!(!version_check_from_output("git version 2.37.9").supported);
        assert!(version_check_from_output("git version 2.100.0").supported);
        assert!(version_check_from_output("git version 3.0.0").supported);

        let check = version_check_from_output("git version 2.47.1.windows.1\n");
        assert_eq!(check.version, "2.47.1.windows.1");
        assert!(check.supported);

        let check = version_check_from_output("  git version abc \n");
        assert!(!check.supported);
        assert_eq!(check.version, "git version abc");

        let check = version_check_from_output("garbage\n");
        assert!(!check.supported);
        assert_eq!(check.version, "garbage");
    }

    #[test]
    fn test_parse_last_touch() {
        let output = concat!(
            "\x1eaaaaaaaa1111\x1fAlice\x1f2026-10-22T10:00:00-07:00\n\nContent/a b.uasset\nshared.txt\n\n",
            "\x1ebbbbbbbb2222\x1fBob\x1f2026-10-21T10:00:00-07:00\n",
            "\x1ecccc\x1fCarol\x1f2026-10-20T10:00:00-07:00\n\nshared.txt\nold.txt\n",
            "\x1ebroken\x1fDan\n\nignored.txt\n"
        );
        let touches = parse_last_touch(output);
        assert_eq!(touches.len(), 3);
        assert_eq!(
            touches["shared.txt"],
            FileTouch {
                sha: "aaaaaaaa".to_string(),
                author: "Alice".to_string(),
                date: "2026-10-22T10:00:00-07:00".to_string(),
            }
        );
        assert_eq!(touches["Content/a b.uasset"].sha, "aaaaaaaa");
        assert_eq!(touches["old.txt"].sha, "cccc");
        assert_eq!(touches["old.txt"].author, "Carol");
        assert!(!touches.contains_key("ignored.txt"));
        assert!(parse_last_touch("").is_empty());
    }

    /// Writes `contents` to `path` and commits it on the current branch.
    fn commit_bytes(dir: &std::path::Path, path: &str, contents: &[u8], msg: &str) -> String {
        std::fs::write(dir.join(path), contents).unwrap();
        git_ok(dir, &["add", "--", path]);
        git_ok(dir, &["commit", "-m", msg]);
        git_ok(dir, &["rev-parse", "HEAD"])
    }

    /// Builds origin/main and origin/x from a common base: `x_files` are committed on x,
    /// `main_files` on main, both pushed and fetched. HEAD is left on the original branch.
    fn setup_diverged_origin(
        main_files: &[(&str, &[u8])],
        x_files: &[(&str, &[u8])],
    ) -> (Git, TempDir, TempDir) {
        let (git, dir, origin) = setup_repo_with_bare_origin();
        let path = git.repo_path.clone();
        git_ok(&path, &["switch", "-c", "x"]);
        for (file, bytes) in x_files {
            commit_bytes(&path, file, bytes, &format!("x {file}"));
        }
        git_ok(&path, &["push", "origin", "HEAD:refs/heads/x"]);
        git_ok(&path, &["switch", "-"]);
        for (file, bytes) in main_files {
            commit_bytes(&path, file, bytes, &format!("main {file}"));
        }
        git_ok(&path, &["push", "origin", "HEAD:refs/heads/main"]);
        git_ok(&path, &["fetch", "origin"]);
        (git, dir, origin)
    }

    /// Working tree/index status, HEAD and every ref, to prove a call changed none of them.
    fn repo_snapshot(dir: &std::path::Path) -> (String, String, String, String) {
        (
            git_ok(dir, &["status", "--porcelain", "--untracked-files=all"]),
            git_ok(dir, &["rev-parse", "HEAD"]),
            git_ok(dir, &["symbolic-ref", "HEAD"]),
            git_ok(dir, &["for-each-ref"]),
        )
    }

    async fn git_supported(git: &Git) -> bool {
        let check = git.check_min_version().await.expect("git version");
        if !check.supported {
            println!("skipped: git {} is older than 2.38", check.version);
        }
        check.supported
    }

    #[tokio::test]
    async fn test_merge_tree_real_clean() {
        let (git, dir, _origin) =
            setup_diverged_origin(&[("m.txt", b"main")], &[("x.txt", b"branch")]);
        if !git_supported(&git).await {
            return;
        }
        let before = repo_snapshot(dir.path());
        let outcome = git
            .predict_merge_conflicts("refs/remotes/origin/main", "refs/remotes/origin/x")
            .await
            .unwrap();
        assert_eq!(outcome, MergeTreeOutcome::Clean);
        assert_eq!(repo_snapshot(dir.path()), before);
    }

    #[tokio::test]
    async fn test_merge_tree_real_conflict() {
        let (git, dir, _origin) = setup_diverged_origin(
            &[("seed.txt", b"main side"), ("m.txt", b"main")],
            &[("seed.txt", b"branch side"), ("x.txt", b"branch")],
        );
        if !git_supported(&git).await {
            return;
        }
        let before = repo_snapshot(dir.path());
        let outcome = git
            .predict_merge_conflicts("refs/remotes/origin/main", "refs/remotes/origin/x")
            .await
            .unwrap();
        assert_eq!(
            outcome,
            MergeTreeOutcome::Conflicts(vec!["seed.txt".to_string()])
        );
        assert_eq!(repo_snapshot(dir.path()), before);
    }

    #[tokio::test]
    async fn test_merge_tree_real_binary_both_sided() {
        let (git, dir, _origin) = setup_diverged_origin(
            &[("a.uasset", b"main\0bytes")],
            &[("a.uasset", b"branch\0bytes"), ("x.txt", b"branch")],
        );
        if !git_supported(&git).await {
            return;
        }
        let before = repo_snapshot(dir.path());
        let outcome = git
            .predict_merge_conflicts("refs/remotes/origin/main", "refs/remotes/origin/x")
            .await
            .unwrap();
        match outcome {
            MergeTreeOutcome::Conflicts(paths) => {
                assert!(paths.contains(&"a.uasset".to_string()), "{paths:?}")
            }
            other => panic!("expected conflicts, got {other:?}"),
        }
        assert_eq!(repo_snapshot(dir.path()), before);
    }

    #[tokio::test]
    async fn test_merge_tree_real_missing_ref() {
        let (git, dir, _origin) =
            setup_diverged_origin(&[("m.txt", b"main")], &[("x.txt", b"branch")]);
        if !git_supported(&git).await {
            return;
        }
        let before = repo_snapshot(dir.path());
        let err = git
            .predict_merge_conflicts("refs/remotes/origin/main", "refs/remotes/origin/nope")
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("not something we can merge"),
            "{err}"
        );
        assert_eq!(repo_snapshot(dir.path()), before);

        assert!(git
            .predict_merge_conflicts("--output=x", "refs/remotes/origin/x")
            .await
            .is_err());
        assert!(git
            .predict_merge_conflicts("refs/remotes/origin/main", "-x")
            .await
            .is_err());
    }

    #[tokio::test]
    async fn test_last_touch_by_path_real() {
        let (git, dir) = setup_repo();
        let path = dir.path();
        let base = git_ok(path, &["rev-parse", "HEAD"]);
        let first = commit_bytes(path, "a.txt", b"one", "first");
        commit_bytes(path, "b.txt", b"b", "second");
        let third = commit_bytes(path, "a.txt", b"two", "third");

        let range = format!("{base}..HEAD");
        let touches = git.last_touch_by_path(&range, 2000).await.unwrap();
        assert_eq!(touches.len(), 2);
        assert_eq!(touches["a.txt"].sha, third[..8]);
        assert_ne!(touches["a.txt"].sha, first[..8]);
        assert_eq!(touches["a.txt"].author, "test");
        assert!(!touches["a.txt"].date.is_empty());

        let newest_only = git.last_touch_by_path(&range, 1).await.unwrap();
        assert_eq!(newest_only.keys().collect::<Vec<_>>(), vec!["a.txt"]);

        assert!(git.last_touch_by_path("--all", 10).await.is_err());
    }

    #[tokio::test]
    async fn test_check_min_version_real() {
        let (git, _dir) = setup_repo();
        let check = git.check_min_version().await.unwrap();
        assert!(!check.version.is_empty());
        assert!(check.supported, "git {} is unsupported", check.version);
        // cached
        assert_eq!(git.check_min_version().await.unwrap(), check);
    }
}
