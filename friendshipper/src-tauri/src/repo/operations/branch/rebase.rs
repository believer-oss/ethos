use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::anyhow;
use axum::extract::State;
use axum::{async_trait, Json};
use tracing::{error, instrument, warn};

use super::cleanup::worktree_git_dirs_or;
use super::rebase_guard::guard_sentence;
use super::rebase_state::{
    build_rebase_state, create_backup_ref, delete_rebase_file, derive_rebase_state,
    ensure_current_step, is_ancestor, is_full_sha, parse_unmerged, prune_backup_refs, read_git,
    read_rebase_file, rebase_dir_exists, rebase_head, refresh_resolved, resolve_git_dir, rev_parse,
    set_resolution, status_porcelain, write_rebase_file, DerivedState, GuidedRebaseFile,
    GuidedRebasePhase, OriginalCommit, RebaseFileRead, CPP_PATHSPECS, REBASE_FILE_LOCK,
    REBASE_FILE_VERSION, REBASE_OP_LOCK,
};
use super::{git_too_old_message, is_valid_branch_name, run_sequence};
use crate::engine::EngineProvider;
use crate::repo::operations::gh::submit::{find_files_with_conflict_markers, is_branch_mode};
use crate::repo::operations::validate::validate_repo_state;
use crate::repo::operations::StatusOp;
use crate::repo::RepoStatusRef;
use crate::state::AppState;
use ethos_core::clients::git::{self, GitVersionCheck};
use ethos_core::types::config::{AppConfigRef, RepoConfigRef, UProject};
use ethos_core::types::errors::CoreError;
use ethos_core::types::repo::{
    ConflictFile, ConflictKind, ConflictSideName, OpenConflictFileRequest, RebaseCommitInfo,
    RebaseFinishResponse, RebaseKeep, RebasePreflight, RebaseState, RebaseStateKind,
    ResolveConflictRequest, UnresolveConflictRequest,
};
use ethos_core::utils::process::describe_blocking_processes;
use ethos_core::worker::Task;

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

fn not_implemented() -> CoreError {
    CoreError::Internal(anyhow!("Guided rebase is not implemented yet"))
}

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

/// Fetches `<trunk>` and, when it exists on origin, `<branch>`; returns whether it does.
async fn fetch_trunk_and_branch(
    git: &git::Git,
    trunk: &str,
    branch: &str,
) -> Result<bool, CoreError> {
    let fetch = |with_branch: bool| async move {
        let mut refspecs = vec![format!("+refs/heads/{trunk}:refs/remotes/origin/{trunk}")];
        if with_branch {
            refspecs.push(format!("+refs/heads/{branch}:refs/remotes/origin/{branch}"));
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

    /// The checked-out feature branch.
    async fn feature_branch(&self) -> Result<String, CoreError> {
        // The full ref: `--short` gives `heads/<branch>` when a tag has the same name.
        let head = head_ref(&self.git_client).await?;
        match head.as_deref().and_then(|r| r.strip_prefix("refs/heads/")) {
            Some(branch)
                if is_valid_branch_name(branch)
                    && is_branch_mode(&self.repo_config.read(), branch) =>
            {
                Ok(branch.to_string())
            }
            _ => Err(input(FEATURE_BRANCHES_ONLY)),
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
}

#[async_trait]
impl<T> Task for RebaseStartOp<T>
where
    T: EngineProvider,
{
    async fn execute(&self) -> Result<(), CoreError> {
        let _op = REBASE_OP_LOCK.lock().await;
        self.emit_phase("Checking repository status");
        let git = &self.git_client;
        let repo_path = repo_path_of(&self.app_config);
        let git_dir = resolve_git_dir(&repo_path);

        let derived = derive(git, &repo_path).await?;
        match derived.kind {
            RebaseStateKind::Idle => {}
            RebaseStateKind::Stale => return Err(input(DISMISS_FIRST)),
            RebaseStateKind::External => return Err(input(EXTERNAL_REBASE)),
            kind => return Err(input(guard_sentence(kind).unwrap_or(DISMISS_FIRST))),
        }

        editor_check(&self.engine).await?;
        index_lock_check(&git_dir)?;
        git_version_gate(&git.check_min_version().await?)?;

        let branch = self.feature_branch().await?;
        let trunk = self.trunk.clone();
        if !is_valid_branch_name(&trunk) {
            return Err(input(NO_TRUNK));
        }

        self.status_op().execute().await?;
        validate_repo_state(&repo_path, &self.repo_status.read())?;
        let status = git.status(vec![]).await?;
        if status
            .lines()
            .any(|line| !line.is_empty() && !line.starts_with("## "))
        {
            return Err(input(DIRTY_TREE));
        }

        self.emit_phase("Fetching latest");
        if !fetch_trunk_and_branch(git, &trunk, &branch).await? {
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
        let engine_association = self.engine_association(&repo_path);

        let now = unix_now();
        match recorded_backup_refs(git).await {
            Some(keep) => prune_backup_refs(git, now, &keep).await?,
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
        // TODO: Implement in Stage 011
        Err(not_implemented())
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
}

#[async_trait]
impl<T> Task for RebaseAbortOp<T>
where
    T: EngineProvider,
{
    async fn execute(&self) -> Result<(), CoreError> {
        // TODO: Implement in Stage 011
        Err(not_implemented())
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
    pub result: Arc<parking_lot::Mutex<Option<RebaseFinishResponse>>>,
}

#[async_trait]
impl<T> Task for RebaseFinishOp<T>
where
    T: EngineProvider,
{
    async fn execute(&self) -> Result<(), CoreError> {
        // TODO: Implement in Stage 013
        Err(not_implemented())
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
}

#[async_trait]
impl<T> Task for RebaseRedoOp<T>
where
    T: EngineProvider,
{
    async fn execute(&self) -> Result<(), CoreError> {
        // TODO: Implement in Stage 013
        Err(not_implemented())
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
}

#[async_trait]
impl<T> Task for RebaseResetToBackupOp<T>
where
    T: EngineProvider,
{
    async fn execute(&self) -> Result<(), CoreError> {
        // TODO: Implement in Stage 013
        Err(not_implemented())
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
        // TODO: Implement in Stage 013
        Err(not_implemented())
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
    State(_state): State<AppState<T>>,
) -> Result<Json<RebaseState>, CoreError>
where
    T: EngineProvider,
{
    // TODO: Implement in Stage 011
    Err(not_implemented())
}

#[instrument(skip_all)]
pub async fn guided_rebase_abort_handler<T>(
    State(_state): State<AppState<T>>,
) -> Result<Json<RebaseState>, CoreError>
where
    T: EngineProvider,
{
    // TODO: Implement in Stage 011
    Err(not_implemented())
}

#[instrument(skip_all)]
pub async fn guided_rebase_finish_handler<T>(
    State(_state): State<AppState<T>>,
) -> Result<Json<RebaseFinishResponse>, CoreError>
where
    T: EngineProvider,
{
    // TODO: Implement in Stage 013
    Err(not_implemented())
}

#[instrument(skip_all)]
pub async fn guided_rebase_redo_handler<T>(
    State(_state): State<AppState<T>>,
) -> Result<Json<RebaseState>, CoreError>
where
    T: EngineProvider,
{
    // TODO: Implement in Stage 013
    Err(not_implemented())
}

#[instrument(skip_all)]
pub async fn guided_rebase_reset_to_backup_handler<T>(
    State(_state): State<AppState<T>>,
) -> Result<Json<RebaseState>, CoreError>
where
    T: EngineProvider,
{
    // TODO: Implement in Stage 013
    Err(not_implemented())
}

#[instrument(skip_all)]
pub async fn editor_rebuild_handler<T>(
    State(_state): State<AppState<T>>,
) -> Result<Json<RebaseCommitInfo>, CoreError>
where
    T: EngineProvider,
{
    // TODO: Implement in Stage 013
    Err(not_implemented())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::mpsc;

    use parking_lot::RwLock;

    use super::*;
    use crate::engine::UnrealEngineProvider;
    use crate::repo::operations::branch::rebase_guard::GUARD_IN_PROGRESS;
    use crate::repo::operations::branch::rebase_state::tests::{
        conflict, diverge_on_a_txt, engine, guided_stop_on_a_txt, info, new_git, original,
        sample_file_for, slash_path, Change, Diverged, Fixture, BRANCH,
    };
    use crate::repo::operations::branch::rebase_state::{json_path, UnmergedEntry};
    use ethos_core::types::config::{AppConfig, RepoConfig, TargetBranchConfig};
    use ethos_core::types::repo::{RebaseStaleReason, RepoStatus};

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

    fn internal_message(result: Result<(), CoreError>) -> String {
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
}
