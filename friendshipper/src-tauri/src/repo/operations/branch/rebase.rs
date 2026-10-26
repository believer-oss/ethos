use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::anyhow;
use axum::extract::State;
use axum::{async_trait, Json};
use tracing::{error, instrument, warn};

use super::cleanup::worktree_git_dirs_or;
use super::post_head_change::{run_post_head_change, PostHeadChangeCtx};
use super::rebase_guard::guard_sentence;
use super::rebase_state::{
    build_rebase_state, commit_info, create_backup_ref, delete_rebase_file, derive_rebase_state,
    ensure_current_step, is_ancestor, is_full_sha, parse_push_porcelain, parse_unmerged,
    parse_untracked, prune_backup_refs, read_git, read_rebase_file, rebase_dir_exists, rebase_head,
    refresh_resolved, replace_current_step, resolve_git_dir, rev_parse, set_resolution,
    status_porcelain, write_rebase_file, DerivedState, GuidedRebaseFile, GuidedRebasePhase,
    OriginalCommit, PushOutcome, RebaseFileRead, CPP_PATHSPECS, REBASE_FILE_LOCK,
    REBASE_FILE_VERSION, REBASE_OP_LOCK,
};
use super::{git_too_old_message, is_valid_branch_name, require_aws_and_storage, run_sequence};
use crate::engine::EngineProvider;
use crate::repo::operations::gh::submit::{find_files_with_conflict_markers, is_branch_mode};
use crate::repo::operations::validate::validate_repo_state;
use crate::repo::operations::StatusOp;
use crate::repo::RepoStatusRef;
use crate::state::AppState;
use ethos_core::clients::git::{self, GitVersionCheck};
use ethos_core::storage::ArtifactStorage;
use ethos_core::types::config::{AppConfigRef, RepoConfigRef, UProject};
use ethos_core::types::errors::CoreError;
use ethos_core::types::repo::{
    ConflictFile, ConflictKind, ConflictSideName, OpenConflictFileRequest, RebaseCommitInfo,
    RebaseFinishOutcome, RebaseFinishResponse, RebaseKeep, RebasePreflight, RebaseState,
    RebaseStateKind, ResolveConflictRequest, UnresolveConflictRequest,
};
use ethos_core::utils::process::describe_blocking_processes;
use ethos_core::worker::Task;
use ethos_core::AWSClient;

/// Every git call that may open an editor runs with these.
pub(crate) const REBASE_ENV: &[(&str, &str)] = &[
    ("GIT_EDITOR", "true"),
    ("GIT_SEQUENCE_EDITOR", "true"),
    ("LC_ALL", "C"),
];

/// Oldest git whose `update-index --unresolve` restores a conflict from the index's
/// resolve-undo data; older versions need `MERGE_HEAD`, which a rebase stop never writes.
pub(crate) const MIN_GUIDED_REBASE_GIT: (u32, u32) = (2, 43);

const PREFLIGHT_CPP_FILES: usize = 20;

const NO_REBASE_IN_PROGRESS: &str = "There is no rebase in progress.";
const STEP_INCOMPLETE: &str = "Git stopped on this commit without a conflict Friendshipper can resolve. Abort to restore your branch.";
const DISMISS_FIRST: &str = "Dismiss the previous rebase notice in Branch Management first.";
const EXTERNAL_REBASE: &str =
    "A rebase not started by Friendshipper is in progress. Resolve it in Diagnostics.";
const FEATURE_BRANCHES_ONLY: &str = "Rebase is only available on feature branches.";
const NO_TRUNK: &str = "The repo config does not define a valid trunkBranch.";
const DIRTY_TREE: &str = "Commit, snapshot, or revert your changes before rebasing.";
const UNSYNCED: &str = "Your branch has unpushed or unsynced commits — submit or Sync first.";
const UNRESOLVABLE_CONFLICT: &str =
    "This conflict can't be resolved in Friendshipper. Abort the rebase.";
const UNRESOLVABLE_STEP: &str =
    "This commit has a conflict Friendshipper can't resolve. Abort the rebase.";
const TEXT_ONLY: &str = "Mark resolved is only available for text files.";
const OP_RUNNING: &str = "A rebase operation is running. Try again when it finishes.";
const RESOLVE_EVERY_FILE: &str = "Resolve every file before continuing.";
const NOT_ADVANCING: &str = "The rebase did not advance; abort it to restore your branch.";
const NOTHING_TO_ABORT: &str = "There is no rebase in progress to abort.";
const NOTHING_TO_PUSH: &str = "There is no rebased branch waiting to be pushed.";
const REDO_ONLY_AFTER_PUSH_FAILED: &str =
    "Redo is only available after pushing the rebased branch failed.";
const RESET_ONLY_AFTER_REPLAY: &str =
    "Reset to backup is only available after a rebase finished replaying.";
const UNSTAGE_FIRST: &str = "Unstage your changes before triggering an editor build.";
const SYNC_FIRST: &str = "Sync before triggering an editor build.";
const NEW_COMMITS_ON_ORIGIN: &str =
    "Branch has new commits on origin — Sync, then trigger the editor build again.";
const OPERATION_IN_PROGRESS: &str = "Finish or abort the merge, cherry-pick, revert or rebase in progress before triggering an editor build.";
const EDITOR_BUILD_MESSAGE: &str = "[ci:editor] Rebuild editor after rebase";
const LISTED_PATHS: usize = 10;

fn input(message: impl Into<String>) -> CoreError {
    CoreError::Input(anyhow!(message.into()))
}

fn internal(message: impl Into<String>) -> CoreError {
    CoreError::Internal(anyhow!(message.into()))
}

async fn editor_check<T: EngineProvider>(engine: &T) -> Result<(), CoreError> {
    engine
        .check_ready_to_sync_repo()
        .await
        .map_err(CoreError::Input)
}

/// Refuses while `index.lock` exists; never waits for it, as a stale lock never goes away.
fn index_lock_check(git_dir: &Path) -> Result<(), CoreError> {
    let lock = git_dir.join("index.lock");
    if lock.exists() {
        return Err(input(format!(
            "Git's index is locked ({}). If no git command is running, delete that file and try again.",
            lock.display()
        )));
    }
    Ok(())
}

/// Refuses a git older than [`MIN_GUIDED_REBASE_GIT`].
fn git_version_gate(check: &GitVersionCheck) -> Result<(), CoreError> {
    let parsed = git::parse_git_version(&format!("git version {}", check.version));
    if !check.supported || parsed.is_none_or(|version| version < MIN_GUIDED_REBASE_GIT) {
        return Err(input(git_too_old_message(
            &check.version,
            MIN_GUIDED_REBASE_GIT,
        )));
    }
    Ok(())
}

/// `msg`, naming the processes running from the repo that may hold its files open.
fn enrich_error(repo_path: &Path, msg: &str) -> String {
    match describe_blocking_processes(repo_path) {
        Some(blocking) => format!("{msg} ({blocking} may be holding files open.)"),
        None => msg.to_string(),
    }
}

async fn derive(git: &git::Git, repo_path: &Path) -> Result<DerivedState, CoreError> {
    derive_rebase_state(git, repo_path)
        .await
        .map_err(|e| CoreError::Internal(e.context("Could not determine the rebase state")))
}

fn repo_path_of(app_config: &AppConfigRef) -> PathBuf {
    PathBuf::from(app_config.read().repo_path.clone())
}

fn engine_path_of(app_config: &AppConfigRef, repo_config: &RepoConfigRef) -> PathBuf {
    app_config
        .read()
        .load_engine_path_from_repo(&repo_config.read())
        .unwrap_or_default()
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

/// The sha `rev` names; Internal when it does not exist.
async fn required_sha(git: &git::Git, rev: &str) -> Result<String, CoreError> {
    rev_parse(git, rev)
        .await?
        .ok_or_else(|| internal(format!("{rev} does not exist.")))
}

/// `refs/heads/<branch>` when HEAD is a branch.
async fn head_ref(git: &git::Git) -> Result<Option<String>, CoreError> {
    let output = read_git(git, &["symbolic-ref", "-q", "HEAD"]).await?;
    Ok((output.exit_code == 0).then(|| output.stdout.trim().to_string()))
}

/// Paths under [`CPP_PATHSPECS`] changed between `from` and `to`.
async fn cpp_files(git: &git::Git, from: &str, to: &str) -> Result<Vec<String>, CoreError> {
    let mut args = vec!["diff", "--name-only", "-z", from, to, "--"];
    args.extend(CPP_PATHSPECS);
    let output = read_git(git, &args).await?;
    if output.exit_code != 0 {
        return Err(internal(format!(
            "Could not list C++ changes: {}",
            output.stderr.trim()
        )));
    }
    Ok(output
        .stdout
        .split('\0')
        .filter(|path| !path.is_empty())
        .map(str::to_string)
        .collect())
}

async fn merge_base(git: &git::Git, a: &str, b: &str) -> Result<String, CoreError> {
    let output = read_git(git, &["merge-base", a, b]).await?;
    let sha = output.stdout.trim();
    if output.exit_code != 0 || !is_full_sha(sha) {
        return Err(internal(format!(
            "Could not find the merge base of {a} and {b}: {}",
            output.stderr.trim()
        )));
    }
    Ok(sha.to_string())
}

/// The non-merge commits of `merge_base..head`, oldest first.
async fn original_commits(
    git: &git::Git,
    merge_base: &str,
    head: &str,
) -> Result<Vec<OriginalCommit>, CoreError> {
    let range = format!("{merge_base}..{head}");
    let output = read_git(
        git,
        &[
            "log",
            "--reverse",
            "--no-merges",
            "--date=raw",
            "--format=%H%x1f%ae%x1f%ad%x1f%s",
            &range,
            "--",
        ],
    )
    .await?;
    if output.exit_code != 0 {
        return Err(internal(format!(
            "Could not list the branch's commits: {}",
            output.stderr.trim()
        )));
    }
    Ok(output
        .stdout
        .lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.splitn(4, '\u{1f}').collect();
            match fields[..] {
                [sha, email, date, subject] if is_full_sha(sha) => Some(OriginalCommit {
                    sha: sha.to_string(),
                    subject: subject.to_string(),
                    author_email: email.to_string(),
                    author_date: date.to_string(),
                }),
                _ => None,
            }
        })
        .collect())
}

/// Fetches `<trunk>` (when given) and, when it exists on origin, `<branch>`; returns whether
/// it does. Never fetches without a refspec, which would fetch every configured ref.
async fn fetch_origin(
    git: &git::Git,
    trunk: Option<&str>,
    branch: &str,
) -> Result<bool, CoreError> {
    let fetch = |with_branch: bool| async move {
        let mut refspecs = vec![];
        if let Some(trunk) = trunk {
            refspecs.push(format!("+refs/heads/{trunk}:refs/remotes/origin/{trunk}"));
        }
        if with_branch {
            refspecs.push(format!("+refs/heads/{branch}:refs/remotes/origin/{branch}"));
        }
        if refspecs.is_empty() {
            return Ok(());
        }
        let mut args = vec![
            "fetch",
            "--no-auto-maintenance",
            "--no-write-fetch-head",
            "origin",
        ];
        args.extend(refspecs.iter().map(String::as_str));
        git.run(
            &args,
            git::Opts::new_with_complete_error().with_env(REBASE_ENV),
        )
        .await
    };

    let present = git.has_remote_branch_exact(branch).await?;
    match fetch(present).await {
        Ok(()) => Ok(present),
        // The branch can vanish between ls-remote and fetch.
        Err(e) if present && e.to_string().contains("couldn't find remote ref") => {
            fetch(false).await?;
            Ok(false)
        }
        Err(e) => Err(e.into()),
    }
}

/// The backup ref of every worktree's record (backup refs are shared by all worktrees). `None`
/// when a worktree or record can't be read, as pruning could then delete a backup in use.
async fn recorded_backup_refs(git: &git::Git) -> Option<Vec<String>> {
    let dirs = worktree_git_dirs_or(git, |common, e| {
        internal(format!(
            "Could not read the worktrees in {}: {e}",
            common.display()
        ))
    })
    .await
    .inspect_err(|e| warn!("{e}"))
    .ok()?;
    let _lock = REBASE_FILE_LOCK.lock().await;
    let mut keep = vec![];
    for dir in dirs {
        match read_rebase_file(&dir) {
            RebaseFileRead::Missing => {}
            RebaseFileRead::Ok(file) => keep.push(file.backup_ref),
            other => {
                warn!(
                    "The rebase record in {} can't be read: {other:?}",
                    dir.display()
                );
                return None;
            }
        }
    }
    Some(keep)
}

/// The checked-out feature branch.
async fn feature_branch(git: &git::Git, repo_config: &RepoConfigRef) -> Result<String, CoreError> {
    // The full ref: `--short` gives `heads/<branch>` when a tag has the same name.
    let head = head_ref(git).await?;
    match head.as_deref().and_then(|r| r.strip_prefix("refs/heads/")) {
        Some(branch)
            if is_valid_branch_name(branch) && is_branch_mode(&repo_config.read(), branch) =>
        {
            Ok(branch.to_string())
        }
        _ => Err(input(FEATURE_BRANCHES_ONLY)),
    }
}

/// Refuses modified, staged or untracked files.
async fn ensure_clean_tree(git: &git::Git) -> Result<(), CoreError> {
    let status = git.status(vec![]).await?;
    if status
        .lines()
        .any(|line| !line.is_empty() && !line.starts_with("## "))
    {
        return Err(input(DIRTY_TREE));
    }
    Ok(())
}

/// What `git push --porcelain` reported for the pushed branch.
struct PushResult {
    outcome: PushOutcome,
    stderr: String,
    exit_code: i32,
}

impl PushResult {
    fn failure(&self) -> String {
        if self.stderr.is_empty() {
            format!("Push failed (exit code {}).", self.exit_code)
        } else {
            format!("Push failed: {}", self.stderr)
        }
    }
}

/// Pushes `sha` to origin's `<branch>`, leased to origin's branch being exactly `expected`.
/// The guided rebase's only push.
async fn leased_push(
    git: &git::Git,
    branch: &str,
    sha: &str,
    expected: &str,
) -> Result<PushResult, CoreError> {
    if !is_valid_branch_name(branch) || !is_full_sha(sha) || !is_full_sha(expected) {
        return Err(internal(format!(
            "Refusing to push {sha} to {branch} leased to {expected}."
        )));
    }
    let lease = format!("--force-with-lease=refs/heads/{branch}:{expected}");
    let refspec = format!("{sha}:refs/heads/{branch}");
    let output = git
        .run_with_status(
            &[
                "push",
                "--porcelain",
                "--no-follow-tags",
                "--no-force-if-includes",
                &lease,
                "origin",
                &refspec,
            ],
            git::Opts::new_with_complete_error().with_env(REBASE_ENV),
        )
        .await?;
    Ok(PushResult {
        outcome: parse_push_porcelain(&output.stdout, branch),
        stderr: output.stderr.trim().to_string(),
        exit_code: output.exit_code,
    })
}

/// `git reset --keep <sha>`, which refuses rather than discard a local change. Ignored files
/// are not local changes: one that `sha` tracks is overwritten.
async fn reset_keep(git: &git::Git, repo_path: &Path, sha: &str) -> Result<(), CoreError> {
    if !is_full_sha(sha) {
        return Err(internal(format!("Refusing to reset to {sha}.")));
    }
    let output = git
        .run_with_status(
            &["reset", "--keep", sha],
            git::Opts::new_with_complete_error().with_env(REBASE_ENV),
        )
        .await?;
    if output.exit_code != 0 {
        let msg = format!("git reset --keep failed: {}", output.stderr.trim());
        return Err(internal(enrich_error(repo_path, &msg)));
    }
    Ok(())
}

/// Backs up the rebased `head` as `<ts>-rebased`, verified before anything moves.
async fn backup_rebased_head(
    git: &git::Git,
    branch: &str,
    head: &str,
) -> Result<String, CoreError> {
    let refname = create_backup_ref(git, branch, head, "-rebased", unix_now()).await?;
    if rev_parse(git, &refname).await?.as_deref() != Some(head) {
        return Err(internal(format!(
            "The backup {refname} of the rebased branch could not be verified; nothing was changed."
        )));
    }
    Ok(refname)
}

/// Marks `file`'s rebase as push failed, origin's branch then at `remote_sha`, if the record
/// on disk is still `file`.
async fn record_push_failed(
    git_dir: &Path,
    file: &GuidedRebaseFile,
    remote_sha: Option<String>,
) -> Result<(), CoreError> {
    let _lock = REBASE_FILE_LOCK.lock().await;
    match read_rebase_file(git_dir) {
        RebaseFileRead::Ok(on_disk) if *on_disk == *file => {}
        other => {
            return Err(internal(format!(
                "The rebase record changed during the push: {other:?}"
            )))
        }
    }
    let updated = GuidedRebaseFile {
        phase: GuidedRebasePhase::PushFailed,
        push_failed_remote_sha: remote_sha.filter(|sha| is_full_sha(sha)),
        ..file.clone()
    };
    write_rebase_file(git_dir, &updated)?;
    match read_rebase_file(git_dir) {
        RebaseFileRead::Ok(written) if *written == updated => Ok(()),
        other => Err(internal(format!(
            "The rebase record did not read back as written: {other:?}"
        ))),
    }
}

/// Origin's `<branch>` after fetching it; `None` when it no longer exists or can't be fetched.
async fn fetched_branch_tip(git: &git::Git, branch: &str) -> Option<String> {
    match fetch_origin(git, None, branch).await {
        Ok(true) => rev_parse(git, &format!("refs/remotes/origin/{branch}"))
            .await
            .inspect_err(|e| warn!("Could not read origin's {branch}: {e}"))
            .ok()
            .flatten(),
        Ok(false) => None,
        Err(e) => {
            warn!("Could not fetch {branch} after the push was rejected: {e}");
            None
        }
    }
}

/// The parents of commit `sha`.
async fn commit_parents(git: &git::Git, sha: &str) -> Result<Vec<String>, CoreError> {
    let output = read_git(git, &["rev-list", "--parents", "-n", "1", sha, "--"]).await?;
    let mut fields = output.stdout.split_whitespace().map(str::to_string);
    if output.exit_code != 0 || fields.next().as_deref() != Some(sha) {
        return Err(internal(format!(
            "Could not read the parents of {sha}: {}",
            output.stderr.trim()
        )));
    }
    Ok(fields.collect())
}

/// Drops the unpushed editor build commit `new`, whose tree is `head`'s, while HEAD is still
/// exactly that commit.
async fn undo_editor_build_commit(
    git: &git::Git,
    repo_path: &Path,
    branch: &str,
    new: &str,
    head: &str,
) -> Result<(), CoreError> {
    if rev_parse(git, "HEAD").await?.as_deref() != Some(new) {
        return Err(internal(format!(
            "The editor build commit could not be pushed and HEAD moved; check {branch} before retrying."
        )));
    }
    reset_keep(git, repo_path, head).await
}

/// The post-HEAD-change context, or why it can't be built (AWS may still be initializing).
fn post_ctx<T: EngineProvider>(
    state: &AppState<T>,
    aws: Result<(AWSClient, ArtifactStorage), CoreError>,
) -> Result<PostHeadChangeCtx<T>, String> {
    aws.map(|(aws_client, storage)| PostHeadChangeCtx::new(state, aws_client, storage))
        .map_err(|e| e.to_string())
}

/// Refreshes status, editor binaries and the engine after HEAD moved; never undoes the move.
async fn run_post<T: EngineProvider>(
    post: &Result<PostHeadChangeCtx<T>, String>,
    old_engine_association: Option<String>,
) -> Result<(), String> {
    match post {
        Ok(ctx) => run_post_head_change(ctx, old_engine_association)
            .await
            .map_err(|e| e.to_string()),
        Err(e) => Err(e.clone()),
    }
}

/// The text for a reset whose post-HEAD-change steps failed.
fn post_failure_note(branch: &str, e: &str) -> String {
    format!(
        "{branch} was reset, but updating editor binaries or engine failed: {}.",
        e.trim_end_matches('.')
    )
}

/// Runs a git command on one conflicted path, taken literally; a failure is Internal.
async fn run_on_path(
    git: &git::Git,
    repo_path: &Path,
    args: &[&str],
    path: &str,
) -> Result<(), CoreError> {
    let mut full_args = vec!["--literal-pathspecs"];
    full_args.extend_from_slice(args);
    full_args.extend(["--", path]);
    let output = git
        .run_with_status(
            &full_args,
            git::Opts::new_with_complete_error().with_env(REBASE_ENV),
        )
        .await?;
    if output.exit_code != 0 {
        let msg = format!(
            "git {} failed for {path}: {}",
            args.join(" "),
            output.stderr.trim()
        );
        return Err(internal(enrich_error(repo_path, &msg)));
    }
    Ok(())
}

async fn is_unmerged(git: &git::Git, path: &str, xy: Option<&str>) -> Result<bool, CoreError> {
    Ok(parse_unmerged(&status_porcelain(git).await?)
        .iter()
        .any(|entry| entry.path == path && xy.is_none_or(|xy| entry.xy == xy)))
}

/// The commit git stopped on, with its conflicts and their current `resolved`.
struct StoppedStep {
    file: GuidedRebaseFile,
    rebase_head: String,
    conflicts: Vec<ConflictFile>,
}

/// The step a resolve or undo acts on, its conflicts cached first so resolving every file never
/// leaves a step without a record of its conflicts. `None` when git stopped without a conflict
/// Friendshipper can resolve.
async fn stopped_step<T: EngineProvider>(
    git: &git::Git,
    repo_path: &Path,
    engine: &T,
    engine_path: &Path,
) -> Result<Option<StoppedStep>, CoreError> {
    let derived = derive(git, repo_path).await?;
    let file = match (derived.kind, derived.file) {
        (RebaseStateKind::InProgress, Some(file)) => file,
        _ => return Err(input(NO_REBASE_IN_PROGRESS)),
    };
    let Some(rebase_head) = rebase_head(&resolve_git_dir(repo_path)) else {
        return Ok(None);
    };
    let Some(step) =
        ensure_current_step(git, repo_path, engine, engine_path, &file, &rebase_head).await?
    else {
        return Ok(None);
    };
    let mut conflicts = step.conflicts;
    refresh_resolved(
        &mut conflicts,
        &parse_unmerged(&status_porcelain(git).await?),
    );
    Ok(Some(StoppedStep {
        file,
        rebase_head,
        conflicts,
    }))
}

/// What the Continue loop does once `rebase --continue` or `--skip` has returned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ContinueStep {
    /// Every commit is replayed.
    Completed,
    /// The rebase ended, but git reported an error.
    EndedWithError,
    /// Git stopped on the conflicts of a later commit.
    Stopped,
    /// Git stopped on the same commit because the resolution left it empty (git before 2.47).
    SkipEmpty,
    Failed,
}

/// Decides the next Continue step from git's exit code, whether a rebase dir and unmerged
/// paths exist, whether the index and working tree match HEAD, and whether HEAD and
/// REBASE_HEAD are where they were before the call. Skipping needs a clean tree, as `--skip`
/// discards every change to tracked files.
fn continue_outcome(
    exit_code: i32,
    rebase_dir: bool,
    unmerged: bool,
    clean_tree: bool,
    head_eq_h0: bool,
    rh_eq_x: bool,
) -> ContinueStep {
    match (rebase_dir, unmerged) {
        (false, _) if exit_code == 0 => ContinueStep::Completed,
        (false, _) => ContinueStep::EndedWithError,
        (true, true) => ContinueStep::Stopped,
        (true, false) if clean_tree && head_eq_h0 && rh_eq_x => ContinueStep::SkipEmpty,
        _ => ContinueStep::Failed,
    }
}

/// Whether neither the index nor the working tree differs from HEAD in a tracked file.
async fn tree_matches_head(git: &git::Git) -> Result<bool, CoreError> {
    for args in [
        &["diff", "--cached", "--quiet", "HEAD", "--"][..],
        &["diff", "--quiet", "--"][..],
    ] {
        if read_git(git, args).await?.exit_code != 0 {
            return Ok(false);
        }
    }
    Ok(true)
}

/// The paths a `git diff --name-only -z` lists.
async fn diff_paths(git: &git::Git, args: &[&str]) -> Result<Vec<String>, CoreError> {
    let mut full_args = vec!["diff", "--name-only", "-z", "--no-renames"];
    full_args.extend_from_slice(args);
    full_args.push("--");
    let output = read_git(git, &full_args).await?;
    if output.exit_code != 0 {
        return Err(internal(format!(
            "git diff failed: {}",
            output.stderr.trim()
        )));
    }
    Ok(output
        .stdout
        .split('\0')
        .filter(|path| !path.is_empty())
        .map(str::to_string)
        .collect())
}

/// Refuses a step whose resolved files are not exactly what Continue would commit: edits left
/// unstaged, staged files that are neither conflicts nor part of the replayed commit, or
/// leftover conflict markers.
async fn check_resolutions(
    git: &git::Git,
    state: &RebaseState,
    rebase_head: &str,
) -> Result<(), CoreError> {
    let in_step = |path: &String| state.conflicts.iter().any(|c| &c.path == path);

    // Unstaged edits are left out of the commit, and `--skip` discards them.
    let (edited_conflicts, edited_others): (Vec<String>, Vec<String>) =
        state.unstaged_edits.iter().cloned().partition(in_step);
    let mut refusals = vec![];
    if !edited_conflicts.is_empty() {
        refusals.push(format!(
            "These files changed after you resolved them; undo and resolve them again before continuing: {}.",
            listed_paths(&edited_conflicts)
        ));
    }
    if !edited_others.is_empty() {
        refusals.push(format!(
            "These files were edited outside the conflict; revert them outside Friendshipper before continuing (aborting discards the edits): {}.",
            listed_paths(&edited_others)
        ));
    }
    if !refusals.is_empty() {
        return Err(input(refusals.join(" ")));
    }

    // Anything the commit brings differs between HEAD and REBASE_HEAD, renames included.
    let brought = diff_paths(git, &["HEAD", rebase_head]).await?;
    let foreign: Vec<String> = diff_paths(git, &["--cached", "HEAD"])
        .await?
        .into_iter()
        .filter(|path| !in_step(path) && !brought.contains(path))
        .collect();
    if !foreign.is_empty() {
        return Err(input(format!(
            "These files are staged but not part of this commit; unstage them outside Friendshipper before continuing: {}.",
            listed_paths(&foreign)
        )));
    }

    // Keep main or branch takes a committed side as it is.
    let hand_merged: Vec<String> = state
        .conflicts
        .iter()
        .filter(|c| {
            c.kind == ConflictKind::Content
                && c.is_text
                && matches!(c.resolution, None | Some(RebaseKeep::Manual))
        })
        .map(|c| c.path.clone())
        .collect();
    let marked = find_files_with_conflict_markers(git, &hand_merged).await?;
    if !marked.is_empty() {
        return Err(input(format!(
            "Remove the conflict markers from {} before continuing.",
            listed_paths(&marked)
        )));
    }
    Ok(())
}

/// The first [`LISTED_PATHS`] of `paths`, comma-separated.
fn listed_paths(paths: &[String]) -> String {
    let listed: Vec<&str> = paths
        .iter()
        .take(LISTED_PATHS)
        .map(String::as_str)
        .collect();
    listed.join(", ")
}

/// The absolute path of a repo-relative `/`-separated path, built component by component.
/// `None` for anything that could leave the repo: empty, `.`/`..`, absolute or drive-prefixed
/// components, or backslashes.
pub(crate) fn conflict_abs_path(repo_path: &Path, rel: &str) -> Option<PathBuf> {
    if rel.is_empty() {
        return None;
    }
    let mut abs = repo_path.to_path_buf();
    for part in rel.split('/') {
        if part.contains([':', '\\']) {
            return None;
        }
        let mut components = Path::new(part).components();
        match (components.next(), components.next()) {
            (Some(Component::Normal(_)), None) => abs.push(part),
            _ => return None,
        }
    }
    Some(abs)
}

/// Windows' default `PATHEXT`.
const DEFAULT_PATHEXT: &str = ".COM;.EXE;.BAT;.CMD;.VBS;.VBE;.JS;.JSE;.WSF;.WSH;.MSC";

/// Extensions Windows runs or follows rather than opens, besides `PATHEXT`.
const RUNNABLE_EXTENSIONS: &[&str] = &[
    "bat", "cmd", "com", "exe", "msi", "vbs", "vbe", "js", "jse", "wsf", "wsh", "ps1", "psm1",
    "py", "pyw", "hta", "msc", "lnk", "url", "reg", "scr", "cpl", "jar",
];

/// Whether opening `path` with its default handler would run it instead of showing it.
/// Windows ignores trailing dots and spaces in file names, so they are ignored here too.
fn windows_would_run(path: &str, pathext: Option<&str>) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    let name = name.trim_end_matches(['.', ' ']);
    let Some((_, extension)) = name.rsplit_once('.') else {
        return false;
    };
    let extension = extension.to_ascii_lowercase();
    RUNNABLE_EXTENSIONS.contains(&extension.as_str())
        || pathext
            .unwrap_or(DEFAULT_PATHEXT)
            .split(';')
            .map(|ext| ext.trim().trim_start_matches('.').to_ascii_lowercase())
            .any(|ext| !ext.is_empty() && ext == extension)
}

#[derive(Clone)]
pub struct RebaseStartOp<T> {
    pub git_client: git::Git,
    pub engine: T,
    pub app_config: AppConfigRef,
    pub repo_config: RepoConfigRef,
    pub repo_status: RepoStatusRef,
    pub sync_phase_tx: std::sync::mpsc::Sender<String>,
    pub trunk: String,
    pub github_username: String,
}

impl<T> RebaseStartOp<T>
where
    T: EngineProvider,
{
    fn emit_phase(&self, phase: &str) {
        let _ = self.sync_phase_tx.send(phase.to_string());
    }

    fn status_op(&self) -> StatusOp<T> {
        StatusOp {
            git_client: self.git_client.clone(),
            github_username: self.github_username.clone(),
            repo_status: self.repo_status.clone(),
            app_config: self.app_config.clone(),
            repo_config: self.repo_config.clone(),
            engine: self.engine.clone(),
            aws_client: None,
            storage: None,
            allow_offline_communication: false,
            skip_display_names: true,
            skip_engine_update: true,
            // internal pre-operation refresh, not a user-initiated one
            fetch_first: false,
        }
    }

    fn engine_association(&self, repo_path: &Path) -> String {
        let uproject_path = repo_path.join(self.repo_config.read().uproject_path.clone());
        match UProject::load(&uproject_path) {
            Ok(uproject) => uproject.engine_association,
            Err(e) => {
                warn!("Could not read the engine association before rebasing: {e}");
                String::new()
            }
        }
    }

    /// Writes the record and reads it back, so every later step can trust it.
    async fn write_record(&self, git_dir: &Path, file: &GuidedRebaseFile) -> Result<(), CoreError> {
        let _lock = REBASE_FILE_LOCK.lock().await;
        write_rebase_file(git_dir, file)?;
        match read_rebase_file(git_dir) {
            RebaseFileRead::Ok(written) if *written == *file => Ok(()),
            other => {
                if let Err(e) = delete_rebase_file(git_dir) {
                    error!("Could not delete the rebase record that failed to verify: {e}");
                }
                Err(internal(format!(
                    "The rebase record did not read back as written: {other:?}"
                )))
            }
        }
    }

    /// Start's gates, record and rebase, after the idle check. A redo passes the `previous`
    /// record: its backup is never pruned, and its engine association is kept, as the installed
    /// engine still matches it.
    async fn start_rebase_inner(
        &self,
        previous: Option<&GuidedRebaseFile>,
    ) -> Result<(), CoreError> {
        let git = &self.git_client;
        let repo_path = repo_path_of(&self.app_config);
        let git_dir = resolve_git_dir(&repo_path);

        editor_check(&self.engine).await?;
        index_lock_check(&git_dir)?;
        git_version_gate(&git.check_min_version().await?)?;

        let branch = feature_branch(git, &self.repo_config).await?;
        let trunk = self.trunk.clone();
        if !is_valid_branch_name(&trunk) {
            return Err(input(NO_TRUNK));
        }

        self.status_op().execute().await?;
        validate_repo_state(&repo_path, &self.repo_status.read())?;
        ensure_clean_tree(git).await?;

        self.emit_phase("Fetching latest");
        if !fetch_origin(git, Some(&trunk), &branch).await? {
            return Err(input(UNSYNCED));
        }
        let head = required_sha(git, "HEAD").await?;
        let origin_branch = rev_parse(git, &format!("refs/remotes/origin/{branch}")).await?;
        if origin_branch.as_deref() != Some(head.as_str()) {
            return Err(input(UNSYNCED));
        }
        let onto = required_sha(git, &format!("refs/remotes/origin/{trunk}")).await?;
        if is_ancestor(git, &onto, &head).await? {
            return Err(input(format!(
                "{branch} already contains the latest {trunk}."
            )));
        }

        let merge_base = merge_base(git, &onto, &head).await?;
        let original_commits = original_commits(git, &merge_base, &head).await?;
        let touches_cpp = !cpp_files(git, &merge_base, &head).await?.is_empty();
        let engine_association = match previous {
            Some(previous) => previous.engine_association.clone(),
            None => self.engine_association(&repo_path),
        };

        let now = unix_now();
        match recorded_backup_refs(git).await {
            Some(mut keep) => {
                keep.extend(previous.map(|previous| previous.backup_ref.clone()));
                prune_backup_refs(git, now, &keep).await?
            }
            None => warn!("Not pruning old backup refs: a worktree's rebase record can't be read"),
        }
        let backup_ref = create_backup_ref(git, &branch, &head, "", now).await?;

        let file = GuidedRebaseFile {
            version: REBASE_FILE_VERSION,
            branch: branch.clone(),
            trunk,
            phase: GuidedRebasePhase::Rebasing,
            backup_ref,
            backup_sha: head.clone(),
            pre_rebase_origin_sha: head.clone(),
            onto: onto.clone(),
            merge_base,
            original_commits,
            touches_cpp,
            engine_association,
            started_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            push_failed_remote_sha: None,
            current_step: None,
        };
        self.write_record(&git_dir, &file).await?;

        self.emit_phase("Replaying commits");
        let (exit_code, stderr) = match git
            .run_with_status(
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
                    &onto,
                ],
                git::Opts::new_with_complete_error().with_env(REBASE_ENV),
            )
            .await
        {
            Ok(output) => (output.exit_code, output.stderr.trim().to_string()),
            Err(e) => (-1, e.to_string()),
        };

        let rebasing = rebase_dir_exists(&git_dir);
        if exit_code == 0 && !rebasing {
            return Ok(());
        }

        let branch_ref = format!("refs/heads/{branch}");
        if exit_code != 0
            && !rebasing
            && rev_parse(git, "HEAD").await?.as_deref() == Some(head.as_str())
            && head_ref(git).await?.as_deref() == Some(branch_ref.as_str())
        {
            // Nothing happened: the record would only describe a rebase that never ran.
            let _lock = REBASE_FILE_LOCK.lock().await;
            delete_rebase_file(&git_dir)?;
            let msg = format!("Rebase could not start: {stderr}");
            return Err(internal(enrich_error(&repo_path, &msg)));
        }

        if exit_code != 0
            && git_dir.join("rebase-merge").is_dir()
            && !parse_unmerged(&status_porcelain(git).await?).is_empty()
        {
            if let Some(rebase_head) = rebase_head(&git_dir) {
                let engine_path = engine_path_of(&self.app_config, &self.repo_config);
                ensure_current_step(
                    git,
                    &repo_path,
                    &self.engine,
                    &engine_path,
                    &file,
                    &rebase_head,
                )
                .await?;
                return Ok(());
            }
        }

        let msg = if stderr.is_empty() {
            format!("git rebase stopped unexpectedly (exit code {exit_code}).")
        } else {
            stderr
        };
        Err(internal(enrich_error(&repo_path, &msg)))
    }
}

#[async_trait]
impl<T> Task for RebaseStartOp<T>
where
    T: EngineProvider,
{
    async fn execute(&self) -> Result<(), CoreError> {
        let _op = REBASE_OP_LOCK.lock().await;
        self.emit_phase("Checking repository status");
        let derived = derive(&self.git_client, &repo_path_of(&self.app_config)).await?;
        match derived.kind {
            RebaseStateKind::Idle => {}
            RebaseStateKind::Stale => return Err(input(DISMISS_FIRST)),
            RebaseStateKind::External => return Err(input(EXTERNAL_REBASE)),
            kind => return Err(input(guard_sentence(kind).unwrap_or(DISMISS_FIRST))),
        }

        self.start_rebase_inner(None).await
    }

    fn get_name(&self) -> String {
        String::from("RebaseStartOp")
    }
}

#[derive(Clone)]
pub struct RebaseResolveOp<T> {
    pub git_client: git::Git,
    pub engine: T,
    pub app_config: AppConfigRef,
    pub repo_config: RepoConfigRef,
    pub repo_status: RepoStatusRef,
    pub sync_phase_tx: std::sync::mpsc::Sender<String>,
    pub path: String,
    pub keep: RebaseKeep,
}

#[async_trait]
impl<T> Task for RebaseResolveOp<T>
where
    T: EngineProvider,
{
    async fn execute(&self) -> Result<(), CoreError> {
        let _op = REBASE_OP_LOCK.lock().await;
        let _ = self.sync_phase_tx.send("Resolving conflict".to_string());
        let git = &self.git_client;
        let repo_path = repo_path_of(&self.app_config);
        let git_dir = resolve_git_dir(&repo_path);
        let path = self.path.as_str();

        editor_check(&self.engine).await?;
        index_lock_check(&git_dir)?;
        let engine_path = engine_path_of(&self.app_config, &self.repo_config);
        let step = stopped_step(git, &repo_path, &self.engine, &engine_path)
            .await?
            .ok_or_else(|| input(STEP_INCOMPLETE))?;
        let conflict = step
            .conflicts
            .iter()
            .find(|c| c.path == path)
            .ok_or_else(|| input(format!("{path} is not a conflict in this step.")))?;
        if conflict.resolved {
            return Err(input(format!(
                "{path} is already resolved. Undo it first to choose again."
            )));
        }
        if conflict.kind == ConflictKind::Unsupported {
            return Err(input(UNRESOLVABLE_CONFLICT));
        }
        if step
            .conflicts
            .iter()
            .any(|c| c.kind == ConflictKind::Unsupported)
        {
            return Err(input(UNRESOLVABLE_STEP));
        }
        if self.keep == RebaseKeep::Manual
            && !(conflict.kind == ConflictKind::Content && conflict.is_text)
        {
            return Err(input(TEXT_ONLY));
        }

        // During a rebase "ours" is main and "theirs" the replayed branch commit.
        match (conflict.kind, conflict.deleted_on, self.keep) {
            (ConflictKind::Content, _, RebaseKeep::Main)
            | (ConflictKind::ModifyDelete, Some(ConflictSideName::Branch), RebaseKeep::Main) => {
                run_on_path(git, &repo_path, &["checkout", "--ours"], path).await?;
                run_on_path(git, &repo_path, &["add"], path).await?;
            }
            (ConflictKind::Content, _, RebaseKeep::Branch)
            | (ConflictKind::ModifyDelete, Some(ConflictSideName::Main), RebaseKeep::Branch) => {
                run_on_path(git, &repo_path, &["checkout", "--theirs"], path).await?;
                run_on_path(git, &repo_path, &["add"], path).await?;
            }
            (ConflictKind::Content, _, RebaseKeep::Manual) => {
                let marked = find_files_with_conflict_markers(git, &[path.to_string()]).await?;
                if !marked.is_empty() {
                    return Err(input(format!("{path} still contains conflict markers.")));
                }
                run_on_path(git, &repo_path, &["add"], path).await?;
            }
            // The kept side is the one that deleted the file.
            (ConflictKind::ModifyDelete, Some(ConflictSideName::Main), RebaseKeep::Main)
            | (ConflictKind::ModifyDelete, Some(ConflictSideName::Branch), RebaseKeep::Branch) => {
                run_on_path(git, &repo_path, &["rm", "--quiet"], path).await?;
            }
            _ => return Err(input(UNRESOLVABLE_CONFLICT)),
        }

        if is_unmerged(git, path, None).await? {
            return Err(internal(format!("Could not resolve {path}.")));
        }
        set_resolution(
            &git_dir,
            &step.file,
            &step.rebase_head,
            path,
            Some(self.keep),
        )
        .await?;
        Ok(())
    }

    fn get_name(&self) -> String {
        String::from("RebaseResolveOp")
    }
}

#[derive(Clone)]
pub struct RebaseUnresolveOp<T> {
    pub git_client: git::Git,
    pub engine: T,
    pub app_config: AppConfigRef,
    pub repo_config: RepoConfigRef,
    pub repo_status: RepoStatusRef,
    pub sync_phase_tx: std::sync::mpsc::Sender<String>,
    pub path: String,
}

#[async_trait]
impl<T> Task for RebaseUnresolveOp<T>
where
    T: EngineProvider,
{
    async fn execute(&self) -> Result<(), CoreError> {
        let _op = REBASE_OP_LOCK.lock().await;
        let _ = self.sync_phase_tx.send("Restoring conflict".to_string());
        let git = &self.git_client;
        let repo_path = repo_path_of(&self.app_config);
        let git_dir = resolve_git_dir(&repo_path);
        let path = self.path.as_str();
        let no_resolution = || input(format!("{path} has no resolution to undo in this step."));

        editor_check(&self.engine).await?;
        index_lock_check(&git_dir)?;
        let engine_path = engine_path_of(&self.app_config, &self.repo_config);
        let step = stopped_step(git, &repo_path, &self.engine, &engine_path)
            .await?
            .ok_or_else(no_resolution)?;
        // A resolved file without a recorded resolution (staged outside Friendshipper, or the
        // record could not be updated) is undone too when git kept its resolve-undo data.
        let conflict = step
            .conflicts
            .iter()
            .find(|c| c.path == path && c.resolved)
            .ok_or_else(no_resolution)?;
        // The unmerged code git reported at the stop, and the command that rewrites the file as
        // git left it then (`checkout -m` needs all three stages, so not for modify/delete).
        let (xy, restore): (&str, &[&str]) = match (conflict.kind, conflict.deleted_on) {
            (ConflictKind::Content, _) => ("UU", &["checkout", "-m"]),
            (ConflictKind::ModifyDelete, Some(ConflictSideName::Main)) => {
                ("DU", &["checkout", "--theirs"])
            }
            (ConflictKind::ModifyDelete, Some(ConflictSideName::Branch)) => {
                ("UD", &["checkout", "--ours"])
            }
            _ => return Err(no_resolution()),
        };

        // Exits 0 even when there is nothing to restore, hence the check after it.
        run_on_path(git, &repo_path, &["update-index", "--unresolve"], path).await?;
        if !is_unmerged(git, path, Some(xy)).await? {
            return Err(internal(format!(
                "Could not restore the conflict for {path}. Abort the rebase to start over."
            )));
        }
        set_resolution(&git_dir, &step.file, &step.rebase_head, path, None).await?;
        run_on_path(git, &repo_path, restore, path).await
    }

    fn get_name(&self) -> String {
        String::from("RebaseUnresolveOp")
    }
}

#[derive(Clone)]
pub struct RebaseContinueOp<T> {
    pub git_client: git::Git,
    pub engine: T,
    pub app_config: AppConfigRef,
    pub repo_config: RepoConfigRef,
    pub repo_status: RepoStatusRef,
    pub sync_phase_tx: std::sync::mpsc::Sender<String>,
}

#[async_trait]
impl<T> Task for RebaseContinueOp<T>
where
    T: EngineProvider,
{
    async fn execute(&self) -> Result<(), CoreError> {
        let _op = REBASE_OP_LOCK.lock().await;
        let _ = self.sync_phase_tx.send("Replaying commits".to_string());
        let git = &self.git_client;
        let repo_path = repo_path_of(&self.app_config);
        let git_dir = resolve_git_dir(&repo_path);

        editor_check(&self.engine).await?;
        index_lock_check(&git_dir)?;
        let derived = derive(git, &repo_path).await?;
        let file = match (derived.kind, &derived.file) {
            (RebaseStateKind::InProgress, Some(file)) => file.clone(),
            _ => return Err(input(NO_REBASE_IN_PROGRESS)),
        };
        let engine_path = engine_path_of(&self.app_config, &self.repo_config);
        let state = build_rebase_state(git, &repo_path, &self.engine, &engine_path, derived, None)
            .await
            .map_err(CoreError::Internal)?;
        if state.step_incomplete {
            return Err(input(STEP_INCOMPLETE));
        }
        if state
            .conflicts
            .iter()
            .any(|c| c.kind == ConflictKind::Unsupported)
        {
            return Err(input(UNRESOLVABLE_STEP));
        }
        if !parse_unmerged(&status_porcelain(git).await?).is_empty() {
            return Err(input(RESOLVE_EVERY_FILE));
        }

        let status = read_git(
            git,
            &["status", "--porcelain=v2", "-z", "--untracked-files=all"],
        )
        .await?;
        if status.exit_code != 0 {
            return Err(internal(format!(
                "git status failed: {}",
                status.stderr.trim()
            )));
        }
        // A later pick would refuse to overwrite an untracked file.
        let untracked = parse_untracked(&status.stdout);
        if !untracked.is_empty() {
            return Err(input(format!(
                "Move or delete these new files before continuing: {}.",
                listed_paths(&untracked)
            )));
        }
        let mut x = rebase_head(&git_dir).ok_or_else(|| input(STEP_INCOMPLETE))?;
        check_resolutions(git, &state, &x).await?;

        let mut h0 = required_sha(git, "HEAD").await?;
        let mut command = "--continue";
        for _ in 0..=state.total_steps {
            let output = git
                .run_with_status(
                    &["-c", "rerere.enabled=false", "rebase", command],
                    git::Opts::new_with_complete_error().with_env(REBASE_ENV),
                )
                .await?;
            let rebase_dir = rebase_dir_exists(&git_dir);
            let unmerged = rebase_dir && !parse_unmerged(&status_porcelain(git).await?).is_empty();
            let clean_tree = rebase_dir && !unmerged && tree_matches_head(git).await?;
            let head = rev_parse(git, "HEAD").await?;
            let rebase_head_now = rebase_head(&git_dir);
            let mut outcome = continue_outcome(
                output.exit_code,
                rebase_dir,
                unmerged,
                clean_tree,
                head.as_deref() == Some(h0.as_str()),
                rebase_head_now.as_deref() == Some(x.as_str()),
            );
            let stderr = output.stderr.trim();
            if outcome == ContinueStep::EndedWithError
                && derive(git, &repo_path).await?.kind == RebaseStateKind::CompletedUnpushed
            {
                warn!("git rebase {command} completed the rebase but failed: {stderr}");
                outcome = ContinueStep::Completed;
            }
            match outcome {
                ContinueStep::Completed | ContinueStep::Stopped => {
                    replace_current_step(git, &repo_path, &self.engine, &engine_path, &file)
                        .await
                        .map_err(CoreError::Internal)?;
                    return Ok(());
                }
                ContinueStep::SkipEmpty => {
                    // A later stop on this same commit with HEAD unmoved means `--skip` did
                    // not advance either.
                    h0 = head.unwrap_or_default();
                    x = rebase_head_now.unwrap_or_default();
                    command = "--skip";
                }
                ContinueStep::EndedWithError | ContinueStep::Failed => {
                    let msg = if stderr.is_empty() {
                        format!(
                            "git rebase {command} stopped unexpectedly (exit code {}).",
                            output.exit_code
                        )
                    } else {
                        stderr.to_string()
                    };
                    return Err(internal(enrich_error(&repo_path, &msg)));
                }
            }
        }
        Err(internal(NOT_ADVANCING))
    }

    fn get_name(&self) -> String {
        String::from("RebaseContinueOp")
    }
}

#[derive(Clone)]
pub struct RebaseAbortOp<T> {
    pub git_client: git::Git,
    pub engine: T,
    pub app_config: AppConfigRef,
    pub repo_config: RepoConfigRef,
    pub repo_status: RepoStatusRef,
    pub sync_phase_tx: std::sync::mpsc::Sender<String>,
    pub github_username: String,
}

#[async_trait]
impl<T> Task for RebaseAbortOp<T>
where
    T: EngineProvider,
{
    async fn execute(&self) -> Result<(), CoreError> {
        let _op = REBASE_OP_LOCK.lock().await;
        let _ = self.sync_phase_tx.send("Aborting rebase".to_string());
        let git = &self.git_client;
        let repo_path = repo_path_of(&self.app_config);
        let git_dir = resolve_git_dir(&repo_path);

        editor_check(&self.engine).await?;
        index_lock_check(&git_dir)?;
        let derived = derive(git, &repo_path).await?;
        let file = match (derived.kind, derived.file) {
            (RebaseStateKind::InProgress, Some(file)) => file,
            _ => return Err(input(NOTHING_TO_ABORT)),
        };

        let output = git
            .run_with_status(
                &["rebase", "--abort"],
                git::Opts::new_with_complete_error().with_env(REBASE_ENV),
            )
            .await?;
        if output.exit_code != 0 {
            let msg = format!("git rebase --abort failed: {}", output.stderr.trim());
            return Err(internal(enrich_error(&repo_path, &msg)));
        }

        // The record stays until the branch is verifiably back at its backup.
        let branch_ref = format!("refs/heads/{}", file.branch);
        let head = rev_parse(git, "HEAD").await?;
        if rebase_dir_exists(&git_dir)
            || head_ref(git).await?.as_deref() != Some(branch_ref.as_str())
            || head.as_deref() != Some(file.backup_sha.as_str())
        {
            let head = match &head {
                Some(sha) => format!("HEAD is {}", sha.get(..8).unwrap_or(sha)),
                None => "HEAD does not resolve".to_string(),
            };
            return Err(internal(format!(
                "Abort did not restore {} to its backup ({head}). Use Reset to backup.",
                file.branch
            )));
        }
        {
            let _lock = REBASE_FILE_LOCK.lock().await;
            delete_rebase_file(&git_dir)?;
        }

        let status_op = StatusOp {
            git_client: git.clone(),
            github_username: self.github_username.clone(),
            repo_status: self.repo_status.clone(),
            app_config: self.app_config.clone(),
            repo_config: self.repo_config.clone(),
            engine: self.engine.clone(),
            aws_client: None,
            storage: None,
            allow_offline_communication: false,
            skip_display_names: true,
            skip_engine_update: false,
            // internal post-operation refresh, not a user-initiated one
            fetch_first: false,
        };
        // The branch is restored either way; the caller refreshes status after a HEAD change.
        if let Err(e) = status_op.execute().await {
            warn!("Could not refresh status after aborting the rebase: {e}");
        }
        Ok(())
    }

    fn get_name(&self) -> String {
        String::from("RebaseAbortOp")
    }
}

#[derive(Clone)]
pub struct RebaseFinishOp<T> {
    pub git_client: git::Git,
    pub engine: T,
    pub app_config: AppConfigRef,
    pub repo_config: RepoConfigRef,
    pub repo_status: RepoStatusRef,
    pub sync_phase_tx: std::sync::mpsc::Sender<String>,
    pub(crate) post: Result<PostHeadChangeCtx<T>, String>,
    pub result: Arc<parking_lot::Mutex<Option<RebaseFinishResponse>>>,
}

impl<T: EngineProvider> RebaseFinishOp<T> {
    async fn state_after(
        &self,
        repo_path: &Path,
        engine_path: &Path,
    ) -> Result<RebaseState, CoreError> {
        let git = &self.git_client;
        let derived = derive(git, repo_path).await?;
        build_rebase_state(git, repo_path, &self.engine, engine_path, derived, None)
            .await
            .map_err(CoreError::Internal)
    }
}

#[async_trait]
impl<T> Task for RebaseFinishOp<T>
where
    T: EngineProvider,
{
    async fn execute(&self) -> Result<(), CoreError> {
        let _op = REBASE_OP_LOCK.lock().await;
        let git = &self.git_client;
        let repo_path = repo_path_of(&self.app_config);
        let git_dir = resolve_git_dir(&repo_path);
        let engine_path = engine_path_of(&self.app_config, &self.repo_config);

        editor_check(&self.engine).await?;
        index_lock_check(&git_dir)?;
        let derived = derive(git, &repo_path).await?;
        let file = match (derived.kind, &derived.file) {
            (RebaseStateKind::CompletedUnpushed, Some(file)) => file.clone(),
            _ => return Err(input(NOTHING_TO_PUSH)),
        };
        let dropped_commits =
            build_rebase_state(git, &repo_path, &self.engine, &engine_path, derived, None)
                .await
                .map_err(CoreError::Internal)?
                .dropped_commits;
        let head = required_sha(git, "HEAD").await?;

        let _ = self
            .sync_phase_tx
            .send("Pushing rebased branch".to_string());
        let push = leased_push(git, &file.branch, &head, &file.pre_rebase_origin_sha).await?;
        let (outcome, post_step_error) = match push.outcome {
            PushOutcome::Pushed => {
                {
                    let _lock = REBASE_FILE_LOCK.lock().await;
                    delete_rebase_file(&git_dir).map_err(|e| {
                        internal(format!(
                            "{} was pushed, but its rebase record could not be deleted: {e}",
                            file.branch
                        ))
                    })?;
                }
                let _ = self
                    .sync_phase_tx
                    .send("Updating editor binaries".to_string());
                let post = run_post(&self.post, Some(file.engine_association.clone())).await;
                (RebaseFinishOutcome::Pushed, post.err())
            }
            PushOutcome::LeaseRejected => {
                let remote_sha = fetched_branch_tip(git, &file.branch).await;
                record_push_failed(&git_dir, &file, remote_sha).await?;
                (RebaseFinishOutcome::LeaseRejected, None)
            }
            PushOutcome::Rejected(summary) => {
                return Err(input(format!("GitHub rejected the push: {summary}")));
            }
            PushOutcome::NoRefLine => return Err(internal(push.failure())),
        };

        let state = match self.state_after(&repo_path, &engine_path).await {
            Ok(state) => state,
            // The push and the record's deletion are done; the page re-reads the state.
            Err(e) if outcome == RebaseFinishOutcome::Pushed => {
                error!("Could not read the rebase state after pushing: {e}");
                RebaseState::default()
            }
            Err(e) => return Err(e),
        };
        *self.result.lock() = Some(RebaseFinishResponse {
            outcome,
            state,
            dropped_commits,
            touches_cpp: file.touches_cpp,
            post_step_error,
        });
        Ok(())
    }

    fn get_name(&self) -> String {
        String::from("RebaseFinishOp")
    }
}

#[derive(Clone)]
pub struct RebaseRedoOp<T> {
    pub git_client: git::Git,
    pub engine: T,
    pub app_config: AppConfigRef,
    pub repo_config: RepoConfigRef,
    pub repo_status: RepoStatusRef,
    pub sync_phase_tx: std::sync::mpsc::Sender<String>,
    pub trunk: String,
    pub github_username: String,
    pub(crate) post: Result<PostHeadChangeCtx<T>, String>,
}

#[async_trait]
impl<T> Task for RebaseRedoOp<T>
where
    T: EngineProvider,
{
    async fn execute(&self) -> Result<(), CoreError> {
        let _op = REBASE_OP_LOCK.lock().await;
        let git = &self.git_client;
        let repo_path = repo_path_of(&self.app_config);
        let git_dir = resolve_git_dir(&repo_path);

        // Every refusal comes before anything moves.
        editor_check(&self.engine).await?;
        index_lock_check(&git_dir)?;
        let derived = derive(git, &repo_path).await?;
        let file = match (derived.kind, derived.file) {
            (RebaseStateKind::PushFailed, Some(file)) => file,
            _ => return Err(input(REDO_ONLY_AFTER_PUSH_FAILED)),
        };
        ensure_clean_tree(git).await?;
        if !is_valid_branch_name(&self.trunk) {
            return Err(input(NO_TRUNK));
        }

        let _ = self.sync_phase_tx.send("Fetching latest".to_string());
        if !fetch_origin(git, Some(&self.trunk), &file.branch).await? {
            return Err(input(format!(
                "{} no longer exists on origin. Use Reset to backup.",
                file.branch
            )));
        }
        let origin_sha = required_sha(git, &format!("refs/remotes/origin/{}", file.branch)).await?;
        // Resetting to origin must never drop the commits the rebase started from.
        if !is_ancestor(git, &file.pre_rebase_origin_sha, &origin_sha).await? {
            return Err(input(format!(
                "{} was rewritten on origin. Use Reset to backup.",
                file.branch
            )));
        }
        if file
            .push_failed_remote_sha
            .as_deref()
            .is_some_and(|seen| seen != origin_sha)
        {
            record_push_failed(&git_dir, &file, Some(origin_sha)).await?;
            return Err(input(format!(
                "{} changed on origin again. Review it and try again.",
                file.branch
            )));
        }
        let head = required_sha(git, "HEAD").await?;
        backup_rebased_head(git, &file.branch, &head).await?;
        // A crash before the record is deleted derives as pushed outside: Dismiss leaves the
        // branch at origin's version, as a finished Redo would. Deleting the record first would
        // instead leave the unpushed rebase unguarded.
        reset_keep(git, &repo_path, &origin_sha).await?;
        {
            let _lock = REBASE_FILE_LOCK.lock().await;
            delete_rebase_file(&git_dir)?;
        }

        let start = RebaseStartOp {
            git_client: git.clone(),
            engine: self.engine.clone(),
            app_config: self.app_config.clone(),
            repo_config: self.repo_config.clone(),
            repo_status: self.repo_status.clone(),
            sync_phase_tx: self.sync_phase_tx.clone(),
            trunk: self.trunk.clone(),
            github_username: self.github_username.clone(),
        };
        let started = start.start_rebase_inner(Some(&file)).await;
        let Err(e) = started else {
            return Ok(());
        };
        // Without a new record HEAD stays at origin's branch, so the post steps run now.
        if !matches!(read_rebase_file(&git_dir), RebaseFileRead::Missing) {
            return Err(e);
        }
        match run_post(&self.post, Some(file.engine_association.clone())).await {
            Ok(()) => Err(e),
            Err(post_e) => {
                let msg = format!("{e} {}", post_failure_note(&file.branch, &post_e));
                Err(match e {
                    CoreError::Input(_) => input(msg),
                    _ => internal(msg),
                })
            }
        }
    }

    fn get_name(&self) -> String {
        String::from("RebaseRedoOp")
    }
}

#[derive(Clone)]
pub struct RebaseResetToBackupOp<T> {
    pub git_client: git::Git,
    pub engine: T,
    pub app_config: AppConfigRef,
    pub repo_config: RepoConfigRef,
    pub repo_status: RepoStatusRef,
    pub sync_phase_tx: std::sync::mpsc::Sender<String>,
    pub(crate) post: Result<PostHeadChangeCtx<T>, String>,
}

#[async_trait]
impl<T> Task for RebaseResetToBackupOp<T>
where
    T: EngineProvider,
{
    async fn execute(&self) -> Result<(), CoreError> {
        let _op = REBASE_OP_LOCK.lock().await;
        let _ = self.sync_phase_tx.send("Resetting to backup".to_string());
        let git = &self.git_client;
        let repo_path = repo_path_of(&self.app_config);
        let git_dir = resolve_git_dir(&repo_path);

        editor_check(&self.engine).await?;
        index_lock_check(&git_dir)?;
        let derived = derive(git, &repo_path).await?;
        let file = match (derived.kind, derived.file) {
            (RebaseStateKind::CompletedUnpushed | RebaseStateKind::PushFailed, Some(file)) => file,
            _ => return Err(input(RESET_ONLY_AFTER_REPLAY)),
        };
        if rev_parse(git, &file.backup_ref).await?.as_deref() != Some(file.backup_sha.as_str()) {
            return Err(internal(format!(
                "The backup {} is missing or moved; nothing was changed.",
                file.backup_ref
            )));
        }

        let head = required_sha(git, "HEAD").await?;
        backup_rebased_head(git, &file.branch, &head).await?;
        reset_keep(git, &repo_path, &file.backup_sha).await?;
        {
            let _lock = REBASE_FILE_LOCK.lock().await;
            delete_rebase_file(&git_dir)?;
        }
        run_post(&self.post, Some(file.engine_association.clone()))
            .await
            .map_err(|e| input(post_failure_note(&file.branch, &e)))
    }

    fn get_name(&self) -> String {
        String::from("RebaseResetToBackupOp")
    }
}

#[derive(Clone)]
pub struct EditorRebuildOp<T> {
    pub git_client: git::Git,
    pub engine: T,
    pub app_config: AppConfigRef,
    pub repo_config: RepoConfigRef,
    pub repo_status: RepoStatusRef,
    pub sync_phase_tx: std::sync::mpsc::Sender<String>,
    pub result: Arc<parking_lot::Mutex<Option<RebaseCommitInfo>>>,
}

#[async_trait]
impl<T> Task for EditorRebuildOp<T>
where
    T: EngineProvider,
{
    async fn execute(&self) -> Result<(), CoreError> {
        let _op = REBASE_OP_LOCK.lock().await;
        let git = &self.git_client;
        let repo_path = repo_path_of(&self.app_config);
        let git_dir = resolve_git_dir(&repo_path);

        // The guard itself refuses while REBASE_OP_LOCK is held, as it is here.
        if let Some(sentence) = guard_sentence(derive(git, &repo_path).await?.kind) {
            return Err(input(sentence));
        }
        index_lock_check(&git_dir)?;
        // A commit now would conclude the merge, cherry-pick, revert or rebase in progress.
        if rebase_dir_exists(&git_dir)
            || ["MERGE_HEAD", "CHERRY_PICK_HEAD", "REVERT_HEAD"]
                .iter()
                .any(|name| git_dir.join(name).exists())
        {
            return Err(input(OPERATION_IN_PROGRESS));
        }
        let branch = feature_branch(git, &self.repo_config).await?;
        // `commit --allow-empty` would commit anything staged.
        if read_git(git, &["diff", "--cached", "--quiet", "HEAD", "--"])
            .await?
            .exit_code
            != 0
        {
            return Err(input(UNSTAGE_FIRST));
        }

        let _ = self.sync_phase_tx.send("Fetching latest".to_string());
        let origin_sha = match fetch_origin(git, None, &branch).await? {
            true => rev_parse(git, &format!("refs/remotes/origin/{branch}")).await?,
            false => None,
        };
        let head = required_sha(git, "HEAD").await?;
        if origin_sha.as_deref() != Some(head.as_str()) {
            return Err(input(SYNC_FIRST));
        }

        git.run(
            &["commit", "--allow-empty", "-m", EDITOR_BUILD_MESSAGE],
            git::Opts::new_with_complete_error(),
        )
        .await?;
        let new = required_sha(git, "HEAD").await?;
        let tree = rev_parse(git, &format!("{new}^{{tree}}")).await?;
        let head_tree = rev_parse(git, &format!("{head}^{{tree}}")).await?;
        if tree != head_tree {
            // A hook changed the files; the commit stays local for the user to inspect.
            return Err(internal(format!(
                "The editor build commit {new} is not an empty commit on {branch}, so it was not pushed."
            )));
        }
        if commit_parents(git, &new).await? != [head.as_str()] {
            undo_editor_build_commit(git, &repo_path, &branch, &new, &head).await?;
            return Err(internal(format!(
                "The editor build commit was not a plain commit on {branch}; it was undone and not pushed."
            )));
        }

        let _ = self
            .sync_phase_tx
            .send("Pushing editor build commit".to_string());
        let push = leased_push(git, &branch, &new, &head).await;
        if let Ok(PushResult {
            outcome: PushOutcome::Pushed,
            ..
        }) = &push
        {
            *self.result.lock() = Some(commit_info(git, &new).await?);
            return Ok(());
        }

        undo_editor_build_commit(git, &repo_path, &branch, &new, &head).await?;
        let push = push?;
        Err(match push.outcome {
            PushOutcome::NoRefLine => internal(push.failure()),
            // Origin itself refuses a push that lost a race with a collaborator.
            PushOutcome::Rejected(summary)
                if fetched_branch_tip(git, &branch).await.as_deref() == Some(head.as_str()) =>
            {
                input(format!("GitHub rejected the push: {summary}"))
            }
            _ => input(NEW_COMMITS_ON_ORIGIN),
        })
    }

    fn get_name(&self) -> String {
        String::from("EditorRebuildOp")
    }
}

/// The derived and built state of the configured repo. With `may_cache`, the conflicts of a new
/// step are cached when no rebase op runs meanwhile.
async fn current_rebase_state<T>(
    state: &AppState<T>,
    may_cache: bool,
) -> Result<RebaseState, CoreError>
where
    T: EngineProvider,
{
    let repo_path = repo_path_of(&state.app_config);
    if repo_path.as_os_str().is_empty() {
        return Ok(RebaseState::default());
    }
    let engine_path = engine_path_of(&state.app_config, &state.repo_config);
    let git = state.git();
    let derived = derive(&git, &repo_path).await?;
    build_rebase_state(
        &git,
        &repo_path,
        &state.engine,
        &engine_path,
        derived,
        may_cache.then_some(&*REBASE_OP_LOCK),
    )
    .await
    .map_err(CoreError::Internal)
}

#[instrument(skip_all)]
pub async fn guided_rebase_state_handler<T>(
    State(state): State<AppState<T>>,
) -> Result<Json<RebaseState>, CoreError>
where
    T: EngineProvider,
{
    Ok(Json(current_rebase_state(&state, true).await?))
}

#[instrument(skip_all)]
pub async fn guided_rebase_preflight_handler<T>(
    State(state): State<AppState<T>>,
) -> Result<Json<RebasePreflight>, CoreError>
where
    T: EngineProvider,
{
    let trunk = state.repo_config.read().trunk_branch.clone();
    let preflight = preflight(&state.git(), &repo_path_of(&state.app_config), &trunk).await?;
    Ok(Json(preflight))
}

/// What a rebase onto `refs/remotes/origin/<trunk>` would replay, without fetching.
async fn preflight(
    git: &git::Git,
    repo_path: &Path,
    trunk: &str,
) -> Result<RebasePreflight, CoreError> {
    derive(git, repo_path).await?;
    git_version_gate(&git.check_min_version().await?)?;
    if !is_valid_branch_name(trunk) {
        return Err(input(NO_TRUNK));
    }

    let branch = read_git(git, &["symbolic-ref", "HEAD"]).await?;
    if branch.exit_code != 0 {
        return Err(input(branch.stderr.trim()));
    }
    let trunk_ref = format!("refs/remotes/origin/{trunk}");
    let base = read_git(git, &["merge-base", &trunk_ref, "HEAD"]).await?;
    let merge_base = base.stdout.trim();
    if base.exit_code != 0 || !is_full_sha(merge_base) {
        let reason = base.stderr.trim();
        return Err(input(if reason.is_empty() {
            format!("HEAD has no history in common with {trunk_ref}.")
        } else {
            reason.to_string()
        }));
    }

    let range = format!("{merge_base}..HEAD");
    let count = read_git(git, &["rev-list", "--count", "--no-merges", &range, "--"]).await?;
    let commit_count = count.stdout.trim().parse().map_err(|_| {
        internal(format!(
            "Could not count the branch's commits: {}",
            count.stderr.trim()
        ))
    })?;
    let mut cpp_files = cpp_files(git, merge_base, "HEAD").await?;
    cpp_files.truncate(PREFLIGHT_CPP_FILES);

    Ok(RebasePreflight {
        touches_cpp: !cpp_files.is_empty(),
        cpp_files,
        commit_count,
    })
}

#[instrument(skip_all)]
pub async fn guided_rebase_start_handler<T>(
    State(state): State<AppState<T>>,
) -> Result<Json<RebaseState>, CoreError>
where
    T: EngineProvider,
{
    let op = RebaseStartOp {
        git_client: state.git(),
        engine: state.engine.clone(),
        app_config: state.app_config.clone(),
        repo_config: state.repo_config.clone(),
        repo_status: state.repo_status.clone(),
        sync_phase_tx: state.sync_phase_tx.clone(),
        trunk: state.repo_config.read().trunk_branch.clone(),
        github_username: state.github_username(),
    };
    run_sequence(&state, Box::new(op)).await?;
    Ok(Json(current_rebase_state(&state, false).await?))
}

#[instrument(skip_all)]
pub async fn guided_rebase_resolve_handler<T>(
    State(state): State<AppState<T>>,
    Json(req): Json<ResolveConflictRequest>,
) -> Result<Json<RebaseState>, CoreError>
where
    T: EngineProvider,
{
    let op = RebaseResolveOp {
        git_client: state.git(),
        engine: state.engine.clone(),
        app_config: state.app_config.clone(),
        repo_config: state.repo_config.clone(),
        repo_status: state.repo_status.clone(),
        sync_phase_tx: state.sync_phase_tx.clone(),
        path: req.path,
        keep: req.keep,
    };
    run_sequence(&state, Box::new(op)).await?;
    Ok(Json(current_rebase_state(&state, false).await?))
}

#[instrument(skip_all)]
pub async fn guided_rebase_unresolve_handler<T>(
    State(state): State<AppState<T>>,
    Json(req): Json<UnresolveConflictRequest>,
) -> Result<Json<RebaseState>, CoreError>
where
    T: EngineProvider,
{
    let op = RebaseUnresolveOp {
        git_client: state.git(),
        engine: state.engine.clone(),
        app_config: state.app_config.clone(),
        repo_config: state.repo_config.clone(),
        repo_status: state.repo_status.clone(),
        sync_phase_tx: state.sync_phase_tx.clone(),
        path: req.path,
    };
    run_sequence(&state, Box::new(op)).await?;
    Ok(Json(current_rebase_state(&state, false).await?))
}

#[instrument(skip_all)]
pub async fn guided_rebase_open_file_handler<T>(
    State(state): State<AppState<T>>,
    Json(req): Json<OpenConflictFileRequest>,
) -> Result<(), CoreError>
where
    T: EngineProvider,
{
    let git = state.git();
    let repo_path = repo_path_of(&state.app_config);
    let path = req.path.as_str();
    let derived = derive(&git, &repo_path).await?;
    let file = match (derived.kind, derived.file) {
        (RebaseStateKind::InProgress, Some(file)) => file,
        _ => return Err(input(NO_REBASE_IN_PROGRESS)),
    };
    let rebase_head = rebase_head(&resolve_git_dir(&repo_path));
    let is_text_conflict = file
        .current_step
        .filter(|step| Some(&step.sha) == rebase_head.as_ref())
        .is_some_and(|step| {
            step.conflicts
                .iter()
                .any(|c| c.path == path && c.kind == ConflictKind::Content && c.is_text)
        });
    if !is_text_conflict {
        return Err(input(format!(
            "{path} is not a text conflict in this step."
        )));
    }
    let abs = conflict_abs_path(&repo_path, path)
        .ok_or_else(|| input(format!("{path} is not a valid repo path.")))?;
    if windows_would_run(path, std::env::var("PATHEXT").ok().as_deref()) {
        return Err(input(format!(
            "Open {path} in your text editor yourself; Friendshipper won't open files that Windows would run."
        )));
    }
    open::that(&abs).map_err(|e| internal(format!("Could not open {path}: {e}")))
}

#[instrument(skip_all)]
pub async fn guided_rebase_dismiss_handler<T>(
    State(state): State<AppState<T>>,
) -> Result<Json<RebaseState>, CoreError>
where
    T: EngineProvider,
{
    let git = state.git();
    dismiss_stale(&git, &repo_path_of(&state.app_config), &REBASE_OP_LOCK).await?;
    Ok(Json(current_rebase_state(&state, false).await?))
}

/// Deletes a stale record; backup refs are kept. Refuses while `op_lock` is held, as a
/// starting rebase derives as stale until git has created its own rebase state.
async fn dismiss_stale(
    git: &git::Git,
    repo_path: &Path,
    op_lock: &tokio::sync::Mutex<()>,
) -> Result<(), CoreError> {
    let Ok(_op) = op_lock.try_lock() else {
        return Err(input(OP_RUNNING));
    };
    let _lock = REBASE_FILE_LOCK.lock().await;
    if derive(git, repo_path).await?.kind != RebaseStateKind::Stale {
        return Err(input("Nothing to dismiss."));
    }
    delete_rebase_file(&resolve_git_dir(repo_path))?;
    Ok(())
}

#[instrument(skip_all)]
pub async fn guided_rebase_continue_handler<T>(
    State(state): State<AppState<T>>,
) -> Result<Json<RebaseState>, CoreError>
where
    T: EngineProvider,
{
    let op = RebaseContinueOp {
        git_client: state.git(),
        engine: state.engine.clone(),
        app_config: state.app_config.clone(),
        repo_config: state.repo_config.clone(),
        repo_status: state.repo_status.clone(),
        sync_phase_tx: state.sync_phase_tx.clone(),
    };
    run_sequence(&state, Box::new(op)).await?;
    Ok(Json(current_rebase_state(&state, false).await?))
}

#[instrument(skip_all)]
pub async fn guided_rebase_abort_handler<T>(
    State(state): State<AppState<T>>,
) -> Result<Json<RebaseState>, CoreError>
where
    T: EngineProvider,
{
    let op = RebaseAbortOp {
        git_client: state.git(),
        engine: state.engine.clone(),
        app_config: state.app_config.clone(),
        repo_config: state.repo_config.clone(),
        repo_status: state.repo_status.clone(),
        sync_phase_tx: state.sync_phase_tx.clone(),
        github_username: state.github_username(),
    };
    run_sequence(&state, Box::new(op)).await?;
    Ok(Json(current_rebase_state(&state, false).await?))
}

#[instrument(skip_all)]
pub async fn guided_rebase_finish_handler<T>(
    State(state): State<AppState<T>>,
) -> Result<Json<RebaseFinishResponse>, CoreError>
where
    T: EngineProvider,
{
    let aws = require_aws_and_storage(&state).await;
    let result = Arc::new(parking_lot::Mutex::new(None));
    let op = RebaseFinishOp {
        git_client: state.git(),
        engine: state.engine.clone(),
        app_config: state.app_config.clone(),
        repo_config: state.repo_config.clone(),
        repo_status: state.repo_status.clone(),
        sync_phase_tx: state.sync_phase_tx.clone(),
        post: post_ctx(&state, aws),
        result: result.clone(),
    };
    run_sequence(&state, Box::new(op)).await?;
    let response = result
        .lock()
        .take()
        .ok_or_else(|| internal("The push finished without a result."))?;
    Ok(Json(response))
}

#[instrument(skip_all)]
pub async fn guided_rebase_redo_handler<T>(
    State(state): State<AppState<T>>,
) -> Result<Json<RebaseState>, CoreError>
where
    T: EngineProvider,
{
    let aws = require_aws_and_storage(&state).await;
    let op = RebaseRedoOp {
        git_client: state.git(),
        engine: state.engine.clone(),
        app_config: state.app_config.clone(),
        repo_config: state.repo_config.clone(),
        repo_status: state.repo_status.clone(),
        sync_phase_tx: state.sync_phase_tx.clone(),
        trunk: state.repo_config.read().trunk_branch.clone(),
        github_username: state.github_username(),
        post: post_ctx(&state, aws),
    };
    run_sequence(&state, Box::new(op)).await?;
    Ok(Json(current_rebase_state(&state, false).await?))
}

#[instrument(skip_all)]
pub async fn guided_rebase_reset_to_backup_handler<T>(
    State(state): State<AppState<T>>,
) -> Result<Json<RebaseState>, CoreError>
where
    T: EngineProvider,
{
    let aws = require_aws_and_storage(&state).await;
    let op = RebaseResetToBackupOp {
        git_client: state.git(),
        engine: state.engine.clone(),
        app_config: state.app_config.clone(),
        repo_config: state.repo_config.clone(),
        repo_status: state.repo_status.clone(),
        sync_phase_tx: state.sync_phase_tx.clone(),
        post: post_ctx(&state, aws),
    };
    run_sequence(&state, Box::new(op)).await?;
    Ok(Json(current_rebase_state(&state, false).await?))
}

#[instrument(skip_all)]
pub async fn editor_rebuild_handler<T>(
    State(state): State<AppState<T>>,
) -> Result<Json<RebaseCommitInfo>, CoreError>
where
    T: EngineProvider,
{
    let result = Arc::new(parking_lot::Mutex::new(None));
    let op = EditorRebuildOp {
        git_client: state.git(),
        engine: state.engine.clone(),
        app_config: state.app_config.clone(),
        repo_config: state.repo_config.clone(),
        repo_status: state.repo_status.clone(),
        sync_phase_tx: state.sync_phase_tx.clone(),
        result: result.clone(),
    };
    run_sequence(&state, Box::new(op)).await?;
    let commit = result
        .lock()
        .take()
        .ok_or_else(|| internal("The editor build finished without a result."))?;
    Ok(Json(commit))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::mpsc;

    use parking_lot::RwLock;

    use super::*;
    use crate::engine::UnrealEngineProvider;
    use crate::repo::operations::branch::rebase_guard::{
        GUARD_COMPLETED_UNPUSHED, GUARD_IN_PROGRESS,
    };
    use crate::repo::operations::branch::rebase_state::tests::{
        conflict, diverge_on_a_txt, engine, guided_stop_on_a_txt, info, new_git, original,
        sample_file_for, slash_path, Change, Diverged, Fixture, BRANCH,
    };
    use crate::repo::operations::branch::rebase_state::{json_path, UnmergedEntry};
    use ethos_core::types::config::{AppConfig, RepoConfig, TargetBranchConfig};
    use ethos_core::types::repo::{DroppedCommit, RebaseStaleReason, RepoStatus};

    const DAY: u64 = 86400;
    const OTHER: &str = "o.txt";

    struct Refs {
        app_config: AppConfigRef,
        repo_config: RepoConfigRef,
        repo_status: RepoStatusRef,
        engine: UnrealEngineProvider,
    }

    fn refs(fx: &Fixture) -> Refs {
        let mut app_config = AppConfig::new(crate::APP_NAME);
        app_config.repo_path = fx.path().to_str().expect("utf8").to_string();
        app_config.target_branch = "main".to_string();
        let repo_config = RepoConfig {
            trunk_branch: "main".to_string(),
            target_branches: ["main", "content-main"]
                .into_iter()
                .map(|name| TargetBranchConfig {
                    name: name.to_string(),
                    uses_merge_queue: false,
                    blocked_file_globs: vec![],
                })
                .collect(),
            ..Default::default()
        };
        let engine = UnrealEngineProvider::new_from_config(app_config.clone(), repo_config.clone());
        Refs {
            app_config: Arc::new(RwLock::new(app_config)),
            repo_config: Arc::new(RwLock::new(repo_config)),
            repo_status: Arc::new(RwLock::new(RepoStatus::new())),
            engine,
        }
    }

    fn start_op(fx: &Fixture) -> (RebaseStartOp<UnrealEngineProvider>, mpsc::Receiver<String>) {
        let refs = refs(fx);
        let (sync_phase_tx, phases) = mpsc::channel();
        let op = RebaseStartOp {
            git_client: fx.git.clone(),
            engine: refs.engine,
            app_config: refs.app_config,
            repo_config: refs.repo_config,
            repo_status: refs.repo_status,
            sync_phase_tx,
            trunk: "main".to_string(),
            github_username: "test_user".to_string(),
        };
        (op, phases)
    }

    fn resolve_op(
        fx: &Fixture,
        path: &str,
        keep: RebaseKeep,
    ) -> RebaseResolveOp<UnrealEngineProvider> {
        let refs = refs(fx);
        RebaseResolveOp {
            git_client: fx.git.clone(),
            engine: refs.engine,
            app_config: refs.app_config,
            repo_config: refs.repo_config,
            repo_status: refs.repo_status,
            sync_phase_tx: mpsc::channel().0,
            path: path.to_string(),
            keep,
        }
    }

    fn unresolve_op(fx: &Fixture, path: &str) -> RebaseUnresolveOp<UnrealEngineProvider> {
        let refs = refs(fx);
        RebaseUnresolveOp {
            git_client: fx.git.clone(),
            engine: refs.engine,
            app_config: refs.app_config,
            repo_config: refs.repo_config,
            repo_status: refs.repo_status,
            sync_phase_tx: mpsc::channel().0,
            path: path.to_string(),
        }
    }

    async fn start(fx: &Fixture) -> Result<(), CoreError> {
        start_op(fx).0.execute().await
    }

    async fn resolve(fx: &Fixture, path: &str, keep: RebaseKeep) -> Result<(), CoreError> {
        resolve_op(fx, path, keep).execute().await
    }

    async fn unresolve(fx: &Fixture, path: &str) -> Result<(), CoreError> {
        unresolve_op(fx, path).execute().await
    }

    /// What `GET …/state` returns, without caching.
    async fn state(fx: &Fixture) -> RebaseState {
        build_rebase_state(
            &fx.git,
            fx.path(),
            &engine(fx.path()),
            Path::new(""),
            fx.derive().await,
            None,
        )
        .await
        .expect("build rebase state")
    }

    fn input_message<V: std::fmt::Debug>(result: Result<V, CoreError>) -> String {
        match result {
            Err(CoreError::Input(e)) => e.to_string(),
            other => panic!("expected Input error, got {other:?}"),
        }
    }

    fn internal_message<V: std::fmt::Debug>(result: Result<V, CoreError>) -> String {
        match result {
            Err(CoreError::Internal(e)) => e.to_string(),
            other => panic!("expected Internal error, got {other:?}"),
        }
    }

    fn read(fx: &Fixture, path: &str) -> Option<Vec<u8>> {
        fs::read(fx.path().join(path)).ok()
    }

    fn record(fx: &Fixture) -> GuidedRebaseFile {
        match read_rebase_file(&fx.git_dir()) {
            RebaseFileRead::Ok(file) => *file,
            other => panic!("expected a readable record, got {other:?}"),
        }
    }

    async fn unmerged(fx: &Fixture) -> Vec<UnmergedEntry> {
        parse_unmerged(&fx.porcelain_v2().await)
    }

    async fn xy_of(fx: &Fixture, path: &str) -> Option<String> {
        unmerged(fx)
            .await
            .into_iter()
            .find(|entry| entry.path == path)
            .map(|entry| entry.xy)
    }

    async fn backup_refs(fx: &Fixture) -> Vec<String> {
        fx.out(&["for-each-ref", "--format=%(refname)", "refs/f11r-backup/"])
            .await
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// A second clone of origin, committing as someone else.
    async fn collaborator(fx: &Fixture) -> (tempfile::TempDir, git::Git) {
        let dir = tempfile::tempdir().expect("collaborator tempdir");
        let git = new_git(dir.path());
        let opts = git::Opts::new_without_logs();
        git.run(
            &[
                "clone",
                "-q",
                &slash_path(fx.origin_path()),
                &slash_path(dir.path()),
            ],
            opts,
        )
        .await
        .expect("clone");
        assert_eq!(
            git.run_and_collect_output(&["remote", "get-url", "origin"], opts)
                .await
                .expect("remote url")
                .trim(),
            slash_path(fx.origin_path()),
            "the collaborator's origin must be the test's temp origin"
        );
        assert_eq!(
            git.run_and_collect_output(&["remote", "get-url", "--push", "origin"], opts)
                .await
                .expect("push url")
                .trim(),
            slash_path(fx.origin_path()),
            "the collaborator pushes only to the test's temp origin"
        );
        let hooks = slash_path(fx.hooks_path());
        for (key, value) in [
            ("user.email", "collab@example.com"),
            ("user.name", "Collaborator"),
            ("commit.gpgsign", "false"),
            ("core.autocrlf", "false"),
            ("core.hooksPath", hooks.as_str()),
        ] {
            git.run(&["config", key, value], opts)
                .await
                .expect("config");
        }
        (dir, git)
    }

    async fn collaborator_push(fx: &Fixture, branch: &str, path: &str) {
        let (dir, git) = collaborator(fx).await;
        let opts = git::Opts::new_without_logs();
        let tracking = format!("refs/remotes/origin/{branch}");
        git.run(&["checkout", "-q", "-B", branch, &tracking], opts)
            .await
            .expect("checkout");
        fs::write(dir.path().join(path), b"collab\n").unwrap();
        git.run(&["add", "--", path], opts).await.expect("add");
        git.run(&["commit", "-qm", "collab commit"], opts)
            .await
            .expect("commit");
        let refspec = format!("HEAD:refs/heads/{branch}");
        git.run(&["push", "-q", "origin", &refspec], opts)
            .await
            .expect("push");
    }

    /// Everything a refused Start must leave alone.
    #[derive(Debug, PartialEq)]
    struct Snapshot {
        head: String,
        branch: String,
        porcelain_v2: String,
        refs: String,
        tracking_refs: String,
        stash_count: usize,
        origin_refs: String,
        json_bytes: Option<Vec<u8>>,
    }

    async fn capture(fx: &Fixture) -> Snapshot {
        let all_refs = fx
            .out(&["for-each-ref", "--format=%(refname) %(objectname)"])
            .await;
        let (tracking, local): (Vec<&str>, Vec<&str>) = all_refs
            .lines()
            .partition(|line| line.starts_with("refs/remotes/"));
        let branch = fx
            .git
            .run_with_status(
                &["symbolic-ref", "-q", "HEAD"],
                git::Opts::new_without_logs(),
            )
            .await
            .unwrap()
            .stdout;
        Snapshot {
            head: fx.head().await,
            branch,
            porcelain_v2: fx.porcelain_v2().await,
            refs: local.join("\n"),
            tracking_refs: tracking.join("\n"),
            stash_count: fx.out(&["stash", "list"]).await.lines().count(),
            origin_refs: new_git(fx.origin_path())
                .run_and_collect_output(
                    &["for-each-ref", "--format=%(refname) %(objectname)"],
                    git::Opts::new_without_logs(),
                )
                .await
                .unwrap(),
            json_bytes: fs::read(json_path(&fx.git_dir())).ok(),
        }
    }

    /// Start is refused with `expected` and changes nothing.
    async fn assert_start_refused(fx: &Fixture, expected: &str) {
        let before = capture(fx).await;
        assert_eq!(input_message(start(fx).await), expected);
        assert_eq!(capture(fx).await, before);
    }

    /// Start is refused with `expected` after fetching; only tracking refs may have moved.
    async fn assert_start_refused_after_fetch(fx: &Fixture, expected: &str) {
        let before = capture(fx).await;
        assert_eq!(input_message(start(fx).await), expected);
        let after = capture(fx).await;
        assert_eq!(
            Snapshot {
                tracking_refs: before.tracking_refs.clone(),
                ..after
            },
            before
        );
    }

    async fn diverge_clean(fx: &Fixture) -> Diverged {
        fx.diverge(
            &[],
            &[Change::Write("m.txt", b"main\n")],
            &[Change::Write("b.txt", b"branch\n")],
        )
        .await
    }

    async fn author_date(fx: &Fixture, sha: &str) -> String {
        fx.out(&["log", "-1", "--date=raw", "--format=%ad", sha])
            .await
    }

    // Start gates

    #[tokio::test]
    async fn start_refused_with_a_stale_record() {
        let fx = Fixture::new().await;
        diverge_on_a_txt(&fx).await;
        let file = fx.guided_file("main").await;
        fx.write_file_json(&file);
        assert_eq!(fx.derive().await.kind, RebaseStateKind::Stale);
        assert_start_refused(&fx, DISMISS_FIRST).await;
    }

    #[tokio::test]
    async fn start_refused_during_a_guided_rebase() {
        let fx = Fixture::new().await;
        guided_stop_on_a_txt(&fx).await;
        assert_start_refused(&fx, GUARD_IN_PROGRESS).await;
    }

    #[tokio::test]
    async fn start_refused_during_an_external_rebase() {
        let fx = Fixture::new().await;
        diverge_on_a_txt(&fx).await;
        assert_eq!(fx.start_raw_rebase("main").await, 1);
        assert_start_refused(&fx, EXTERNAL_REBASE).await;
    }

    #[tokio::test]
    async fn start_refused_while_the_index_is_locked() {
        let fx = Fixture::new().await;
        diverge_clean(&fx).await;
        let lock = fx.git_dir().join("index.lock");
        fs::write(&lock, b"").unwrap();
        assert_start_refused(
            &fx,
            &format!(
                "Git's index is locked ({}). If no git command is running, delete that file and try again.",
                lock.display()
            ),
        )
        .await;
        assert!(lock.exists(), "the lock must not be deleted");
    }

    #[test]
    fn git_version_gate_text() {
        // Branch Management's own minimum is 2.38; guided rebase needs 2.43.
        for (version, supported) in [("2.37.9", false), ("2.38.0", true), ("2.42.4", true)] {
            let result = git_version_gate(&GitVersionCheck {
                version: version.into(),
                supported,
            });
            assert_eq!(
                input_message(result),
                format!(
                    "Update git to use Branch Management (found {version}, need 2.43 or newer)."
                )
            );
        }
        for version in ["2.43.0", "2.47.1.windows.1", "2.55.0.windows.3", "3.0.0"] {
            git_version_gate(&GitVersionCheck {
                version: version.into(),
                supported: true,
            })
            .expect(version);
        }
    }

    #[tokio::test]
    async fn start_refused_on_a_detached_head() {
        let fx = Fixture::new().await;
        diverge_clean(&fx).await;
        fx.run(&["checkout", "-q", "--detach"]).await;
        assert_start_refused(&fx, FEATURE_BRANCHES_ONLY).await;
    }

    #[tokio::test]
    async fn start_refused_on_protected_and_quick_submit_branches() {
        let fx = Fixture::new().await;
        diverge_clean(&fx).await;
        fx.checkout("main").await;
        assert_start_refused(&fx, FEATURE_BRANCHES_ONLY).await;
        for branch in ["content-main", "f11r-main-Jane-1"] {
            fx.branch(branch).await;
            assert_start_refused(&fx, FEATURE_BRANCHES_ONLY).await;
        }
    }

    #[tokio::test]
    async fn start_refused_with_a_modified_file() {
        let fx = Fixture::new().await;
        diverge_clean(&fx).await;
        fx.write("b.txt", b"edited\n");
        assert_start_refused(&fx, DIRTY_TREE).await;
    }

    #[tokio::test]
    async fn start_refused_with_an_untracked_file() {
        let fx = Fixture::new().await;
        diverge_clean(&fx).await;
        fx.write("new.txt", b"new\n");
        assert_start_refused(&fx, DIRTY_TREE).await;
        assert_eq!(read(&fx, "new.txt").as_deref(), Some(&b"new\n"[..]));
    }

    #[tokio::test]
    async fn start_refused_with_an_unpushed_commit() {
        let fx = Fixture::new().await;
        diverge_clean(&fx).await;
        fx.commit_file("local.txt", b"local\n", "local only").await;
        assert_start_refused_after_fetch(&fx, UNSYNCED).await;
    }

    #[tokio::test]
    async fn start_refused_when_a_collaborator_pushed_to_the_branch() {
        let fx = Fixture::new().await;
        diverge_clean(&fx).await;
        collaborator_push(&fx, BRANCH, "collab.txt").await;
        assert_start_refused_after_fetch(&fx, UNSYNCED).await;
    }

    #[tokio::test]
    async fn start_refused_on_a_branch_never_pushed() {
        let fx = Fixture::new().await;
        diverge_clean(&fx).await;
        fx.branch("f11r-branch-new").await;
        fx.commit_file("new.txt", b"new\n", "new branch").await;
        assert_start_refused_after_fetch(&fx, UNSYNCED).await;
    }

    #[tokio::test]
    async fn start_refused_on_a_branch_deleted_on_origin() {
        let fx = Fixture::new().await;
        diverge_clean(&fx).await;
        // The local tracking ref still matches HEAD, but origin no longer has the branch.
        let refname = format!("refs/heads/{BRANCH}");
        new_git(fx.origin_path())
            .run(
                &["update-ref", "-d", &refname],
                git::Opts::new_without_logs(),
            )
            .await
            .unwrap();
        assert_start_refused_after_fetch(&fx, UNSYNCED).await;
    }

    #[tokio::test]
    async fn start_refused_when_the_branch_contains_latest_main() {
        let fx = Fixture::new().await;
        fx.branch(BRANCH).await;
        fx.commit_file("b.txt", b"branch\n", "branch change").await;
        fx.push(BRANCH).await;
        assert_start_refused_after_fetch(
            &fx,
            &format!("{BRANCH} already contains the latest main."),
        )
        .await;
    }

    // Start outcomes

    #[tokio::test]
    async fn start_replays_cleanly() {
        let fx = Fixture::new().await;
        let d = diverge_clean(&fx).await;
        let origin_before = capture(&fx).await.origin_refs;
        let (op, phases) = start_op(&fx);
        op.execute().await.expect("start");

        let file = record(&fx);
        let ts = file
            .backup_ref
            .strip_prefix(&format!("refs/f11r-backup/{BRANCH}/"))
            .expect("backup ref prefix");
        assert!(
            !ts.is_empty() && ts.chars().all(|c| c.is_ascii_digit()),
            "{ts}"
        );
        assert_eq!(fx.sha(&file.backup_ref).await, d.branch);
        assert!(chrono::DateTime::parse_from_rfc3339(&file.started_at).is_ok());
        assert_eq!(
            file,
            GuidedRebaseFile {
                version: REBASE_FILE_VERSION,
                branch: BRANCH.to_string(),
                trunk: "main".to_string(),
                phase: GuidedRebasePhase::Rebasing,
                backup_ref: file.backup_ref.clone(),
                backup_sha: d.branch.clone(),
                pre_rebase_origin_sha: d.branch.clone(),
                onto: d.main.clone(),
                merge_base: d.base.clone(),
                original_commits: vec![original(
                    &d.branch,
                    "user@example.com",
                    &author_date(&fx, &d.branch).await,
                    "branch change",
                )],
                touches_cpp: false,
                engine_association: String::new(),
                started_at: file.started_at.clone(),
                push_failed_remote_sha: None,
                current_step: None,
            }
        );

        assert_eq!(
            state(&fx).await,
            RebaseState {
                state: RebaseStateKind::CompletedUnpushed,
                branch: BRANCH.to_string(),
                backup_ref: file.backup_ref.clone(),
                backup_sha: d.branch.clone(),
                ..Default::default()
            }
        );
        assert_eq!(fx.sha("HEAD~1").await, d.main);
        assert_eq!(fx.out(&["log", "-1", "--format=%s"]).await, "branch change");
        assert_eq!(fx.porcelain_v2().await, "");
        assert_eq!(capture(&fx).await.origin_refs, origin_before);
        assert_eq!(
            phases.try_iter().collect::<Vec<_>>(),
            [
                "Checking repository status",
                "Fetching latest",
                "Replaying commits"
            ]
        );
    }

    #[tokio::test]
    async fn start_record_reads_back_as_valid() {
        let fx = Fixture::new().await;
        diverge_on_a_txt(&fx).await;
        start(&fx).await.expect("start");
        let bytes = fs::read(json_path(&fx.git_dir())).unwrap();
        let file: GuidedRebaseFile = serde_json::from_slice(&bytes).unwrap();
        assert!(
            matches!(read_rebase_file(&fx.git_dir()), RebaseFileRead::Ok(read) if *read == file)
        );
    }

    #[tokio::test]
    async fn start_stops_on_a_conflict() {
        let fx = Fixture::new().await;
        let d = diverge_on_a_txt(&fx).await;
        start(&fx).await.expect("start");

        let file = record(&fx);
        let expected_conflict = conflict("a.txt", ConflictKind::Content, &d);
        assert_eq!(
            state(&fx).await,
            RebaseState {
                state: RebaseStateKind::InProgress,
                branch: BRANCH.to_string(),
                step: 1,
                total_steps: 1,
                current_commit: Some(info(&d.branch, "branch change")),
                conflicts: vec![expected_conflict.clone()],
                step_incomplete: false,
                backup_ref: file.backup_ref.clone(),
                backup_sha: d.branch.clone(),
                ..Default::default()
            }
        );
        let step = file.current_step.expect("cached step");
        assert_eq!(step.sha, fx.rebase_head());
        assert_eq!(step.conflicts, vec![expected_conflict]);
    }

    #[tokio::test]
    async fn start_detects_cpp_changes() {
        let fx = Fixture::new().await;
        let path = "Game/Source/M/M.Build.cs";
        fx.diverge(
            &[],
            &[Change::Write("m.txt", b"main\n")],
            &[Change::Write(path, b"build\n")],
        )
        .await;
        assert_eq!(
            preflight(&fx.git, fx.path(), "main").await.unwrap(),
            RebasePreflight {
                touches_cpp: true,
                cpp_files: vec![path.to_string()],
                commit_count: 1,
            }
        );
        start(&fx).await.expect("start");
        assert!(record(&fx).touches_cpp);
        assert!(state(&fx).await.touches_cpp);
    }

    #[tokio::test]
    async fn start_ignores_content_named_like_source() {
        let fx = Fixture::new().await;
        fx.diverge(
            &[],
            &[Change::Write("m.txt", b"main\n")],
            &[Change::Write("Content/Sourceful/x.uasset", b"asset\n")],
        )
        .await;
        assert_eq!(
            preflight(&fx.git, fx.path(), "main").await.unwrap(),
            RebasePreflight {
                touches_cpp: false,
                cpp_files: vec![],
                commit_count: 1,
            }
        );
        start(&fx).await.expect("start");
        assert!(!record(&fx).touches_cpp);
        assert!(!state(&fx).await.touches_cpp);
    }

    #[tokio::test]
    async fn start_ignores_hostile_user_config() {
        let fx = Fixture::new().await;
        fx.commit_file("a.txt", b"base\n", "base change").await;
        fx.branch(BRANCH).await;
        let target = fx.commit_file("b.txt", b"b\n", "branch change").await;
        fx.commit_file("f.txt", b"f\n", "fixup! branch change")
            .await;
        fx.commit_file("a.txt", b"branch\n", "conflicting change")
            .await;
        fx.run(&["branch", "f11r-branch-side", &target]).await;
        fx.push(BRANCH).await;
        fx.checkout("main").await;
        fx.commit_file("a.txt", b"main\n", "main change").await;
        fx.push("main").await;
        fx.checkout(BRANCH).await;

        // Record a resolution of the same conflict for rerere to replay.
        let editor: &[(&str, &str)] = &[("GIT_EDITOR", "true")];
        let stopped = fx
            .git
            .run_with_status(
                &["-c", "rerere.enabled=true", "rebase", "--merge", "main"],
                git::Opts::new_without_logs().with_env(editor),
            )
            .await
            .unwrap();
        assert_eq!(stopped.exit_code, 1, "{}", stopped.stderr);
        fx.write("a.txt", b"recorded resolution\n");
        fx.run(&["-c", "rerere.enabled=true", "rerere"]).await;
        fx.run(&["rebase", "--abort"]).await;
        let rr_cache = fs::read_dir(fx.git_dir().join("rr-cache")).unwrap();
        assert!(rr_cache
            .flatten()
            .any(|entry| entry.path().join("postimage").exists()));

        for (key, value) in [
            ("rebase.backend", "apply"),
            ("rebase.autoStash", "true"),
            ("rebase.updateRefs", "true"),
            ("rebase.autoSquash", "true"),
            ("rerere.enabled", "true"),
            ("core.editor", "false"),
            ("sequence.editor", "false"),
        ] {
            fx.run(&["config", key, value]).await;
        }

        start(&fx).await.expect("start");
        assert!(fx.git_dir().join("rebase-merge").is_dir());
        assert!(!fx.git_dir().join("rebase-apply").exists());
        // The fixup was replayed as its own commit before the stop.
        assert_eq!(
            fx.out(&["log", "--format=%s", "refs/remotes/origin/main..HEAD"])
                .await,
            "fixup! branch change\nbranch change"
        );
        assert_eq!(fx.sha("f11r-branch-side").await, target);
        assert_eq!(xy_of(&fx, "a.txt").await.as_deref(), Some("UU"));
        let content = String::from_utf8(read(&fx, "a.txt").unwrap()).unwrap();
        assert!(content.contains("<<<<<<<"), "{content}");
        assert!(!content.contains("recorded resolution"), "{content}");
        assert_eq!(state(&fx).await.state, RebaseStateKind::InProgress);
    }

    #[tokio::test]
    async fn start_refused_by_a_pre_rebase_hook() {
        let fx = Fixture::new().await;
        let d = diverge_clean(&fx).await;
        let hook = fx.hooks_path().join("pre-rebase");
        fs::write(&hook, "#!/bin/sh\necho refused by hook >&2\nexit 1\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
        }

        let message = internal_message(start(&fx).await);
        assert!(message.starts_with("Rebase could not start:"), "{message}");
        assert!(message.contains("refused by hook"), "{message}");
        assert!(!json_path(&fx.git_dir()).exists());
        assert_eq!(state(&fx).await, RebaseState::default());
        assert_eq!(fx.head().await, d.branch);
        let backups = backup_refs(&fx).await;
        assert_eq!(backups.len(), 1, "{backups:?}");
        assert_eq!(fx.sha(&backups[0]).await, d.branch);
    }

    #[tokio::test]
    async fn start_linearizes_merge_commits() {
        let fx = Fixture::new().await;
        fx.branch(BRANCH).await;
        fx.commit_file("x.txt", b"x\n", "x").await;
        fx.branch("side").await;
        fx.commit_file("y.txt", b"y\n", "y").await;
        fx.checkout(BRANCH).await;
        fx.commit_file("z.txt", b"z\n", "z").await;
        fx.run(&["merge", "-q", "--no-ff", "-m", "merge side", "side"])
            .await;
        fx.push(BRANCH).await;
        fx.checkout("main").await;
        fx.commit_file("m.txt", b"main\n", "main change").await;
        fx.push("main").await;
        fx.checkout(BRANCH).await;

        assert_eq!(
            preflight(&fx.git, fx.path(), "main")
                .await
                .unwrap()
                .commit_count,
            3
        );
        start(&fx).await.expect("start");
        assert_eq!(state(&fx).await.state, RebaseStateKind::CompletedUnpushed);
        assert_eq!(
            fx.out(&["rev-list", "--merges", "refs/remotes/origin/main..HEAD"])
                .await,
            ""
        );
        assert_eq!(
            fx.out(&["rev-list", "--count", "refs/remotes/origin/main..HEAD"])
                .await,
            "3"
        );
        let mut subjects: Vec<String> = record(&fx)
            .original_commits
            .into_iter()
            .map(|commit| commit.subject)
            .collect();
        subjects.sort();
        assert_eq!(subjects, ["x", "y", "z"]);
    }

    #[tokio::test]
    async fn start_works_when_a_tag_has_the_branch_name() {
        let fx = Fixture::new().await;
        diverge_clean(&fx).await;
        fx.run(&["tag", BRANCH, "main"]).await;
        start(&fx).await.expect("start");
        assert_eq!(state(&fx).await.state, RebaseStateKind::CompletedUnpushed);
        assert_eq!(record(&fx).branch, BRANCH);
    }

    /// Adds a linked worktree on a new branch; returns its dir and its git dir.
    async fn add_worktree(fx: &Fixture) -> (tempfile::TempDir, PathBuf) {
        let wt_dir = tempfile::tempdir().expect("worktree tempdir");
        fx.run(&[
            "worktree",
            "add",
            "-q",
            "-b",
            "f11r-branch-wt",
            &slash_path(wt_dir.path()),
            "main",
        ])
        .await;
        let wt_git_dir = fs::read_dir(fx.git_dir().join("worktrees"))
            .unwrap()
            .flatten()
            .next()
            .expect("linked worktree git dir")
            .path();
        (wt_dir, wt_git_dir)
    }

    #[tokio::test]
    async fn start_keeps_backups_recorded_in_other_worktrees() {
        let fx = Fixture::new().await;
        let d = diverge_clean(&fx).await;
        let (_wt_dir, wt_git_dir) = add_worktree(&fx).await;
        let old = unix_now() - 15 * DAY;
        let recorded = GuidedRebaseFile {
            backup_ref: format!("refs/f11r-backup/f11r-branch-wt/{old}"),
            ..sample_file_for("f11r-branch-wt")
        };
        write_rebase_file(&wt_git_dir, &recorded).unwrap();
        let unrecorded = format!("refs/f11r-backup/f11r-branch-gone/{old}");
        for refname in [&recorded.backup_ref, &unrecorded] {
            fx.run(&["update-ref", refname, &d.branch]).await;
        }

        start(&fx).await.expect("start");
        let backups = backup_refs(&fx).await;
        assert!(backups.contains(&recorded.backup_ref), "{backups:?}");
        assert!(!backups.contains(&unrecorded), "{backups:?}");
    }

    #[tokio::test]
    async fn start_skips_pruning_when_another_worktrees_record_is_unreadable() {
        let fx = Fixture::new().await;
        let d = diverge_clean(&fx).await;
        let (_wt_dir, wt_git_dir) = add_worktree(&fx).await;
        fs::write(json_path(&wt_git_dir), "{").unwrap();
        let old = format!(
            "refs/f11r-backup/f11r-branch-gone/{}",
            unix_now() - 15 * DAY
        );
        fx.run(&["update-ref", &old, &d.branch]).await;

        start(&fx).await.expect("start");
        assert!(backup_refs(&fx).await.contains(&old));
    }

    #[tokio::test]
    async fn start_prunes_old_backup_refs() {
        let fx = Fixture::new().await;
        let d = diverge_clean(&fx).await;
        let now = unix_now();
        let old = format!("refs/f11r-backup/{BRANCH}/{}", now - 15 * DAY);
        let recent = format!("refs/f11r-backup/{BRANCH}/{}", now - 13 * DAY);
        let notes = format!("refs/f11r-backup/{BRANCH}/notes");
        for refname in [&old, &recent, &notes] {
            fx.run(&["update-ref", refname, &d.branch]).await;
        }

        start(&fx).await.expect("start");
        let backups = backup_refs(&fx).await;
        assert!(!backups.contains(&old), "{backups:?}");
        assert!(backups.contains(&recent), "{backups:?}");
        assert!(backups.contains(&notes), "{backups:?}");
        assert!(backups.contains(&record(&fx).backup_ref), "{backups:?}");
    }

    // Preflight

    #[tokio::test]
    async fn preflight_is_read_only_and_does_not_fetch() {
        let fx = Fixture::new().await;
        fx.diverge(
            &[],
            &[Change::Write("m.txt", b"main\n")],
            &[
                Change::Write("Source/G/a.cpp", b"cpp\n"),
                Change::Write("readme.txt", b"text\n"),
            ],
        )
        .await;
        collaborator_push(&fx, "main", "collab.txt").await;

        let before = capture(&fx).await;
        assert_eq!(
            preflight(&fx.git, fx.path(), "main").await.unwrap(),
            RebasePreflight {
                touches_cpp: true,
                cpp_files: vec!["Source/G/a.cpp".to_string()],
                commit_count: 1,
            }
        );
        assert_eq!(capture(&fx).await, before);
    }

    #[tokio::test]
    async fn preflight_refuses_a_missing_trunk_and_a_detached_head() {
        let fx = Fixture::new().await;
        diverge_clean(&fx).await;
        let message = input_message(preflight(&fx.git, fx.path(), "trunk").await);
        assert!(message.contains("refs/remotes/origin/trunk"), "{message}");
        fx.run(&["checkout", "-q", "--detach"]).await;
        let message = input_message(preflight(&fx.git, fx.path(), "main").await);
        assert!(message.contains("HEAD"), "{message}");
    }

    // Resolve

    /// Stops a guided rebase on `a.txt` as given plus a content conflict on `o.txt`.
    async fn stop(fx: &Fixture, main: Change<'_>, branch: Change<'_>) -> Diverged {
        let d = fx
            .diverge(
                &[
                    Change::Write("a.txt", b"base\n"),
                    Change::Write(OTHER, b"base o\n"),
                ],
                &[main, Change::Write(OTHER, b"main o\n")],
                &[branch, Change::Write(OTHER, b"branch o\n")],
            )
            .await;
        start(fx).await.expect("start");
        assert_eq!(state(fx).await.state, RebaseStateKind::InProgress);
        d
    }

    async fn stop_content(fx: &Fixture) -> Diverged {
        stop(
            fx,
            Change::Write("a.txt", b"main\n"),
            Change::Write("a.txt", b"branch\n"),
        )
        .await
    }

    async fn stop_deleted_on_main(fx: &Fixture) -> Diverged {
        stop(
            fx,
            Change::Delete("a.txt"),
            Change::Write("a.txt", b"branch\n"),
        )
        .await
    }

    async fn stop_deleted_on_branch(fx: &Fixture) -> Diverged {
        stop(
            fx,
            Change::Write("a.txt", b"main\n"),
            Change::Delete("a.txt"),
        )
        .await
    }

    async fn conflict_in_state(fx: &Fixture, path: &str) -> ConflictFile {
        state(fx)
            .await
            .conflicts
            .into_iter()
            .find(|c| c.path == path)
            .unwrap_or_else(|| panic!("{path} is not a conflict"))
    }

    /// `a.txt` is resolved as `keep` in the index, the state and the record; `o.txt` is
    /// untouched.
    async fn assert_resolved(fx: &Fixture, keep: RebaseKeep, other_before: &[u8]) {
        assert_eq!(xy_of(fx, "a.txt").await, None);
        let resolved = conflict_in_state(fx, "a.txt").await;
        assert!(resolved.resolved);
        assert_eq!(resolved.resolution, Some(keep));
        let step = record(fx).current_step.expect("cached step");
        let cached = step.conflicts.iter().find(|c| c.path == "a.txt").unwrap();
        assert_eq!(cached.resolution, Some(keep));

        assert_eq!(xy_of(fx, OTHER).await.as_deref(), Some("UU"));
        assert_eq!(read(fx, OTHER).as_deref(), Some(other_before));
        assert!(!conflict_in_state(fx, OTHER).await.resolved);
    }

    async fn resolve_and_check(fx: &Fixture, keep: RebaseKeep, expected: Option<&[u8]>) {
        let other_before = read(fx, OTHER).unwrap();
        resolve(fx, "a.txt", keep).await.expect("resolve");
        assert_eq!(read(fx, "a.txt").as_deref(), expected);
        assert_resolved(fx, keep, &other_before).await;
    }

    #[tokio::test]
    async fn resolve_content_keep_main() {
        let fx = Fixture::new().await;
        stop_content(&fx).await;
        resolve_and_check(&fx, RebaseKeep::Main, Some(b"main\n")).await;
    }

    #[tokio::test]
    async fn resolve_content_keep_branch() {
        let fx = Fixture::new().await;
        stop_content(&fx).await;
        resolve_and_check(&fx, RebaseKeep::Branch, Some(b"branch\n")).await;
    }

    #[tokio::test]
    async fn resolve_content_manual_refuses_markers() {
        let fx = Fixture::new().await;
        stop_content(&fx).await;
        let before = read(&fx, "a.txt");
        assert_eq!(
            input_message(resolve(&fx, "a.txt", RebaseKeep::Manual).await),
            "a.txt still contains conflict markers."
        );
        assert_eq!(xy_of(&fx, "a.txt").await.as_deref(), Some("UU"));
        assert_eq!(read(&fx, "a.txt"), before);
        assert_eq!(conflict_in_state(&fx, "a.txt").await.resolution, None);
    }

    #[tokio::test]
    async fn resolve_content_manual_accepts_a_hand_merge() {
        let fx = Fixture::new().await;
        stop_content(&fx).await;
        fx.write("a.txt", b"merged by hand\n");
        resolve_and_check(&fx, RebaseKeep::Manual, Some(b"merged by hand\n")).await;
    }

    #[tokio::test]
    async fn resolve_deleted_on_main_keep_main() {
        let fx = Fixture::new().await;
        stop_deleted_on_main(&fx).await;
        assert_eq!(xy_of(&fx, "a.txt").await.as_deref(), Some("DU"));
        resolve_and_check(&fx, RebaseKeep::Main, None).await;
    }

    #[tokio::test]
    async fn resolve_deleted_on_main_keep_branch() {
        let fx = Fixture::new().await;
        stop_deleted_on_main(&fx).await;
        resolve_and_check(&fx, RebaseKeep::Branch, Some(b"branch\n")).await;
    }

    #[tokio::test]
    async fn resolve_deleted_on_branch_keep_main() {
        let fx = Fixture::new().await;
        stop_deleted_on_branch(&fx).await;
        assert_eq!(xy_of(&fx, "a.txt").await.as_deref(), Some("UD"));
        resolve_and_check(&fx, RebaseKeep::Main, Some(b"main\n")).await;
    }

    #[tokio::test]
    async fn resolve_deleted_on_branch_keep_branch() {
        let fx = Fixture::new().await;
        stop_deleted_on_branch(&fx).await;
        resolve_and_check(&fx, RebaseKeep::Branch, None).await;
    }

    #[tokio::test]
    async fn resolve_manual_refused_for_modify_delete() {
        for deleted_on_main in [true, false] {
            let fx = Fixture::new().await;
            if deleted_on_main {
                stop_deleted_on_main(&fx).await;
            } else {
                stop_deleted_on_branch(&fx).await;
            }
            let before = capture(&fx).await;
            assert_eq!(
                input_message(resolve(&fx, "a.txt", RebaseKeep::Manual).await),
                TEXT_ONLY
            );
            assert_eq!(capture(&fx).await, before);
        }
    }

    #[tokio::test]
    async fn resolve_twice_is_refused() {
        let fx = Fixture::new().await;
        stop_content(&fx).await;
        resolve(&fx, "a.txt", RebaseKeep::Main).await.unwrap();
        assert_eq!(
            input_message(resolve(&fx, "a.txt", RebaseKeep::Branch).await),
            "a.txt is already resolved. Undo it first to choose again."
        );
        assert_eq!(read(&fx, "a.txt").as_deref(), Some(&b"main\n"[..]));
    }

    #[tokio::test]
    async fn resolve_refused_in_a_step_with_an_unsupported_conflict() {
        let fx = Fixture::new().await;
        fx.diverge(
            &[Change::Write(OTHER, b"base o\n")],
            &[
                Change::Write("n.txt", b"main\n"),
                Change::Write(OTHER, b"main o\n"),
            ],
            &[
                Change::Write("n.txt", b"branch\n"),
                Change::Write(OTHER, b"branch o\n"),
            ],
        )
        .await;
        start(&fx).await.expect("start");
        assert_eq!(xy_of(&fx, "n.txt").await.as_deref(), Some("AA"));
        let before = capture(&fx).await;
        assert_eq!(
            input_message(resolve(&fx, "n.txt", RebaseKeep::Main).await),
            UNRESOLVABLE_CONFLICT
        );
        assert_eq!(
            input_message(resolve(&fx, OTHER, RebaseKeep::Main).await),
            UNRESOLVABLE_STEP
        );
        assert_eq!(capture(&fx).await, before);
    }

    /// A content conflict on `path` that is not text refuses a manual resolution.
    async fn assert_manual_refused(attributes: Option<&[u8]>, path: &str, sides: [&[u8]; 3]) {
        let [base, main, branch] = sides;
        let fx = Fixture::new().await;
        let mut base_changes = vec![];
        if let Some(attributes) = attributes {
            base_changes.push(Change::Write(".gitattributes", attributes));
        }
        base_changes.push(Change::Write(path, base));
        fx.diverge(
            &base_changes,
            &[Change::Write(path, main)],
            &[Change::Write(path, branch)],
        )
        .await;
        start(&fx).await.expect("start");
        assert!(!conflict_in_state(&fx, path).await.is_text);
        assert_eq!(
            input_message(resolve(&fx, path, RebaseKeep::Manual).await),
            TEXT_ONLY
        );
        assert_eq!(xy_of(&fx, path).await.as_deref(), Some("UU"));
    }

    #[tokio::test]
    async fn resolve_manual_refused_for_a_binary_file() {
        let sides: [&[u8]; 3] = [b"base\0bin\n", b"main\0bin\n", b"branch\0bin\n"];
        assert_manual_refused(None, "x.bin", sides).await;
    }

    #[tokio::test]
    async fn resolve_manual_refused_for_an_lfs_file() {
        let sides: [&[u8]; 3] = [b"base\n", b"main\n", b"branch\n"];
        assert_manual_refused(Some(b"*.uasset filter=lfs\n"), "Content/a.uasset", sides).await;
    }

    #[tokio::test]
    async fn resolve_refused_for_a_path_outside_the_step() {
        let fx = Fixture::new().await;
        stop_content(&fx).await;
        assert_eq!(
            input_message(resolve(&fx, "seed.txt", RebaseKeep::Main).await),
            "seed.txt is not a conflict in this step."
        );
        assert_eq!(read(&fx, "seed.txt").as_deref(), Some(&b"seed\n"[..]));
    }

    #[tokio::test]
    async fn resolve_refused_without_a_rebase_in_progress() {
        let fx = Fixture::new().await;
        assert_eq!(
            input_message(resolve(&fx, "a.txt", RebaseKeep::Main).await),
            NO_REBASE_IN_PROGRESS
        );
        fx.guided_complete().await;
        assert_eq!(
            input_message(resolve(&fx, "a.txt", RebaseKeep::Main).await),
            NO_REBASE_IN_PROGRESS
        );
    }

    #[tokio::test]
    async fn resolve_refused_when_the_step_is_incomplete() {
        let fx = Fixture::new().await;
        guided_stop_on_a_txt(&fx).await;
        // Resolved outside Friendshipper before any conflict was recorded.
        fx.run(&["checkout", "--theirs", "--", "a.txt"]).await;
        fx.run(&["add", "--", "a.txt"]).await;
        assert!(state(&fx).await.step_incomplete);
        assert_eq!(
            input_message(resolve(&fx, "a.txt", RebaseKeep::Main).await),
            STEP_INCOMPLETE
        );
        assert_eq!(read(&fx, "a.txt").as_deref(), Some(&b"branch\n"[..]));
    }

    #[tokio::test]
    async fn resolve_records_the_step_before_staging() {
        let fx = Fixture::new().await;
        fx.guided_stop(
            &[
                Change::Write("a.txt", b"base\n"),
                Change::Write(OTHER, b"base o\n"),
            ],
            &[
                Change::Write("a.txt", b"main\n"),
                Change::Write(OTHER, b"main o\n"),
            ],
            &[
                Change::Write("a.txt", b"branch\n"),
                Change::Write(OTHER, b"branch o\n"),
            ],
        )
        .await;
        assert_eq!(record(&fx).current_step, None);

        resolve(&fx, "a.txt", RebaseKeep::Main).await.unwrap();
        let step = record(&fx).current_step.expect("cached step");
        assert_eq!(step.sha, fx.rebase_head());
        assert_eq!(step.conflicts.len(), 2);
        resolve(&fx, OTHER, RebaseKeep::Branch).await.unwrap();

        let state = state(&fx).await;
        assert!(!state.step_incomplete);
        assert!(state.conflicts.iter().all(|c| c.resolved));
        assert!(unmerged(&fx).await.is_empty());
    }

    // Undo

    /// Resolves `a.txt` with `keep`, undoes it and checks the conflict is back as git left it;
    /// then resolves it with `again`, and `o.txt` too, leaving the step ready to continue.
    async fn undo_case(
        fx: &Fixture,
        xy: &str,
        keep: RebaseKeep,
        again: RebaseKeep,
        again_expected: Option<&[u8]>,
    ) {
        resolve(fx, "a.txt", keep).await.expect("resolve");
        unresolve(fx, "a.txt").await.expect("unresolve");

        assert_eq!(xy_of(fx, "a.txt").await.as_deref(), Some(xy));
        let content = String::from_utf8(read(fx, "a.txt").expect("a.txt restored")).unwrap();
        match xy {
            "UU" => {
                assert!(content.contains("<<<<<<< ours"), "{content}");
                assert!(content.contains(">>>>>>> theirs"), "{content}");
                assert!(content.contains("main\n") && content.contains("branch\n"));
            }
            "DU" => assert_eq!(content, "branch\n"),
            "UD" => assert_eq!(content, "main\n"),
            _ => unreachable!(),
        }
        let undone = conflict_in_state(fx, "a.txt").await;
        assert!(!undone.resolved);
        assert_eq!(undone.resolution, None);
        let step = record(fx).current_step.expect("cached step");
        let cached = step.conflicts.iter().find(|c| c.path == "a.txt").unwrap();
        assert_eq!(cached.resolution, None);

        resolve(fx, "a.txt", again).await.expect("resolve again");
        assert_eq!(read(fx, "a.txt").as_deref(), again_expected);
        assert_eq!(conflict_in_state(fx, "a.txt").await.resolution, Some(again));
        resolve(fx, OTHER, RebaseKeep::Main)
            .await
            .expect("resolve o.txt");
        assert!(!state(fx).await.step_incomplete);
        assert!(unmerged(fx).await.is_empty());
    }

    #[tokio::test]
    async fn undo_content_keep_main() {
        let fx = Fixture::new().await;
        stop_content(&fx).await;
        undo_case(
            &fx,
            "UU",
            RebaseKeep::Main,
            RebaseKeep::Branch,
            Some(b"branch\n"),
        )
        .await;
    }

    #[tokio::test]
    async fn undo_content_keep_branch() {
        let fx = Fixture::new().await;
        stop_content(&fx).await;
        undo_case(
            &fx,
            "UU",
            RebaseKeep::Branch,
            RebaseKeep::Main,
            Some(b"main\n"),
        )
        .await;
    }

    #[tokio::test]
    async fn undo_content_manual() {
        let fx = Fixture::new().await;
        stop_content(&fx).await;
        fx.write("a.txt", b"merged by hand\n");
        undo_case(
            &fx,
            "UU",
            RebaseKeep::Manual,
            RebaseKeep::Main,
            Some(b"main\n"),
        )
        .await;
    }

    #[tokio::test]
    async fn undo_manual_discards_the_hand_edit() {
        let fx = Fixture::new().await;
        stop_content(&fx).await;
        fx.write("a.txt", b"merged by hand\n");
        resolve(&fx, "a.txt", RebaseKeep::Manual).await.unwrap();
        unresolve(&fx, "a.txt").await.unwrap();
        let content = String::from_utf8(read(&fx, "a.txt").unwrap()).unwrap();
        assert!(!content.contains("merged by hand"), "{content}");
        assert!(content.contains("<<<<<<<"), "{content}");
    }

    #[tokio::test]
    async fn undo_deleted_on_main_keep_main() {
        let fx = Fixture::new().await;
        stop_deleted_on_main(&fx).await;
        undo_case(
            &fx,
            "DU",
            RebaseKeep::Main,
            RebaseKeep::Branch,
            Some(b"branch\n"),
        )
        .await;
    }

    #[tokio::test]
    async fn undo_deleted_on_main_keep_branch() {
        let fx = Fixture::new().await;
        stop_deleted_on_main(&fx).await;
        undo_case(&fx, "DU", RebaseKeep::Branch, RebaseKeep::Main, None).await;
    }

    #[tokio::test]
    async fn undo_deleted_on_branch_keep_main() {
        let fx = Fixture::new().await;
        stop_deleted_on_branch(&fx).await;
        undo_case(&fx, "UD", RebaseKeep::Main, RebaseKeep::Branch, None).await;
    }

    #[tokio::test]
    async fn undo_deleted_on_branch_keep_branch() {
        let fx = Fixture::new().await;
        stop_deleted_on_branch(&fx).await;
        undo_case(
            &fx,
            "UD",
            RebaseKeep::Branch,
            RebaseKeep::Main,
            Some(b"main\n"),
        )
        .await;
    }

    #[tokio::test]
    async fn undo_refused_without_a_resolution() {
        let fx = Fixture::new().await;
        stop_content(&fx).await;
        let before = capture(&fx).await;
        for path in ["a.txt", "unknown.txt"] {
            assert_eq!(
                input_message(unresolve(&fx, path).await),
                format!("{path} has no resolution to undo in this step.")
            );
        }
        assert_eq!(capture(&fx).await, before);
    }

    #[tokio::test]
    async fn undo_refused_without_a_rebase_in_progress() {
        let fx = Fixture::new().await;
        assert_eq!(
            input_message(unresolve(&fx, "a.txt").await),
            NO_REBASE_IN_PROGRESS
        );
    }

    #[tokio::test]
    async fn undo_a_file_resolved_outside_friendshipper() {
        let fx = Fixture::new().await;
        stop_content(&fx).await;
        fx.run(&["checkout", "--theirs", "--", "a.txt"]).await;
        fx.run(&["add", "--", "a.txt"]).await;
        let resolved = conflict_in_state(&fx, "a.txt").await;
        assert!(resolved.resolved);
        assert_eq!(resolved.resolution, None);

        unresolve(&fx, "a.txt").await.expect("unresolve");
        assert_eq!(xy_of(&fx, "a.txt").await.as_deref(), Some("UU"));
        let content = String::from_utf8(read(&fx, "a.txt").unwrap()).unwrap();
        assert!(content.contains("<<<<<<< ours"), "{content}");
        resolve(&fx, "a.txt", RebaseKeep::Main)
            .await
            .expect("resolve again");
        assert_eq!(read(&fx, "a.txt").as_deref(), Some(&b"main\n"[..]));
    }

    #[tokio::test]
    async fn undo_a_binary_conflict() {
        let fx = Fixture::new().await;
        fx.diverge(
            &[Change::Write("x.bin", b"base\0bin\n")],
            &[Change::Write("x.bin", b"main\0bin\n")],
            &[Change::Write("x.bin", b"branch\0bin\n")],
        )
        .await;
        start(&fx).await.expect("start");
        let at_stop = read(&fx, "x.bin");
        resolve(&fx, "x.bin", RebaseKeep::Branch).await.unwrap();
        unresolve(&fx, "x.bin").await.expect("unresolve");
        assert_eq!(xy_of(&fx, "x.bin").await.as_deref(), Some("UU"));
        // Git leaves main's version of a binary conflict, and `checkout -m` restores it.
        assert_eq!(read(&fx, "x.bin"), at_stop);
        assert_eq!(conflict_in_state(&fx, "x.bin").await.resolution, None);
    }

    #[tokio::test]
    async fn undo_fails_when_the_resolve_undo_data_is_lost() {
        let fx = Fixture::new().await;
        stop_content(&fx).await;
        resolve(&fx, "a.txt", RebaseKeep::Main).await.unwrap();
        fx.run(&["update-index", "--clear-resolve-undo"]).await;

        assert_eq!(
            internal_message(unresolve(&fx, "a.txt").await),
            "Could not restore the conflict for a.txt. Abort the rebase to start over."
        );
        assert_eq!(xy_of(&fx, "a.txt").await, None);
        assert_eq!(read(&fx, "a.txt").as_deref(), Some(&b"main\n"[..]));
        let still = conflict_in_state(&fx, "a.txt").await;
        assert!(still.resolved);
        assert_eq!(still.resolution, Some(RebaseKeep::Main));
    }

    // Continue

    fn continue_op(
        fx: &Fixture,
    ) -> (
        RebaseContinueOp<UnrealEngineProvider>,
        mpsc::Receiver<String>,
    ) {
        let refs = refs(fx);
        let (sync_phase_tx, phases) = mpsc::channel();
        let op = RebaseContinueOp {
            git_client: fx.git.clone(),
            engine: refs.engine,
            app_config: refs.app_config,
            repo_config: refs.repo_config,
            repo_status: refs.repo_status,
            sync_phase_tx,
        };
        (op, phases)
    }

    async fn continue_rebase(fx: &Fixture) -> Result<(), CoreError> {
        continue_op(fx).0.execute().await
    }

    /// Continue is refused with `expected` and changes nothing.
    async fn assert_continue_refused(fx: &Fixture, expected: &str) {
        let before = capture(fx).await;
        assert_eq!(input_message(continue_rebase(fx).await), expected);
        assert_eq!(capture(fx).await, before);
    }

    fn conflict_file(
        path: &str,
        kind: ConflictKind,
        deleted_on: Option<ConflictSideName>,
        is_text: bool,
        main: &str,
        branch: (&str, &str),
    ) -> ConflictFile {
        ConflictFile {
            path: path.to_string(),
            display_name: path.rsplit('/').next().unwrap().to_string(),
            kind,
            deleted_on,
            unsupported_reason: None,
            is_text,
            resolved: false,
            resolution: None,
            main_side: Some(info(main, "main change")),
            branch_side: info(branch.0, branch.1),
        }
    }

    fn dropped(sha: &str, subject: &str) -> DroppedCommit {
        DroppedCommit {
            sha: sha.to_string(),
            subject: subject.to_string(),
        }
    }

    /// The state of a finished rebase that has not been pushed.
    fn completed(file: &GuidedRebaseFile, dropped_commits: Vec<DroppedCommit>) -> RebaseState {
        RebaseState {
            state: RebaseStateKind::CompletedUnpushed,
            branch: BRANCH.to_string(),
            dropped_commits,
            backup_ref: file.backup_ref.clone(),
            backup_sha: file.backup_sha.clone(),
            ..Default::default()
        }
    }

    struct FourCommits {
        base: String,
        main: String,
        commits: [String; 4],
    }

    /// `BRANCH` with four commits: C1 conflicts on `a.txt`, C2 is clean, C3 conflicts on the
    /// LFS-attributed `b.uasset` and on `c.txt`, which main deleted, and C4 is clean.
    async fn four_commits(fx: &Fixture) -> FourCommits {
        let base = fx
            .commit(
                &[
                    Change::Write(".gitattributes", b"*.uasset filter=lfs\n"),
                    Change::Write("a.txt", b"base a\n"),
                    Change::Write("b.uasset", b"base\0b\n"),
                    Change::Write("c.txt", b"base c\n"),
                ],
                "base change",
            )
            .await;
        fx.branch(BRANCH).await;
        let c1 = fx.commit_file("a.txt", b"branch a\n", "C1").await;
        let c2 = fx.commit_file("d.txt", b"d\n", "C2").await;
        let c3 = fx
            .commit(
                &[
                    Change::Write("b.uasset", b"branch\0b\n"),
                    Change::Write("c.txt", b"branch c\n"),
                ],
                "C3",
            )
            .await;
        let c4 = fx.commit_file("e.txt", b"e\n", "C4").await;
        fx.push(BRANCH).await;
        fx.checkout("main").await;
        let main = fx
            .commit(
                &[
                    Change::Write("a.txt", b"main a\n"),
                    Change::Write("b.uasset", b"main\0b\n"),
                    Change::Delete("c.txt"),
                ],
                "main change",
            )
            .await;
        fx.push("main").await;
        fx.checkout(BRANCH).await;
        FourCommits {
            base,
            main,
            commits: [c1, c2, c3, c4],
        }
    }

    /// Starts the four-commit rebase, keeps the branch's `a.txt` and continues to C3.
    async fn continue_to_c3(fx: &Fixture) {
        start(fx).await.expect("start");
        resolve(fx, "a.txt", RebaseKeep::Branch).await.unwrap();
        continue_rebase(fx).await.expect("continue to C3");
        assert_eq!(state(fx).await.step, 3);
    }

    /// `(author, email, date, subject)` of each commit in `range`, oldest first.
    async fn commit_identities(fx: &Fixture, range: &str) -> String {
        fx.out(&[
            "log",
            "--reverse",
            "--date=raw",
            "--format=%an%x1f%ae%x1f%ad%x1f%s",
            range,
        ])
        .await
    }

    #[tokio::test]
    async fn continue_replays_every_step() {
        let fx = Fixture::new().await;
        let four = four_commits(&fx).await;
        let [c1, _, c3, _] = &four.commits;
        start(&fx).await.expect("start");
        let file = record(&fx);
        let in_progress = RebaseState {
            state: RebaseStateKind::InProgress,
            branch: BRANCH.to_string(),
            total_steps: 4,
            backup_ref: file.backup_ref.clone(),
            backup_sha: file.backup_sha.clone(),
            ..Default::default()
        };
        assert_eq!(
            state(&fx).await,
            RebaseState {
                step: 1,
                current_commit: Some(info(c1, "C1")),
                conflicts: vec![conflict_file(
                    "a.txt",
                    ConflictKind::Content,
                    None,
                    true,
                    &four.main,
                    (c1, "C1"),
                )],
                ..in_progress.clone()
            }
        );

        resolve(&fx, "a.txt", RebaseKeep::Branch).await.unwrap();
        let (op, phases) = continue_op(&fx);
        op.execute().await.expect("continue to C3");
        assert_eq!(phases.try_iter().collect::<Vec<_>>(), ["Replaying commits"]);
        let c3_conflicts = vec![
            conflict_file(
                "b.uasset",
                ConflictKind::Content,
                None,
                false,
                &four.main,
                (c3, "C3"),
            ),
            conflict_file(
                "c.txt",
                ConflictKind::ModifyDelete,
                Some(ConflictSideName::Main),
                true,
                &four.main,
                (c3, "C3"),
            ),
        ];
        assert_eq!(
            state(&fx).await,
            RebaseState {
                step: 3,
                current_commit: Some(info(c3, "C3")),
                conflicts: c3_conflicts.clone(),
                ..in_progress.clone()
            }
        );
        let step = record(&fx).current_step.expect("cached step");
        assert_eq!(step.sha, *c3);
        assert_eq!(step.conflicts, c3_conflicts);

        resolve(&fx, "b.uasset", RebaseKeep::Main).await.unwrap();
        resolve(&fx, "c.txt", RebaseKeep::Branch).await.unwrap();
        continue_rebase(&fx).await.expect("continue to the end");
        assert_eq!(state(&fx).await, completed(&file, vec![]));
        assert_eq!(fx.build(true).await, completed(&file, vec![]));
        assert_eq!(record(&fx).current_step, None);

        // The tree is main plus the choices made, and every commit kept its identity.
        assert_eq!(fx.sha("HEAD~4").await, four.main);
        assert_eq!(
            fx.out(&["ls-tree", "-r", "--name-only", "HEAD"]).await,
            ".gitattributes\na.txt\nb.uasset\nc.txt\nd.txt\ne.txt\nseed.txt"
        );
        for (path, side) in [
            ("a.txt", c1.as_str()),
            ("b.uasset", four.main.as_str()),
            ("c.txt", c3.as_str()),
            ("d.txt", four.commits[1].as_str()),
            ("e.txt", four.commits[3].as_str()),
        ] {
            assert_eq!(
                fx.sha(&format!("HEAD:{path}")).await,
                fx.sha(&format!("{side}:{path}")).await,
                "{path}"
            );
        }
        assert_eq!(
            commit_identities(&fx, "refs/remotes/origin/main..HEAD").await,
            commit_identities(&fx, &format!("{}..{}", four.base, file.backup_sha)).await
        );
        assert_eq!(fx.porcelain_v2().await, "");
    }

    #[tokio::test]
    async fn continue_drops_a_commit_its_resolution_emptied() {
        let fx = Fixture::new().await;
        let d = diverge_on_a_txt(&fx).await;
        start(&fx).await.expect("start");
        resolve(&fx, "a.txt", RebaseKeep::Main).await.unwrap();
        continue_rebase(&fx).await.expect("continue");

        let expected = completed(&record(&fx), vec![dropped(&d.branch, "branch change")]);
        assert_eq!(state(&fx).await, expected);
        assert_eq!(fx.build(true).await, expected);
        assert_eq!(fx.head().await, d.main);
        assert_eq!(
            fx.out(&["rev-list", "refs/remotes/origin/main..HEAD"])
                .await,
            ""
        );
    }

    #[tokio::test]
    async fn start_drops_a_commit_already_on_main() {
        let fx = Fixture::new().await;
        fx.branch(BRANCH).await;
        let shared = fx.commit_file("s.txt", b"shared\n", "shared change").await;
        fx.commit_file("b.txt", b"b\n", "own change").await;
        fx.push(BRANCH).await;
        fx.checkout("main").await;
        fx.commit_file("m.txt", b"main\n", "main change").await;
        fx.run(&["cherry-pick", &shared]).await;
        fx.push("main").await;
        fx.checkout(BRANCH).await;

        start(&fx).await.expect("start");
        let expected = completed(&record(&fx), vec![dropped(&shared, "shared change")]);
        assert_eq!(state(&fx).await, expected);
        assert_eq!(fx.build(true).await, expected);
        assert_eq!(
            fx.out(&["log", "--format=%s", "refs/remotes/origin/main..HEAD"])
                .await,
            "own change"
        );
    }

    #[test]
    fn continue_outcomes() {
        use ContinueStep::{Completed, EndedWithError, Failed, SkipEmpty, Stopped};
        // (exit code, rebase dir, unmerged, clean tree, HEAD == H0, REBASE_HEAD == X)
        for (exit_code, rebase_dir, unmerged, clean, head_same, rh_same, expected) in [
            (0, false, false, false, false, false, Completed),
            (0, false, false, true, true, true, Completed),
            (1, false, false, true, true, true, EndedWithError),
            (128, false, false, false, false, false, EndedWithError),
            (1, true, true, false, false, false, Stopped),
            (1, true, true, true, true, true, Stopped),
            (0, true, true, false, true, true, Stopped),
            (1, true, false, true, true, true, SkipEmpty),
            (0, true, false, true, true, true, SkipEmpty),
            (1, true, false, false, true, true, Failed),
            (1, true, false, true, false, true, Failed),
            (1, true, false, true, true, false, Failed),
            (0, true, false, true, false, false, Failed),
        ] {
            assert_eq!(
                continue_outcome(exit_code, rebase_dir, unmerged, clean, head_same, rh_same),
                expected,
                "{exit_code} {rebase_dir} {unmerged} {clean} {head_same} {rh_same}"
            );
        }
    }

    /// Replaces the rest of the todo list with `count` failing `exec` lines that put
    /// REBASE_HEAD back: git then stops on the same commit with HEAD unmoved, as git before
    /// 2.47 does for a commit its resolution left empty.
    fn simulate_now_empty_stops(fx: &Fixture, count: usize) {
        let git_dir = fx.git_dir();
        fs::copy(
            git_dir.join("REBASE_HEAD"),
            git_dir.join("saved-rebase-head"),
        )
        .unwrap();
        let todo = "exec cp .git/saved-rebase-head .git/REBASE_HEAD && false\n".repeat(count);
        fs::write(git_dir.join("rebase-merge").join("git-rebase-todo"), todo).unwrap();
    }

    #[tokio::test]
    async fn continue_skips_a_commit_git_stopped_on_as_empty() {
        let fx = Fixture::new().await;
        let d = diverge_on_a_txt(&fx).await;
        start(&fx).await.expect("start");
        resolve(&fx, "a.txt", RebaseKeep::Main).await.unwrap();
        simulate_now_empty_stops(&fx, 1);

        // Git reports every command it runs on this channel.
        let (tx, commands) = mpsc::channel();
        let (mut op, _) = continue_op(&fx);
        op.git_client = git::Git::new(fx.path().to_path_buf(), tx);
        op.execute().await.expect("continue");
        let rebase_commands: Vec<String> = commands
            .try_iter()
            .filter(|command| command.contains(" rebase "))
            .collect();
        assert_eq!(
            rebase_commands,
            [
                "Running 'git -c rerere.enabled=false rebase --continue'",
                "Running 'git -c rerere.enabled=false rebase --skip'",
            ]
        );
        assert_eq!(
            state(&fx).await,
            completed(&record(&fx), vec![dropped(&d.branch, "branch change")])
        );
        assert_eq!(fx.head().await, d.main);
        assert_eq!(record(&fx).current_step, None);
    }

    #[tokio::test]
    async fn continue_gives_up_when_git_does_not_advance() {
        let fx = Fixture::new().await;
        let d = diverge_on_a_txt(&fx).await;
        start(&fx).await.expect("start");
        resolve(&fx, "a.txt", RebaseKeep::Main).await.unwrap();
        simulate_now_empty_stops(&fx, 3);
        let json_before = fx.json_bytes();

        assert_eq!(internal_message(continue_rebase(&fx).await), NOT_ADVANCING);
        assert_eq!(fx.json_bytes(), json_before);
        assert_eq!(state(&fx).await.state, RebaseStateKind::InProgress);
        abort(&fx).await.expect("abort");
        assert_eq!(fx.head().await, d.branch);
    }

    /// Resolves `a.txt` with the branch's version and makes git stop after it without a
    /// conflict: the next step is incomplete.
    async fn stop_after_a_failed_exec(fx: &Fixture) {
        start(fx).await.expect("start");
        resolve(fx, "a.txt", RebaseKeep::Branch).await.unwrap();
        let todo = fx.git_dir().join("rebase-merge").join("git-rebase-todo");
        fs::write(todo, "exec false\n").unwrap();
        let json_before = fx.json_bytes();

        let message = internal_message(continue_rebase(fx).await);
        assert!(message.contains("execution failed: false"), "{message}");
        assert_eq!(fx.json_bytes(), json_before);
        let state = state(fx).await;
        assert_eq!(state.state, RebaseStateKind::InProgress);
        assert!(state.step_incomplete, "{state:?}");
    }

    #[tokio::test]
    async fn continue_leaves_an_unexpected_stop_to_abort() {
        let fx = Fixture::new().await;
        let d = diverge_on_a_txt(&fx).await;
        stop_after_a_failed_exec(&fx).await;
        // The resolved commit was replayed before git stopped.
        assert_eq!(fx.sha("HEAD~1").await, d.main);
        assert_continue_refused(&fx, STEP_INCOMPLETE).await;
    }

    #[tokio::test]
    async fn continue_refused_with_an_unresolved_file() {
        let fx = Fixture::new().await;
        stop_content(&fx).await;
        resolve(&fx, "a.txt", RebaseKeep::Main).await.unwrap();
        assert_continue_refused(&fx, RESOLVE_EVERY_FILE).await;
    }

    #[tokio::test]
    async fn continue_refused_with_an_unsupported_conflict() {
        let fx = Fixture::new().await;
        fx.diverge(
            &[],
            &[Change::Write("n.txt", b"main\n")],
            &[Change::Write("n.txt", b"branch\n")],
        )
        .await;
        start(&fx).await.expect("start");
        assert_continue_refused(&fx, UNRESOLVABLE_STEP).await;
        // Resolved outside Friendshipper, so the unsupported conflict is all that is left.
        fx.run(&["checkout", "--ours", "--", "n.txt"]).await;
        fx.run(&["add", "--", "n.txt"]).await;
        let state = state(&fx).await;
        assert!(state.conflicts.iter().all(|c| c.resolved), "{state:?}");
        assert!(state.unstaged_edits.is_empty());
        assert_continue_refused(&fx, UNRESOLVABLE_STEP).await;
    }

    #[tokio::test]
    async fn continue_refused_when_the_step_is_incomplete() {
        let fx = Fixture::new().await;
        guided_stop_on_a_txt(&fx).await;
        fx.run(&["checkout", "--theirs", "--", "a.txt"]).await;
        fx.run(&["add", "--", "a.txt"]).await;
        assert!(state(&fx).await.step_incomplete);
        assert_continue_refused(&fx, STEP_INCOMPLETE).await;
    }

    async fn resolve_both(fx: &Fixture) {
        resolve(fx, "a.txt", RebaseKeep::Main).await.unwrap();
        resolve(fx, OTHER, RebaseKeep::Branch).await.unwrap();
    }

    #[tokio::test]
    async fn continue_refused_with_untracked_files() {
        let fx = Fixture::new().await;
        stop_content(&fx).await;
        resolve_both(&fx).await;
        fx.write("x.tmp", b"new\n");
        assert_continue_refused(
            &fx,
            "Move or delete these new files before continuing: x.tmp.",
        )
        .await;

        fs::remove_file(fx.path().join("x.tmp")).unwrap();
        for i in 0..12 {
            fx.write(&format!("new/f{i:02}.tmp"), b"new\n");
        }
        let listed: Vec<String> = (0..10).map(|i| format!("new/f{i:02}.tmp")).collect();
        assert_continue_refused(
            &fx,
            &format!(
                "Move or delete these new files before continuing: {}.",
                listed.join(", ")
            ),
        )
        .await;
        assert!(fx.path().join("new/f11.tmp").exists());
    }

    #[tokio::test]
    async fn continue_refused_with_unresolved_edits() {
        let fx = Fixture::new().await;
        let d = stop_content(&fx).await;
        resolve_both(&fx).await;
        fx.write("seed.txt", b"edited\n");
        fx.write("a.txt", b"edited after resolving\n");
        assert_eq!(state(&fx).await.unstaged_edits, ["a.txt", "seed.txt"]);
        assert_continue_refused(
            &fx,
            "These files changed after you resolved them; undo and resolve them again before continuing: a.txt. \
             These files were edited outside the conflict; revert them outside Friendshipper before continuing (aborting discards the edits): seed.txt.",
        )
        .await;
        assert_eq!(read(&fx, "seed.txt").as_deref(), Some(&b"edited\n"[..]));

        // Undo and resolve again inside Friendshipper; revert the other file outside it.
        unresolve(&fx, "a.txt").await.expect("unresolve");
        resolve(&fx, "a.txt", RebaseKeep::Main).await.unwrap();
        assert_continue_refused(
            &fx,
            "These files were edited outside the conflict; revert them outside Friendshipper before continuing (aborting discards the edits): seed.txt.",
        )
        .await;
        fx.run(&["checkout", "--", "seed.txt"]).await;
        continue_rebase(&fx).await.expect("continue");
        assert_eq!(state(&fx).await.state, RebaseStateKind::CompletedUnpushed);
        assert_eq!(
            fx.sha("HEAD:a.txt").await,
            fx.sha(&format!("{}:a.txt", d.main)).await
        );
    }

    #[tokio::test]
    async fn abort_discards_the_listed_unstaged_edits() {
        let fx = Fixture::new().await;
        stop_content(&fx).await;
        fx.write("seed.txt", b"edited\n");
        assert_eq!(state(&fx).await.unstaged_edits, ["seed.txt"]);
        abort(&fx).await.expect("abort");
        assert_eq!(read(&fx, "seed.txt").as_deref(), Some(&b"seed\n"[..]));
    }

    #[tokio::test]
    async fn continue_refused_with_a_staged_file_outside_the_commit() {
        let fx = Fixture::new().await;
        stop_content(&fx).await;
        resolve_both(&fx).await;
        fx.write("seed.txt", b"staged\n");
        fx.run(&["add", "--", "seed.txt"]).await;
        assert_continue_refused(
            &fx,
            "These files are staged but not part of this commit; unstage them outside Friendshipper before continuing: seed.txt.",
        )
        .await;
    }

    #[tokio::test]
    async fn continue_refused_with_conflict_markers() {
        let fx = Fixture::new().await;
        stop_content(&fx).await;
        // Staged with its markers outside Friendshipper.
        fx.run(&["add", "--", "a.txt"]).await;
        resolve(&fx, OTHER, RebaseKeep::Branch).await.unwrap();
        assert_eq!(conflict_in_state(&fx, "a.txt").await.resolution, None);
        assert_continue_refused(
            &fx,
            "Remove the conflict markers from a.txt before continuing.",
        )
        .await;

        // A manual resolution whose markers came back and were staged outside Friendshipper.
        unresolve(&fx, "a.txt").await.expect("unresolve");
        fx.write("a.txt", b"merged by hand\n");
        resolve(&fx, "a.txt", RebaseKeep::Manual).await.unwrap();
        fx.write(
            "a.txt",
            b"<<<<<<< ours\nmain\n=======\nbranch\n>>>>>>> theirs\n",
        );
        fx.run(&["add", "--", "a.txt"]).await;
        assert_continue_refused(
            &fx,
            "Remove the conflict markers from a.txt before continuing.",
        )
        .await;
    }

    #[tokio::test]
    async fn continue_accepts_everything_the_commit_brings() {
        let fx = Fixture::new().await;
        let lines = b"one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\n";
        let edited = b"one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nTEN\n";
        let d = fx
            .diverge(
                &[
                    Change::Write("a.txt", b"base\n"),
                    Change::Write("r.txt", lines),
                    Change::Write("m.txt", b"1\n2\n3\n4\n5\n"),
                ],
                &[
                    Change::Write("a.txt", b"main\n"),
                    Change::Rename("r.txt", "renamed.txt"),
                    Change::Write("m.txt", b"main\n2\n3\n4\n5\n"),
                ],
                &[
                    Change::Write("a.txt", b"branch\n"),
                    Change::Write("r.txt", edited),
                    Change::Write("m.txt", b"1\n2\n3\n4\nbranch\n"),
                    Change::Write("new.txt", b"new\n"),
                ],
            )
            .await;
        start(&fx).await.expect("start");
        assert_eq!(
            state(&fx)
                .await
                .conflicts
                .iter()
                .map(|c| c.path.as_str())
                .collect::<Vec<_>>(),
            ["a.txt"]
        );
        resolve(&fx, "a.txt", RebaseKeep::Branch).await.unwrap();
        continue_rebase(&fx).await.expect("continue");
        assert_eq!(state(&fx).await.state, RebaseStateKind::CompletedUnpushed);
        assert_eq!(fx.sha("HEAD~1").await, d.main);
        assert_eq!(read(&fx, "renamed.txt").as_deref(), Some(&edited[..]));
        assert_eq!(
            read(&fx, "m.txt").as_deref(),
            Some(&b"main\n2\n3\n4\nbranch\n"[..])
        );
        assert_eq!(read(&fx, "new.txt").as_deref(), Some(&b"new\n"[..]));
    }

    #[tokio::test]
    async fn continue_refused_without_a_rebase_in_progress() {
        let fx = Fixture::new().await;
        assert_continue_refused(&fx, NO_REBASE_IN_PROGRESS).await;
        diverge_clean(&fx).await;
        start(&fx).await.expect("start");
        assert_eq!(state(&fx).await.state, RebaseStateKind::CompletedUnpushed);
        assert_continue_refused(&fx, NO_REBASE_IN_PROGRESS).await;
    }

    #[tokio::test]
    async fn continue_refused_while_the_index_is_locked() {
        let fx = Fixture::new().await;
        stop_content(&fx).await;
        resolve_both(&fx).await;
        let lock = fx.git_dir().join("index.lock");
        fs::write(&lock, b"").unwrap();
        assert_continue_refused(
            &fx,
            &format!(
                "Git's index is locked ({}). If no git command is running, delete that file and try again.",
                lock.display()
            ),
        )
        .await;
        assert!(lock.exists());
    }

    // Confirmation step

    #[tokio::test]
    async fn a_resolved_step_waits_for_continue() {
        let fx = Fixture::new().await;
        let d = stop_content(&fx).await;
        fx.write("a.txt", b"merged by hand\n");
        resolve(&fx, "a.txt", RebaseKeep::Manual).await.unwrap();
        resolve(&fx, OTHER, RebaseKeep::Branch).await.unwrap();
        let head = fx.head().await;
        let replayed = fx
            .out(&["rev-list", "refs/remotes/origin/main..HEAD"])
            .await;

        // Nothing outlives a restart but the disk, and the state comes from it alone.
        let file = record(&fx);
        let resolved = |path: &str, keep: RebaseKeep| ConflictFile {
            resolved: true,
            resolution: Some(keep),
            ..conflict(path, ConflictKind::Content, &d)
        };
        assert_eq!(
            fx.build(true).await,
            RebaseState {
                state: RebaseStateKind::InProgress,
                branch: BRANCH.to_string(),
                step: 1,
                total_steps: 1,
                current_commit: Some(info(&d.branch, "branch change")),
                conflicts: vec![
                    resolved("a.txt", RebaseKeep::Manual),
                    resolved(OTHER, RebaseKeep::Branch),
                ],
                backup_ref: file.backup_ref.clone(),
                backup_sha: file.backup_sha.clone(),
                ..Default::default()
            }
        );
        assert_eq!(fx.head().await, head);
        assert_eq!(
            fx.out(&["rev-list", "refs/remotes/origin/main..HEAD"])
                .await,
            replayed
        );
        assert_eq!(
            read(&fx, "a.txt").as_deref(),
            Some(&b"merged by hand\n"[..])
        );

        for path in ["a.txt", OTHER] {
            unresolve(&fx, path).await.expect(path);
            assert_eq!(xy_of(&fx, path).await.as_deref(), Some("UU"));
        }
        assert_eq!(fx.head().await, head);
    }

    #[tokio::test]
    async fn undo_refused_for_a_file_of_a_previous_step() {
        let fx = Fixture::new().await;
        four_commits(&fx).await;
        continue_to_c3(&fx).await;
        let before = capture(&fx).await;
        assert_eq!(
            input_message(unresolve(&fx, "a.txt").await),
            "a.txt has no resolution to undo in this step."
        );
        assert_eq!(capture(&fx).await, before);
    }

    // Abort

    fn abort_op(
        fx: &Fixture,
    ) -> (
        RebaseAbortOp<UnrealEngineProvider>,
        RepoStatusRef,
        mpsc::Receiver<String>,
    ) {
        let refs = refs(fx);
        let (sync_phase_tx, phases) = mpsc::channel();
        let op = RebaseAbortOp {
            git_client: fx.git.clone(),
            engine: refs.engine,
            app_config: refs.app_config,
            repo_config: refs.repo_config,
            repo_status: refs.repo_status.clone(),
            sync_phase_tx,
            github_username: "test_user".to_string(),
        };
        (op, refs.repo_status, phases)
    }

    async fn abort(fx: &Fixture) -> Result<(), CoreError> {
        abort_op(fx).0.execute().await
    }

    /// Everything outside the rebased branch that Abort must leave alone.
    #[derive(Debug, PartialEq)]
    struct Bystanders {
        local_branches: String,
        stash_count: usize,
        origin_refs: String,
        ignored: Option<Vec<u8>>,
    }

    async fn bystanders(fx: &Fixture) -> Bystanders {
        let snapshot = capture(fx).await;
        Bystanders {
            local_branches: fx
                .out(&[
                    "for-each-ref",
                    "--format=%(refname) %(objectname)",
                    "refs/heads/",
                ])
                .await,
            stash_count: snapshot.stash_count,
            origin_refs: snapshot.origin_refs,
            ignored: read(fx, "keep.log"),
        }
    }

    /// An ignored file, a stash entry and another branch, created before Start.
    async fn add_bystanders(fx: &Fixture) -> Bystanders {
        fs::write(fx.git_dir().join("info").join("exclude"), "*.log\n").unwrap();
        fx.write("keep.log", b"ignored\n");
        fx.write("seed.txt", b"stashed\n");
        fx.run(&["stash", "push", "-q", "-m", "bystander"]).await;
        fx.run(&[
            "branch",
            "f11r-branch-bystander",
            "refs/remotes/origin/main",
        ])
        .await;
        let bystanders = bystanders(fx).await;
        assert_eq!(bystanders.stash_count, 1);
        bystanders
    }

    /// The branch is back at its backup and nothing else changed.
    async fn assert_aborted(fx: &Fixture, file: &GuidedRebaseFile, before: &Bystanders) {
        assert_eq!(
            fx.out(&["symbolic-ref", "-q", "HEAD"]).await,
            format!("refs/heads/{BRANCH}")
        );
        assert_eq!(fx.head().await, file.backup_sha);
        assert_eq!(fx.out(&["status", "--porcelain"]).await, "");
        assert!(!json_path(&fx.git_dir()).exists());
        assert!(!fx.git_dir().join("rebase-merge").exists());
        assert_eq!(fx.sha(&file.backup_ref).await, file.backup_sha);
        assert_eq!(&bystanders(fx).await, before);
        assert_eq!(state(fx).await, RebaseState::default());
    }

    /// Content conflicts on `a.txt` and `o.txt`, not started.
    async fn diverge_two_conflicts(fx: &Fixture) -> Diverged {
        fx.diverge(
            &[
                Change::Write(
                    "a.txt", b"base
",
                ),
                Change::Write(
                    OTHER, b"base o
",
                ),
            ],
            &[
                Change::Write(
                    "a.txt", b"main
",
                ),
                Change::Write(
                    OTHER, b"main o
",
                ),
            ],
            &[
                Change::Write(
                    "a.txt", b"branch
",
                ),
                Change::Write(
                    OTHER,
                    b"branch o
",
                ),
            ],
        )
        .await
    }

    enum AbortFrom {
        Unresolved,
        PartlyResolved,
        LaterStep,
        IncompleteStep,
        UnsupportedStep,
    }

    async fn abort_case(from: AbortFrom) {
        let fx = Fixture::new().await;
        match from {
            AbortFrom::Unresolved | AbortFrom::PartlyResolved => {
                diverge_two_conflicts(&fx).await;
            }
            AbortFrom::LaterStep => {
                four_commits(&fx).await;
            }
            AbortFrom::IncompleteStep => {
                diverge_on_a_txt(&fx).await;
            }
            AbortFrom::UnsupportedStep => {
                fx.diverge(
                    &[],
                    &[Change::Write("n.txt", b"main\n")],
                    &[Change::Write("n.txt", b"branch\n")],
                )
                .await;
            }
        }
        let before = add_bystanders(&fx).await;

        match from {
            AbortFrom::Unresolved => start(&fx).await.expect("start"),
            AbortFrom::PartlyResolved => {
                start(&fx).await.expect("start");
                fx.write("a.txt", b"merged by hand\n");
                resolve(&fx, "a.txt", RebaseKeep::Manual).await.unwrap();
            }
            AbortFrom::LaterStep => continue_to_c3(&fx).await,
            AbortFrom::IncompleteStep => stop_after_a_failed_exec(&fx).await,
            AbortFrom::UnsupportedStep => {
                start(&fx).await.expect("start");
                assert_eq!(
                    conflict_in_state(&fx, "n.txt").await.kind,
                    ConflictKind::Unsupported
                );
            }
        }
        assert_eq!(state(&fx).await.state, RebaseStateKind::InProgress);
        let file = record(&fx);

        let (op, repo_status, phases) = abort_op(&fx);
        op.execute().await.expect("abort");
        assert_aborted(&fx, &file, &before).await;
        assert_eq!(phases.try_iter().collect::<Vec<_>>(), ["Aborting rebase"]);
        let status = repo_status.read();
        assert_eq!(status.branch, BRANCH);
        assert!(!status.detached_head);
    }

    #[tokio::test]
    async fn abort_with_nothing_resolved() {
        abort_case(AbortFrom::Unresolved).await;
    }

    #[tokio::test]
    async fn abort_with_a_manual_resolution() {
        abort_case(AbortFrom::PartlyResolved).await;
    }

    #[tokio::test]
    async fn abort_after_a_continue() {
        abort_case(AbortFrom::LaterStep).await;
    }

    #[tokio::test]
    async fn abort_an_incomplete_step() {
        abort_case(AbortFrom::IncompleteStep).await;
    }

    #[tokio::test]
    async fn abort_an_unsupported_step() {
        abort_case(AbortFrom::UnsupportedStep).await;
    }

    async fn assert_abort_refused(fx: &Fixture) {
        let before = capture(fx).await;
        assert_eq!(input_message(abort(fx).await), NOTHING_TO_ABORT);
        assert_eq!(capture(fx).await, before);
    }

    #[tokio::test]
    async fn abort_refused_without_a_guided_rebase_in_progress() {
        let fx = Fixture::new().await;
        assert_abort_refused(&fx).await;
        diverge_clean(&fx).await;
        start(&fx).await.expect("start");
        assert_eq!(state(&fx).await.state, RebaseStateKind::CompletedUnpushed);
        assert_abort_refused(&fx).await;
    }

    #[tokio::test]
    async fn abort_refused_for_a_rebase_started_elsewhere() {
        let fx = Fixture::new().await;
        diverge_on_a_txt(&fx).await;
        assert_eq!(fx.start_raw_rebase("main").await, 1);
        assert_eq!(fx.derive().await.kind, RebaseStateKind::External);
        assert_abort_refused(&fx).await;
        assert!(fx.git_dir().join("rebase-merge").is_dir());
    }

    #[tokio::test]
    async fn abort_that_does_not_restore_keeps_the_record() {
        let fx = Fixture::new().await;
        let d = stop_content(&fx).await;
        let json_before = fx.json_bytes();
        // Git returns the branch to `orig-head`; point it elsewhere.
        fs::write(
            fx.git_dir().join("rebase-merge").join("orig-head"),
            format!("{}\n", d.main),
        )
        .unwrap();

        assert_eq!(
            internal_message(abort(&fx).await),
            format!(
                "Abort did not restore {BRANCH} to its backup (HEAD is {}). Use Reset to backup.",
                &d.main[..8]
            )
        );
        assert_eq!(fx.json_bytes(), json_before);
        assert_eq!(fx.derive().await.kind, RebaseStateKind::CompletedUnpushed);
        assert_eq!(fx.sha(&record(&fx).backup_ref).await, d.branch);
    }

    #[tokio::test]
    async fn abort_restores_the_backup_after_the_branch_ref_moved() {
        let fx = Fixture::new().await;
        let d = stop_content(&fx).await;
        let tree = format!("{}^{{tree}}", d.main);
        let moved = fx
            .out(&["commit-tree", "-p", &d.main, "-m", "moved", &tree])
            .await;
        fx.run(&["update-ref", &format!("refs/heads/{BRANCH}"), &moved])
            .await;

        abort(&fx).await.expect("abort");
        assert_eq!(fx.head().await, d.branch);
        assert_eq!(fx.sha(BRANCH).await, d.branch);
        assert!(!json_path(&fx.git_dir()).exists());
    }

    #[tokio::test]
    async fn abort_refused_by_git_keeps_untracked_files_and_the_record() {
        let fx = Fixture::new().await;
        four_commits(&fx).await;
        continue_to_c3(&fx).await;
        // C4's file, not replayed yet, would overwrite it.
        fx.write("e.txt", b"untracked\n");
        let json_before = fx.json_bytes();

        let message = internal_message(abort(&fx).await);
        assert!(
            message.starts_with("git rebase --abort failed:"),
            "{message}"
        );
        assert!(message.contains("e.txt"), "{message}");
        assert_eq!(read(&fx, "e.txt").as_deref(), Some(&b"untracked\n"[..]));
        assert_eq!(fx.json_bytes(), json_before);
        assert_eq!(state(&fx).await.state, RebaseStateKind::InProgress);
    }

    // Interference from a terminal

    #[tokio::test]
    async fn aborted_by_hand_is_stale_and_dismissable() {
        let fx = Fixture::new().await;
        let d = stop_content(&fx).await;
        let backup_ref = record(&fx).backup_ref;
        fx.run(&["rebase", "--abort"]).await;
        assert_eq!(
            fx.derive().await.stale_reason,
            Some(RebaseStaleReason::AbortedOutside)
        );
        assert_continue_refused(&fx, NO_REBASE_IN_PROGRESS).await;
        assert_abort_refused(&fx).await;

        dismiss_stale(&fx.git, fx.path(), &tokio::sync::Mutex::new(()))
            .await
            .expect("dismiss");
        assert_eq!(state(&fx).await, RebaseState::default());
        assert_eq!(fx.sha(&backup_ref).await, d.branch);
    }

    #[tokio::test]
    async fn continued_by_hand_is_completed_unpushed() {
        let fx = Fixture::new().await;
        let d = diverge_on_a_txt(&fx).await;
        start(&fx).await.expect("start");
        fx.run(&["checkout", "--theirs", "--", "a.txt"]).await;
        fx.run(&["add", "--", "a.txt"]).await;
        let editor: &[(&str, &str)] = &[("GIT_EDITOR", "true")];
        let continued = fx
            .git
            .run_with_status(
                &["rebase", "--continue"],
                git::Opts::new_without_logs().with_env(editor),
            )
            .await
            .unwrap();
        assert_eq!(continued.exit_code, 0, "{}", continued.stderr);

        assert_eq!(state(&fx).await, completed(&record(&fx), vec![]));
        assert_eq!(fx.sha("HEAD~1").await, d.main);
        assert_continue_refused(&fx, NO_REBASE_IN_PROGRESS).await;
        assert_abort_refused(&fx).await;
    }

    #[tokio::test]
    async fn quit_by_hand_is_branch_changed() {
        let fx = Fixture::new().await;
        stop_content(&fx).await;
        fx.run(&["rebase", "--quit"]).await;
        let derived = fx.derive().await;
        assert_eq!(
            (derived.kind, derived.stale_reason),
            (
                RebaseStateKind::Stale,
                Some(RebaseStaleReason::BranchChanged)
            )
        );
        assert_abort_refused(&fx).await;
    }

    // Open file

    #[test]
    fn runnable_files_are_not_opened() {
        for path in [
            "Tools/run.BAT",
            "a/b.cmd",
            "x.ps1",
            "x.py",
            "Shortcut.lnk",
            "site.url",
            "x.reg",
            "trailing.bat.",
            "trailing.vbs ",
            "x.msc",
        ] {
            assert!(windows_would_run(path, None), "{path}");
        }
        for path in [
            "Config/DefaultEngine.ini",
            "notes.txt",
            "Makefile",
            "a.bat/b.txt",
        ] {
            assert!(!windows_would_run(path, None), "{path}");
        }
        assert!(windows_would_run("x.foo", Some(".COM;.FOO")));
        assert!(!windows_would_run("x.ini", Some(".COM;.FOO;;")));
        // The fixed list applies whatever PATHEXT says.
        assert!(windows_would_run("x.bat", Some("")));
    }

    #[test]
    fn conflict_abs_path_builds_native_paths() {
        let repo = Path::new("repo");
        assert_eq!(
            conflict_abs_path(repo, "a/b c.txt"),
            Some(repo.join("a").join("b c.txt"))
        );
        #[cfg(windows)]
        assert_eq!(
            conflict_abs_path(Path::new(r"C:\repo"), "a/b c.txt")
                .unwrap()
                .to_str(),
            Some(r"C:\repo\a\b c.txt")
        );
        for rel in [
            "", "../x", "a/../x", "./x", "/x", "C:/x", "a//b", "a/", r"a\b", "c:x",
        ] {
            assert_eq!(conflict_abs_path(repo, rel), None, "{rel:?}");
        }
    }

    // Dismiss

    #[tokio::test]
    async fn dismiss_deletes_a_stale_record_and_keeps_backups() {
        let fx = Fixture::new().await;
        let d = diverge_on_a_txt(&fx).await;
        let file = fx.guided_file("main").await;
        fx.run(&["update-ref", &file.backup_ref, &d.branch]).await;
        fx.write_file_json(&file);
        assert_eq!(
            fx.derive().await.stale_reason,
            Some(RebaseStaleReason::AbortedOutside)
        );

        dismiss_stale(&fx.git, fx.path(), &tokio::sync::Mutex::new(()))
            .await
            .expect("dismiss");
        assert!(!json_path(&fx.git_dir()).exists());
        assert_eq!(fx.sha(&file.backup_ref).await, d.branch);
        assert_eq!(state(&fx).await, RebaseState::default());
    }

    #[tokio::test]
    async fn dismiss_deletes_an_unreadable_record() {
        let fx = Fixture::new().await;
        fs::write(json_path(&fx.git_dir()), "{").unwrap();
        dismiss_stale(&fx.git, fx.path(), &tokio::sync::Mutex::new(()))
            .await
            .expect("dismiss");
        assert!(!json_path(&fx.git_dir()).exists());
    }

    #[tokio::test]
    async fn dismiss_refused_unless_stale() {
        let fx = Fixture::new().await;
        let op_lock = tokio::sync::Mutex::new(());
        assert_eq!(
            input_message(dismiss_stale(&fx.git, fx.path(), &op_lock).await),
            "Nothing to dismiss."
        );

        stop_content(&fx).await;
        let before = fs::read(json_path(&fx.git_dir())).unwrap();
        assert_eq!(
            input_message(dismiss_stale(&fx.git, fx.path(), &op_lock).await),
            "Nothing to dismiss."
        );
        assert_eq!(fs::read(json_path(&fx.git_dir())).unwrap(), before);
    }

    #[tokio::test]
    async fn dismiss_refused_while_a_rebase_op_runs() {
        let fx = Fixture::new().await;
        diverge_on_a_txt(&fx).await;
        let file = fx.guided_file("main").await;
        fx.write_file_json(&file);
        let op_lock = tokio::sync::Mutex::new(());
        let _running = op_lock.lock().await;
        assert_eq!(
            input_message(dismiss_stale(&fx.git, fx.path(), &op_lock).await),
            OP_RUNNING
        );
        assert!(json_path(&fx.git_dir()).exists());
    }

    // Finish

    const POST_ERROR: &str = "test";

    fn finish_op(fx: &Fixture) -> (RebaseFinishOp<UnrealEngineProvider>, mpsc::Receiver<String>) {
        let refs = refs(fx);
        let (sync_phase_tx, phases) = mpsc::channel();
        let op = RebaseFinishOp {
            git_client: fx.git.clone(),
            engine: refs.engine,
            app_config: refs.app_config,
            repo_config: refs.repo_config,
            repo_status: refs.repo_status,
            sync_phase_tx,
            post: Err(POST_ERROR.to_string()),
            result: Arc::new(parking_lot::Mutex::new(None)),
        };
        (op, phases)
    }

    async fn finish(fx: &Fixture) -> Result<RebaseFinishResponse, CoreError> {
        let (op, _) = finish_op(fx);
        op.execute().await?;
        let response = op.result.lock().take().expect("finish response");
        Ok(response)
    }

    /// Origin's `<branch>`, read in the bare origin itself.
    async fn origin_tip(fx: &Fixture, branch: &str) -> Option<String> {
        let refname = format!("refs/heads/{branch}");
        let output = new_git(fx.origin_path())
            .run_with_status(
                &["rev-parse", "-q", "--verify", &refname],
                git::Opts::new_without_logs(),
            )
            .await
            .expect("rev-parse in origin");
        (output.exit_code == 0).then(|| output.stdout.trim().to_string())
    }

    async fn origin_subject(fx: &Fixture, rev: &str) -> String {
        new_git(fx.origin_path())
            .run_and_collect_output(
                &["log", "-1", "--format=%s", rev],
                git::Opts::new_without_logs(),
            )
            .await
            .expect("log in origin")
            .trim()
            .to_string()
    }

    /// A rebase of `diverge_clean` that replayed and waits to be pushed.
    async fn completed_clean(fx: &Fixture) -> Diverged {
        let d = diverge_clean(fx).await;
        start(fx).await.expect("start");
        assert_eq!(state(fx).await.state, RebaseStateKind::CompletedUnpushed);
        d
    }

    /// A collaborator pushes `path` to the branch, then Finish's lease is rejected; returns
    /// the collaborator's commit.
    async fn reject_push(fx: &Fixture, path: &str) -> String {
        collaborator_push(fx, BRANCH, path).await;
        let collab = origin_tip(fx, BRANCH).await.expect("collaborator commit");
        let response = finish(fx).await.expect("finish");
        assert_eq!(response.outcome, RebaseFinishOutcome::LeaseRejected);
        collab
    }

    /// The `-rebased` backup refs.
    async fn rebased_backups(fx: &Fixture) -> Vec<String> {
        backup_refs(fx)
            .await
            .into_iter()
            .filter(|refname| refname.ends_with("-rebased"))
            .collect()
    }

    #[tokio::test]
    async fn finish_pushes_the_rebased_branch() {
        let fx = Fixture::new().await;
        completed_clean(&fx).await;
        let file = record(&fx);
        let head = fx.head().await;
        let (op, phases) = finish_op(&fx);
        op.execute().await.expect("finish");
        let response = op.result.lock().take().expect("finish response");

        assert_eq!(
            response,
            RebaseFinishResponse {
                outcome: RebaseFinishOutcome::Pushed,
                state: RebaseState::default(),
                dropped_commits: vec![],
                touches_cpp: false,
                post_step_error: Some(POST_ERROR.to_string()),
            }
        );
        assert_eq!(
            origin_tip(&fx, BRANCH).await.as_deref(),
            Some(head.as_str())
        );
        assert_eq!(fx.sha(&format!("refs/remotes/origin/{BRANCH}")).await, head);
        assert_eq!(fx.head().await, head);
        assert!(!json_path(&fx.git_dir()).exists());
        assert_eq!(fx.sha(&file.backup_ref).await, file.backup_sha);
        assert_eq!(state(&fx).await, RebaseState::default());
        assert_eq!(
            phases.try_iter().collect::<Vec<_>>(),
            ["Pushing rebased branch", "Updating editor binaries"]
        );
    }

    #[tokio::test]
    async fn finish_reports_the_dropped_commits() {
        let fx = Fixture::new().await;
        fx.branch(BRANCH).await;
        let shared = fx.commit_file("s.txt", b"shared\n", "shared change").await;
        fx.commit_file("b.txt", b"b\n", "own change").await;
        fx.push(BRANCH).await;
        fx.checkout("main").await;
        fx.commit_file("m.txt", b"main\n", "main change").await;
        fx.run(&["cherry-pick", &shared]).await;
        fx.push("main").await;
        fx.checkout(BRANCH).await;
        start(&fx).await.expect("start");
        let before = state(&fx).await.dropped_commits;
        assert_eq!(before, vec![dropped(&shared, "shared change")]);

        let response = finish(&fx).await.expect("finish");
        assert_eq!(response.outcome, RebaseFinishOutcome::Pushed);
        assert_eq!(response.dropped_commits, before);
    }

    #[tokio::test]
    async fn finish_pushes_a_ci_tagged_tip_unchanged() {
        let fx = Fixture::new().await;
        diverge_clean(&fx).await;
        fx.commit_file("t.txt", b"t\n", "[ci:editor] tagged tip")
            .await;
        fx.push(BRANCH).await;
        start(&fx).await.expect("start");

        finish(&fx).await.expect("finish");
        assert_eq!(
            origin_subject(&fx, &format!("refs/heads/{BRANCH}")).await,
            "[ci:editor] tagged tip"
        );
    }

    #[tokio::test]
    async fn finish_after_an_earlier_push_is_up_to_date() {
        let fx = Fixture::new().await;
        completed_clean(&fx).await;
        let file = record(&fx);
        let head = fx.head().await;
        // A crash right after a push: origin has the rebased branch, the record remains and the
        // tracking ref was not updated.
        let refspec = format!("+{head}:refs/heads/{BRANCH}");
        fx.run(&["push", "-q", "origin", &refspec]).await;
        let tracking = format!("refs/remotes/origin/{BRANCH}");
        fx.run(&["update-ref", &tracking, &file.backup_sha]).await;
        assert_eq!(state(&fx).await.state, RebaseStateKind::CompletedUnpushed);

        let response = finish(&fx).await.expect("finish");
        assert_eq!(response.outcome, RebaseFinishOutcome::Pushed);
        assert_eq!(
            origin_tip(&fx, BRANCH).await.as_deref(),
            Some(head.as_str())
        );
        assert!(!json_path(&fx.git_dir()).exists());
    }

    #[tokio::test]
    async fn finish_lease_rejected_keeps_the_collaborators_commit() {
        let fx = Fixture::new().await;
        completed_clean(&fx).await;
        let file = record(&fx);
        let head = fx.head().await;
        collaborator_push(&fx, BRANCH, "c.txt").await;
        let collab = origin_tip(&fx, BRANCH).await.expect("collaborator commit");

        let response = finish(&fx).await.expect("lease rejection is not an error");
        let expected_state = RebaseState {
            state: RebaseStateKind::PushFailed,
            push_failed_remote_sha: Some(collab.clone()),
            ..completed(&file, vec![])
        };
        assert_eq!(
            response,
            RebaseFinishResponse {
                outcome: RebaseFinishOutcome::LeaseRejected,
                state: expected_state.clone(),
                dropped_commits: vec![],
                touches_cpp: false,
                post_step_error: None,
            }
        );
        assert_eq!(
            record(&fx),
            GuidedRebaseFile {
                phase: GuidedRebasePhase::PushFailed,
                push_failed_remote_sha: Some(collab.clone()),
                ..file
            }
        );
        assert_eq!(origin_tip(&fx, BRANCH).await, Some(collab));
        assert_eq!(fx.head().await, head);
        assert_eq!(state(&fx).await, expected_state);
    }

    #[tokio::test]
    async fn finish_does_not_recreate_a_branch_deleted_on_origin() {
        let fx = Fixture::new().await;
        completed_clean(&fx).await;
        let file = record(&fx);
        let (_dir, collab) = collaborator(&fx).await;
        let delete = format!(":refs/heads/{BRANCH}");
        collab
            .run(
                &["push", "-q", "origin", &delete],
                git::Opts::new_without_logs(),
            )
            .await
            .expect("delete on origin");

        let response = finish(&fx).await.expect("lease rejection is not an error");
        assert_eq!(response.outcome, RebaseFinishOutcome::LeaseRejected);
        assert_eq!(response.state.state, RebaseStateKind::PushFailed);
        assert_eq!(response.state.push_failed_remote_sha, None);
        assert_eq!(record(&fx).phase, GuidedRebasePhase::PushFailed);
        assert_eq!(record(&fx).push_failed_remote_sha, None);
        assert_eq!(origin_tip(&fx, BRANCH).await, None);
        assert_eq!(fx.sha(&file.backup_ref).await, file.backup_sha);
    }

    /// A `pre-receive` hook in the temp origin that refuses every push.
    async fn refuse_pushes_on_origin(fx: &Fixture) {
        let origin_hooks = fx.origin_path().join("hooks");
        fs::create_dir_all(&origin_hooks).unwrap();
        fs::write(
            origin_hooks.join("pre-receive"),
            "#!/bin/sh\necho 'protected branch hook declined' >&2\nexit 1\n",
        )
        .unwrap();
        new_git(fx.origin_path())
            .run(
                &["config", "core.hooksPath", &slash_path(&origin_hooks)],
                git::Opts::new_without_logs(),
            )
            .await
            .expect("config");
    }

    #[tokio::test]
    async fn finish_rejected_by_origin_keeps_the_record() {
        let fx = Fixture::new().await;
        completed_clean(&fx).await;
        refuse_pushes_on_origin(&fx).await;
        let before = capture(&fx).await;

        let message = input_message(finish(&fx).await);
        assert!(
            message.starts_with("GitHub rejected the push: [remote rejected]"),
            "{message}"
        );
        assert_eq!(capture(&fx).await, before);
        assert_eq!(state(&fx).await.state, RebaseStateKind::CompletedUnpushed);
    }

    #[tokio::test]
    async fn finish_network_failure_keeps_the_record() {
        let fx = Fixture::new().await;
        completed_clean(&fx).await;
        let url = fx.out(&["remote", "get-url", "origin"]).await;
        let missing = slash_path(&fx.path().join("missing.git"));
        fx.run(&["remote", "set-url", "origin", &missing]).await;
        let before = capture(&fx).await;

        let message = internal_message(finish(&fx).await);
        assert!(message.starts_with("Push failed:"), "{message}");
        assert_eq!(capture(&fx).await, before);
        assert_eq!(state(&fx).await.state, RebaseStateKind::CompletedUnpushed);

        fx.run(&["remote", "set-url", "origin", &url]).await;
        assert_eq!(
            finish(&fx).await.expect("finish").outcome,
            RebaseFinishOutcome::Pushed
        );
    }

    /// Finish is refused and changes nothing, origin included.
    async fn assert_finish_refused(fx: &Fixture) {
        let before = capture(fx).await;
        assert_eq!(input_message(finish(fx).await), NOTHING_TO_PUSH);
        assert_eq!(capture(fx).await, before);
    }

    #[tokio::test]
    async fn finish_refused_unless_completed_unpushed() {
        let fx = Fixture::new().await;
        assert_finish_refused(&fx).await;

        stop_content(&fx).await;
        assert_finish_refused(&fx).await;

        let fx = Fixture::new().await;
        completed_clean(&fx).await;
        reject_push(&fx, "c.txt").await;
        assert_finish_refused(&fx).await;
    }

    /// The pushes git ran, as reported on the command channel.
    fn pushes(commands: &mpsc::Receiver<String>) -> Vec<String> {
        commands
            .try_iter()
            .filter(|command| command.starts_with("Running 'git push"))
            .collect()
    }

    /// Every push names the branch's exact expected sha and never forces without a lease.
    fn assert_leased(pushes: &[String]) {
        assert!(!pushes.is_empty());
        let lease = format!("--force-with-lease=refs/heads/{BRANCH}:");
        for push in pushes {
            let args: Vec<&str> = push.trim_end_matches('\'').split(' ').collect();
            let leases: Vec<&str> = args
                .iter()
                .filter_map(|arg| arg.strip_prefix(&lease))
                .collect();
            assert_eq!(leases.len(), 1, "{push}");
            assert!(is_full_sha(leases[0]), "{push}");
            assert!(args.contains(&"--porcelain"), "{push}");
            assert!(
                !args
                    .iter()
                    .any(|arg| *arg == "--force" || *arg == "-f" || arg.starts_with('+')),
                "{push}"
            );
        }
    }

    #[tokio::test]
    async fn every_push_has_an_explicit_lease() {
        let fx = Fixture::new().await;
        completed_clean(&fx).await;
        let (tx, commands) = mpsc::channel();
        let (mut op, _) = finish_op(&fx);
        op.git_client = git::Git::new(fx.path().to_path_buf(), tx.clone());
        op.execute().await.expect("finish");
        let finish_pushes = pushes(&commands);
        assert_eq!(finish_pushes.len(), 1);
        assert_leased(&finish_pushes);

        let (mut op, _) = rebuild_op(&fx);
        op.git_client = git::Git::new(fx.path().to_path_buf(), tx);
        op.execute().await.expect("rebuild");
        let rebuild_pushes = pushes(&commands);
        assert_eq!(rebuild_pushes.len(), 1);
        assert_leased(&rebuild_pushes);
    }

    // Redo

    fn redo_op(fx: &Fixture) -> RebaseRedoOp<UnrealEngineProvider> {
        let refs = refs(fx);
        RebaseRedoOp {
            git_client: fx.git.clone(),
            engine: refs.engine,
            app_config: refs.app_config,
            repo_config: refs.repo_config,
            repo_status: refs.repo_status,
            sync_phase_tx: mpsc::channel().0,
            trunk: "main".to_string(),
            github_username: "test_user".to_string(),
            post: Err(POST_ERROR.to_string()),
        }
    }

    async fn redo(fx: &Fixture) -> Result<(), CoreError> {
        redo_op(fx).execute().await
    }

    #[tokio::test]
    async fn redo_rebases_origins_branch_onto_latest_main() {
        let fx = Fixture::new().await;
        completed_clean(&fx).await;
        let old = record(&fx);
        let rebased = fx.head().await;
        let collab = reject_push(&fx, "c.txt").await;
        collaborator_push(&fx, "main", "m2.txt").await;
        let latest_main = origin_tip(&fx, "main").await.expect("main");

        redo(&fx).await.expect("redo");
        let new = record(&fx);
        assert_eq!(rebased_backups(&fx).await.len(), 1);
        assert_eq!(fx.sha(&rebased_backups(&fx).await[0]).await, rebased);
        assert_eq!(fx.sha(&old.backup_ref).await, old.backup_sha);
        assert_ne!(new.backup_ref, old.backup_ref);
        assert_eq!(fx.sha(&new.backup_ref).await, collab);
        assert_eq!(new.backup_sha, collab);
        assert_eq!(new.pre_rebase_origin_sha, collab);
        assert_eq!(new.onto, latest_main);
        assert_eq!(new.phase, GuidedRebasePhase::Rebasing);
        assert_eq!(new.push_failed_remote_sha, None);
        assert_eq!(state(&fx).await, completed(&new, vec![]));
        assert_eq!(
            fx.out(&["log", "--format=%s", &format!("{latest_main}..HEAD")])
                .await,
            "collab commit\nbranch change"
        );

        let response = finish(&fx).await.expect("finish");
        assert_eq!(response.outcome, RebaseFinishOutcome::Pushed);
        let tip = origin_tip(&fx, BRANCH).await.expect("branch on origin");
        assert_eq!(tip, fx.head().await);
        assert!(is_ancestor(&fx.git, &latest_main, &tip).await.unwrap());
        assert_eq!(read(&fx, "c.txt").as_deref(), Some(&b"collab\n"[..]));
    }

    #[tokio::test]
    async fn redo_stops_on_a_conflicting_collaborator_change() {
        let fx = Fixture::new().await;
        let d = fx
            .diverge(
                &[Change::Write("a.txt", b"base\n")],
                &[Change::Write("a.txt", b"main\n")],
                &[Change::Write("b.txt", b"branch\n")],
            )
            .await;
        start(&fx).await.expect("start");
        let collab = reject_push(&fx, "a.txt").await;

        redo(&fx).await.expect("redo");
        let new = record(&fx);
        assert_eq!(new.backup_sha, collab);
        let state = state(&fx).await;
        assert_eq!(state.state, RebaseStateKind::InProgress);
        assert_eq!(
            state.current_commit.map(|c| (c.sha, c.subject)),
            Some((collab, "collab commit".to_string()))
        );
        assert_eq!(
            state
                .conflicts
                .iter()
                .map(|c| c.path.as_str())
                .collect::<Vec<_>>(),
            ["a.txt"]
        );
        assert_eq!(new.onto, d.main);
    }

    #[tokio::test]
    async fn redo_refused_with_local_changes_moves_nothing() {
        let fx = Fixture::new().await;
        completed_clean(&fx).await;
        reject_push(&fx, "c.txt").await;
        fx.write("seed.txt", b"edited\n");
        let before = capture(&fx).await;

        assert_eq!(input_message(redo(&fx).await), DIRTY_TREE);
        assert_eq!(capture(&fx).await, before);
        assert!(rebased_backups(&fx).await.is_empty());
    }

    #[tokio::test]
    async fn redo_refused_when_the_branch_was_deleted_on_origin() {
        let fx = Fixture::new().await;
        completed_clean(&fx).await;
        let (_dir, collab) = collaborator(&fx).await;
        let delete = format!(":refs/heads/{BRANCH}");
        collab
            .run(
                &["push", "-q", "origin", &delete],
                git::Opts::new_without_logs(),
            )
            .await
            .expect("delete on origin");
        finish(&fx).await.expect("finish");
        let before = capture(&fx).await;

        assert_eq!(
            input_message(redo(&fx).await),
            format!("{BRANCH} no longer exists on origin. Use Reset to backup.")
        );
        let after = capture(&fx).await;
        assert_eq!(
            Snapshot {
                tracking_refs: before.tracking_refs.clone(),
                ..after
            },
            before
        );
        assert!(rebased_backups(&fx).await.is_empty());
    }

    /// A clone of origin's `BRANCH` in the collaborator's clone.
    async fn collaborator_on_branch(fx: &Fixture) -> (tempfile::TempDir, git::Git) {
        let (dir, collab) = collaborator(fx).await;
        let tracking = format!("refs/remotes/origin/{BRANCH}");
        collab
            .run(
                &["checkout", "-q", "-B", BRANCH, &tracking],
                git::Opts::new_without_logs(),
            )
            .await
            .expect("checkout");
        (dir, collab)
    }

    /// Redo is refused with `expected` and moves nothing; only tracking refs may change.
    async fn assert_redo_refused_after_fetch(fx: &Fixture, expected: &str) {
        let before = capture(fx).await;
        assert_eq!(input_message(redo(fx).await), expected);
        let after = capture(fx).await;
        assert_eq!(
            Snapshot {
                tracking_refs: before.tracking_refs.clone(),
                ..after
            },
            before
        );
        assert!(rebased_backups(fx).await.is_empty());
    }

    #[tokio::test]
    async fn redo_reports_a_start_gate_after_the_reset() {
        let fx = Fixture::new().await;
        completed_clean(&fx).await;
        let rebased = fx.head().await;
        // The collaborator merges main into origin's branch, so it contains the latest main.
        let (_dir, collab) = collaborator_on_branch(&fx).await;
        let opts = git::Opts::new_without_logs();
        collab
            .run(
                &["merge", "-q", "--no-edit", "refs/remotes/origin/main"],
                opts,
            )
            .await
            .expect("merge");
        let refspec = format!("HEAD:refs/heads/{BRANCH}");
        collab
            .run(&["push", "-q", "origin", &refspec], opts)
            .await
            .expect("push");
        let origin = origin_tip(&fx, BRANCH).await.expect("branch on origin");
        assert_eq!(
            finish(&fx).await.expect("finish").outcome,
            RebaseFinishOutcome::LeaseRejected
        );

        assert_eq!(
            input_message(redo(&fx).await),
            format!(
                "{BRANCH} already contains the latest main. {BRANCH} was reset, but updating editor binaries or engine failed: test."
            )
        );
        assert_eq!(state(&fx).await, RebaseState::default());
        assert_eq!(fx.head().await, origin);
        assert_eq!(
            fx.out(&["symbolic-ref", "-q", "HEAD"]).await,
            format!("refs/heads/{BRANCH}")
        );
        assert!(!json_path(&fx.git_dir()).exists());
        assert_eq!(fx.sha(&rebased_backups(&fx).await[0]).await, rebased);
    }

    #[tokio::test]
    async fn redo_refused_when_origin_rebased_the_branch() {
        let fx = Fixture::new().await;
        completed_clean(&fx).await;
        // The collaborator rebases origin's branch onto main themselves and force-pushes it.
        let (_dir, collab) = collaborator_on_branch(&fx).await;
        let opts = git::Opts::new_without_logs();
        collab
            .run(&["rebase", "-q", "refs/remotes/origin/main"], opts)
            .await
            .expect("rebase");
        let refspec = format!("+HEAD:refs/heads/{BRANCH}");
        collab
            .run(&["push", "-q", "origin", &refspec], opts)
            .await
            .expect("push");
        finish(&fx).await.expect("finish");
        assert_eq!(state(&fx).await.state, RebaseStateKind::PushFailed);

        assert_redo_refused_after_fetch(
            &fx,
            &format!("{BRANCH} was rewritten on origin. Use Reset to backup."),
        )
        .await;
    }

    #[tokio::test]
    async fn redo_refused_when_origin_recreated_the_branch_from_main() {
        let fx = Fixture::new().await;
        completed_clean(&fx).await;
        let (_dir, collab) = collaborator(&fx).await;
        let opts = git::Opts::new_without_logs();
        let delete = format!(":refs/heads/{BRANCH}");
        collab
            .run(&["push", "-q", "origin", &delete], opts)
            .await
            .expect("delete on origin");
        finish(&fx).await.expect("finish");
        let recreate = format!("refs/remotes/origin/main:refs/heads/{BRANCH}");
        collab
            .run(&["push", "-q", "origin", &recreate], opts)
            .await
            .expect("recreate on origin");

        assert_redo_refused_after_fetch(
            &fx,
            &format!("{BRANCH} was rewritten on origin. Use Reset to backup."),
        )
        .await;
    }

    #[tokio::test]
    async fn redo_refused_when_origin_moved_again() {
        let fx = Fixture::new().await;
        completed_clean(&fx).await;
        let rebased = fx.head().await;
        let seen = reject_push(&fx, "c.txt").await;
        assert_eq!(state(&fx).await.push_failed_remote_sha, Some(seen.clone()));
        collaborator_push(&fx, BRANCH, "d.txt").await;
        let moved = origin_tip(&fx, BRANCH).await.expect("branch on origin");
        let json_before = fx.json_bytes();

        let message = input_message(redo(&fx).await);
        assert_eq!(
            message,
            format!("{BRANCH} changed on origin again. Review it and try again.")
        );
        assert_ne!(fx.json_bytes(), json_before);
        let state = state(&fx).await;
        assert_eq!(state.state, RebaseStateKind::PushFailed);
        assert_eq!(state.push_failed_remote_sha, Some(moved.clone()));
        assert_eq!(fx.head().await, rebased);
        assert!(rebased_backups(&fx).await.is_empty());

        // Once seen, the same origin can be redone onto.
        redo(&fx).await.expect("redo");
        assert_eq!(record(&fx).backup_sha, moved);
    }

    #[tokio::test]
    async fn redo_keeps_an_old_original_backup() {
        let fx = Fixture::new().await;
        completed_clean(&fx).await;
        let file = record(&fx);
        let old = unix_now() - 15 * DAY;
        let old_backup = format!("refs/f11r-backup/{BRANCH}/{old}");
        let unrecorded = format!("refs/f11r-backup/{BRANCH}/{}", old - 1);
        fx.run(&["update-ref", &old_backup, &file.backup_sha]).await;
        fx.run(&["update-ref", &unrecorded, &file.backup_sha]).await;
        fx.write_file_json(&GuidedRebaseFile {
            backup_ref: old_backup.clone(),
            ..file.clone()
        });
        reject_push(&fx, "c.txt").await;

        redo(&fx).await.expect("redo");
        assert_eq!(fx.sha(&old_backup).await, file.backup_sha);
        assert!(!backup_refs(&fx).await.contains(&unrecorded));
    }

    #[tokio::test]
    async fn redo_interrupted_after_the_reset_is_dismissable() {
        let fx = Fixture::new().await;
        completed_clean(&fx).await;
        let file = record(&fx);
        let collab = reject_push(&fx, "c.txt").await;
        // Redo's reset ran, then the app died before deleting the record.
        fx.run(&["reset", "-q", "--keep", &collab]).await;

        let derived = fx.derive().await;
        assert_eq!(
            (derived.kind, derived.stale_reason),
            (
                RebaseStateKind::Stale,
                Some(RebaseStaleReason::PushedOutside)
            )
        );
        dismiss_stale(&fx.git, fx.path(), &tokio::sync::Mutex::new(()))
            .await
            .expect("dismiss");
        assert_eq!(state(&fx).await, RebaseState::default());
        assert_eq!(fx.head().await, collab);
        assert_eq!(fx.sha(&file.backup_ref).await, file.backup_sha);
    }

    #[tokio::test]
    async fn redo_refused_unless_push_failed() {
        let fx = Fixture::new().await;
        completed_clean(&fx).await;
        let before = capture(&fx).await;
        assert_eq!(input_message(redo(&fx).await), REDO_ONLY_AFTER_PUSH_FAILED);
        assert_eq!(capture(&fx).await, before);
    }

    // Reset to backup

    fn reset_op(fx: &Fixture) -> RebaseResetToBackupOp<UnrealEngineProvider> {
        let refs = refs(fx);
        RebaseResetToBackupOp {
            git_client: fx.git.clone(),
            engine: refs.engine,
            app_config: refs.app_config,
            repo_config: refs.repo_config,
            repo_status: refs.repo_status,
            sync_phase_tx: mpsc::channel().0,
            post: Err(POST_ERROR.to_string()),
        }
    }

    async fn reset_to_backup(fx: &Fixture) -> Result<(), CoreError> {
        reset_op(fx).execute().await
    }

    /// Resets, and checks the branch is at its backup with every backup ref kept.
    async fn assert_reset(fx: &Fixture) {
        let file = record(fx);
        let rebased = fx.head().await;
        let backups_before = backup_refs(fx).await;
        let origin_before = capture(fx).await.origin_refs;

        assert_eq!(
            input_message(reset_to_backup(fx).await),
            format!("{BRANCH} was reset, but updating editor binaries or engine failed: test.")
        );
        assert_eq!(fx.head().await, file.backup_sha);
        assert_eq!(
            fx.out(&["symbolic-ref", "-q", "HEAD"]).await,
            format!("refs/heads/{BRANCH}")
        );
        assert!(!json_path(&fx.git_dir()).exists());
        let backups = backup_refs(fx).await;
        assert!(backups_before.iter().all(|b| backups.contains(b)));
        assert_eq!(fx.sha(&file.backup_ref).await, file.backup_sha);
        let rebased_refs = rebased_backups(fx).await;
        assert_eq!(rebased_refs.len(), 1);
        assert_eq!(fx.sha(&rebased_refs[0]).await, rebased);
        assert_eq!(capture(fx).await.origin_refs, origin_before);
        assert_eq!(state(fx).await, RebaseState::default());
    }

    #[tokio::test]
    async fn reset_to_backup_from_completed_unpushed() {
        let fx = Fixture::new().await;
        completed_clean(&fx).await;
        assert_reset(&fx).await;
    }

    #[tokio::test]
    async fn reset_to_backup_from_push_failed() {
        let fx = Fixture::new().await;
        completed_clean(&fx).await;
        reject_push(&fx, "c.txt").await;
        assert_reset(&fx).await;
    }

    #[tokio::test]
    async fn reset_to_backup_keeps_an_unrelated_local_change() {
        let fx = Fixture::new().await;
        completed_clean(&fx).await;
        fx.write("seed.txt", b"edited\n");
        let file = record(&fx);
        input_message(reset_to_backup(&fx).await);
        assert_eq!(fx.head().await, file.backup_sha);
        assert_eq!(read(&fx, "seed.txt").as_deref(), Some(&b"edited\n"[..]));
        assert!(!json_path(&fx.git_dir()).exists());
    }

    #[tokio::test]
    async fn reset_to_backup_refused_by_a_conflicting_local_change() {
        let fx = Fixture::new().await;
        completed_clean(&fx).await;
        // m.txt comes from main and is not in the backup.
        fx.write("m.txt", b"edited\n");
        let head = fx.head().await;
        let json_before = fx.json_bytes();

        let message = internal_message(reset_to_backup(&fx).await);
        assert!(message.starts_with("git reset --keep failed:"), "{message}");
        assert_eq!(fx.head().await, head);
        assert_eq!(fx.json_bytes(), json_before);
        assert_eq!(read(&fx, "m.txt").as_deref(), Some(&b"edited\n"[..]));
        assert_eq!(state(&fx).await.state, RebaseStateKind::CompletedUnpushed);
    }

    #[tokio::test]
    async fn reset_to_backup_refused_when_the_backup_is_missing() {
        let fx = Fixture::new().await;
        completed_clean(&fx).await;
        let file = record(&fx);
        fx.run(&["update-ref", "-d", &file.backup_ref]).await;
        let before = capture(&fx).await;

        assert_eq!(
            internal_message(reset_to_backup(&fx).await),
            format!(
                "The backup {} is missing or moved; nothing was changed.",
                file.backup_ref
            )
        );
        assert_eq!(capture(&fx).await, before);
    }

    #[tokio::test]
    async fn reset_to_backup_refused_unless_replayed() {
        let fx = Fixture::new().await;
        assert_eq!(
            input_message(reset_to_backup(&fx).await),
            RESET_ONLY_AFTER_REPLAY
        );
        stop_content(&fx).await;
        let before = capture(&fx).await;
        assert_eq!(
            input_message(reset_to_backup(&fx).await),
            RESET_ONLY_AFTER_REPLAY
        );
        assert_eq!(capture(&fx).await, before);
    }

    // Editor rebuild

    fn rebuild_op(
        fx: &Fixture,
    ) -> (
        EditorRebuildOp<UnrealEngineProvider>,
        mpsc::Receiver<String>,
    ) {
        let refs = refs(fx);
        let (sync_phase_tx, phases) = mpsc::channel();
        let op = EditorRebuildOp {
            git_client: fx.git.clone(),
            engine: refs.engine,
            app_config: refs.app_config,
            repo_config: refs.repo_config,
            repo_status: refs.repo_status,
            sync_phase_tx,
            result: Arc::new(parking_lot::Mutex::new(None)),
        };
        (op, phases)
    }

    async fn rebuild(fx: &Fixture) -> Result<RebaseCommitInfo, CoreError> {
        let (op, _) = rebuild_op(fx);
        op.execute().await?;
        let commit = op.result.lock().take().expect("rebuild result");
        Ok(commit)
    }

    /// `BRANCH` with one commit, pushed.
    async fn pushed_branch(fx: &Fixture) -> String {
        fx.branch(BRANCH).await;
        let tip = fx.commit_file("b.txt", b"branch\n", "branch change").await;
        fx.push(BRANCH).await;
        tip
    }

    #[tokio::test]
    async fn rebuild_pushes_an_empty_ci_commit() {
        let fx = Fixture::new().await;
        let tip = pushed_branch(&fx).await;

        let commit = rebuild(&fx).await.expect("rebuild");
        let origin = origin_tip(&fx, BRANCH).await.expect("branch on origin");
        assert_eq!(
            commit,
            RebaseCommitInfo {
                sha: origin.clone(),
                subject: EDITOR_BUILD_MESSAGE.to_string(),
                author: "User".to_string(),
            }
        );
        assert_eq!(fx.head().await, origin);
        assert_eq!(fx.sha(&format!("{origin}^")).await, tip);
        assert_eq!(
            fx.sha(&format!("{origin}^{{tree}}")).await,
            fx.sha(&format!("{tip}^{{tree}}")).await
        );
    }

    #[tokio::test]
    async fn rebuild_race_leaves_the_collaborators_commit() {
        let fx = Fixture::new().await;
        let tip = pushed_branch(&fx).await;
        // The collaborator's commit is ready; a one-shot pre-push hook of the user clone pushes
        // it after the op's fetch and before its own push reaches origin.
        let (collab_dir, collab) = collaborator(&fx).await;
        let opts = git::Opts::new_without_logs();
        let tracking = format!("refs/remotes/origin/{BRANCH}");
        collab
            .run(&["checkout", "-q", "-B", BRANCH, &tracking], opts)
            .await
            .expect("checkout");
        fs::write(collab_dir.path().join("c.txt"), b"collab\n").unwrap();
        collab.run(&["add", "c.txt"], opts).await.expect("add");
        collab
            .run(&["commit", "-qm", "collab commit"], opts)
            .await
            .expect("commit");
        let collab_sha = collab
            .run_and_collect_output(&["rev-parse", "HEAD"], opts)
            .await
            .unwrap()
            .trim()
            .to_string();
        let race_hooks = tempfile::tempdir().expect("hooks tempdir");
        fs::write(
            race_hooks.path().join("pre-push"),
            format!(
                "#!/bin/sh\nrm -f \"$0\"\nunset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_CONFIG_PARAMETERS\ncd '{}' && git push -q origin HEAD:refs/heads/{BRANCH}\n",
                slash_path(collab_dir.path())
            ),
        )
        .unwrap();
        fx.run(&["config", "core.hooksPath", &slash_path(race_hooks.path())])
            .await;

        assert_eq!(input_message(rebuild(&fx).await), NEW_COMMITS_ON_ORIGIN);
        assert!(!race_hooks.path().join("pre-push").exists());
        assert_eq!(fx.head().await, tip);
        assert_eq!(fx.out(&["status", "--porcelain"]).await, "");
        assert_eq!(origin_tip(&fx, BRANCH).await, Some(collab_sha.clone()));
        assert_eq!(
            new_git(fx.origin_path())
                .run_and_collect_output(&["rev-parse", &format!("{collab_sha}^")], opts)
                .await
                .unwrap()
                .trim(),
            tip
        );
        assert_eq!(
            fx.out(&[
                "log",
                "-1",
                "--format=%s",
                "--all",
                "--grep=ci:editor",
                "--fixed-strings"
            ])
            .await,
            ""
        );
    }

    /// The rebuild is refused with `expected` and changes nothing but tracking refs.
    async fn assert_rebuild_refused(fx: &Fixture, expected: &str) {
        let before = capture(fx).await;
        assert_eq!(input_message(rebuild(fx).await), expected);
        let after = capture(fx).await;
        assert_eq!(
            Snapshot {
                tracking_refs: before.tracking_refs.clone(),
                ..after
            },
            before
        );
    }

    #[tokio::test]
    async fn rebuild_refused_when_not_in_sync() {
        let fx = Fixture::new().await;
        pushed_branch(&fx).await;
        fx.commit_file("x.txt", b"x\n", "unpushed").await;
        assert_rebuild_refused(&fx, SYNC_FIRST).await;
    }

    #[tokio::test]
    async fn rebuild_refused_with_a_staged_file() {
        let fx = Fixture::new().await;
        pushed_branch(&fx).await;
        fx.write("staged.txt", b"staged\n");
        fx.run(&["add", "staged.txt"]).await;
        assert_rebuild_refused(&fx, UNSTAGE_FIRST).await;
        assert_eq!(
            fx.out(&["diff", "--cached", "--name-only"]).await,
            "staged.txt"
        );
    }

    #[tokio::test]
    async fn rebuild_refused_during_a_guided_rebase() {
        let fx = Fixture::new().await;
        completed_clean(&fx).await;
        assert_rebuild_refused(&fx, GUARD_COMPLETED_UNPUSHED).await;
    }

    #[tokio::test]
    async fn rebuild_refused_on_a_protected_branch() {
        let fx = Fixture::new().await;
        assert_rebuild_refused(&fx, FEATURE_BRANCHES_ONLY).await;
    }

    #[tokio::test]
    async fn rebuild_refused_during_a_merge() {
        let fx = Fixture::new().await;
        let tip = pushed_branch(&fx).await;
        // A merge that changes no file: `commit --allow-empty` would conclude it.
        let tree = format!("{tip}^{{tree}}");
        let side = fx
            .out(&["commit-tree", &tree, "-p", &tip, "-m", "side"])
            .await;
        fx.run(&["merge", "-q", "--no-ff", "--no-commit", &side])
            .await;
        assert!(fx.git_dir().join("MERGE_HEAD").exists());

        assert_rebuild_refused(&fx, OPERATION_IN_PROGRESS).await;
        assert!(fx.git_dir().join("MERGE_HEAD").exists());
        assert_eq!(fx.head().await, tip);
    }

    #[tokio::test]
    async fn rebuild_rejected_by_origin_drops_the_commit() {
        let fx = Fixture::new().await;
        let tip = pushed_branch(&fx).await;
        refuse_pushes_on_origin(&fx).await;
        let origin_before = capture(&fx).await.origin_refs;

        let message = input_message(rebuild(&fx).await);
        assert!(
            message.starts_with("GitHub rejected the push: [remote rejected]"),
            "{message}"
        );
        assert_eq!(fx.head().await, tip);
        assert_eq!(fx.out(&["status", "--porcelain"]).await, "");
        assert_eq!(capture(&fx).await.origin_refs, origin_before);
    }

    #[tokio::test]
    async fn rebuild_network_failure_drops_the_commit() {
        let fx = Fixture::new().await;
        let tip = pushed_branch(&fx).await;
        // The fetch succeeds; the push goes to a missing local path.
        let missing = slash_path(&fx.path().join("missing.git"));
        fx.run(&["remote", "set-url", "--push", "origin", &missing])
            .await;

        let message = internal_message(rebuild(&fx).await);
        assert!(message.starts_with("Push failed:"), "{message}");
        assert_eq!(fx.head().await, tip);
        assert_eq!(origin_tip(&fx, BRANCH).await, Some(tip));
    }
}
