use std::collections::{HashMap, HashSet};
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context};
use lazy_static::lazy_static;
use serde::{Deserialize, Serialize};
use tracing::warn;

use super::is_valid_branch_name;
use crate::engine::{CommunicationType, EngineProvider};
use crate::repo::operations::gh::submit::filter_to_textlike_paths;
use ethos_core::clients::git;
use ethos_core::types::repo::{
    ConflictFile, ConflictKind, ConflictSideName, DroppedCommit, RebaseCommitInfo, RebaseKeep,
    RebaseStaleReason, RebaseState, RebaseStateKind,
};

const REBASE_FILE: &str = "f11r-rebase.json";
const REBASE_FILE_TMP: &str = "f11r-rebase.json.tmp";
pub(crate) const REBASE_FILE_VERSION: u32 = 1;
const BACKUP_REF_PREFIX: &str = "refs/f11r-backup/";
const BACKUP_RETENTION_SECS: u64 = 14 * 86400;
const ZERO_SHA: &str = "0000000000000000000000000000000000000000";
const TEXT_SNIFF_BYTES: u64 = 8000;
const LOCALE_ENV: &[(&str, &str)] = &[("LC_ALL", "C")];
const COMMIT_INFO_FORMAT: &str = "--format=%H%x1f%an%x1f%s";

const BOTH_ADDED: &str = "Both main and the branch added this file.";
const RENAMED_DIFFERENTLY: &str = "This file was renamed differently on main and on the branch.";
const FILE_AND_FOLDER: &str = "A file and a folder with the same name conflict.";
const UNRESOLVABLE: &str = "This conflict can't be resolved in Friendshipper.";

/// Project and plugin `Source/` trees plus module and target rules files. Pathspec magic: never
/// pass them to a git call with `--literal-pathspecs`.
pub(crate) const CPP_PATHSPECS: [&str; 3] = [
    ":(glob)**/Source/**",
    ":(glob)**/*.Build.cs",
    ":(glob)**/*.Target.cs",
];

lazy_static! {
    /// Held for every read-modify-write of `f11r-rebase.json`.
    pub(crate) static ref REBASE_FILE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::new(());
    /// Held by every queued rebase op for its whole run; taken before `REBASE_FILE_LOCK`.
    pub(crate) static ref REBASE_OP_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::new(());
}

/// Read-only git calls that never take `index.lock`.
fn read_opts<'a>() -> git::Opts<'a> {
    git::Opts::new_without_logs()
        .with_skip_notify_frontend()
        .with_env(LOCALE_ENV)
}

pub(crate) async fn read_git(git: &git::Git, args: &[&str]) -> anyhow::Result<git::GitRunOutput> {
    let mut full_args = Vec::with_capacity(args.len() + 1);
    full_args.push("--no-optional-locks");
    full_args.extend_from_slice(args);
    git.run_with_status(&full_args, read_opts()).await
}

/// The per-worktree git dir: `.git` itself, or the target of a `gitdir:` file.
pub(crate) fn resolve_git_dir(repo_path: &Path) -> PathBuf {
    try_resolve_git_dir(repo_path).unwrap_or_else(|_| repo_path.join(".git"))
}

/// [`resolve_git_dir`], failing when `.git` is a file that can't be read or names no git dir.
pub(crate) fn try_resolve_git_dir(repo_path: &Path) -> anyhow::Result<PathBuf> {
    let dot_git = repo_path.join(".git");
    if !dot_git.is_file() {
        return Ok(dot_git);
    }
    let content = std::fs::read_to_string(&dot_git)
        .with_context(|| format!("Could not read {}", dot_git.display()))?;
    match content.trim().strip_prefix("gitdir:").map(str::trim) {
        Some(target) if !target.is_empty() => Ok(repo_path.join(target)),
        _ => bail!("{} does not name a git dir", dot_git.display()),
    }
}

pub(crate) fn json_path(git_dir: &Path) -> PathBuf {
    git_dir.join(REBASE_FILE)
}

fn tmp_path(git_dir: &Path) -> PathBuf {
    git_dir.join(REBASE_FILE_TMP)
}

/// Whether git has a rebase of either backend in progress.
pub(crate) fn rebase_dir_exists(git_dir: &Path) -> bool {
    git_dir.join("rebase-merge").is_dir() || git_dir.join("rebase-apply").is_dir()
}

fn read_trimmed(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
}

fn read_number(path: &Path) -> u32 {
    read_trimmed(path).and_then(|s| s.parse().ok()).unwrap_or(0)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GuidedRebasePhase {
    Rebasing,
    PushFailed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct OriginalCommit {
    pub sha: String,
    pub subject: String,
    pub author_email: String,
    /// `--date=raw`, e.g. `1761000000 +0200`.
    pub author_date: String,
}

/// The conflicts of the commit git stopped on, cached so resolutions survive a reload.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CurrentStep {
    pub sha: String,
    pub conflicts: Vec<ConflictFile>,
}

/// `<git-dir>/f11r-rebase.json`: what Friendshipper knows about a rebase it started.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GuidedRebaseFile {
    pub version: u32,
    pub branch: String,
    pub trunk: String,
    pub phase: GuidedRebasePhase,
    pub backup_ref: String,
    pub backup_sha: String,
    pub pre_rebase_origin_sha: String,
    pub onto: String,
    pub merge_base: String,
    pub original_commits: Vec<OriginalCommit>,
    pub touches_cpp: bool,
    pub engine_association: String,
    /// RFC 3339.
    pub started_at: String,
    pub push_failed_remote_sha: Option<String>,
    pub current_step: Option<CurrentStep>,
}

/// A full SHA-1 or SHA-256 object name, as git prints it.
pub(crate) fn is_full_sha(s: &str) -> bool {
    (s.len() == 40 || s.len() == 64) && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

impl GuidedRebaseFile {
    /// Everything later passed to git or used as a ref is a valid name or a full sha.
    fn is_well_formed(&self) -> bool {
        let backup_prefix = format!("{BACKUP_REF_PREFIX}{}/", self.branch);
        is_valid_branch_name(&self.branch)
            && is_valid_branch_name(&self.trunk)
            && self.backup_ref.starts_with(&backup_prefix)
            && [
                &self.backup_sha,
                &self.pre_rebase_origin_sha,
                &self.onto,
                &self.merge_base,
            ]
            .iter()
            .all(|sha| is_full_sha(sha))
            && self
                .push_failed_remote_sha
                .as_deref()
                .is_none_or(is_full_sha)
            && self
                .current_step
                .as_ref()
                .is_none_or(|step| is_full_sha(&step.sha))
            && self
                .original_commits
                .iter()
                .all(|commit| is_full_sha(&commit.sha))
    }
}

#[derive(Debug)]
pub(crate) enum RebaseFileRead {
    Missing,
    /// The file exists but does not parse or validate.
    Unreadable(String),
    /// The file could not be read; its state is unknown.
    Failed(std::io::Error),
    Ok(Box<GuidedRebaseFile>),
}

pub(crate) fn read_rebase_file(git_dir: &Path) -> RebaseFileRead {
    let content = match std::fs::read(json_path(git_dir)) {
        Ok(content) => content,
        Err(e) if e.kind() == ErrorKind::NotFound => return RebaseFileRead::Missing,
        Err(e) => return RebaseFileRead::Failed(e),
    };
    match serde_json::from_slice::<GuidedRebaseFile>(&content) {
        Ok(file) if file.version != REBASE_FILE_VERSION => {
            RebaseFileRead::Unreadable(format!("unsupported version {}", file.version))
        }
        Ok(file) if !file.is_well_formed() => {
            RebaseFileRead::Unreadable("invalid branch, ref or sha".to_string())
        }
        Ok(file) => RebaseFileRead::Ok(Box::new(file)),
        Err(e) => RebaseFileRead::Unreadable(e.to_string()),
    }
}

const RENAME_ATTEMPTS: u32 = 5;
const RENAME_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(50);

/// Replaces the file atomically: a crash leaves either the old or the new content.
pub(crate) fn write_rebase_file(git_dir: &Path, file: &GuidedRebaseFile) -> anyhow::Result<()> {
    let json = serde_json::to_vec_pretty(file)?;
    let tmp = tmp_path(git_dir);
    {
        let mut out = std::fs::File::create(&tmp)
            .with_context(|| format!("Could not create {}", tmp.display()))?;
        out.write_all(&json)?;
        out.sync_all()?;
    }
    // Windows refuses the rename while another process (often a virus scanner) has the
    // target open; retry briefly, like git does.
    let target = json_path(git_dir);
    let mut attempt = 1;
    loop {
        match std::fs::rename(&tmp, &target) {
            Ok(()) => return Ok(()),
            Err(e) if e.kind() == ErrorKind::PermissionDenied && attempt < RENAME_ATTEMPTS => {
                attempt += 1;
                std::thread::sleep(RENAME_RETRY_DELAY);
            }
            Err(e) => {
                return Err(anyhow!("Could not replace {}: {e}", target.display()));
            }
        }
    }
}

pub(crate) fn delete_rebase_file(git_dir: &Path) -> anyhow::Result<()> {
    match std::fs::remove_file(json_path(git_dir)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
        Err(e) => Err(anyhow!(
            "Could not delete {}: {e}",
            json_path(git_dir).display()
        )),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DerivedState {
    pub kind: RebaseStateKind,
    pub stale_reason: Option<RebaseStaleReason>,
    pub file: Option<GuidedRebaseFile>,
}

/// States the guard refuses other operations in.
pub(crate) fn is_active(kind: RebaseStateKind) -> bool {
    matches!(
        kind,
        RebaseStateKind::InProgress
            | RebaseStateKind::CompletedUnpushed
            | RebaseStateKind::PushFailed
    )
}

/// Derives the guided rebase state from the json and git's own rebase state. Read-only: no
/// command takes `index.lock`. Fails when a file cannot be read or git cannot be run, so a
/// failure is never mistaken for an inactive state.
pub(crate) async fn derive_rebase_state(
    git: &git::Git,
    repo_path: &Path,
) -> anyhow::Result<DerivedState> {
    let git_dir = resolve_git_dir(repo_path);
    let rebase_dir = rebase_dir_exists(&git_dir);
    let file = match read_rebase_file(&git_dir) {
        RebaseFileRead::Missing => {
            let kind = if rebase_dir {
                RebaseStateKind::External
            } else {
                RebaseStateKind::Idle
            };
            return Ok(DerivedState {
                kind,
                stale_reason: None,
                file: None,
            });
        }
        RebaseFileRead::Unreadable(e) => {
            warn!("Guided rebase record is unreadable: {e}");
            let kind = if rebase_dir {
                RebaseStateKind::External
            } else {
                RebaseStateKind::Stale
            };
            return Ok(DerivedState {
                kind,
                stale_reason: Some(RebaseStaleReason::Unreadable),
                file: None,
            });
        }
        RebaseFileRead::Failed(e) => {
            bail!("Could not read {}: {e}", json_path(&git_dir).display())
        }
        RebaseFileRead::Ok(file) => *file,
    };

    let (kind, stale_reason) = derive_from_file(git, &git_dir, &file)
        .await
        .context("Could not determine the guided rebase state")?;
    Ok(DerivedState {
        kind,
        stale_reason,
        file: Some(file),
    })
}

/// A file's trimmed content, `None` if it does not exist.
fn read_existing(path: &Path) -> anyhow::Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(content) => Ok(Some(content.trim().to_string())),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(anyhow!("Could not read {}: {e}", path.display())),
    }
}

/// Whether `ancestor` is reachable from `rev`; a missing object is not an ancestor.
pub(crate) async fn is_ancestor(git: &git::Git, ancestor: &str, rev: &str) -> anyhow::Result<bool> {
    let output = read_git(git, &["merge-base", "--is-ancestor", ancestor, rev]).await?;
    Ok(output.exit_code == 0)
}

fn stale(reason: RebaseStaleReason) -> (RebaseStateKind, Option<RebaseStaleReason>) {
    (RebaseStateKind::Stale, Some(reason))
}

/// Rows 4-11 of the derivation table, in order, with the backup check before row 9.
async fn derive_from_file(
    git: &git::Git,
    git_dir: &Path,
    file: &GuidedRebaseFile,
) -> anyhow::Result<(RebaseStateKind, Option<RebaseStaleReason>)> {
    let branch_ref = format!("refs/heads/{}", file.branch);
    let merge_dir = git_dir.join("rebase-merge");
    if merge_dir.is_dir()
        && read_existing(&merge_dir.join("head-name"))?.as_deref() == Some(branch_ref.as_str())
        && read_existing(&merge_dir.join("onto"))?.as_deref() == Some(file.onto.as_str())
    {
        return Ok((RebaseStateKind::InProgress, None));
    }
    if rebase_dir_exists(git_dir) {
        return Ok(stale(RebaseStaleReason::OtherRebase));
    }

    let head_ref = read_git(git, &["symbolic-ref", "-q", "HEAD"]).await?;
    match head_ref.exit_code {
        0 if head_ref.stdout.trim() == branch_ref => {}
        // 1 is a detached HEAD
        0 | 1 => return Ok(stale(RebaseStaleReason::BranchChanged)),
        code => bail!(
            "symbolic-ref exited with {code}: {}",
            head_ref.stderr.trim()
        ),
    }

    let head = rev_parse(git, "HEAD")
        .await?
        .ok_or_else(|| anyhow!("HEAD does not resolve"))?;
    if head == file.backup_sha {
        return Ok(stale(RebaseStaleReason::AbortedOutside));
    }

    let origin_ref = format!("refs/remotes/origin/{}", file.branch);
    if rev_parse(git, &origin_ref).await?.as_deref() == Some(head.as_str()) {
        return Ok(stale(RebaseStaleReason::PushedOutside));
    }

    // A rebase rewrites every branch commit, so HEAD contains the backup only when the branch
    // had no commits of its own (the backup is then part of `onto`) or the original branch
    // came back, e.g. aborted outside and then merged with main.
    if is_ancestor(git, &file.backup_sha, "HEAD").await?
        && !is_ancestor(git, &file.backup_sha, &file.onto).await?
    {
        return Ok(stale(RebaseStaleReason::Unknown));
    }

    if !is_ancestor(git, &file.onto, "HEAD").await? {
        return Ok(stale(RebaseStaleReason::Unknown));
    }

    Ok(match file.phase {
        GuidedRebasePhase::PushFailed => (RebaseStateKind::PushFailed, None),
        GuidedRebasePhase::Rebasing => (RebaseStateKind::CompletedUnpushed, None),
    })
}

/// The sha `rev` names, or `None` when it does not exist.
pub(crate) async fn rev_parse(git: &git::Git, rev: &str) -> anyhow::Result<Option<String>> {
    let output = read_git(git, &["rev-parse", "-q", "--verify", rev]).await?;
    match output.exit_code {
        0 => Ok(Some(output.stdout.trim().to_string())),
        1 => Ok(None),
        code => bail!(
            "rev-parse {rev} exited with {code}: {}",
            output.stderr.trim()
        ),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UnmergedEntry {
    pub xy: String,
    /// `N...` unless the path is a submodule.
    pub sub: String,
    /// Repo-relative, `/` separators.
    pub path: String,
}

impl UnmergedEntry {
    fn is_submodule(&self) -> bool {
        self.sub != "N..."
    }
}

/// Unmerged (`u`) records of `git status --porcelain=v2 -z`.
pub(crate) fn parse_unmerged(porcelain_z: &str) -> Vec<UnmergedEntry> {
    let mut entries = vec![];
    let mut records = porcelain_z.split('\0');
    while let Some(record) = records.next() {
        if record.starts_with("u ") {
            let fields: Vec<&str> = record.splitn(11, ' ').collect();
            if fields.len() == 11 {
                entries.push(UnmergedEntry {
                    xy: fields[1].to_string(),
                    sub: fields[2].to_string(),
                    path: fields[10].to_string(),
                });
            }
        } else if record.starts_with("2 ") {
            // A rename or copy is followed by its original path.
            records.next();
        }
    }
    entries
}

/// Untracked (`?`) paths of `git status --porcelain=v2 -z`.
#[allow(dead_code)] // TODO: Stage 011
pub(crate) fn parse_untracked(porcelain_z: &str) -> Vec<String> {
    let mut paths = vec![];
    let mut records = porcelain_z.split('\0');
    while let Some(record) = records.next() {
        if let Some(path) = record.strip_prefix("? ") {
            paths.push(path.to_string());
        } else if record.starts_with("2 ") {
            records.next();
        }
    }
    paths
}

/// Maps an unmerged XY code (ours = main, theirs = the replayed branch commit). An unknown
/// code is never resolvable.
fn classify_xy(xy: &str) -> (ConflictKind, Option<ConflictSideName>, Option<String>) {
    match xy {
        "UU" => (ConflictKind::Content, None, None),
        "DU" => (
            ConflictKind::ModifyDelete,
            Some(ConflictSideName::Main),
            None,
        ),
        "UD" => (
            ConflictKind::ModifyDelete,
            Some(ConflictSideName::Branch),
            None,
        ),
        "AA" => (
            ConflictKind::Unsupported,
            None,
            Some(BOTH_ADDED.to_string()),
        ),
        "AU" | "UA" | "DD" => (
            ConflictKind::Unsupported,
            None,
            Some(RENAMED_DIFFERENTLY.to_string()),
        ),
        _ => (
            ConflictKind::Unsupported,
            None,
            Some(UNRESOLVABLE.to_string()),
        ),
    }
}

/// Like `classify_xy`, but a submodule conflict is never resolvable.
fn classify_entry(
    entry: &UnmergedEntry,
) -> (ConflictKind, Option<ConflictSideName>, Option<String>) {
    if entry.is_submodule() {
        (
            ConflictKind::Unsupported,
            None,
            Some(UNRESOLVABLE.to_string()),
        )
    } else {
        classify_xy(&entry.xy)
    }
}

/// Absolute path of a repo-relative `/`-separated path, with native separators.
pub(crate) fn repo_file_path(repo_path: &Path, path: &str) -> PathBuf {
    let mut buf = repo_path.to_path_buf();
    for component in path.split('/') {
        buf.push(component);
    }
    buf
}

/// Whether the file exists and its first bytes contain no NUL.
fn starts_as_text(path: &Path) -> bool {
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    let mut head = Vec::new();
    if file.take(TEXT_SNIFF_BYTES).read_to_end(&mut head).is_err() {
        return false;
    }
    !head.contains(&0)
}

fn last_segment(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_string()
}

/// Parses a `%H%x1f%an%x1f%s` line.
fn parse_commit_info(line: &str) -> Option<RebaseCommitInfo> {
    let mut fields = line.trim_end_matches(['\r', '\n']).splitn(3, '\u{1f}');
    let sha = fields.next()?;
    if sha.is_empty() {
        return None;
    }
    Some(RebaseCommitInfo {
        sha: sha.to_string(),
        author: fields.next()?.to_string(),
        subject: fields.next().unwrap_or_default().to_string(),
    })
}

async fn commit_info(git: &git::Git, sha: &str) -> anyhow::Result<RebaseCommitInfo> {
    let output = read_git(git, &["log", "-1", COMMIT_INFO_FORMAT, sha, "--"]).await?;
    if output.exit_code != 0 {
        bail!("Could not read commit {sha}: {}", output.stderr.trim());
    }
    parse_commit_info(output.stdout.trim()).ok_or_else(|| anyhow!("Could not read commit {sha}"))
}

/// The latest commit on the main side (since `merge_base`) that touched `path`.
async fn last_change(
    git: &git::Git,
    merge_base: &str,
    path: &str,
) -> anyhow::Result<Option<RebaseCommitInfo>> {
    let range = format!("{merge_base}..HEAD");
    let output = read_git(
        git,
        &[
            "--literal-pathspecs",
            "log",
            "-1",
            COMMIT_INFO_FORMAT,
            &range,
            "--",
            path,
        ],
    )
    .await?;
    if output.exit_code != 0 {
        bail!(
            "Could not read the history of {path}: {}",
            output.stderr.trim()
        );
    }
    Ok(parse_commit_info(output.stdout.trim()))
}

async fn exists_in_tree(git: &git::Git, rev: &str, path: &str) -> anyhow::Result<bool> {
    let object = format!("{rev}:{path}");
    Ok(read_git(git, &["cat-file", "-e", &object]).await?.exit_code == 0)
}

pub(crate) async fn status_porcelain(git: &git::Git) -> anyhow::Result<String> {
    let output = read_git(
        git,
        &["status", "--porcelain=v2", "-z", "--untracked-files=no"],
    )
    .await?;
    if output.exit_code != 0 {
        bail!("git status failed: {}", output.stderr.trim());
    }
    Ok(output.stdout)
}

/// Classifies every unmerged path of the step git stopped on at `rebase_head`.
pub(crate) async fn classify_conflicts<T: EngineProvider>(
    git: &git::Git,
    repo_path: &Path,
    engine: &T,
    engine_path: &Path,
    merge_base: &str,
    rebase_head: &str,
) -> anyhow::Result<Vec<ConflictFile>> {
    let unmerged = parse_unmerged(&status_porcelain(git).await?);
    if unmerged.is_empty() {
        return Ok(vec![]);
    }

    let branch_side = commit_info(git, rebase_head).await?;
    let paths: Vec<String> = unmerged.iter().map(|e| e.path.clone()).collect();
    let textlike: HashSet<String> = filter_to_textlike_paths(git, &paths)
        .await?
        .into_iter()
        .collect();
    let display_names = engine
        .get_asset_display_names(CommunicationType::OfflineFallback, engine_path, &paths)
        .await;

    let mut conflicts = Vec::with_capacity(unmerged.len());
    for (i, entry) in unmerged.iter().enumerate() {
        let (mut kind, mut deleted_on, mut unsupported_reason) = classify_entry(entry);
        // Git moves a file that collides with a folder aside to `<path>~<side>`, which exists
        // in neither tree; real modify/delete and rename paths exist on at least one side.
        if !entry.is_submodule()
            && matches!(entry.xy.as_str(), "UD" | "DU" | "AU" | "UA")
            && !exists_in_tree(git, "HEAD", &entry.path).await?
            && !exists_in_tree(git, rebase_head, &entry.path).await?
        {
            kind = ConflictKind::Unsupported;
            deleted_on = None;
            unsupported_reason = Some(FILE_AND_FOLDER.to_string());
        }

        let display_name = display_names
            .get(i)
            .filter(|name| !name.is_empty())
            .cloned()
            .unwrap_or_else(|| last_segment(&entry.path));

        conflicts.push(ConflictFile {
            path: entry.path.clone(),
            display_name,
            kind,
            deleted_on,
            unsupported_reason,
            is_text: textlike.contains(&entry.path)
                && starts_as_text(&repo_file_path(repo_path, &entry.path)),
            resolved: false,
            resolution: None,
            main_side: last_change(git, merge_base, &entry.path).await?,
            branch_side: branch_side.clone(),
        });
    }
    Ok(conflicts)
}

/// Recomputes `resolved` from the current unmerged paths; a file that is unmerged again
/// loses its resolution.
pub(crate) fn refresh_resolved(conflicts: &mut [ConflictFile], unmerged: &[UnmergedEntry]) {
    let unmerged: HashSet<&str> = unmerged.iter().map(|e| e.path.as_str()).collect();
    for conflict in conflicts {
        conflict.resolved = !unmerged.contains(conflict.path.as_str());
        if !conflict.resolved {
            conflict.resolution = None;
        }
    }
}

/// The sha of the commit git stopped on, if any.
pub(crate) fn rebase_head(git_dir: &Path) -> Option<String> {
    read_trimmed(&git_dir.join("REBASE_HEAD")).filter(|sha| is_full_sha(sha))
}

/// The json, if it still describes the rebase `file` describes. Call under `REBASE_FILE_LOCK`.
fn same_record(git_dir: &Path, file: &GuidedRebaseFile) -> anyhow::Result<GuidedRebaseFile> {
    match read_rebase_file(git_dir) {
        RebaseFileRead::Ok(current)
            if current.onto == file.onto && current.backup_sha == file.backup_sha =>
        {
            Ok(*current)
        }
        RebaseFileRead::Ok(_) => bail!("The rebase record now describes a different rebase"),
        other => bail!("The rebase record could not be read: {other:?}"),
    }
}

/// Stores `step` in the json; fails when the json now describes a different rebase.
async fn store_current_step(
    git_dir: &Path,
    file: &GuidedRebaseFile,
    step: CurrentStep,
) -> anyhow::Result<()> {
    let _lock = REBASE_FILE_LOCK.lock().await;
    let mut current = same_record(git_dir, file)?;
    current.current_step = Some(step);
    write_rebase_file(git_dir, &current)
}

/// Caches `step`, classified from `head` and `unmerged`, unless a rebase op holds `op_lock` or
/// git's state has moved since: a step classified while an op ran is never stored.
async fn cache_if_unchanged(
    git: &git::Git,
    git_dir: &Path,
    file: &GuidedRebaseFile,
    op_lock: &tokio::sync::Mutex<()>,
    step: CurrentStep,
    head: &str,
    unmerged: &[UnmergedEntry],
) {
    let Ok(_op) = op_lock.try_lock() else {
        return;
    };
    let unchanged = rebase_head(git_dir).as_deref() == Some(step.sha.as_str())
        && matches!(rev_parse(git, "HEAD").await, Ok(Some(now)) if now == head)
        && matches!(status_porcelain(git).await, Ok(status) if parse_unmerged(&status) == unmerged);
    if !unchanged {
        return;
    }
    let _lock = REBASE_FILE_LOCK.lock().await;
    let result = same_record(git_dir, file).and_then(|mut current| {
        // An op that finished meanwhile cached this step itself, maybe with resolutions.
        if current
            .current_step
            .as_ref()
            .is_some_and(|cached| cached.sha == step.sha && !cached.conflicts.is_empty())
        {
            return Ok(());
        }
        current.current_step = Some(step);
        write_rebase_file(git_dir, &current)
    });
    if let Err(e) = result {
        warn!("Could not cache the current rebase step: {e}");
    }
}

/// The cached step for `rebase_head`, classified and stored first when the cache is missing or
/// belongs to another step. `None` when nothing is unmerged and nothing is cached. Callers hold
/// `REBASE_OP_LOCK`, so nothing else writes the cache meanwhile.
pub(crate) async fn ensure_current_step<T: EngineProvider>(
    git: &git::Git,
    repo_path: &Path,
    engine: &T,
    engine_path: &Path,
    file: &GuidedRebaseFile,
    rebase_head: &str,
) -> anyhow::Result<Option<CurrentStep>> {
    let git_dir = resolve_git_dir(repo_path);
    let cached = {
        let _lock = REBASE_FILE_LOCK.lock().await;
        same_record(&git_dir, file)?
            .current_step
            .filter(|step| step.sha == rebase_head && !step.conflicts.is_empty())
    };
    if cached.is_some() {
        return Ok(cached);
    }

    let conflicts = classify_conflicts(
        git,
        repo_path,
        engine,
        engine_path,
        &file.merge_base,
        rebase_head,
    )
    .await?;
    if conflicts.is_empty() {
        return Ok(None);
    }
    let step = CurrentStep {
        sha: rebase_head.to_string(),
        conflicts,
    };
    store_current_step(&git_dir, file, step.clone()).await?;
    Ok(Some(step))
}

/// Sets the cached resolution of `path` in the step for `rebase_head`.
pub(crate) async fn set_resolution(
    git_dir: &Path,
    file: &GuidedRebaseFile,
    rebase_head: &str,
    path: &str,
    resolution: Option<RebaseKeep>,
) -> anyhow::Result<()> {
    let _lock = REBASE_FILE_LOCK.lock().await;
    let mut current = same_record(git_dir, file)?;
    let conflict = current
        .current_step
        .as_mut()
        .filter(|step| step.sha == rebase_head)
        .and_then(|step| step.conflicts.iter_mut().find(|c| c.path == path))
        .ok_or_else(|| anyhow!("{path} is not cached for the current step"))?;
    conflict.resolution = resolution;
    conflict.resolved = resolution.is_some();
    write_rebase_file(git_dir, &current)
}

/// Shas the rebase has not replayed yet: the `pick` lines of the todo list.
fn todo_picks(todo: &str) -> Vec<String> {
    todo.lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            match words.next()? {
                "pick" | "p" => words.next().map(str::to_string),
                _ => None,
            }
        })
        .collect()
}

/// `(authorEmail, authorDate, subject)` of every commit replayed onto `onto`.
async fn replayed_commits(
    git: &git::Git,
    onto: &str,
) -> anyhow::Result<Vec<(String, String, String)>> {
    let range = format!("{onto}..HEAD");
    let output = read_git(
        git,
        &[
            "log",
            "--date=raw",
            "--format=%ae%x1f%ad%x1f%s",
            &range,
            "--",
        ],
    )
    .await?;
    if output.exit_code != 0 {
        bail!(
            "Could not list the replayed commits: {}",
            output.stderr.trim()
        );
    }
    Ok(output
        .stdout
        .lines()
        .filter_map(|line| {
            let mut fields = line.splitn(3, '\u{1f}');
            Some((
                fields.next()?.to_string(),
                fields.next()?.to_string(),
                fields.next().unwrap_or_default().to_string(),
            ))
        })
        .collect())
}

/// Original commits that were processed but have no replayed counterpart, matched as a
/// multiset of `(authorEmail, authorDate, subject)`. Covers commits already on main and
/// commits made empty by a resolution, whatever the git version.
pub(crate) fn dropped_commits(
    original: &[OriginalCommit],
    remaining: &HashSet<String>,
    replayed: &[(String, String, String)],
) -> Vec<DroppedCommit> {
    let mut available: HashMap<(&str, &str, &str), usize> = HashMap::new();
    for (email, date, subject) in replayed {
        *available
            .entry((email.as_str(), date.as_str(), subject.as_str()))
            .or_default() += 1;
    }
    original
        .iter()
        .filter(|commit| !remaining.contains(&commit.sha))
        .filter(|commit| {
            let key = (
                commit.author_email.as_str(),
                commit.author_date.as_str(),
                commit.subject.as_str(),
            );
            match available.get_mut(&key) {
                Some(count) if *count > 0 => {
                    *count -= 1;
                    false
                }
                _ => true,
            }
        })
        .map(|commit| DroppedCommit {
            sha: commit.sha.clone(),
            subject: commit.subject.clone(),
        })
        .collect()
}

/// Builds the wire state for `derived`. Conflicts of a new step are classified and, with a
/// `cache_lock` (`REBASE_OP_LOCK`), stored in the json while no rebase op holds it; a step
/// already cached only has `resolved` recomputed.
pub(crate) async fn build_rebase_state<T: EngineProvider>(
    git: &git::Git,
    repo_path: &Path,
    engine: &T,
    engine_path: &Path,
    derived: DerivedState,
    cache_lock: Option<&tokio::sync::Mutex<()>>,
) -> anyhow::Result<RebaseState> {
    let mut state = RebaseState {
        state: derived.kind,
        stale_reason: derived.stale_reason,
        ..Default::default()
    };
    let Some(file) = derived.file else {
        return Ok(state);
    };
    state.branch.clone_from(&file.branch);
    state.touches_cpp = file.touches_cpp;
    state.backup_ref.clone_from(&file.backup_ref);
    state.backup_sha.clone_from(&file.backup_sha);
    state
        .push_failed_remote_sha
        .clone_from(&file.push_failed_remote_sha);
    if !is_active(derived.kind) {
        return Ok(state);
    }

    let git_dir = resolve_git_dir(repo_path);
    let mut remaining = HashSet::new();
    if derived.kind == RebaseStateKind::InProgress {
        let merge_dir = git_dir.join("rebase-merge");
        state.step = read_number(&merge_dir.join("msgnum"));
        state.total_steps = read_number(&merge_dir.join("end"));
        if let Ok(todo) = std::fs::read_to_string(merge_dir.join("git-rebase-todo")) {
            remaining.extend(todo_picks(&todo));
        }

        let unmerged = parse_unmerged(&status_porcelain(git).await?);
        match rebase_head(&git_dir) {
            Some(rebase_head) => {
                remaining.insert(rebase_head.clone());
                state.current_commit = Some(commit_info(git, &rebase_head).await?);
                let cached = file
                    .current_step
                    .as_ref()
                    .filter(|step| step.sha == rebase_head && !step.conflicts.is_empty());
                if let Some(step) = cached {
                    state.conflicts.clone_from(&step.conflicts);
                    refresh_resolved(&mut state.conflicts, &unmerged);
                } else if !unmerged.is_empty() {
                    let head = rev_parse(git, "HEAD")
                        .await?
                        .ok_or_else(|| anyhow!("HEAD does not resolve"))?;
                    state.conflicts = classify_conflicts(
                        git,
                        repo_path,
                        engine,
                        engine_path,
                        &file.merge_base,
                        &rebase_head,
                    )
                    .await?;
                    if let Some(op_lock) = cache_lock.filter(|_| !state.conflicts.is_empty()) {
                        let step = CurrentStep {
                            sha: rebase_head.clone(),
                            conflicts: state.conflicts.clone(),
                        };
                        cache_if_unchanged(git, &git_dir, &file, op_lock, step, &head, &unmerged)
                            .await;
                    }
                }
                state.step_incomplete = unmerged.is_empty() && cached.is_none();
            }
            // Stopped with no commit being replayed: nothing Friendshipper can resolve.
            None => state.step_incomplete = true,
        }
    }

    let replayed = replayed_commits(git, &file.onto).await?;
    state.dropped_commits = dropped_commits(&file.original_commits, &remaining, &replayed);
    Ok(state)
}

/// Leading digits of a backup ref's last segment (`<ts>` or `<ts>-rebased`).
pub(crate) fn backup_ref_ts(refname: &str) -> Option<u64> {
    let last = refname.rsplit('/').next()?;
    let digits: String = last.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

/// Backup refs older than 14 days, except `keep`; refs without a timestamp are kept.
pub(crate) fn refs_to_prune(refs: &[String], now_ts: u64, keep: &[String]) -> Vec<String> {
    refs.iter()
        .filter(|refname| refname.starts_with(BACKUP_REF_PREFIX) && !keep.contains(refname))
        .filter(|refname| {
            backup_ref_ts(refname)
                .is_some_and(|ts| now_ts.saturating_sub(ts) > BACKUP_RETENTION_SECS)
        })
        .cloned()
        .collect()
}

/// Creates `refs/f11r-backup/<branch>/<ts><suffix>` at `sha`, never overwriting an existing
/// ref; retries once with `ts + 1`.
pub(crate) async fn create_backup_ref(
    git: &git::Git,
    branch: &str,
    sha: &str,
    suffix: &str,
    now_ts: u64,
) -> anyhow::Result<String> {
    if !is_valid_branch_name(branch) || !is_full_sha(sha) {
        bail!("Invalid backup ref target {branch} at {sha}");
    }
    let mut last_error = String::new();
    for ts in [now_ts, now_ts + 1] {
        let refname = format!("{BACKUP_REF_PREFIX}{branch}/{ts}{suffix}");
        let output = git
            .run_with_status(
                &["update-ref", &refname, sha, ZERO_SHA],
                git::Opts::default().with_env(LOCALE_ENV),
            )
            .await?;
        if output.exit_code == 0 {
            return Ok(refname);
        }
        last_error = output.stderr.trim().to_string();
    }
    bail!("Could not create a backup ref for {branch}: {last_error}")
}

/// Deletes backup refs older than 14 days except `keep` (the current record's backup, read
/// by the caller under `REBASE_FILE_LOCK`). A ref that cannot be deleted is skipped.
pub(crate) async fn prune_backup_refs(
    git: &git::Git,
    now_ts: u64,
    keep: &[String],
) -> anyhow::Result<()> {
    let output = git
        .run_with_status(
            &["for-each-ref", "--format=%(refname)", BACKUP_REF_PREFIX],
            read_opts(),
        )
        .await?;
    if output.exit_code != 0 {
        bail!("Could not list backup refs: {}", output.stderr.trim());
    }
    let refs: Vec<String> = output.stdout.lines().map(str::to_string).collect();
    for refname in refs_to_prune(&refs, now_ts, keep) {
        let deleted = git
            .run_with_status(
                &["update-ref", "-d", &refname],
                git::Opts::default().with_env(LOCALE_ENV),
            )
            .await?;
        if deleted.exit_code != 0 {
            warn!("Could not prune {refname}: {}", deleted.stderr.trim());
        }
    }
    Ok(())
}

#[allow(dead_code)] // TODO: Stage 013
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PushOutcome {
    /// Updated or created, or already identical on origin.
    Pushed,
    /// The lease no longer matched origin.
    LeaseRejected,
    Rejected(String),
    /// The push never reached a ref update (network, auth, pre-push hook).
    NoRefLine,
}

/// The flag and summary of the `git push --porcelain` line that updated `refs/heads/<branch>`.
#[allow(dead_code)] // TODO: Stage 013
pub(crate) fn push_ref_status(stdout: &str, branch: &str) -> Option<(char, String)> {
    let target = format!(":refs/heads/{branch}");
    stdout.lines().find_map(|line| {
        let mut fields = line.trim_end_matches('\r').splitn(3, '\t');
        let flag = fields.next()?;
        if !fields.next()?.ends_with(&target) {
            return None;
        }
        let mut flag_chars = flag.chars();
        let flag_char = flag_chars.next()?;
        if flag_chars.next().is_some() {
            return None;
        }
        Some((flag_char, fields.next().unwrap_or_default().to_string()))
    })
}

#[allow(dead_code)] // TODO: Stage 013
pub(crate) fn parse_push_porcelain(stdout: &str, branch: &str) -> PushOutcome {
    match push_ref_status(stdout, branch) {
        None => PushOutcome::NoRefLine,
        Some(('+' | ' ' | '=' | '*', _)) => PushOutcome::Pushed,
        Some(('!', summary)) if summary.contains("(stale info)") => PushOutcome::LeaseRejected,
        Some((_, summary)) => PushOutcome::Rejected(summary),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::fs;
    use std::sync::mpsc;
    use std::time::SystemTime;

    use super::*;
    use crate::engine::UnrealEngineProvider;
    use ethos_core::types::config::{AppConfig, RepoConfig};
    use ethos_core::types::repo::RebaseKeep;

    pub(crate) const BRANCH: &str = "f11r-branch-feature";
    pub(crate) const OTHER_BRANCH: &str = "f11r-branch-other";
    const RAW_REBASE_ENV: &[(&str, &str)] = &[("GIT_EDITOR", "true"), ("LC_ALL", "C")];
    const NOW: u64 = 1_761_060_000;
    const DAY: u64 = 86400;
    const LINES: &[u8] = b"one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\n";

    pub(crate) fn new_git(path: &Path) -> git::Git {
        let (tx, _rx) = mpsc::channel::<String>();
        git::Git::new(path.to_path_buf(), tx)
    }

    pub(crate) fn slash_path(path: &Path) -> String {
        path.to_str().expect("utf8 path").replace('\\', "/")
    }

    async fn run_git(git: &git::Git, args: &[&str]) {
        git.run(args, git::Opts::new_without_logs())
            .await
            .unwrap_or_else(|e| panic!("git {args:?} failed: {e}"));
    }

    async fn git_out(git: &git::Git, args: &[&str]) -> String {
        git.run_and_collect_output(args, git::Opts::new_without_logs())
            .await
            .unwrap_or_else(|e| panic!("git {args:?} failed: {e}"))
            .trim()
            .to_string()
    }

    /// The pinned rebase invocation; returns git's exit code.
    pub(crate) async fn start_raw_rebase(git: &git::Git, onto: &str) -> i32 {
        git.run_with_status(
            &[
                "-c",
                "rebase.rebaseMerges=false",
                "-c",
                "rerere.enabled=false",
                "rebase",
                "--merge",
                "--no-autostash",
                "--no-update-refs",
                "--no-autosquash",
                "--empty=drop",
                onto,
            ],
            git::Opts::new_without_logs().with_env(RAW_REBASE_ENV),
        )
        .await
        .expect("spawn rebase")
        .exit_code
    }

    /// The record Start writes for the current branch before rebasing onto `onto`.
    pub(crate) async fn guided_record(git: &git::Git, onto: &str) -> GuidedRebaseFile {
        let branch = git_out(git, &["symbolic-ref", "--short", "HEAD"]).await;
        let backup_sha = git_out(git, &["rev-parse", "HEAD"]).await;
        let merge_base = git_out(git, &["merge-base", onto, "HEAD"]).await;
        let range = format!("{merge_base}..HEAD");
        let log = git_out(
            git,
            &[
                "log",
                "--reverse",
                "--no-merges",
                "--date=raw",
                "--format=%H%x1f%ae%x1f%ad%x1f%s",
                &range,
            ],
        )
        .await;
        let original_commits = log
            .lines()
            .map(|line| {
                let fields: Vec<&str> = line.splitn(4, '\u{1f}').collect();
                original(fields[0], fields[1], fields[2], fields[3])
            })
            .collect();
        GuidedRebaseFile {
            version: 1,
            backup_ref: format!("{BACKUP_REF_PREFIX}{branch}/{NOW}"),
            branch,
            trunk: "main".to_string(),
            phase: GuidedRebasePhase::Rebasing,
            pre_rebase_origin_sha: backup_sha.clone(),
            backup_sha,
            onto: git_out(git, &["rev-parse", onto]).await,
            merge_base,
            original_commits,
            touches_cpp: false,
            engine_association: String::new(),
            started_at: "2026-10-21T15:00:00Z".to_string(),
            push_failed_remote_sha: None,
            current_step: None,
        }
    }

    pub(crate) fn info(sha: &str, subject: &str) -> RebaseCommitInfo {
        RebaseCommitInfo {
            sha: sha.to_string(),
            subject: subject.to_string(),
            author: "User".to_string(),
        }
    }

    pub(crate) fn engine(repo_path: &Path) -> UnrealEngineProvider {
        let mut app_config = AppConfig::new(crate::APP_NAME);
        app_config.repo_path = slash_path(repo_path);
        UnrealEngineProvider::new_from_config(app_config, RepoConfig::default())
    }

    /// One file change of a commit.
    pub(crate) enum Change<'a> {
        Write(&'a str, &'a [u8]),
        Delete(&'a str),
        Rename(&'a str, &'a str),
        Gitlink(&'a str, &'a str),
    }

    /// `main` and `BRANCH` each one commit past `base`.
    pub(crate) struct Diverged {
        pub(crate) base: String,
        pub(crate) main: String,
        pub(crate) branch: String,
    }

    /// A temp bare origin with `main` and a user clone on `main`; hooks come only from a temp dir.
    pub(crate) struct Fixture {
        origin_dir: tempfile::TempDir,
        dir: tempfile::TempDir,
        hooks: tempfile::TempDir,
        pub(crate) git: git::Git,
    }

    impl Fixture {
        pub(crate) async fn new() -> Self {
            let origin_dir = tempfile::tempdir().expect("origin tempdir");
            let dir = tempfile::tempdir().expect("user tempdir");
            let hooks = tempfile::tempdir().expect("hooks tempdir");

            run_git(&new_git(origin_dir.path()), &["init", "-q", "--bare"]).await;
            let git = new_git(dir.path());
            run_git(
                &git,
                &[
                    "clone",
                    "-q",
                    &slash_path(origin_dir.path()),
                    &slash_path(dir.path()),
                ],
            )
            .await;
            // Local values override the developer's global config that the tests depend on.
            for (key, value) in [
                ("user.email", "user@example.com"),
                ("user.name", "User"),
                ("commit.gpgsign", "false"),
                ("core.autocrlf", "false"),
                ("core.safecrlf", "false"),
                ("core.fsmonitor", "false"),
                ("merge.conflictStyle", "merge"),
                ("rerere.enabled", "false"),
            ] {
                run_git(&git, &["config", key, value]).await;
            }
            run_git(
                &git,
                &["config", "core.hooksPath", &slash_path(hooks.path())],
            )
            .await;

            let fx = Self {
                origin_dir,
                dir,
                hooks,
                git,
            };
            fx.run(&["checkout", "-q", "-b", "main"]).await;
            fx.commit_file("seed.txt", b"seed\n", "seed").await;
            fx.run(&["push", "-q", "-u", "origin", "main"]).await;
            fx
        }

        pub(crate) fn path(&self) -> &Path {
            self.dir.path()
        }

        pub(crate) fn git_dir(&self) -> PathBuf {
            self.path().join(".git")
        }

        pub(crate) fn origin_path(&self) -> &Path {
            self.origin_dir.path()
        }

        pub(crate) fn hooks_path(&self) -> &Path {
            self.hooks.path()
        }

        pub(crate) async fn run(&self, args: &[&str]) {
            run_git(&self.git, args).await;
        }

        pub(crate) async fn out(&self, args: &[&str]) -> String {
            git_out(&self.git, args).await
        }

        pub(crate) async fn sha(&self, rev: &str) -> String {
            self.out(&["rev-parse", rev]).await
        }

        pub(crate) async fn head(&self) -> String {
            self.sha("HEAD").await
        }

        pub(crate) fn write(&self, path: &str, bytes: &[u8]) {
            let abs = repo_file_path(self.path(), path);
            fs::create_dir_all(abs.parent().unwrap()).unwrap();
            fs::write(abs, bytes).unwrap();
        }

        /// Commits `changes` (or an empty commit) on the current branch; returns the sha.
        pub(crate) async fn commit(&self, changes: &[Change<'_>], message: &str) -> String {
            for change in changes {
                match change {
                    Change::Write(path, bytes) => {
                        self.write(path, bytes);
                        self.run(&["add", "--", path]).await;
                    }
                    Change::Delete(path) => self.run(&["rm", "-q", "--", path]).await,
                    Change::Rename(from, to) => self.run(&["mv", "--", from, to]).await,
                    Change::Gitlink(path, sha) => {
                        let info = format!("160000,{sha},{path}");
                        self.run(&["update-index", "--add", "--cacheinfo", &info])
                            .await
                    }
                }
            }
            self.run(&["commit", "-q", "--allow-empty", "-m", message])
                .await;
            self.head().await
        }

        pub(crate) async fn commit_file(&self, path: &str, bytes: &[u8], message: &str) -> String {
            self.commit(&[Change::Write(path, bytes)], message).await
        }

        pub(crate) async fn branch(&self, name: &str) {
            self.run(&["checkout", "-q", "-b", name]).await;
        }

        pub(crate) async fn checkout(&self, name: &str) {
            self.run(&["checkout", "-q", name]).await;
        }

        /// Force-pushes `name` to origin, updating its remote-tracking ref.
        pub(crate) async fn push(&self, name: &str) {
            let refspec = format!("+{name}:refs/heads/{name}");
            self.run(&["push", "-q", "-u", "origin", &refspec]).await;
        }

        pub(crate) async fn start_raw_rebase(&self, onto: &str) -> i32 {
            start_raw_rebase(&self.git, onto).await
        }

        pub(crate) fn write_file_json(&self, file: &GuidedRebaseFile) {
            write_rebase_file(&self.git_dir(), file).expect("write json");
        }

        pub(crate) fn read_file_json(&self) -> GuidedRebaseFile {
            match read_rebase_file(&self.git_dir()) {
                RebaseFileRead::Ok(file) => *file,
                other => panic!("expected a readable json, got {other:?}"),
            }
        }

        pub(crate) fn json_bytes(&self) -> Option<Vec<u8>> {
            fs::read(json_path(&self.git_dir())).ok()
        }

        pub(crate) async fn porcelain_v2(&self) -> String {
            self.out(&[
                "--no-optional-locks",
                "status",
                "--porcelain=v2",
                "-z",
                "--untracked-files=all",
            ])
            .await
        }

        pub(crate) fn rebase_head(&self) -> String {
            read_trimmed(&self.git_dir().join("REBASE_HEAD")).expect("REBASE_HEAD")
        }

        pub(crate) async fn guided_file(&self, onto: &str) -> GuidedRebaseFile {
            guided_record(&self.git, onto).await
        }

        /// Pushes `base` and `branch` as `BRANCH`, then `main` on top of `base`; leaves the
        /// clone on `BRANCH`.
        pub(crate) async fn diverge(
            &self,
            base: &[Change<'_>],
            main: &[Change<'_>],
            branch: &[Change<'_>],
        ) -> Diverged {
            let base = self.commit(base, "base change").await;
            self.branch(BRANCH).await;
            let branch_sha = self.commit(branch, "branch change").await;
            self.push(BRANCH).await;
            self.checkout("main").await;
            let main_sha = self.commit(main, "main change").await;
            self.push("main").await;
            self.checkout(BRANCH).await;
            Diverged {
                base,
                main: main_sha,
                branch: branch_sha,
            }
        }

        /// Diverges, writes the json and starts the guided rebase, which must stop.
        pub(crate) async fn guided_stop(
            &self,
            base: &[Change<'_>],
            main: &[Change<'_>],
            branch: &[Change<'_>],
        ) -> (Diverged, GuidedRebaseFile) {
            let diverged = self.diverge(base, main, branch).await;
            let file = self.guided_file("main").await;
            self.write_file_json(&file);
            assert_eq!(
                self.start_raw_rebase(&file.onto).await,
                1,
                "rebase should stop"
            );
            (diverged, file)
        }

        /// Diverges without conflicts, writes the json and completes the rebase.
        pub(crate) async fn guided_complete(&self) -> GuidedRebaseFile {
            self.diverge(
                &[],
                &[Change::Write("m.txt", b"main\n")],
                &[Change::Write("b.txt", b"branch\n")],
            )
            .await;
            let file = self.guided_file("main").await;
            self.write_file_json(&file);
            assert_eq!(self.start_raw_rebase(&file.onto).await, 0);
            file
        }

        pub(crate) async fn derive(&self) -> DerivedState {
            derive_rebase_state(&self.git, self.path())
                .await
                .expect("derive rebase state")
        }

        pub(crate) async fn build_from(
            &self,
            derived: DerivedState,
            may_cache: bool,
        ) -> RebaseState {
            let op_lock = tokio::sync::Mutex::new(());
            build_rebase_state(
                &self.git,
                self.path(),
                &engine(self.path()),
                Path::new(""),
                derived,
                may_cache.then_some(&op_lock),
            )
            .await
            .expect("build rebase state")
        }

        pub(crate) async fn build(&self, may_cache: bool) -> RebaseState {
            let derived = self.derive().await;
            self.build_from(derived, may_cache).await
        }

        async fn classify(&self, merge_base: &str) -> Vec<ConflictFile> {
            classify_conflicts(
                &self.git,
                self.path(),
                &engine(self.path()),
                Path::new(""),
                merge_base,
                &self.rebase_head(),
            )
            .await
            .expect("classify conflicts")
        }
    }

    async fn assert_derived(
        fx: &Fixture,
        kind: RebaseStateKind,
        reason: Option<RebaseStaleReason>,
    ) {
        let derived = fx.derive().await;
        assert_eq!(
            (derived.kind, derived.stale_reason),
            (kind, reason),
            "derived {derived:?}"
        );
    }

    pub(crate) async fn diverge_on_a_txt(fx: &Fixture) -> Diverged {
        fx.diverge(
            &[Change::Write("a.txt", b"base\n")],
            &[Change::Write("a.txt", b"main\n")],
            &[Change::Write("a.txt", b"branch\n")],
        )
        .await
    }

    pub(crate) async fn guided_stop_on_a_txt(fx: &Fixture) -> (Diverged, GuidedRebaseFile) {
        fx.guided_stop(
            &[Change::Write("a.txt", b"base\n")],
            &[Change::Write("a.txt", b"main\n")],
            &[Change::Write("a.txt", b"branch\n")],
        )
        .await
    }

    pub(crate) fn conflict(path: &str, kind: ConflictKind, d: &Diverged) -> ConflictFile {
        ConflictFile {
            path: path.to_string(),
            display_name: last_segment(path),
            kind,
            deleted_on: None,
            unsupported_reason: None,
            is_text: true,
            resolved: false,
            resolution: None,
            main_side: Some(info(&d.main, "main change")),
            branch_side: info(&d.branch, "branch change"),
        }
    }

    // Derivation rows

    #[tokio::test]
    async fn row1_external_merge_backend() {
        let fx = Fixture::new().await;
        diverge_on_a_txt(&fx).await;
        assert_eq!(fx.start_raw_rebase("main").await, 1);
        assert_eq!(
            fx.derive().await,
            DerivedState {
                kind: RebaseStateKind::External,
                stale_reason: None,
                file: None,
            }
        );
        assert_eq!(
            fx.build(true).await,
            RebaseState {
                state: RebaseStateKind::External,
                ..Default::default()
            }
        );
    }

    #[tokio::test]
    async fn row1b_external_apply_backend() {
        let fx = Fixture::new().await;
        diverge_on_a_txt(&fx).await;
        let output = fx
            .git
            .run_with_status(
                &["rebase", "--apply", "main"],
                git::Opts::new_without_logs().with_env(RAW_REBASE_ENV),
            )
            .await
            .unwrap();
        assert_ne!(output.exit_code, 0);
        assert!(fx.git_dir().join("rebase-apply").is_dir());
        assert!(!fx.git_dir().join("rebase-merge").exists());
        assert_derived(&fx, RebaseStateKind::External, None).await;
    }

    #[tokio::test]
    async fn row2_idle() {
        let fx = Fixture::new().await;
        assert_eq!(
            fx.derive().await,
            DerivedState {
                kind: RebaseStateKind::Idle,
                stale_reason: None,
                file: None,
            }
        );
        assert_eq!(fx.build(true).await, RebaseState::default());
    }

    #[tokio::test]
    async fn row3_garbage_json_is_unreadable() {
        let fx = Fixture::new().await;
        fs::write(json_path(&fx.git_dir()), "{").unwrap();
        assert_eq!(
            fx.derive().await,
            DerivedState {
                kind: RebaseStateKind::Stale,
                stale_reason: Some(RebaseStaleReason::Unreadable),
                file: None,
            }
        );
        assert_eq!(
            fx.build(true).await,
            RebaseState {
                state: RebaseStateKind::Stale,
                stale_reason: Some(RebaseStaleReason::Unreadable),
                ..Default::default()
            }
        );
    }

    #[tokio::test]
    async fn row3b_unknown_version_is_unreadable() {
        let fx = Fixture::new().await;
        fx.branch(BRANCH).await;
        let mut file = fx.guided_file("main").await;
        file.version = 2;
        fx.write_file_json(&file);
        assert_derived(
            &fx,
            RebaseStateKind::Stale,
            Some(RebaseStaleReason::Unreadable),
        )
        .await;
    }

    #[tokio::test]
    async fn row3c_garbage_json_with_rebase_dir_is_external() {
        let fx = Fixture::new().await;
        diverge_on_a_txt(&fx).await;
        assert_eq!(fx.start_raw_rebase("main").await, 1);
        fs::write(json_path(&fx.git_dir()), "{").unwrap();
        assert_derived(
            &fx,
            RebaseStateKind::External,
            Some(RebaseStaleReason::Unreadable),
        )
        .await;
    }

    #[tokio::test]
    async fn row4_in_progress() {
        let fx = Fixture::new().await;
        let (_, file) = guided_stop_on_a_txt(&fx).await;
        assert_eq!(
            fx.derive().await,
            DerivedState {
                kind: RebaseStateKind::InProgress,
                stale_reason: None,
                file: Some(file),
            }
        );
    }

    #[tokio::test]
    async fn row5_rebase_of_another_branch() {
        let fx = Fixture::new().await;
        let (_, mut file) = guided_stop_on_a_txt(&fx).await;
        file.branch = OTHER_BRANCH.to_string();
        file.backup_ref = format!("{BACKUP_REF_PREFIX}{OTHER_BRANCH}/{NOW}");
        fx.write_file_json(&file);
        assert_derived(
            &fx,
            RebaseStateKind::Stale,
            Some(RebaseStaleReason::OtherRebase),
        )
        .await;
    }

    #[tokio::test]
    async fn row5b_rebase_onto_another_commit() {
        let fx = Fixture::new().await;
        let (diverged, mut file) = guided_stop_on_a_txt(&fx).await;
        file.onto = diverged.base;
        fx.write_file_json(&file);
        assert_derived(
            &fx,
            RebaseStateKind::Stale,
            Some(RebaseStaleReason::OtherRebase),
        )
        .await;
    }

    #[tokio::test]
    async fn row6a_switched_to_another_branch() {
        let fx = Fixture::new().await;
        let file = fx.guided_complete().await;
        fx.checkout("main").await;
        assert_eq!(
            fx.derive().await,
            DerivedState {
                kind: RebaseStateKind::Stale,
                stale_reason: Some(RebaseStaleReason::BranchChanged),
                file: Some(file.clone()),
            }
        );
        assert_eq!(
            fx.build(true).await,
            RebaseState {
                state: RebaseStateKind::Stale,
                branch: BRANCH.to_string(),
                stale_reason: Some(RebaseStaleReason::BranchChanged),
                backup_ref: file.backup_ref,
                backup_sha: file.backup_sha,
                ..Default::default()
            }
        );
    }

    #[tokio::test]
    async fn row6b_detached_head() {
        let fx = Fixture::new().await;
        fx.guided_complete().await;
        fx.run(&["checkout", "-q", "--detach"]).await;
        assert_derived(
            &fx,
            RebaseStateKind::Stale,
            Some(RebaseStaleReason::BranchChanged),
        )
        .await;
    }

    #[tokio::test]
    async fn row7_aborted_outside() {
        let fx = Fixture::new().await;
        diverge_on_a_txt(&fx).await;
        let file = fx.guided_file("main").await;
        fx.write_file_json(&file);
        assert_derived(
            &fx,
            RebaseStateKind::Stale,
            Some(RebaseStaleReason::AbortedOutside),
        )
        .await;

        // The same after a real abort in a terminal.
        assert_eq!(fx.start_raw_rebase(&file.onto).await, 1);
        fx.run(&["rebase", "--abort"]).await;
        assert_eq!(fx.head().await, file.backup_sha);
        assert_derived(
            &fx,
            RebaseStateKind::Stale,
            Some(RebaseStaleReason::AbortedOutside),
        )
        .await;
    }

    #[tokio::test]
    async fn row8_pushed_outside() {
        let fx = Fixture::new().await;
        fx.guided_complete().await;
        fx.push(BRANCH).await;
        assert_derived(
            &fx,
            RebaseStateKind::Stale,
            Some(RebaseStaleReason::PushedOutside),
        )
        .await;
    }

    #[tokio::test]
    async fn row9_branch_reset_outside() {
        let fx = Fixture::new().await;
        let file = fx.guided_complete().await;
        fx.run(&["reset", "-q", "--hard", &file.merge_base]).await;
        assert_derived(
            &fx,
            RebaseStateKind::Stale,
            Some(RebaseStaleReason::Unknown),
        )
        .await;
    }

    #[tokio::test]
    async fn row9_missing_onto_object_is_unknown() {
        let fx = Fixture::new().await;
        let mut file = fx.guided_complete().await;
        file.onto = "1234567890123456789012345678901234567890".to_string();
        fx.write_file_json(&file);
        assert_derived(
            &fx,
            RebaseStateKind::Stale,
            Some(RebaseStaleReason::Unknown),
        )
        .await;
    }

    #[tokio::test]
    async fn row10_push_failed() {
        let fx = Fixture::new().await;
        let mut file = fx.guided_complete().await;
        let remote = file.backup_sha.clone();
        file.phase = GuidedRebasePhase::PushFailed;
        file.push_failed_remote_sha = Some(remote.clone());
        file.touches_cpp = true;
        fx.write_file_json(&file);
        assert_eq!(
            fx.build(true).await,
            RebaseState {
                state: RebaseStateKind::PushFailed,
                branch: BRANCH.to_string(),
                touches_cpp: true,
                backup_ref: file.backup_ref,
                backup_sha: file.backup_sha,
                push_failed_remote_sha: Some(remote),
                ..Default::default()
            }
        );
    }

    #[tokio::test]
    async fn row11_completed_unpushed() {
        let fx = Fixture::new().await;
        let file = fx.guided_complete().await;
        assert_eq!(
            fx.build(true).await,
            RebaseState {
                state: RebaseStateKind::CompletedUnpushed,
                branch: BRANCH.to_string(),
                backup_ref: file.backup_ref.clone(),
                backup_sha: file.backup_sha.clone(),
                ..Default::default()
            }
        );
        assert_eq!(fx.read_file_json(), file);
    }

    #[tokio::test]
    async fn row4_wins_over_row7() {
        let fx = Fixture::new().await;
        let (_, mut file) = guided_stop_on_a_txt(&fx).await;
        file.backup_sha = fx.head().await;
        fx.write_file_json(&file);
        assert_derived(&fx, RebaseStateKind::InProgress, None).await;
    }

    #[tokio::test]
    async fn completed_rebase_lists_commit_already_on_main() {
        let fx = Fixture::new().await;
        fx.branch(BRANCH).await;
        let picked = fx.commit_file("p.txt", b"picked\n", "picked").await;
        fx.commit_file("k.txt", b"kept\n", "kept").await;
        fx.push(BRANCH).await;
        fx.checkout("main").await;
        fx.run(&["cherry-pick", &picked]).await;
        fx.push("main").await;
        fx.checkout(BRANCH).await;
        let file = fx.guided_file("main").await;
        fx.write_file_json(&file);
        assert_eq!(fx.start_raw_rebase(&file.onto).await, 0);
        assert_eq!(
            fx.build(true).await,
            RebaseState {
                state: RebaseStateKind::CompletedUnpushed,
                branch: BRANCH.to_string(),
                dropped_commits: vec![dropped(&picked, "picked")],
                backup_ref: file.backup_ref,
                backup_sha: file.backup_sha,
                ..Default::default()
            }
        );
    }

    #[tokio::test]
    async fn aborted_outside_then_merged_main_is_unknown() {
        let fx = Fixture::new().await;
        diverge_on_a_txt(&fx).await;
        let file = fx.guided_file("main").await;
        fx.write_file_json(&file);
        assert_eq!(fx.start_raw_rebase(&file.onto).await, 1);
        fx.run(&["rebase", "--abort"]).await;
        fx.run(&["merge", "-q", "-X", "ours", "--no-edit", "main"])
            .await;
        assert!(is_ancestor(&fx.git, &file.onto, "HEAD").await.unwrap());
        assert_derived(
            &fx,
            RebaseStateKind::Stale,
            Some(RebaseStaleReason::Unknown),
        )
        .await;
    }

    #[tokio::test]
    async fn branch_without_own_commits_rebases_to_completed() {
        let fx = Fixture::new().await;
        fx.branch(BRANCH).await;
        fx.push(BRANCH).await;
        fx.checkout("main").await;
        fx.commit_file("m.txt", b"main\n", "main change").await;
        fx.push("main").await;
        fx.checkout(BRANCH).await;
        let file = fx.guided_file("main").await;
        fx.write_file_json(&file);
        assert_eq!(fx.start_raw_rebase(&file.onto).await, 0);
        assert_eq!(fx.head().await, file.onto);
        assert_derived(&fx, RebaseStateKind::CompletedUnpushed, None).await;
    }

    #[tokio::test]
    async fn unreadable_json_file_fails_derivation() {
        let fx = Fixture::new().await;
        fx.guided_complete().await;
        let json = json_path(&fx.git_dir());
        fs::remove_file(&json).unwrap();
        // A directory in its place makes reading fail with something other than NotFound.
        fs::create_dir(&json).unwrap();
        assert!(derive_rebase_state(&fx.git, fx.path()).await.is_err());
    }

    #[tokio::test]
    async fn git_failure_fails_derivation() {
        let fx = Fixture::new().await;
        fx.guided_complete().await;
        let missing = fx.path().join("missing");
        let broken = new_git(&missing);
        assert!(derive_rebase_state(&broken, fx.path()).await.is_err());
        // Still derivable with a working git.
        assert_derived(&fx, RebaseStateKind::CompletedUnpushed, None).await;
    }

    #[tokio::test]
    async fn classify_submodule_conflict() {
        let fx = Fixture::new().await;
        let (d, _) = fx
            .guided_stop(
                &[Change::Gitlink("sub", &"1".repeat(40))],
                &[Change::Gitlink("sub", &"3".repeat(40))],
                &[Change::Gitlink("sub", &"2".repeat(40))],
            )
            .await;
        assert_eq!(
            fx.classify(&d.base).await,
            vec![ConflictFile {
                unsupported_reason: Some(UNRESOLVABLE.to_string()),
                is_text: false,
                ..conflict("sub", ConflictKind::Unsupported, &d)
            }]
        );
    }

    // resolve_git_dir

    #[test]
    fn git_dir_is_the_dot_git_dir() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join(".git")).unwrap();
        assert_eq!(resolve_git_dir(dir.path()), dir.path().join(".git"));
    }

    #[test]
    fn git_dir_missing() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(resolve_git_dir(dir.path()), dir.path().join(".git"));
    }

    #[tokio::test]
    async fn git_dir_of_linked_worktrees() {
        let fx = Fixture::new().await;
        let parent = tempfile::tempdir().unwrap();
        let worktree = parent.path().join("wt");
        fx.run(&["worktree", "add", "-q", "-b", "wt", &slash_path(&worktree)])
            .await;
        let expected = fx
            .git_dir()
            .join("worktrees")
            .join("wt")
            .canonicalize()
            .unwrap();

        // git writes an absolute `gitdir:`
        let absolute = resolve_git_dir(&worktree);
        assert!(absolute.is_absolute(), "{absolute:?}");
        assert_eq!(absolute.canonicalize().unwrap(), expected);

        let relative = relative_path(&expected, &worktree);
        assert!(relative.starts_with("../"), "{relative}");
        fs::write(worktree.join(".git"), format!("gitdir: {relative}\n")).unwrap();
        assert_eq!(resolve_git_dir(&worktree).canonicalize().unwrap(), expected);
    }

    /// `target` relative to `base`, with `/` separators.
    fn relative_path(target: &Path, base: &Path) -> String {
        let target = target.canonicalize().unwrap();
        let base = base.canonicalize().unwrap();
        let target: Vec<_> = target.components().collect();
        let base: Vec<_> = base.components().collect();
        let common = target.iter().zip(&base).take_while(|(a, b)| a == b).count();
        let mut parts = vec!["..".to_string(); base.len() - common];
        parts.extend(
            target[common..]
                .iter()
                .map(|c| c.as_os_str().to_str().unwrap().to_string()),
        );
        parts.join("/")
    }

    // Conflict classification from real git

    #[tokio::test]
    async fn classify_content_conflict() {
        let fx = Fixture::new().await;
        let (d, _) = guided_stop_on_a_txt(&fx).await;
        assert_eq!(fx.rebase_head(), d.branch);
        assert_eq!(
            fx.classify(&d.base).await,
            vec![conflict("a.txt", ConflictKind::Content, &d)]
        );
    }

    #[tokio::test]
    async fn classify_deleted_on_main() {
        let fx = Fixture::new().await;
        let (d, _) = fx
            .guided_stop(
                &[Change::Write("a.txt", b"base\n")],
                &[Change::Delete("a.txt")],
                &[Change::Write("a.txt", b"branch\n")],
            )
            .await;
        assert_eq!(
            fx.classify(&d.base).await,
            vec![ConflictFile {
                deleted_on: Some(ConflictSideName::Main),
                ..conflict("a.txt", ConflictKind::ModifyDelete, &d)
            }]
        );
    }

    #[tokio::test]
    async fn classify_deleted_on_branch() {
        let fx = Fixture::new().await;
        let (d, _) = fx
            .guided_stop(
                &[Change::Write("a.txt", b"base\n")],
                &[Change::Write("a.txt", b"main\n")],
                &[Change::Delete("a.txt")],
            )
            .await;
        assert_eq!(
            fx.classify(&d.base).await,
            vec![ConflictFile {
                deleted_on: Some(ConflictSideName::Branch),
                ..conflict("a.txt", ConflictKind::ModifyDelete, &d)
            }]
        );
    }

    #[tokio::test]
    async fn classify_rename_on_main_vs_delete_on_branch() {
        let fx = Fixture::new().await;
        let (d, _) = fx
            .guided_stop(
                &[Change::Write("r.txt", LINES)],
                &[Change::Rename("r.txt", "s.txt")],
                &[Change::Delete("r.txt")],
            )
            .await;
        assert_eq!(
            fx.classify(&d.base).await,
            vec![ConflictFile {
                deleted_on: Some(ConflictSideName::Branch),
                ..conflict("s.txt", ConflictKind::ModifyDelete, &d)
            }]
        );
    }

    #[tokio::test]
    async fn classify_rename_and_edit_vs_edit() {
        let fx = Fixture::new().await;
        let (d, _) = fx
            .guided_stop(
                &[Change::Write("r.txt", LINES)],
                &[
                    Change::Rename("r.txt", "s.txt"),
                    Change::Write(
                        "s.txt",
                        b"one main\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\n",
                    ),
                ],
                &[Change::Write(
                    "r.txt",
                    b"one branch\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\n",
                )],
            )
            .await;
        assert_eq!(
            fx.classify(&d.base).await,
            vec![conflict("s.txt", ConflictKind::Content, &d)]
        );
    }

    #[tokio::test]
    async fn classify_both_added() {
        let fx = Fixture::new().await;
        let (d, _) = fx
            .guided_stop(
                &[],
                &[Change::Write("n.txt", b"main\n")],
                &[Change::Write("n.txt", b"branch\n")],
            )
            .await;
        assert_eq!(
            fx.classify(&d.base).await,
            vec![ConflictFile {
                unsupported_reason: Some(BOTH_ADDED.to_string()),
                ..conflict("n.txt", ConflictKind::Unsupported, &d)
            }]
        );
    }

    #[tokio::test]
    async fn classify_rename_rename() {
        let fx = Fixture::new().await;
        let (d, _) = fx
            .guided_stop(
                &[Change::Write("r.txt", LINES)],
                &[Change::Rename("r.txt", "m.txt")],
                &[Change::Rename("r.txt", "b.txt")],
            )
            .await;
        let renamed = |path: &str| ConflictFile {
            unsupported_reason: Some(RENAMED_DIFFERENTLY.to_string()),
            ..conflict(path, ConflictKind::Unsupported, &d)
        };
        assert_eq!(
            fx.classify(&d.base).await,
            vec![
                ConflictFile {
                    main_side: None,
                    ..renamed("b.txt")
                },
                renamed("m.txt"),
                ConflictFile {
                    is_text: false,
                    ..renamed("r.txt")
                },
            ]
        );
    }

    #[tokio::test]
    async fn classify_file_on_main_vs_folder_on_branch() {
        let fx = Fixture::new().await;
        let (d, _) = fx
            .guided_stop(
                &[],
                &[Change::Write("fd", b"file\n")],
                &[Change::Write("fd/x", b"x\n")],
            )
            .await;
        assert_eq!(
            fx.classify(&d.base).await,
            vec![ConflictFile {
                unsupported_reason: Some(FILE_AND_FOLDER.to_string()),
                main_side: None,
                ..conflict("fd~HEAD", ConflictKind::Unsupported, &d)
            }]
        );
    }

    #[tokio::test]
    async fn classify_folder_on_main_vs_edited_file_on_branch() {
        let fx = Fixture::new().await;
        let (d, _) = fx
            .guided_stop(
                &[Change::Write("fd", b"file\n")],
                &[Change::Delete("fd"), Change::Write("fd/x", b"x\n")],
                &[Change::Write("fd", b"edited\n")],
            )
            .await;
        let conflicts = fx.classify(&d.base).await;
        assert_eq!(conflicts.len(), 1, "{conflicts:?}");
        // git names the moved-aside file after the branch commit: `fd~<abbrev> (<subject>)`
        let path = conflicts[0].path.clone();
        assert!(path.starts_with("fd~"), "{path}");
        assert_eq!(
            conflicts,
            vec![ConflictFile {
                unsupported_reason: Some(FILE_AND_FOLDER.to_string()),
                main_side: None,
                ..conflict(&path, ConflictKind::Unsupported, &d)
            }]
        );
    }

    #[tokio::test]
    async fn classify_binary_is_not_text() {
        let fx = Fixture::new().await;
        let (d, _) = fx
            .guided_stop(
                &[Change::Write("x.bin", b"base\0bin\n")],
                &[Change::Write("x.bin", b"main\0bin\n")],
                &[Change::Write("x.bin", b"branch\0bin\n")],
            )
            .await;
        assert_eq!(
            fx.classify(&d.base).await,
            vec![ConflictFile {
                is_text: false,
                ..conflict("x.bin", ConflictKind::Content, &d)
            }]
        );
    }

    #[tokio::test]
    async fn classify_lfs_attributed_is_not_text() {
        let fx = Fixture::new().await;
        let (d, _) = fx
            .guided_stop(
                &[
                    Change::Write(".gitattributes", b"*.uasset filter=lfs\n"),
                    Change::Write("Content/a.uasset", b"base\n"),
                ],
                &[Change::Write("Content/a.uasset", b"main\n")],
                &[Change::Write("Content/a.uasset", b"branch\n")],
            )
            .await;
        assert_eq!(
            fx.classify(&d.base).await,
            vec![ConflictFile {
                is_text: false,
                ..conflict("Content/a.uasset", ConflictKind::Content, &d)
            }]
        );
    }

    #[tokio::test]
    async fn classify_path_with_spaces_and_non_ascii() {
        let fx = Fixture::new().await;
        let path = "Content/a b/Ünï.txt";
        let (d, _) = fx
            .guided_stop(
                &[Change::Write(path, b"base\n")],
                &[Change::Write(path, b"main\n")],
                &[Change::Write(path, b"branch\n")],
            )
            .await;
        let conflicts = fx.classify(&d.base).await;
        assert_eq!(conflicts[0].display_name, "Ünï.txt");
        assert_eq!(conflicts, vec![conflict(path, ConflictKind::Content, &d)]);
    }

    // build_rebase_state and the cache

    /// `BRANCH` has three commits; the first conflicts with `main` on `a.txt`. Returns the
    /// json, the first branch commit and the main commit.
    async fn three_commit_stop(fx: &Fixture) -> (GuidedRebaseFile, String, String) {
        fx.commit_file("a.txt", b"base\n", "base change").await;
        fx.branch(BRANCH).await;
        let first = fx.commit_file("a.txt", b"branch\n", "first").await;
        fx.commit_file("x.txt", b"x\n", "second").await;
        fx.commit_file("y.txt", b"y\n", "third").await;
        fx.push(BRANCH).await;
        fx.checkout("main").await;
        let main = fx.commit_file("a.txt", b"main\n", "main change").await;
        fx.push("main").await;
        fx.checkout(BRANCH).await;
        let file = fx.guided_file("main").await;
        fx.write_file_json(&file);
        assert_eq!(fx.start_raw_rebase(&file.onto).await, 1);
        (file, first, main)
    }

    fn first_step_conflict(first: &str, main: &str) -> ConflictFile {
        ConflictFile {
            path: "a.txt".to_string(),
            display_name: "a.txt".to_string(),
            kind: ConflictKind::Content,
            deleted_on: None,
            unsupported_reason: None,
            is_text: true,
            resolved: false,
            resolution: None,
            main_side: Some(info(main, "main change")),
            branch_side: info(first, "first"),
        }
    }

    #[tokio::test]
    async fn in_progress_state() {
        let fx = Fixture::new().await;
        let (file, first, main) = three_commit_stop(&fx).await;
        assert_eq!(
            fx.build(false).await,
            RebaseState {
                state: RebaseStateKind::InProgress,
                branch: BRANCH.to_string(),
                step: 1,
                total_steps: 3,
                current_commit: Some(info(&first, "first")),
                conflicts: vec![first_step_conflict(&first, &main)],
                step_incomplete: false,
                dropped_commits: vec![],
                stale_reason: None,
                touches_cpp: false,
                backup_ref: file.backup_ref,
                backup_sha: file.backup_sha,
                push_failed_remote_sha: None,
            }
        );
    }

    #[tokio::test]
    async fn conflicts_are_cached_and_resolved_is_recomputed() {
        let fx = Fixture::new().await;
        let (_, first, main) = three_commit_stop(&fx).await;
        let expected = first_step_conflict(&first, &main);
        assert_eq!(fx.build(true).await.conflicts, vec![expected.clone()]);
        assert_eq!(
            fx.read_file_json().current_step,
            Some(CurrentStep {
                sha: first.clone(),
                conflicts: vec![expected.clone()],
            })
        );

        // A cached step is used as is; only `resolved` is recomputed.
        let mut cached = fx.read_file_json();
        let step = cached.current_step.as_mut().unwrap();
        step.conflicts[0].display_name = "Cached".to_string();
        step.conflicts[0].resolution = Some(RebaseKeep::Main);
        fx.write_file_json(&cached);
        fx.run(&["checkout", "--ours", "--", "a.txt"]).await;
        fx.run(&["add", "--", "a.txt"]).await;
        let state = fx.build(true).await;
        assert_eq!(
            state.conflicts,
            vec![ConflictFile {
                display_name: "Cached".to_string(),
                resolved: true,
                resolution: Some(RebaseKeep::Main),
                ..expected.clone()
            }]
        );
        assert!(!state.step_incomplete);

        // Unmerged again, e.g. after a crash mid-undo: the resolution is dropped.
        fx.run(&["update-index", "--unresolve", "--", "a.txt"])
            .await;
        assert_eq!(
            fx.build(true).await.conflicts,
            vec![ConflictFile {
                display_name: "Cached".to_string(),
                ..expected
            }]
        );
        assert_eq!(fx.read_file_json(), cached);
    }

    #[tokio::test]
    async fn may_cache_false_never_writes() {
        let fx = Fixture::new().await;
        three_commit_stop(&fx).await;
        let before = fx.json_bytes();
        assert_eq!(fx.build(false).await.conflicts.len(), 1);
        assert_eq!(fx.json_bytes(), before);
        assert_eq!(fx.read_file_json().current_step, None);
    }

    #[tokio::test]
    async fn cache_is_not_written_into_a_replaced_record() {
        let fx = Fixture::new().await;
        let (file, _, _) = three_commit_stop(&fx).await;
        let derived = fx.derive().await;
        let mut replaced = file.clone();
        replaced.backup_sha.clone_from(&file.merge_base);
        fx.write_file_json(&replaced);
        assert_eq!(fx.build_from(derived, true).await.conflicts.len(), 1);
        assert_eq!(fx.read_file_json(), replaced);
    }

    #[tokio::test]
    async fn cache_is_not_written_while_an_op_holds_the_lock() {
        let fx = Fixture::new().await;
        three_commit_stop(&fx).await;
        let op_lock = tokio::sync::Mutex::new(());
        let _running = op_lock.lock().await;
        let state = build_rebase_state(
            &fx.git,
            fx.path(),
            &engine(fx.path()),
            Path::new(""),
            fx.derive().await,
            Some(&op_lock),
        )
        .await
        .unwrap();
        assert_eq!(state.conflicts.len(), 1);
        assert_eq!(fx.read_file_json().current_step, None);
    }

    #[tokio::test]
    async fn cache_is_not_written_when_git_moved_during_classification() {
        let fx = Fixture::new().await;
        let (file, _, _) = three_commit_stop(&fx).await;
        let op_lock = tokio::sync::Mutex::new(());
        let head = fx.head().await;
        let unmerged = parse_unmerged(&fx.porcelain_v2().await);
        let step = CurrentStep {
            sha: fx.rebase_head(),
            conflicts: fx.classify(&file.merge_base).await,
        };

        for (head, unmerged) in [
            (file.merge_base.as_str(), unmerged.as_slice()),
            (head.as_str(), &[][..]),
        ] {
            cache_if_unchanged(
                &fx.git,
                &fx.git_dir(),
                &file,
                &op_lock,
                step.clone(),
                head,
                unmerged,
            )
            .await;
            assert_eq!(fx.read_file_json().current_step, None);
        }

        cache_if_unchanged(
            &fx.git,
            &fx.git_dir(),
            &file,
            &op_lock,
            step.clone(),
            &head,
            &unmerged,
        )
        .await;
        assert_eq!(fx.read_file_json().current_step, Some(step));
    }

    #[tokio::test]
    async fn cache_keeps_a_step_an_op_cached_meanwhile() {
        let fx = Fixture::new().await;
        let (file, _, _) = three_commit_stop(&fx).await;
        let head = fx.head().await;
        let unmerged = parse_unmerged(&fx.porcelain_v2().await);
        let classified = CurrentStep {
            sha: fx.rebase_head(),
            conflicts: fx.classify(&file.merge_base).await,
        };
        let mut cached = classified.clone();
        cached.conflicts[0].resolution = Some(RebaseKeep::Main);
        let mut with_cache = file.clone();
        with_cache.current_step = Some(cached.clone());
        fx.write_file_json(&with_cache);

        cache_if_unchanged(
            &fx.git,
            &fx.git_dir(),
            &file,
            &tokio::sync::Mutex::new(()),
            classified,
            &head,
            &unmerged,
        )
        .await;
        assert_eq!(fx.read_file_json().current_step, Some(cached));
    }

    #[tokio::test]
    async fn step_incomplete_without_conflicts_or_cache() {
        let fx = Fixture::new().await;
        let (_, first, _) = three_commit_stop(&fx).await;
        fx.run(&["checkout", "--theirs", "--", "a.txt"]).await;
        fx.run(&["add", "--", "a.txt"]).await;
        let state = fx.build(true).await;
        assert!(state.step_incomplete);
        assert!(state.conflicts.is_empty());
        assert_eq!(state.current_commit, Some(info(&first, "first")));
        assert_eq!(fx.read_file_json().current_step, None);
    }

    #[tokio::test]
    async fn step_complete_with_matching_cache() {
        let fx = Fixture::new().await;
        three_commit_stop(&fx).await;
        fx.build(true).await;
        fx.run(&["checkout", "--theirs", "--", "a.txt"]).await;
        fx.run(&["add", "--", "a.txt"]).await;
        let state = fx.build(true).await;
        assert!(!state.step_incomplete);
        assert_eq!(state.conflicts.len(), 1);
        assert!(state.conflicts[0].resolved);
    }

    #[tokio::test]
    async fn step_incomplete_with_cache_of_another_step() {
        let fx = Fixture::new().await;
        let (mut file, _, main) = three_commit_stop(&fx).await;
        fx.run(&["checkout", "--theirs", "--", "a.txt"]).await;
        fx.run(&["add", "--", "a.txt"]).await;
        file.current_step = Some(CurrentStep {
            sha: main.clone(),
            conflicts: vec![first_step_conflict(&main, &main)],
        });
        fx.write_file_json(&file);
        let state = fx.build(true).await;
        assert!(state.step_incomplete);
        assert!(state.conflicts.is_empty());
    }

    #[tokio::test]
    async fn in_progress_dropped_commits_exclude_remaining() {
        let fx = Fixture::new().await;
        fx.commit_file("a.txt", b"base\n", "base change").await;
        fx.branch(BRANCH).await;
        let picked = fx.commit_file("p.txt", b"picked\n", "picked").await;
        fx.commit_file("a.txt", b"branch\n", "conflicting").await;
        fx.commit_file("y.txt", b"y\n", "later").await;
        fx.push(BRANCH).await;
        fx.checkout("main").await;
        fx.run(&["cherry-pick", &picked]).await;
        fx.commit_file("a.txt", b"main\n", "main change").await;
        fx.push("main").await;
        fx.checkout(BRANCH).await;
        let file = fx.guided_file("main").await;
        fx.write_file_json(&file);
        assert_eq!(fx.start_raw_rebase(&file.onto).await, 1);
        let state = fx.build(false).await;
        assert_eq!(state.state, RebaseStateKind::InProgress);
        assert_eq!(state.dropped_commits, vec![dropped(&picked, "picked")]);
    }

    // Pure helpers

    #[test]
    fn parses_unmerged_records() {
        let porcelain = [
            "1 .M N... 100644 100644 100644 aaaa bbbb plain.txt",
            "2 R. N... 100644 100644 100644 aaaa bbbb R100 new name.txt",
            "u UU old name.txt",
            "u UU N... 100644 100644 100644 100644 h1 h2 h3 Content/a b/c d.txt",
            "? untracked file.txt",
            "! ignored.txt",
            "u DU N... 100644 000000 100644 100644 h1 h2 h3 gone.txt",
            "u UU S... 160000 160000 160000 160000 h1 h2 h3 Plugins/Sub",
            "",
        ]
        .join("\0");
        let entries = parse_unmerged(&porcelain);
        assert_eq!(
            entries,
            vec![
                unmerged("UU", "N...", "Content/a b/c d.txt"),
                unmerged("DU", "N...", "gone.txt"),
                unmerged("UU", "S...", "Plugins/Sub"),
            ]
        );
        assert_eq!(
            classify_entry(&entries[0]),
            (ConflictKind::Content, None, None)
        );
        assert_eq!(
            classify_entry(&entries[2]),
            (
                ConflictKind::Unsupported,
                None,
                Some(UNRESOLVABLE.to_string())
            )
        );
        assert_eq!(parse_untracked(&porcelain), vec!["untracked file.txt"]);
        assert!(parse_unmerged("").is_empty());
    }

    fn unmerged(xy: &str, sub: &str, path: &str) -> UnmergedEntry {
        UnmergedEntry {
            xy: xy.to_string(),
            sub: sub.to_string(),
            path: path.to_string(),
        }
    }

    #[test]
    fn rename_original_path_is_not_a_record() {
        // The original path of a `2` record is consumed even when it looks like a record.
        let porcelain = [
            "2 R. N... 100644 100644 100644 aaaa bbbb R100 new.txt",
            "u UU N... 100644 100644 100644 100644 h1 h2 h3 fake.txt",
            "u XY N... 000000 100644 100644 100644 h1 h2 h3 real.txt",
        ]
        .join("\0");
        let entries = parse_unmerged(&porcelain);
        assert_eq!(entries, vec![unmerged("XY", "N...", "real.txt")]);
        assert_eq!(
            classify_xy(&entries[0].xy),
            (
                ConflictKind::Unsupported,
                None,
                Some(UNRESOLVABLE.to_string())
            )
        );
    }

    #[test]
    fn classifies_xy_codes() {
        use ConflictKind::{Content, ModifyDelete, Unsupported};
        use ConflictSideName::{Branch, Main};
        assert_eq!(classify_xy("UU"), (Content, None, None));
        assert_eq!(classify_xy("DU"), (ModifyDelete, Some(Main), None));
        assert_eq!(classify_xy("UD"), (ModifyDelete, Some(Branch), None));
        assert_eq!(
            classify_xy("AA"),
            (Unsupported, None, Some(BOTH_ADDED.to_string()))
        );
        for xy in ["AU", "UA", "DD"] {
            assert_eq!(
                classify_xy(xy),
                (Unsupported, None, Some(RENAMED_DIFFERENTLY.to_string()))
            );
        }
        for xy in ["XY", "", "U", "MM"] {
            assert_eq!(
                classify_xy(xy),
                (Unsupported, None, Some(UNRESOLVABLE.to_string()))
            );
        }
    }

    #[test]
    fn parses_commit_info() {
        let named = |sha: &str, author: &str, subject: &str| RebaseCommitInfo {
            sha: sha.to_string(),
            subject: subject.to_string(),
            author: author.to_string(),
        };
        assert_eq!(
            parse_commit_info("abc\u{1f}Jane Doe\u{1f}Fix: a\u{1f}b\n"),
            Some(named("abc", "Jane Doe", "Fix: a\u{1f}b"))
        );
        assert_eq!(
            parse_commit_info("abc\u{1f}Jane\u{1f}"),
            Some(named("abc", "Jane", ""))
        );
        assert_eq!(parse_commit_info(""), None);
        assert_eq!(parse_commit_info("abc"), None);
    }

    pub(crate) fn original(sha: &str, email: &str, date: &str, subject: &str) -> OriginalCommit {
        OriginalCommit {
            sha: sha.to_string(),
            subject: subject.to_string(),
            author_email: email.to_string(),
            author_date: date.to_string(),
        }
    }

    fn replayed(email: &str, date: &str, subject: &str) -> (String, String, String) {
        (email.to_string(), date.to_string(), subject.to_string())
    }

    fn dropped(sha: &str, subject: &str) -> DroppedCommit {
        DroppedCommit {
            sha: sha.to_string(),
            subject: subject.to_string(),
        }
    }

    #[test]
    fn dropped_commit_rule() {
        let originals = vec![
            original("a1", "a@x", "100 +0000", "already on main"),
            original("b2", "a@x", "200 +0000", "kept"),
            original("c3", "a@x", "300 +0000", "emptied by resolution"),
            original("d4", "a@x", "400 +0000", "not replayed yet"),
        ];
        let remaining: HashSet<String> = ["d4".to_string()].into();
        assert_eq!(
            dropped_commits(
                &originals,
                &remaining,
                &[replayed("a@x", "200 +0000", "kept")]
            ),
            vec![
                dropped("a1", "already on main"),
                dropped("c3", "emptied by resolution"),
            ]
        );
    }

    #[test]
    fn dropped_commits_match_as_a_multiset() {
        let originals = vec![
            original("a1", "a@x", "100 +0000", "wip"),
            original("b2", "a@x", "100 +0000", "wip"),
            original("c3", "a@x", "100 +0000", "wip"),
        ];
        let replays = [
            replayed("a@x", "100 +0000", "wip"),
            replayed("a@x", "100 +0000", "wip"),
        ];
        assert_eq!(
            dropped_commits(&originals, &HashSet::new(), &replays),
            vec![dropped("c3", "wip")]
        );
    }

    #[test]
    fn nothing_dropped_when_every_commit_replays() {
        let originals = vec![
            original("a1", "a@x", "100 +0000", "one"),
            original("b2", "b@x", "200 +0100", "two"),
        ];
        let replays = [
            replayed("b@x", "200 +0100", "two"),
            replayed("a@x", "100 +0000", "one"),
        ];
        assert!(dropped_commits(&originals, &HashSet::new(), &replays).is_empty());
        // Commits still to be replayed are never dropped, even with nothing replayed.
        let remaining: HashSet<String> = ["a1".to_string(), "b2".to_string()].into();
        assert!(dropped_commits(&originals, &remaining, &[]).is_empty());
    }

    fn push_output(flag: &str, dst: &str, summary: &str) -> String {
        format!("To https://example.com/r.git\n{flag}\tHEAD:{dst}\t{summary}\nDone\n")
    }

    #[test]
    fn parses_push_porcelain() {
        let x = "refs/heads/x";
        assert_eq!(
            parse_push_porcelain(
                &push_output("+", x, "b12baab...cc9140b (forced update)"),
                "x"
            ),
            PushOutcome::Pushed
        );
        assert_eq!(
            parse_push_porcelain(&push_output(" ", x, "b12baab..cc9140b"), "x"),
            PushOutcome::Pushed
        );
        assert_eq!(
            parse_push_porcelain(&push_output("=", x, "[up to date]"), "x"),
            PushOutcome::Pushed
        );
        assert_eq!(
            parse_push_porcelain(&push_output("!", x, "[rejected] (stale info)"), "x"),
            PushOutcome::LeaseRejected
        );
        assert_eq!(
            parse_push_porcelain(
                &push_output("!", x, "[remote rejected] (protected branch hook declined)"),
                "x"
            ),
            PushOutcome::Rejected("[remote rejected] (protected branch hook declined)".to_string())
        );
        assert_eq!(
            parse_push_porcelain(
                &push_output("+", "refs/heads/y", "a...b (forced update)"),
                "x"
            ),
            PushOutcome::NoRefLine
        );
        assert_eq!(
            parse_push_porcelain(&push_output("*", x, "[new branch]"), "x"),
            PushOutcome::Pushed
        );
        assert_eq!(parse_push_porcelain("", "x"), PushOutcome::NoRefLine);
        assert_eq!(
            parse_push_porcelain(
                "fatal: unable to access 'https://example.com/r.git/'\n",
                "x"
            ),
            PushOutcome::NoRefLine
        );
        assert_eq!(
            push_ref_status(&push_output(" ", x, "a..b"), "x"),
            Some((' ', "a..b".to_string()))
        );
        assert_eq!(
            parse_push_porcelain(
                &push_output("+", x, "a...b (forced update)").replace('\n', "\r\n"),
                "x"
            ),
            PushOutcome::Pushed
        );
    }

    #[test]
    fn push_porcelain_matches_the_branch_exactly() {
        for other in ["refs/heads/ax", "refs/heads/a/x", "refs/heads/x2"] {
            assert_eq!(
                parse_push_porcelain(&push_output("!", other, "[rejected] (stale info)"), "x"),
                PushOutcome::NoRefLine,
                "{other}"
            );
        }
        let both = push_output("!", "refs/heads/ax", "[rejected] (stale info)")
            + &push_output("+", "refs/heads/x", "a...b (forced update)");
        assert_eq!(parse_push_porcelain(&both, "x"), PushOutcome::Pushed);
    }

    #[test]
    fn backup_ref_timestamps() {
        let base = format!("{BACKUP_REF_PREFIX}{BRANCH}");
        assert_eq!(
            backup_ref_ts(&format!("{base}/1761060000")),
            Some(1761060000)
        );
        assert_eq!(
            backup_ref_ts(&format!("{base}/1761060000-rebased")),
            Some(1761060000)
        );
        assert_eq!(backup_ref_ts(&format!("{base}/notes")), None);
        assert_eq!(
            backup_ref_ts(&format!("{BACKUP_REF_PREFIX}feature/12/notes")),
            None
        );
    }

    #[test]
    fn prunes_refs_older_than_14_days() {
        let base = format!("{BACKUP_REF_PREFIX}{BRANCH}");
        let kept = format!("{base}/{}", NOW - 14 * DAY);
        let pruned = format!("{base}/{}", NOW - 14 * DAY - 1);
        let pruned_rebased = format!("{base}/{}-rebased", NOW - 30 * DAY);
        let refs = vec![
            kept,
            pruned.clone(),
            pruned_rebased.clone(),
            format!("{base}/{}", NOW + DAY),
            format!("{base}/notes"),
            format!("refs/heads/{}", NOW - 30 * DAY),
        ];
        assert_eq!(
            refs_to_prune(&refs, NOW, &[]),
            vec![pruned.clone(), pruned_rebased.clone()]
        );
        assert_eq!(
            refs_to_prune(&refs, NOW, std::slice::from_ref(&pruned)),
            vec![pruned_rebased]
        );
    }

    #[tokio::test]
    async fn create_backup_ref_never_overwrites() {
        let fx = Fixture::new().await;
        let first_sha = fx.head().await;
        let second_sha = fx.commit_file("b.txt", b"b\n", "second").await;

        let first = create_backup_ref(&fx.git, BRANCH, &first_sha, "", NOW)
            .await
            .unwrap();
        assert_eq!(first, format!("{BACKUP_REF_PREFIX}{BRANCH}/{NOW}"));
        assert_eq!(fx.sha(&first).await, first_sha);

        let second = create_backup_ref(&fx.git, BRANCH, &second_sha, "", NOW)
            .await
            .unwrap();
        assert_eq!(second, format!("{BACKUP_REF_PREFIX}{BRANCH}/{}", NOW + 1));
        assert_eq!(fx.sha(&second).await, second_sha);

        assert!(create_backup_ref(&fx.git, BRANCH, &second_sha, "", NOW)
            .await
            .is_err());
        assert_eq!(fx.sha(&first).await, first_sha);
        assert_eq!(fx.sha(&second).await, second_sha);

        let rebased = create_backup_ref(&fx.git, BRANCH, &second_sha, "-rebased", NOW)
            .await
            .unwrap();
        assert_eq!(
            rebased,
            format!("{BACKUP_REF_PREFIX}{BRANCH}/{NOW}-rebased")
        );
    }

    #[tokio::test]
    async fn prune_backup_refs_deletes_only_old_backups() {
        let fx = Fixture::new().await;
        let head = fx.head().await;
        let old = format!("{BACKUP_REF_PREFIX}{BRANCH}/{}", NOW - 15 * DAY);
        let new = format!("{BACKUP_REF_PREFIX}{BRANCH}/{NOW}");
        let notes = format!("{BACKUP_REF_PREFIX}{BRANCH}/notes");
        for refname in [&old, &new, &notes] {
            fx.run(&["update-ref", refname, &head]).await;
        }
        fx.run(&["branch", &format!("{}", NOW - 30 * DAY)]).await;
        let list_branches = [
            "for-each-ref",
            "--format=%(refname) %(objectname)",
            "refs/heads/",
        ];
        let branches_before = fx.out(&list_branches).await;

        prune_backup_refs(&fx.git, NOW, &[]).await.unwrap();

        let backups = fx
            .out(&["for-each-ref", "--format=%(refname)", BACKUP_REF_PREFIX])
            .await;
        assert_eq!(backups.lines().collect::<Vec<_>>(), vec![new, notes]);
        assert_eq!(fx.out(&list_branches).await, branches_before);
    }

    #[tokio::test]
    async fn prune_keeps_the_current_records_backup() {
        let fx = Fixture::new().await;
        let head = fx.head().await;
        let current = format!("{BACKUP_REF_PREFIX}{BRANCH}/{}", NOW - 30 * DAY);
        let other = format!("{BACKUP_REF_PREFIX}{BRANCH}/{}", NOW - 20 * DAY);
        for refname in [&current, &other] {
            fx.run(&["update-ref", refname, &head]).await;
        }

        prune_backup_refs(&fx.git, NOW, std::slice::from_ref(&current))
            .await
            .unwrap();

        let backups = fx
            .out(&["for-each-ref", "--format=%(refname)", BACKUP_REF_PREFIX])
            .await;
        assert_eq!(backups, current);
        assert_eq!(fx.sha(&current).await, head);
    }

    // Json IO

    pub(crate) fn sample_file() -> GuidedRebaseFile {
        let sha = |c: char| c.to_string().repeat(40);
        GuidedRebaseFile {
            version: 1,
            branch: BRANCH.to_string(),
            trunk: "main".to_string(),
            phase: GuidedRebasePhase::PushFailed,
            backup_ref: format!("{BACKUP_REF_PREFIX}{BRANCH}/{NOW}"),
            backup_sha: sha('a'),
            pre_rebase_origin_sha: sha('a'),
            onto: sha('b'),
            merge_base: sha('c'),
            original_commits: vec![original(&sha('d'), "a@x", "1761000000 +0200", "subject")],
            touches_cpp: true,
            engine_association: "{GUID}".to_string(),
            started_at: "2026-10-21T15:00:00Z".to_string(),
            push_failed_remote_sha: Some(sha('e')),
            current_step: None,
        }
    }

    /// A well-formed record for `branch`; its shas name no real commits.
    pub(crate) fn sample_file_for(branch: &str) -> GuidedRebaseFile {
        GuidedRebaseFile {
            branch: branch.to_string(),
            backup_ref: format!("{BACKUP_REF_PREFIX}{branch}/{NOW}"),
            ..sample_file()
        }
    }

    #[test]
    fn json_round_trip_and_wire_names() {
        let dir = tempfile::tempdir().unwrap();
        let file = sample_file();
        write_rebase_file(dir.path(), &file).unwrap();
        assert!(matches!(read_rebase_file(dir.path()), RebaseFileRead::Ok(read) if *read == file));
        assert!(!tmp_path(dir.path()).exists());

        let value: serde_json::Value =
            serde_json::from_slice(&fs::read(json_path(dir.path())).unwrap()).unwrap();
        assert_eq!(value["version"], 1);
        assert_eq!(value["backupRef"], file.backup_ref.as_str());
        assert_eq!(value["backupSha"], file.backup_sha.as_str());
        assert_eq!(
            value["preRebaseOriginSha"],
            file.pre_rebase_origin_sha.as_str()
        );
        assert_eq!(value["mergeBase"], file.merge_base.as_str());
        assert_eq!(value["phase"], "push_failed");
        assert_eq!(value["currentStep"], serde_json::Value::Null);
        assert_eq!(value["originalCommits"][0]["authorEmail"], "a@x");
        assert_eq!(
            value["originalCommits"][0]["authorDate"],
            "1761000000 +0200"
        );
        assert_eq!(value["touchesCpp"], true);
        assert_eq!(value["engineAssociation"], "{GUID}");
        assert_eq!(value["startedAt"], "2026-10-21T15:00:00Z");
        assert_eq!(value["pushFailedRemoteSha"], "e".repeat(40).as_str());
        assert_eq!(
            serde_json::to_value(GuidedRebasePhase::Rebasing).unwrap(),
            "rebasing"
        );
    }

    #[test]
    fn malformed_fields_are_unreadable() {
        let dir = tempfile::tempdir().unwrap();
        let upper = "A".repeat(40);
        type Mutation = Box<dyn Fn(&mut GuidedRebaseFile)>;
        let mutations: Vec<(&str, Mutation)> = vec![
            ("short sha", Box::new(|f| f.onto = "abc123".to_string())),
            (
                "uppercase sha",
                Box::new(move |f| f.backup_sha.clone_from(&upper)),
            ),
            ("merge base", Box::new(|f| f.merge_base = "x".repeat(40))),
            (
                "origin sha",
                Box::new(|f| f.pre_rebase_origin_sha = "-".repeat(40)),
            ),
            (
                "push failed sha",
                Box::new(|f| f.push_failed_remote_sha = Some("1234".to_string())),
            ),
            (
                "current step sha",
                Box::new(|f| {
                    f.current_step = Some(CurrentStep {
                        sha: "HEAD".to_string(),
                        conflicts: vec![],
                    })
                }),
            ),
            (
                "original commit sha",
                Box::new(|f| f.original_commits[0].sha = "--all".to_string()),
            ),
            (
                "trunk",
                Box::new(|f| f.trunk = "--upload-pack=x".to_string()),
            ),
            (
                "backup ref of another branch",
                Box::new(|f| f.backup_ref = format!("{BACKUP_REF_PREFIX}{OTHER_BRANCH}/{NOW}")),
            ),
            (
                "backup ref outside the namespace",
                Box::new(|f| f.backup_ref = format!("refs/heads/{BRANCH}")),
            ),
        ];
        for (name, mutate) in mutations {
            let mut file = sample_file();
            mutate(&mut file);
            write_rebase_file(dir.path(), &file).unwrap();
            assert!(
                matches!(read_rebase_file(dir.path()), RebaseFileRead::Unreadable(_)),
                "{name}"
            );
        }

        let mut sha256 = sample_file();
        sha256.onto = "b".repeat(64);
        sha256.current_step = Some(CurrentStep {
            sha: "f".repeat(64),
            conflicts: vec![],
        });
        write_rebase_file(dir.path(), &sha256).unwrap();
        assert!(matches!(
            read_rebase_file(dir.path()),
            RebaseFileRead::Ok(_)
        ));
    }

    #[cfg(windows)]
    #[test]
    fn write_retries_while_the_record_is_held_open() {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_READ: u32 = 1;

        let dir = tempfile::tempdir().unwrap();
        write_rebase_file(dir.path(), &sample_file()).unwrap();
        let hold = || {
            fs::OpenOptions::new()
                .read(true)
                .share_mode(FILE_SHARE_READ)
                .open(json_path(dir.path()))
                .unwrap()
        };

        // Held for the whole retry window: the write fails and leaves the record as it was.
        let held = hold();
        let probe = dir.path().join("probe");
        fs::write(&probe, "x").unwrap();
        assert_eq!(
            fs::rename(&probe, json_path(dir.path()))
                .unwrap_err()
                .kind(),
            ErrorKind::PermissionDenied
        );
        let mut updated = sample_file();
        updated.touches_cpp = false;
        assert!(write_rebase_file(dir.path(), &updated).is_err());
        drop(held);
        assert!(
            matches!(read_rebase_file(dir.path()), RebaseFileRead::Ok(read) if *read == sample_file())
        );

        // Released during the retries: the write succeeds.
        let held = hold();
        let release = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(80));
            drop(held);
        });
        write_rebase_file(dir.path(), &updated).unwrap();
        release.join().unwrap();
        assert!(
            matches!(read_rebase_file(dir.path()), RebaseFileRead::Ok(read) if *read == updated)
        );
    }

    #[test]
    fn stale_tmp_file_does_not_break_writes() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(tmp_path(dir.path()), "{ half written").unwrap();
        let file = sample_file();
        write_rebase_file(dir.path(), &file).unwrap();
        assert!(matches!(read_rebase_file(dir.path()), RebaseFileRead::Ok(read) if *read == file));
        assert!(!tmp_path(dir.path()).exists());
    }

    #[test]
    fn unreadable_json() {
        let dir = tempfile::tempdir().unwrap();
        let read = || read_rebase_file(dir.path());
        assert!(matches!(read(), RebaseFileRead::Missing));
        delete_rebase_file(dir.path()).unwrap();

        fs::write(json_path(dir.path()), "{").unwrap();
        assert!(matches!(read(), RebaseFileRead::Unreadable(_)));

        let mut versioned = sample_file();
        versioned.version = 2;
        let mut bad_sha = sample_file();
        bad_sha.onto = "--upload-pack=x".to_string();
        let mut bad_branch = sample_file();
        bad_branch.branch = "-b".to_string();
        for file in [versioned, bad_sha, bad_branch] {
            write_rebase_file(dir.path(), &file).unwrap();
            assert!(matches!(read(), RebaseFileRead::Unreadable(_)), "{file:?}");
        }

        delete_rebase_file(dir.path()).unwrap();
        assert!(matches!(read(), RebaseFileRead::Missing));
    }

    // Read-only guarantee

    #[derive(Debug, PartialEq)]
    struct Snapshot {
        status: String,
        refs: String,
        index_mtime: SystemTime,
        json: Option<Vec<u8>>,
    }

    async fn snapshot(fx: &Fixture) -> Snapshot {
        Snapshot {
            status: fx.porcelain_v2().await,
            refs: fx
                .out(&["for-each-ref", "--format=%(refname) %(objectname)"])
                .await,
            index_mtime: fs::metadata(fx.git_dir().join("index"))
                .unwrap()
                .modified()
                .unwrap(),
            json: fx.json_bytes(),
        }
    }

    async fn assert_read_only(fx: &Fixture, kind: RebaseStateKind) {
        // Same content, new mtime: a status that took optional locks would rewrite the index.
        fx.write("seed.txt", b"seed\n");
        let before = snapshot(fx).await;
        assert_eq!(fx.build(false).await.state, kind);
        assert_eq!(snapshot(fx).await, before);
        assert!(!fx.git_dir().join("index.lock").exists());
    }

    #[tokio::test]
    async fn reading_in_progress_state_changes_nothing() {
        let fx = Fixture::new().await;
        three_commit_stop(&fx).await;
        fx.write("a.txt", b"edited by hand\n");
        assert_read_only(&fx, RebaseStateKind::InProgress).await;
    }

    #[tokio::test]
    async fn reading_aborted_outside_state_changes_nothing() {
        let fx = Fixture::new().await;
        diverge_on_a_txt(&fx).await;
        let file = fx.guided_file("main").await;
        fx.write_file_json(&file);
        assert_read_only(&fx, RebaseStateKind::Stale).await;
    }

    #[tokio::test]
    async fn reading_push_failed_state_changes_nothing() {
        let fx = Fixture::new().await;
        let mut file = fx.guided_complete().await;
        file.phase = GuidedRebasePhase::PushFailed;
        fx.write_file_json(&file);
        assert_read_only(&fx, RebaseStateKind::PushFailed).await;
    }
}
