use std::collections::{HashMap, HashSet};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::anyhow;
use axum::extract::State;
use axum::{async_trait, Json};
use futures::stream::{self, StreamExt};
use tracing::{info, instrument, warn};

use super::analyze::{merged_pr_covers_commit, quiet};
use super::{is_valid_branch_name, run_sequence, GITHUB_PAT_MISSING};
use crate::engine::EngineProvider;
use crate::state::AppState;
use ethos_core::clients::git;
use ethos_core::types::config::RepoConfig;
use ethos_core::types::errors::CoreError;
use ethos_core::types::repo::{
    is_feature_branch, BranchCleanupCandidate, BranchCleanupCandidates, BranchCleanupCommit,
    BranchCleanupPr, BranchCleanupRequest, BranchCleanupResult, BranchCleanupTarget,
    BranchPullRequest,
};
use ethos_core::worker::Task;

const UNMERGED_COMMIT_LIST_LIMIT: usize = 20;
const PR_LOOKUP_CONCURRENCY: usize = 4;
const ORIGIN_PREFIX: &str = "refs/remotes/origin/";
const NO_BRANCH_CHECKED_OUT: &str =
    "No branch is checked out (detached HEAD or rebase in progress). Check out a branch to clean up branches.";
const BRANCH_FORMAT: &str = "--format=%(refname)%00%(objectname)%00%(upstream)%00%(symref)%00%(authorname)%00%(authordate:iso-strict)%00%(contents:subject)";
/// Files in a worktree's git dir that name the branch a rebase or bisect will return to.
const IN_PROGRESS_FILES: [&str; 3] = [
    "rebase-merge/head-name",
    "rebase-apply/head-name",
    "BISECT_START",
];

type PrLookup = Result<Option<BranchPullRequest>, String>;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct LocalBranch {
    name: String,
    sha: String,
    /// Full upstream tracking ref (e.g. `refs/remotes/origin/x`); empty when none is configured.
    upstream: String,
    /// The ref a symbolic ref points at; empty for an ordinary branch.
    symref: String,
    author: String,
    date: String,
    subject: String,
}

fn parse_local_branches(output: &str) -> Vec<LocalBranch> {
    output
        .lines()
        .filter_map(|line| {
            let mut parts = line.splitn(7, '\0');
            let name = parts.next()?.strip_prefix("refs/heads/")?.to_string();
            Some(LocalBranch {
                name,
                sha: parts.next()?.to_string(),
                upstream: parts.next()?.to_string(),
                symref: parts.next()?.to_string(),
                author: parts.next()?.to_string(),
                date: parts.next()?.to_string(),
                subject: parts.next().unwrap_or_default().to_string(),
            })
        })
        .collect()
}

async fn local_branches(git: &git::Git) -> Result<Vec<LocalBranch>, CoreError> {
    let output = git
        .run_and_collect_output(&["for-each-ref", BRANCH_FORMAT, "refs/heads/"], quiet())
        .await?;
    Ok(parse_local_branches(&output))
}

async fn origin_refs(git: &git::Git) -> Result<HashSet<String>, CoreError> {
    let output = git
        .run_and_collect_output(
            &["for-each-ref", "--format=%(refname)", ORIGIN_PREFIX],
            quiet(),
        )
        .await?;
    Ok(output.lines().map(str::to_string).collect())
}

/// Branches a rebase or bisect in the worktree with git dir `git_dir` will return to.
fn in_progress_branches(git_dir: &Path) -> Result<Vec<String>, String> {
    let mut branches = vec![];
    for file in IN_PROGRESS_FILES {
        let path = git_dir.join(file);
        let content = match std::fs::read_to_string(&path) {
            Ok(content) => content,
            Err(e) if e.kind() == ErrorKind::NotFound => continue,
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        let name = content.trim();
        // head-name holds a full ref (or "detached HEAD"); BISECT_START a short name or a sha.
        let name = match name.strip_prefix("refs/heads/") {
            Some(short) => short,
            None if file == "BISECT_START" => name,
            None => continue,
        };
        if !name.is_empty() {
            branches.push(name.to_string());
        }
    }
    Ok(branches)
}

/// The git dir of every worktree: the common dir for the main worktree, plus each linked
/// worktree's `worktrees/<id>`, including ones whose directory is gone.
async fn worktree_git_dirs(git: &git::Git) -> Result<Vec<PathBuf>, CoreError> {
    let output = git
        .run_and_collect_output(
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
            quiet(),
        )
        .await?;
    let common = PathBuf::from(output.trim());
    let unreadable = |e: std::io::Error| {
        CoreError::Input(anyhow!(
            "Could not read the worktrees in {}: {e}. Branch cleanup needs every worktree's state.",
            common.display()
        ))
    };
    let mut dirs = vec![common.clone()];
    match std::fs::read_dir(common.join("worktrees")) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry.map_err(unreadable)?;
                if entry.file_type().map_err(unreadable)?.is_dir() {
                    dirs.push(entry.path());
                }
            }
        }
        Err(e) if e.kind() == ErrorKind::NotFound => {}
        Err(e) => return Err(unreadable(e)),
    }
    Ok(dirs)
}

/// Branches cleanup must not touch: the current branch, branches checked out in any worktree
/// and branches being rebased or bisected in any worktree (those report a detached HEAD).
/// Refuses a detached HEAD here, which includes a rebase in progress, and any worktree whose
/// state can't be read.
async fn checked_out_branches(git: &git::Git) -> Result<HashSet<String>, CoreError> {
    let current = git.current_branch().await?;
    if current.is_empty() {
        return Err(CoreError::Input(anyhow!(NO_BRANCH_CHECKED_OUT)));
    }
    let output = git
        .run_and_collect_output(&["worktree", "list", "--porcelain"], quiet())
        .await?;
    let mut branches: HashSet<String> = output
        .lines()
        .filter_map(|line| line.strip_prefix("branch refs/heads/"))
        .map(str::to_string)
        .collect();
    for dir in worktree_git_dirs(git).await? {
        let in_progress = in_progress_branches(&dir).map_err(|e| {
            CoreError::Input(anyhow!(
                "Could not read the rebase or bisect state of a worktree ({e}). Branch cleanup needs every worktree's state."
            ))
        })?;
        branches.extend(in_progress);
    }
    branches.insert(current);
    Ok(branches)
}

async fn prune_fetch(git: &git::Git) -> Result<(), CoreError> {
    git.run(
        &[
            "fetch",
            "--no-auto-maintenance",
            "--no-write-fetch-head",
            "--prune",
            "origin",
        ],
        quiet().with_complete_error(),
    )
    .await?;
    Ok(())
}

fn is_cleanup_name(name: &str) -> bool {
    is_feature_branch(name) && is_valid_branch_name(name)
}

fn is_full_sha(sha: &str) -> bool {
    (sha.len() == 40 || sha.len() == 64)
        && sha
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
}

/// Whether the branch tracked an origin branch that no longer exists: an upstream on origin is
/// configured, and neither it nor `refs/remotes/origin/<name>` exists after a prune fetch.
/// A branch that was never pushed has no upstream and is never treated as gone.
fn is_gone_on_origin(branch: &LocalBranch, origin_refs: &HashSet<String>) -> bool {
    branch.upstream.starts_with(ORIGIN_PREFIX)
        && !origin_refs.contains(&branch.upstream)
        && !origin_refs.contains(&format!("{ORIGIN_PREFIX}{}", branch.name))
}

fn select_candidates(
    branches: Vec<LocalBranch>,
    origin_refs: &HashSet<String>,
    checked_out: &HashSet<String>,
) -> Vec<LocalBranch> {
    branches
        .into_iter()
        .filter(|b| {
            is_cleanup_name(&b.name)
                && b.symref.is_empty()
                && is_full_sha(&b.sha)
                && !checked_out.contains(&b.name)
                && is_gone_on_origin(b, origin_refs)
        })
        .collect()
}

fn unpicked_shas(cherry_output: &str) -> HashSet<String> {
    cherry_output
        .lines()
        .filter_map(|line| line.strip_prefix("+ "))
        .filter_map(|rest| rest.split(' ').next())
        .map(str::to_string)
        .collect()
}

/// Classifies `branch` against `trunk_ref` with local git only.
async fn classify(
    git: &git::Git,
    trunk_ref: &str,
    branch: &LocalBranch,
) -> Result<BranchCleanupCandidate, CoreError> {
    let mut candidate = BranchCleanupCandidate {
        name: branch.name.clone(),
        sha: branch.sha.clone(),
        subject: branch.subject.clone(),
        author: branch.author.clone(),
        date: branch.date.clone(),
        ..Default::default()
    };

    if git
        .run(
            &["merge-base", "--is-ancestor", &branch.sha, trunk_ref],
            quiet(),
        )
        .await
        .is_ok()
    {
        candidate.merged_into_trunk = true;
        candidate.safe = true;
        return Ok(candidate);
    }

    // `git cherry` skips merge commits, so merges not on the trunk always count as unmerged.
    let cherry = git
        .run_and_collect_output(&["cherry", trunk_ref, &branch.sha], quiet())
        .await?;
    let unpicked = unpicked_shas(&cherry);
    let range = format!("{trunk_ref}..{}", branch.sha);
    let log = git
        .run_and_collect_output(&["log", "--format=%H%x00%P%x00%s", &range], quiet())
        .await?;

    let mut count = 0u32;
    for line in log.lines() {
        let mut parts = line.splitn(3, '\0');
        let (Some(sha), Some(parents)) = (parts.next(), parts.next()) else {
            continue;
        };
        let is_merge = parents.split_whitespace().count() > 1;
        if !is_merge && !unpicked.contains(sha) {
            continue;
        }
        count += 1;
        if candidate.unmerged_commits.len() < UNMERGED_COMMIT_LIST_LIMIT {
            candidate.unmerged_commits.push(BranchCleanupCommit {
                sha: sha.to_string(),
                subject: parts.next().unwrap_or_default().to_string(),
            });
        }
    }

    candidate.unmerged_commit_count = count;
    candidate.patch_equivalent = count == 0;
    candidate.safe = candidate.patch_equivalent;
    Ok(candidate)
}

/// Prune-fetches origin and lists the local `f11r-branch-*` branches whose origin branch is gone,
/// classified against `origin/<trunk>`.
#[derive(Clone)]
pub struct BranchCleanupCandidatesOp {
    pub git_client: git::Git,
    pub trunk_branch: String,
    pub result: Arc<parking_lot::Mutex<Option<Vec<BranchCleanupCandidate>>>>,
}

#[async_trait]
impl Task for BranchCleanupCandidatesOp {
    #[instrument(name = "BranchCleanupCandidatesOp::execute", skip(self))]
    async fn execute(&self) -> Result<(), CoreError> {
        let git = &self.git_client;
        prune_fetch(git).await?;

        let trunk_ref = format!("{ORIGIN_PREFIX}{}", self.trunk_branch);
        let trunk_commit = format!("{trunk_ref}^{{commit}}");
        if git
            .run(
                &["rev-parse", "--verify", "--quiet", &trunk_commit],
                quiet(),
            )
            .await
            .is_err()
        {
            return Err(CoreError::Input(anyhow!(
                "origin/{} was not found.",
                self.trunk_branch
            )));
        }

        let origin = origin_refs(git).await?;
        let local = local_branches(git).await?;
        let checked_out = checked_out_branches(git).await?;
        let mut candidates = vec![];
        for branch in select_candidates(local, &origin, &checked_out) {
            candidates.push(classify(git, &trunk_ref, &branch).await?);
        }
        *self.result.lock() = Some(candidates);
        Ok(())
    }

    fn get_name(&self) -> String {
        String::from("BranchCleanupCandidatesOp")
    }
}

/// Candidate indices whose MERGED pull request still needs its head checked against the tip.
fn merged_pr_checks(
    candidates: &[BranchCleanupCandidate],
    lookups: &[(usize, PrLookup)],
) -> Vec<(usize, String, String)> {
    lookups
        .iter()
        .filter_map(|(index, lookup)| match lookup {
            Ok(Some(pr)) if pr.state == "MERGED" => candidates
                .get(*index)
                .map(|c| (*index, c.sha.clone(), pr.head_ref_oid.clone())),
            _ => None,
        })
        .collect()
}

async fn covered_by_merged_pr(
    git: &git::Git,
    checks: &[(usize, String, String)],
) -> HashSet<usize> {
    let mut covered = HashSet::new();
    for (index, tip, head_ref_oid) in checks {
        if merged_pr_covers_commit(git, tip, head_ref_oid).await {
            covered.insert(*index);
        }
    }
    covered
}

/// Applies the pull request lookups; a merged PR counts as safe only into the trunk or a target
/// branch. Returns the first lookup error.
fn apply_pr_lookups(
    candidates: &mut [BranchCleanupCandidate],
    lookups: Vec<(usize, PrLookup)>,
    covered: &HashSet<usize>,
    merge_targets: &[String],
) -> Option<String> {
    let mut first_error = None;
    for (index, lookup) in lookups {
        let Some(candidate) = candidates.get_mut(index) else {
            continue;
        };
        match lookup {
            Err(e) => {
                candidate.pr_unknown = true;
                first_error.get_or_insert(e);
            }
            Ok(Some(pr)) if pr.state == "MERGED" && covered.contains(&index) => {
                if merge_targets.contains(&pr.base_ref_name) {
                    candidate.safe = true;
                }
                candidate.merged_via_pr = Some(BranchCleanupPr {
                    number: pr.number,
                    url: pr.url,
                    base_ref_name: pr.base_ref_name,
                });
            }
            Ok(_) => {}
        }
    }
    first_error
}

fn merge_targets(repo_config: &RepoConfig) -> Vec<String> {
    let mut targets = vec![repo_config.trunk_branch.clone()];
    targets.extend(
        repo_config
            .target_branches
            .iter()
            .map(|target| target.name.clone()),
    );
    targets
}

/// Runs the merged-PR tip checks on the worker so their fetches never interleave with Sync.
#[derive(Clone)]
struct MergedPrCoversOp {
    git_client: git::Git,
    checks: Vec<(usize, String, String)>,
    result: Arc<parking_lot::Mutex<HashSet<usize>>>,
}

#[async_trait]
impl Task for MergedPrCoversOp {
    #[instrument(name = "MergedPrCoversOp::execute", skip(self))]
    async fn execute(&self) -> Result<(), CoreError> {
        *self.result.lock() = covered_by_merged_pr(&self.git_client, &self.checks).await;
        Ok(())
    }

    fn get_name(&self) -> String {
        String::from("MergedPrCoversOp")
    }
}

#[instrument(skip(state))]
pub async fn branch_cleanup_candidates_handler<T>(
    State(state): State<AppState<T>>,
) -> Result<Json<BranchCleanupCandidates>, CoreError>
where
    T: EngineProvider,
{
    let (trunk, targets) = {
        let repo_config = state.repo_config.read();
        (
            repo_config.trunk_branch.clone(),
            merge_targets(&repo_config),
        )
    };
    if trunk.is_empty() {
        return Err(CoreError::Input(anyhow!(
            "The repo config does not define trunkBranch."
        )));
    }
    let (owner, repo) = {
        let status = state.repo_status.read();
        (status.repo_owner.clone(), status.repo_name.clone())
    };

    let result = Arc::new(parking_lot::Mutex::new(None));
    let op = BranchCleanupCandidatesOp {
        git_client: state.git(),
        trunk_branch: trunk.clone(),
        result: result.clone(),
    };
    run_sequence(&state, Box::new(op)).await?;
    let mut candidates = result
        .lock()
        .take()
        .ok_or_else(|| CoreError::Internal(anyhow!("Branch cleanup produced no candidate list")))?;

    let needs_lookup: Vec<(usize, String)> = candidates
        .iter()
        .enumerate()
        .filter(|(_, c)| !c.safe)
        .map(|(index, c)| (index, c.name.clone()))
        .collect();
    if needs_lookup.is_empty() {
        return Ok(Json(BranchCleanupCandidates {
            trunk_branch: trunk,
            candidates,
            pr_error: None,
        }));
    }

    // GitHub is queried off the worker so a slow GitHub never delays Sync or status.
    let github_client = state.github_client.read().clone();
    let unavailable = match &github_client {
        None => Some(GITHUB_PAT_MISSING.to_string()),
        Some(_) if owner.is_empty() || repo.is_empty() => {
            Some("Repository information not yet available".to_string())
        }
        Some(_) => None,
    };
    let lookups: Vec<(usize, PrLookup)> = match (github_client, unavailable) {
        (Some(client), None) => {
            let (client, owner, repo, trunk) = (&client, &owner, &repo, &trunk);
            stream::iter(needs_lookup)
                .map(|(index, name)| async move {
                    let lookup = client
                        .get_branch_pull_request(owner, repo, &name, trunk)
                        .await
                        .map_err(|e| e.to_string());
                    (index, lookup)
                })
                .buffer_unordered(PR_LOOKUP_CONCURRENCY)
                .collect()
                .await
        }
        (_, reason) => {
            let reason = reason.unwrap_or_else(|| GITHUB_PAT_MISSING.to_string());
            needs_lookup
                .into_iter()
                .map(|(index, _)| (index, Err(reason.clone())))
                .collect()
        }
    };

    let checks = merged_pr_checks(&candidates, &lookups);
    let covered = if checks.is_empty() {
        HashSet::new()
    } else {
        let covered = Arc::new(parking_lot::Mutex::new(HashSet::new()));
        let op = MergedPrCoversOp {
            git_client: state.git(),
            checks,
            result: covered.clone(),
        };
        run_sequence(&state, Box::new(op)).await?;
        let covered = std::mem::take(&mut *covered.lock());
        covered
    };

    let pr_error = apply_pr_lookups(&mut candidates, lookups, &covered, &targets);
    Ok(Json(BranchCleanupCandidates {
        trunk_branch: trunk,
        candidates,
        pr_error,
    }))
}

/// Why `target` must not be deleted, if anything; `local`, `origin_refs` and `checked_out` are
/// read after a fresh prune fetch.
fn cleanup_refusal(
    target: &BranchCleanupTarget,
    checked_out: &HashSet<String>,
    local: &HashMap<String, LocalBranch>,
    origin_refs: &HashSet<String>,
) -> Option<String> {
    if !is_cleanup_name(&target.name) {
        return Some("Only local f11r-branch-* branches can be cleaned up.".to_string());
    }
    if !is_full_sha(&target.sha) {
        return Some("The branch tip sha is invalid.".to_string());
    }
    if checked_out.contains(&target.name) {
        return Some("The branch is checked out.".to_string());
    }
    let Some(branch) = local.get(&target.name) else {
        return Some("The branch no longer exists locally.".to_string());
    };
    if !branch.symref.is_empty() {
        return Some("The branch is a symbolic ref.".to_string());
    }
    if branch.sha != target.sha {
        return Some(format!(
            "The branch moved to {}; refresh and review it again.",
            &branch.sha[..branch.sha.len().min(8)]
        ));
    }
    if !branch.upstream.starts_with(ORIGIN_PREFIX) {
        return Some("The branch does not track a branch on origin.".to_string());
    }
    if !is_gone_on_origin(branch, origin_refs) {
        return Some("The branch still exists on origin.".to_string());
    }
    None
}

/// Refuses a branch the client saw as merged by git checks unless it still is. A merge via PR
/// can't be rechecked offline; it rests on the tip being unchanged.
async fn safety_refusal(
    git: &git::Git,
    trunk: &str,
    target: &BranchCleanupTarget,
    branch: &LocalBranch,
) -> Option<String> {
    if !target.safe || target.safe_via_pr {
        return None;
    }
    let trunk_ref = format!("{ORIGIN_PREFIX}{trunk}");
    match classify(git, &trunk_ref, branch).await {
        Ok(candidate) if candidate.safe => None,
        outcome => {
            if let Err(e) = outcome {
                warn!("Could not recheck {} against {trunk_ref}: {e}", target.name);
            }
            Some(format!(
                "{} is no longer merged into {trunk}; review it again.",
                target.name
            ))
        }
    }
}

/// Deletes `refs/heads/<name>` itself (never a symref's target) only if it still points at
/// `sha`, then drops its config section.
async fn delete_local_branch(git: &git::Git, name: &str, sha: &str) -> Result<(), String> {
    let refname = format!("refs/heads/{name}");
    git.run(
        &["update-ref", "--no-deref", "-d", &refname, sha],
        quiet().with_complete_error(),
    )
    .await
    .map_err(|e| format!("The branch was not deleted: {e}"))?;
    info!("Deleted local branch {name} at {sha}");

    let section = format!("branch.{name}");
    if let Err(e) = git
        .run(&["config", "--remove-section", &section], quiet())
        .await
    {
        warn!("Deleted {name} but could not remove its {section} config: {e}");
    }
    Ok(())
}

/// Deletes exactly the requested branches that are still local `f11r-branch-*` branches, not
/// checked out, gone on origin, at the requested tip and, when requested as merged by git
/// checks, still merged into `origin/<trunk>`.
#[derive(Clone)]
pub struct BranchCleanupOp {
    pub git_client: git::Git,
    pub trunk_branch: String,
    pub targets: Vec<BranchCleanupTarget>,
    pub result: Arc<parking_lot::Mutex<Vec<BranchCleanupResult>>>,
}

#[async_trait]
impl Task for BranchCleanupOp {
    #[instrument(name = "BranchCleanupOp::execute", skip(self))]
    async fn execute(&self) -> Result<(), CoreError> {
        let git = &self.git_client;
        prune_fetch(git).await?;
        let local: HashMap<String, LocalBranch> = local_branches(git)
            .await?
            .into_iter()
            .map(|b| (b.name.clone(), b))
            .collect();
        let origin = origin_refs(git).await?;
        let checked_out = checked_out_branches(git).await?;

        let mut seen = HashSet::new();
        let mut results = vec![];
        for target in &self.targets {
            let refusal = if seen.insert(target.name.clone()) {
                cleanup_refusal(target, &checked_out, &local, &origin)
            } else {
                Some("The branch was listed more than once.".to_string())
            };
            let refusal = match (refusal, local.get(&target.name)) {
                (None, Some(branch)) => {
                    safety_refusal(git, &self.trunk_branch, target, branch).await
                }
                (refusal, _) => refusal,
            };
            let outcome = match refusal {
                Some(reason) => Err(reason),
                None => delete_local_branch(git, &target.name, &target.sha).await,
            };
            if let Err(e) = &outcome {
                warn!("Did not delete {}: {e}", target.name);
            }
            results.push(BranchCleanupResult {
                name: target.name.clone(),
                sha: target.sha.clone(),
                deleted: outcome.is_ok(),
                error: outcome.err(),
            });
        }
        *self.result.lock() = results;
        Ok(())
    }

    fn get_name(&self) -> String {
        String::from("BranchCleanupOp")
    }
}

#[instrument(skip(state))]
pub async fn branch_cleanup_handler<T>(
    State(state): State<AppState<T>>,
    Json(req): Json<BranchCleanupRequest>,
) -> Result<Json<Vec<BranchCleanupResult>>, CoreError>
where
    T: EngineProvider,
{
    if req.branches.is_empty() {
        return Err(CoreError::Input(anyhow!("No branches were selected.")));
    }
    let trunk = state.repo_config.read().trunk_branch.clone();
    if trunk.is_empty() {
        return Err(CoreError::Input(anyhow!(
            "The repo config does not define trunkBranch."
        )));
    }
    let result = Arc::new(parking_lot::Mutex::new(vec![]));
    let op = BranchCleanupOp {
        git_client: state.git(),
        trunk_branch: trunk,
        targets: req.branches,
        result: result.clone(),
    };
    run_sequence(&state, Box::new(op)).await?;
    let results = std::mem::take(&mut *result.lock());
    Ok(Json(results))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::sync::mpsc;

    use super::*;

    const GONE: &str = "f11r-branch-gone";

    fn new_git(path: &Path) -> git::Git {
        let (tx, _rx) = mpsc::channel::<String>();
        git::Git::new(path.to_path_buf(), tx)
    }

    fn slash_path(path: &Path) -> String {
        path.to_str().expect("utf8 path").replace('\\', "/")
    }

    async fn run_git(git: &git::Git, args: &[&str]) {
        git.run(args, git::Opts::default())
            .await
            .unwrap_or_else(|e| panic!("git {args:?} failed: {e}"));
    }

    async fn git_output(git: &git::Git, args: &[&str]) -> String {
        git.run_and_collect_output(args, git::Opts::default())
            .await
            .unwrap_or_else(|e| panic!("git {args:?} failed: {e}"))
            .trim()
            .to_string()
    }

    /// A temp bare origin with `main` and a user clone on `main`; hooks come only from a temp dir.
    struct Fixture {
        _origin_dir: tempfile::TempDir,
        user_dir: tempfile::TempDir,
        _hooks: tempfile::TempDir,
        origin: git::Git,
        user: git::Git,
    }

    impl Fixture {
        async fn new() -> Self {
            let origin_dir = tempfile::tempdir().expect("origin tempdir");
            let user_dir = tempfile::tempdir().expect("user tempdir");
            let hooks = tempfile::tempdir().expect("hooks tempdir");

            let origin = new_git(origin_dir.path());
            run_git(&origin, &["init", "-q", "--bare"]).await;
            let user = new_git(user_dir.path());
            run_git(
                &user,
                &[
                    "clone",
                    "-q",
                    &slash_path(origin_dir.path()),
                    &slash_path(user_dir.path()),
                ],
            )
            .await;
            for (key, value) in [
                ("user.email", "user@example.com"),
                ("user.name", "User"),
                ("commit.gpgsign", "false"),
                ("core.autocrlf", "false"),
            ] {
                run_git(&user, &["config", key, value]).await;
            }
            run_git(
                &user,
                &["config", "core.hooksPath", &slash_path(hooks.path())],
            )
            .await;

            let fx = Self {
                _origin_dir: origin_dir,
                user_dir,
                _hooks: hooks,
                origin,
                user,
            };
            run_git(&fx.user, &["checkout", "-q", "-b", "main"]).await;
            fx.commit("seed").await;
            run_git(&fx.user, &["push", "-q", "-u", "origin", "main"]).await;
            fx
        }

        /// Commits a new file named after `message` on the current branch; returns the sha.
        async fn commit(&self, message: &str) -> String {
            fs::write(self.user_dir.path().join(format!("{message}.txt")), message).unwrap();
            run_git(&self.user, &["add", "."]).await;
            run_git(&self.user, &["commit", "-qm", message]).await;
            self.sha("HEAD").await
        }

        async fn sha(&self, rev: &str) -> String {
            git_output(&self.user, &["rev-parse", rev]).await
        }

        async fn checkout(&self, branch: &str) {
            run_git(&self.user, &["checkout", "-q", branch]).await;
        }

        /// Branches `name` off `main` with one commit per message, pushes it with an upstream and
        /// returns to `main`; returns the tip.
        async fn pushed_branch(&self, name: &str, messages: &[&str]) -> String {
            run_git(&self.user, &["checkout", "-q", "-b", name, "main"]).await;
            for message in messages {
                self.commit(message).await;
            }
            run_git(&self.user, &["push", "-q", "-u", "origin", name]).await;
            let tip = self.sha("HEAD").await;
            self.checkout("main").await;
            tip
        }

        async fn delete_on_origin(&self, name: &str) {
            let refname = format!("refs/heads/{name}");
            run_git(&self.origin, &["update-ref", "-d", &refname]).await;
        }

        async fn gone_branch(&self, name: &str, messages: &[&str]) -> String {
            let tip = self.pushed_branch(name, messages).await;
            self.delete_on_origin(name).await;
            tip
        }

        /// Adds a commit to `main`, cherry-picks `shas` onto it and pushes `main`.
        async fn cherry_pick_onto_main(&self, shas: &[&str]) {
            self.checkout("main").await;
            self.commit(&format!("main-before-{}", &shas[0][..8])).await;
            for sha in shas {
                run_git(&self.user, &["cherry-pick", sha]).await;
            }
            run_git(&self.user, &["push", "-q", "origin", "main"]).await;
        }

        async fn local_tip(&self, name: &str) -> Option<String> {
            let refname = format!("refs/heads/{name}");
            self.user
                .run_and_collect_output(&["rev-parse", "--verify", "--quiet", &refname], quiet())
                .await
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        }

        async fn has_upstream_config(&self, name: &str) -> bool {
            let key = format!("branch.{name}.merge");
            self.user
                .run_and_collect_output(&["config", "--get", &key], quiet())
                .await
                .is_ok_and(|s| !s.trim().is_empty())
        }

        async fn candidates(&self) -> Result<Vec<BranchCleanupCandidate>, CoreError> {
            let op = BranchCleanupCandidatesOp {
                git_client: self.user.clone(),
                trunk_branch: "main".to_string(),
                result: Arc::new(parking_lot::Mutex::new(None)),
            };
            op.execute().await?;
            let candidates = op.result.lock().take().expect("candidates");
            Ok(candidates)
        }

        async fn cleanup_targets(
            &self,
            targets: Vec<BranchCleanupTarget>,
        ) -> Result<Vec<BranchCleanupResult>, CoreError> {
            let op = BranchCleanupOp {
                git_client: self.user.clone(),
                trunk_branch: "main".to_string(),
                targets,
                result: Arc::new(parking_lot::Mutex::new(vec![])),
            };
            op.execute().await?;
            let results = std::mem::take(&mut *op.result.lock());
            Ok(results)
        }

        /// Cleans up `targets` as branches the user acknowledged as unsafe.
        async fn cleanup_result(
            &self,
            targets: &[(&str, &str)],
        ) -> Result<Vec<BranchCleanupResult>, CoreError> {
            self.cleanup_targets(
                targets
                    .iter()
                    .map(|(name, sha)| target(name, sha, false, false))
                    .collect(),
            )
            .await
        }

        /// Adds a linked worktree on `branch` in a fresh temp dir.
        async fn add_worktree(&self, branch: &str) -> (tempfile::TempDir, git::Git) {
            let dir = tempfile::tempdir().expect("worktree tempdir");
            let path = dir.path().join("wt");
            run_git(
                &self.user,
                &["worktree", "add", "-q", &slash_path(&path), branch],
            )
            .await;
            (dir, new_git(&path))
        }

        async fn cleanup(&self, targets: &[(&str, &str)]) -> Vec<BranchCleanupResult> {
            self.cleanup_result(targets).await.expect("cleanup")
        }
    }

    fn target(name: &str, sha: &str, safe: bool, safe_via_pr: bool) -> BranchCleanupTarget {
        BranchCleanupTarget {
            name: name.to_string(),
            sha: sha.to_string(),
            safe,
            safe_via_pr,
        }
    }

    fn names(list: &[BranchCleanupCandidate]) -> Vec<&str> {
        list.iter().map(|c| c.name.as_str()).collect()
    }

    fn find<'a>(list: &'a [BranchCleanupCandidate], name: &str) -> &'a BranchCleanupCandidate {
        list.iter()
            .find(|c| c.name == name)
            .unwrap_or_else(|| panic!("{name} not listed in {list:?}"))
    }

    fn subjects(c: &BranchCleanupCandidate) -> Vec<&str> {
        c.unmerged_commits
            .iter()
            .map(|commit| commit.subject.as_str())
            .collect()
    }

    fn assert_input<T: std::fmt::Debug>(result: Result<T, CoreError>, expected: &str) {
        match result {
            Err(CoreError::Input(e)) => assert_eq!(e.to_string(), expected),
            other => panic!("expected Input error, got {other:?}"),
        }
    }

    #[test]
    fn parses_for_each_ref_lines() {
        let out = "refs/heads/f11r-branch-a\0abc\0refs/remotes/origin/f11r-branch-a\0refs/heads/main\0Jane\x002026-01-01T00:00:00+00:00\0subject\nrefs/tags/x\0def\0\0\0\0\0\n";
        assert_eq!(
            parse_local_branches(out),
            vec![LocalBranch {
                name: "f11r-branch-a".to_string(),
                sha: "abc".to_string(),
                upstream: "refs/remotes/origin/f11r-branch-a".to_string(),
                symref: "refs/heads/main".to_string(),
                author: "Jane".to_string(),
                date: "2026-01-01T00:00:00+00:00".to_string(),
                subject: "subject".to_string(),
            }]
        );
    }

    #[test]
    fn gone_requires_a_missing_origin_upstream() {
        let origin: HashSet<String> = ["refs/remotes/origin/main".to_string()].into();
        let branch = |upstream: &str| LocalBranch {
            name: "f11r-branch-a".to_string(),
            upstream: upstream.to_string(),
            ..Default::default()
        };
        assert!(is_gone_on_origin(
            &branch("refs/remotes/origin/f11r-branch-a"),
            &origin
        ));
        assert!(!is_gone_on_origin(&branch(""), &origin));
        assert!(!is_gone_on_origin(&branch("refs/heads/main"), &origin));
        assert!(!is_gone_on_origin(
            &branch("refs/remotes/fork/f11r-branch-a"),
            &origin
        ));
        assert!(!is_gone_on_origin(
            &branch("refs/remotes/origin/main"),
            &origin
        ));
        // Upstream gone, but a branch with the local name exists on origin.
        let same_name: HashSet<String> = ["refs/remotes/origin/f11r-branch-a".to_string()].into();
        assert!(!is_gone_on_origin(
            &branch("refs/remotes/origin/other"),
            &same_name
        ));
    }

    #[test]
    fn full_sha_check() {
        assert!(is_full_sha(&"a".repeat(40)));
        assert!(is_full_sha(&"0".repeat(64)));
        let upper = "A".repeat(40);
        let not_hex = "g".repeat(40);
        let long = "a".repeat(41);
        for sha in ["", "abc", "HEAD", &upper, &not_hex, &long] {
            assert!(!is_full_sha(sha), "{sha}");
        }
    }

    #[tokio::test]
    async fn candidates_are_gone_f11r_branches_not_checked_out() {
        let fx = Fixture::new().await;
        let gone_tip = fx.gone_branch(GONE, &["g1"]).await;
        fx.pushed_branch("f11r-branch-present", &["p1"]).await;
        run_git(&fx.user, &["branch", "f11r-branch-local", "main"]).await;
        fx.gone_branch("feature-gone", &["o1"]).await;
        fx.gone_branch("f11r-branch-current", &["c1"]).await;
        fx.checkout("f11r-branch-current").await;
        let fetch_head = fx.user_dir.path().join(".git").join("FETCH_HEAD");
        let fetch_head_before = fs::read(&fetch_head).ok();

        let list = fx.candidates().await.expect("candidates");

        let names: Vec<&str> = list.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, vec![GONE]);
        let c = &list[0];
        assert_eq!(c.sha, gone_tip);
        assert_eq!(c.subject, "g1");
        assert_eq!(c.author, "User");
        assert!(!c.date.is_empty());
        // The prune fetch removed the stale tracking ref and left FETCH_HEAD alone.
        assert!(fx
            .user
            .run(
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    "refs/remotes/origin/f11r-branch-gone"
                ],
                quiet()
            )
            .await
            .is_err());
        assert_eq!(fs::read(&fetch_head).ok(), fetch_head_before);
        // Listing deletes nothing.
        assert_eq!(fx.local_tip(GONE).await, Some(gone_tip));
        assert_eq!(
            git_output(&fx.user, &["branch", "--show-current"]).await,
            "f11r-branch-current"
        );
    }

    #[tokio::test]
    async fn candidates_refuse_detached_head() {
        let fx = Fixture::new().await;
        fx.gone_branch(GONE, &["g1"]).await;
        run_git(&fx.user, &["checkout", "-q", "--detach"]).await;
        assert_input(fx.candidates().await, NO_BRANCH_CHECKED_OUT);
    }

    #[tokio::test]
    async fn candidates_fail_without_trunk_on_origin() {
        let fx = Fixture::new().await;
        fx.gone_branch(GONE, &["g1"]).await;
        let op = BranchCleanupCandidatesOp {
            git_client: fx.user.clone(),
            trunk_branch: "no-such-trunk".to_string(),
            result: Arc::new(parking_lot::Mutex::new(None)),
        };
        assert_input(op.execute().await, "origin/no-such-trunk was not found.");
    }

    #[tokio::test]
    async fn classifies_ancestor_as_merged_into_trunk() {
        let fx = Fixture::new().await;
        let merged = fx.pushed_branch(GONE, &["m1", "m2"]).await;
        run_git(&fx.user, &["merge", "-q", "--ff-only", GONE]).await;
        run_git(&fx.user, &["push", "-q", "origin", "main"]).await;
        fx.delete_on_origin(GONE).await;
        fx.gone_branch("f11r-branch-empty", &[]).await;

        let list = fx.candidates().await.expect("candidates");
        for name in [GONE, "f11r-branch-empty"] {
            let c = find(&list, name);
            assert!(c.merged_into_trunk, "{name}");
            assert!(!c.patch_equivalent, "{name}");
            assert!(c.safe, "{name}");
            assert_eq!(c.unmerged_commit_count, 0, "{name}");
        }
        assert_eq!(find(&list, GONE).sha, merged);
    }

    #[tokio::test]
    async fn classifies_cherry_picked_as_patch_equivalent() {
        let fx = Fixture::new().await;
        run_git(&fx.user, &["checkout", "-q", "-b", GONE, "main"]).await;
        let one = fx.commit("one").await;
        let two = fx.commit("two").await;
        run_git(&fx.user, &["push", "-q", "-u", "origin", GONE]).await;
        fx.cherry_pick_onto_main(&[&one, &two]).await;
        fx.delete_on_origin(GONE).await;

        let list = fx.candidates().await.expect("candidates");
        let c = find(&list, GONE);
        assert!(!c.merged_into_trunk);
        assert!(c.patch_equivalent);
        assert!(c.safe);
        assert_eq!(c.unmerged_commit_count, 0);
        assert!(c.unmerged_commits.is_empty());
    }

    #[tokio::test]
    async fn classifies_unmerged_commits_newest_first() {
        let fx = Fixture::new().await;
        run_git(&fx.user, &["checkout", "-q", "-b", GONE, "main"]).await;
        let u1 = fx.commit("u1").await;
        fx.commit("u2").await;
        let u3 = fx.commit("u3").await;
        run_git(&fx.user, &["push", "-q", "-u", "origin", GONE]).await;
        fx.cherry_pick_onto_main(&[&u1]).await;
        fx.delete_on_origin(GONE).await;

        let messages: Vec<String> = (1..=22).map(|i| format!("many{i:02}")).collect();
        let refs: Vec<&str> = messages.iter().map(String::as_str).collect();
        fx.gone_branch("f11r-branch-many", &refs).await;

        let list = fx.candidates().await.expect("candidates");
        let c = find(&list, GONE);
        assert!(!c.merged_into_trunk);
        assert!(!c.patch_equivalent);
        assert!(!c.safe);
        assert_eq!(c.unmerged_commit_count, 2);
        assert_eq!(subjects(c), vec!["u3", "u2"]);
        assert_eq!(c.unmerged_commits[0].sha, u3);

        let many = find(&list, "f11r-branch-many");
        assert_eq!(many.unmerged_commit_count, 22);
        assert_eq!(many.unmerged_commits.len(), UNMERGED_COMMIT_LIST_LIMIT);
        assert_eq!(many.unmerged_commits[0].subject, "many22");
        assert!(!many.safe);
    }

    #[tokio::test]
    async fn merge_commits_off_trunk_count_as_unmerged() {
        let fx = Fixture::new().await;
        run_git(&fx.user, &["checkout", "-q", "-b", "side", "main"]).await;
        let s1 = fx.commit("s1").await;
        run_git(&fx.user, &["checkout", "-q", "-b", GONE, "main"]).await;
        let m1 = fx.commit("m1").await;
        run_git(
            &fx.user,
            &["merge", "-q", "--no-ff", "-m", "merge side", "side"],
        )
        .await;
        run_git(&fx.user, &["push", "-q", "-u", "origin", GONE]).await;
        // Every non-merge commit has an equivalent on main; only the merge itself does not.
        fx.cherry_pick_onto_main(&[&m1, &s1]).await;
        fx.delete_on_origin(GONE).await;

        let list = fx.candidates().await.expect("candidates");
        let c = find(&list, GONE);
        assert!(!c.merged_into_trunk);
        assert!(!c.patch_equivalent);
        assert!(!c.safe);
        assert_eq!(c.unmerged_commit_count, 1);
        assert_eq!(subjects(c), vec!["merge side"]);
    }

    #[tokio::test]
    async fn trunk_merged_into_branch_hides_patch_equivalence() {
        // After the trunk is merged into the branch, `git cherry` cannot pair the branch's
        // commits with their copies on the trunk, so they stay unmerged (never unsafe-as-safe).
        let fx = Fixture::new().await;
        run_git(&fx.user, &["checkout", "-q", "-b", GONE, "main"]).await;
        let m1 = fx.commit("m1").await;
        run_git(&fx.user, &["push", "-q", "-u", "origin", GONE]).await;
        fx.cherry_pick_onto_main(&[&m1]).await;
        fx.checkout(GONE).await;
        run_git(
            &fx.user,
            &["merge", "-q", "--no-ff", "-m", "merge main", "main"],
        )
        .await;
        run_git(&fx.user, &["push", "-q", "origin", GONE]).await;
        fx.checkout("main").await;
        fx.delete_on_origin(GONE).await;

        let list = fx.candidates().await.expect("candidates");
        let c = find(&list, GONE);
        assert!(!c.safe);
        assert_eq!(c.unmerged_commit_count, 2);
        assert_eq!(subjects(c), vec!["merge main", "m1"]);
    }

    fn pr(number: i64, state: &str, oid: &str, base: &str) -> BranchPullRequest {
        BranchPullRequest {
            number,
            url: format!("https://github.com/o/r/pull/{number}"),
            state: state.to_string(),
            head_ref_oid: oid.to_string(),
            base_ref_name: base.to_string(),
            ..Default::default()
        }
    }

    struct PrCase {
        label: &'static str,
        lookup: PrLookup,
        safe: bool,
        merged_via_pr: Option<i64>,
        unknown: bool,
    }

    #[tokio::test]
    async fn pr_lookups_classify_merged_open_and_unknown() {
        let fx = Fixture::new().await;
        let tip = fx.gone_branch(GONE, &["work"]).await;
        let main_sha = fx.sha("main").await;
        // An unreferenced commit on top of the tip, as when the PR head moved past the local tip.
        let tree = format!("{tip}^{{tree}}");
        let descendant =
            git_output(&fx.user, &["commit-tree", &tree, "-p", &tip, "-m", "later"]).await;

        let list = fx.candidates().await.expect("candidates");
        let base = find(&list, GONE).clone();
        assert!(!base.safe);
        let targets = vec!["main".to_string(), "content-main".to_string()];

        let case = |label, lookup, safe, merged_via_pr, unknown| PrCase {
            label,
            lookup,
            safe,
            merged_via_pr,
            unknown,
        };
        let cases = vec![
            case(
                "tip",
                Ok(Some(pr(7, "MERGED", &tip, "main"))),
                true,
                Some(7),
                false,
            ),
            case(
                "descendant into a target",
                Ok(Some(pr(8, "MERGED", &descendant, "content-main"))),
                true,
                Some(8),
                false,
            ),
            case(
                "unrelated head",
                Ok(Some(pr(9, "MERGED", &main_sha, "main"))),
                false,
                None,
                false,
            ),
            case(
                "base not a target",
                Ok(Some(pr(10, "MERGED", &tip, "f11r-branch-other"))),
                false,
                Some(10),
                false,
            ),
            case(
                "open",
                Ok(Some(pr(11, "OPEN", &tip, "main"))),
                false,
                None,
                false,
            ),
            case("none", Ok(None), false, None, false),
            case(
                "invalid oid",
                Ok(Some(pr(12, "MERGED", "--upload-pack=x", "main"))),
                false,
                None,
                false,
            ),
            case(
                "github down",
                Err("GitHub down".to_string()),
                false,
                None,
                true,
            ),
        ];

        for c in cases {
            let mut candidates = vec![base.clone()];
            let lookups = vec![(0, c.lookup)];
            let checks = merged_pr_checks(&candidates, &lookups);
            let covered = covered_by_merged_pr(&fx.user, &checks).await;
            let error = apply_pr_lookups(&mut candidates, lookups, &covered, &targets);
            let got = &candidates[0];
            assert_eq!(got.safe, c.safe, "{}", c.label);
            assert_eq!(
                got.merged_via_pr.as_ref().map(|p| p.number),
                c.merged_via_pr,
                "{}",
                c.label
            );
            assert_eq!(got.pr_unknown, c.unknown, "{}", c.label);
            assert_eq!(error.is_some(), c.unknown, "{}", c.label);
            // The commits stay listed so the UI can show what the branch held.
            assert_eq!(got.unmerged_commit_count, 1, "{}", c.label);
        }
    }

    #[tokio::test]
    async fn cleanup_deletes_only_requested_branches() {
        let fx = Fixture::new().await;
        let a = fx.gone_branch("f11r-branch-a", &["a1"]).await;
        let b = fx.gone_branch("f11r-branch-b", &["b1"]).await;
        let c = fx.pushed_branch("f11r-branch-c", &["c1"]).await;
        let head = fx.sha("HEAD").await;

        let results = fx.cleanup(&[("f11r-branch-a", &a)]).await;

        assert_eq!(
            results,
            vec![BranchCleanupResult {
                name: "f11r-branch-a".to_string(),
                sha: a.clone(),
                deleted: true,
                error: None,
            }]
        );
        assert_eq!(fx.local_tip("f11r-branch-a").await, None);
        assert!(!fx.has_upstream_config("f11r-branch-a").await);
        assert_eq!(fx.local_tip("f11r-branch-b").await, Some(b));
        assert!(fx.has_upstream_config("f11r-branch-b").await);
        assert_eq!(fx.local_tip("f11r-branch-c").await, Some(c));
        assert_eq!(fx.sha("HEAD").await, head);
        assert_eq!(git_output(&fx.user, &["status", "--porcelain"]).await, "");
        // The deleted tip is still reachable by sha.
        assert_eq!(fx.sha(&a).await, a);
    }

    #[tokio::test]
    async fn cleanup_refuses_moved_tip() {
        let fx = Fixture::new().await;
        let old = fx.gone_branch(GONE, &["g1"]).await;
        fx.checkout(GONE).await;
        let moved = fx.commit("g2").await;
        fx.checkout("main").await;

        let results = fx.cleanup(&[(GONE, &old)]).await;

        assert!(!results[0].deleted);
        let error = results[0].error.as_deref().unwrap_or_default();
        assert!(error.contains("moved"), "{error}");
        assert_eq!(fx.local_tip(GONE).await, Some(moved));
        assert!(fx.has_upstream_config(GONE).await);
    }

    #[tokio::test]
    async fn delete_is_compare_and_swap() {
        let fx = Fixture::new().await;
        let tip = fx.gone_branch(GONE, &["g1"]).await;
        let other = fx.sha("main").await;

        assert!(delete_local_branch(&fx.user, GONE, &other).await.is_err());
        assert_eq!(fx.local_tip(GONE).await, Some(tip.clone()));
        assert!(fx.has_upstream_config(GONE).await);

        assert!(delete_local_branch(&fx.user, GONE, &tip).await.is_ok());
        assert_eq!(fx.local_tip(GONE).await, None);
    }

    #[tokio::test]
    async fn cleanup_refuses_everything_but_gone_f11r_branches() {
        let fx = Fixture::new().await;
        let a = fx.gone_branch("f11r-branch-a", &["a1"]).await;
        let present = fx.pushed_branch("f11r-branch-present", &["p1"]).await;
        let other = fx.gone_branch("feature-gone", &["o1"]).await;
        run_git(&fx.user, &["branch", "f11r-branch-local", "main"]).await;
        let local = fx.sha("f11r-branch-local").await;
        let current = fx.gone_branch("f11r-branch-current", &["c1"]).await;
        fx.checkout("f11r-branch-current").await;

        let results = fx
            .cleanup(&[
                ("f11r-branch-present", &present),
                ("feature-gone", &other),
                ("f11r-branch-local", &local),
                ("f11r-branch-current", &current),
                ("f11r-branch-x", "HEAD"),
                ("f11r-branch-missing", &a),
                ("f11r-branch-a", &a),
                ("f11r-branch-a", &a),
            ])
            .await;

        let outcome: Vec<(&str, bool, &str)> = results
            .iter()
            .map(|r| {
                (
                    r.name.as_str(),
                    r.deleted,
                    r.error.as_deref().unwrap_or_default(),
                )
            })
            .collect();
        assert_eq!(
            outcome,
            vec![
                (
                    "f11r-branch-present",
                    false,
                    "The branch still exists on origin."
                ),
                (
                    "feature-gone",
                    false,
                    "Only local f11r-branch-* branches can be cleaned up."
                ),
                (
                    "f11r-branch-local",
                    false,
                    "The branch does not track a branch on origin."
                ),
                ("f11r-branch-current", false, "The branch is checked out."),
                ("f11r-branch-x", false, "The branch tip sha is invalid."),
                (
                    "f11r-branch-missing",
                    false,
                    "The branch no longer exists locally."
                ),
                ("f11r-branch-a", true, ""),
                (
                    "f11r-branch-a",
                    false,
                    "The branch was listed more than once."
                ),
            ]
        );
        assert_eq!(fx.local_tip("f11r-branch-present").await, Some(present));
        assert_eq!(fx.local_tip("feature-gone").await, Some(other));
        assert_eq!(fx.local_tip("f11r-branch-local").await, Some(local));
        assert_eq!(fx.local_tip("f11r-branch-current").await, Some(current));
        assert_eq!(fx.local_tip("f11r-branch-a").await, None);
    }

    #[tokio::test]
    async fn cleanup_refuses_detached_head() {
        let fx = Fixture::new().await;
        let tip = fx.gone_branch(GONE, &["g1"]).await;
        run_git(&fx.user, &["checkout", "-q", "--detach"]).await;
        assert_input(
            fx.cleanup_result(&[(GONE, &tip)]).await,
            NO_BRANCH_CHECKED_OUT,
        );
        assert_eq!(fx.local_tip(GONE).await, Some(tip));
    }

    #[tokio::test]
    async fn branches_checked_out_in_linked_worktrees_are_excluded() {
        let fx = Fixture::new().await;
        let tip = fx.gone_branch(GONE, &["g1"]).await;
        let (_dir, _wt) = fx.add_worktree(GONE).await;

        assert!(fx.candidates().await.expect("candidates").is_empty());
        let results = fx.cleanup(&[(GONE, &tip)]).await;
        assert_eq!(
            results[0].error.as_deref(),
            Some("The branch is checked out.")
        );
        assert_eq!(fx.local_tip(GONE).await, Some(tip));
    }

    #[tokio::test]
    async fn branches_rebasing_in_linked_worktrees_are_excluded() {
        let fx = Fixture::new().await;
        let tip = fx.gone_branch(GONE, &["g1", "g2"]).await;
        let other = fx.gone_branch("f11r-branch-other", &["o1"]).await;
        let (_dir, wt) = fx.add_worktree(GONE).await;
        // An interactive rebase paused by a failing exec leaves the worktree detached.
        let paused = wt
            .run(
                &[
                    "-c",
                    "sequence.editor=true",
                    "rebase",
                    "-i",
                    "--no-ff",
                    "-x",
                    "false",
                    "HEAD~1",
                ],
                quiet(),
            )
            .await;
        assert!(paused.is_err(), "the rebase should pause");
        let porcelain = git_output(&fx.user, &["worktree", "list", "--porcelain"]).await;
        assert!(!porcelain.contains(&format!("branch refs/heads/{GONE}")));
        assert!(porcelain.contains("detached"), "{porcelain}");

        let list = fx.candidates().await.expect("candidates");
        assert_eq!(names(&list), vec!["f11r-branch-other"]);
        let results = fx
            .cleanup(&[(GONE, &tip), ("f11r-branch-other", &other)])
            .await;
        assert_eq!(
            results[0].error.as_deref(),
            Some("The branch is checked out.")
        );
        assert!(results[1].deleted);
        assert_eq!(fx.local_tip(GONE).await, Some(tip));
    }

    #[tokio::test]
    async fn branches_bisected_in_linked_worktrees_are_excluded() {
        let fx = Fixture::new().await;
        let tip = fx.gone_branch(GONE, &["b1", "b2", "b3"]).await;
        let (_dir, wt) = fx.add_worktree(GONE).await;
        run_git(&wt, &["bisect", "start", GONE, "main"]).await;
        let porcelain = git_output(&fx.user, &["worktree", "list", "--porcelain"]).await;
        assert!(!porcelain.contains(&format!("branch refs/heads/{GONE}")));

        assert!(fx.candidates().await.expect("candidates").is_empty());
        let results = fx.cleanup(&[(GONE, &tip)]).await;
        assert_eq!(
            results[0].error.as_deref(),
            Some("The branch is checked out.")
        );
        assert_eq!(fx.local_tip(GONE).await, Some(tip));
    }

    #[test]
    fn in_progress_branches_reads_rebase_and_bisect_state() {
        let dir = tempfile::tempdir().expect("gitdir");
        assert!(in_progress_branches(dir.path()).expect("empty").is_empty());
        fs::create_dir_all(dir.path().join("rebase-merge")).unwrap();
        fs::write(
            dir.path().join("rebase-merge/head-name"),
            "refs/heads/f11r-branch-a\n",
        )
        .unwrap();
        fs::create_dir_all(dir.path().join("rebase-apply")).unwrap();
        fs::write(dir.path().join("rebase-apply/head-name"), "detached HEAD\n").unwrap();
        fs::write(dir.path().join("BISECT_START"), "f11r-branch-b\n").unwrap();
        assert_eq!(
            in_progress_branches(dir.path()).expect("state"),
            vec!["f11r-branch-a", "f11r-branch-b"]
        );
        // State that exists but can't be read is an error, not "no state".
        fs::remove_file(dir.path().join("BISECT_START")).unwrap();
        fs::create_dir_all(dir.path().join("BISECT_START")).unwrap();
        assert!(in_progress_branches(dir.path()).is_err());
    }

    #[tokio::test]
    async fn symbolic_refs_are_never_cleaned_up() {
        let fx = Fixture::new().await;
        let main = fx.sha("main").await;
        let sym = "f11r-branch-sym";
        let symref = format!("refs/heads/{sym}");
        run_git(&fx.user, &["symbolic-ref", &symref, "refs/heads/main"]).await;
        // Configured like a pushed branch whose origin branch is gone.
        run_git(
            &fx.user,
            &["config", &format!("branch.{sym}.remote"), "origin"],
        )
        .await;
        run_git(
            &fx.user,
            &["config", &format!("branch.{sym}.merge"), &symref],
        )
        .await;

        assert!(fx.candidates().await.expect("candidates").is_empty());
        let results = fx.cleanup(&[(sym, &main)]).await;
        assert_eq!(
            results[0].error.as_deref(),
            Some("The branch is a symbolic ref.")
        );
        assert_eq!(fx.local_tip("main").await, Some(main.clone()));

        // Deleting the symref itself never deletes its target.
        assert!(delete_local_branch(&fx.user, sym, &main).await.is_ok());
        assert_eq!(fx.local_tip(sym).await, None);
        assert_eq!(fx.local_tip("main").await, Some(main));
    }

    #[tokio::test]
    async fn cleanup_rechecks_branches_sent_as_merged() {
        let fx = Fixture::new().await;
        let seed = fx.sha("main").await;
        let names = [
            "f11r-branch-a",
            "f11r-branch-b",
            "f11r-branch-c",
            "f11r-branch-d",
        ];
        let mut tips = vec![];
        for name in names {
            tips.push(fx.pushed_branch(name, &[name]).await);
        }
        for name in names {
            run_git(&fx.user, &["merge", "-q", "--no-edit", name]).await;
            fx.delete_on_origin(name).await;
        }
        run_git(&fx.user, &["push", "-q", "origin", "main"]).await;
        let list = fx.candidates().await.expect("candidates");
        assert_eq!(list.len(), 4);
        assert!(list.iter().all(|c| c.safe), "{list:?}");
        // origin/main is rewritten, so none of them is merged any more.
        run_git(&fx.user, &["reset", "-q", "--hard", &seed]).await;
        run_git(&fx.user, &["push", "-q", "--force", "origin", "main"]).await;

        let results = fx
            .cleanup_targets(vec![
                target(names[0], &tips[0], true, false),
                target(names[1], &tips[1], true, true),
                target(names[2], &tips[2], false, false),
                target(names[3], &tips[3], true, false),
            ])
            .await
            .expect("cleanup");

        let outcome: Vec<(&str, bool)> = results
            .iter()
            .map(|r| (r.name.as_str(), r.deleted))
            .collect();
        assert_eq!(
            outcome,
            vec![
                (names[0], false),
                (names[1], true),
                (names[2], true),
                (names[3], false),
            ]
        );
        assert_eq!(
            results[0].error.as_deref(),
            Some("f11r-branch-a is no longer merged into main; review it again.")
        );
        assert_eq!(fx.local_tip(names[0]).await, Some(tips[0].clone()));
        assert_eq!(fx.local_tip(names[1]).await, None);
        assert_eq!(fx.local_tip(names[2]).await, None);
        assert_eq!(fx.local_tip(names[3]).await, Some(tips[3].clone()));
    }

    #[tokio::test]
    async fn cleanup_deletes_branches_still_merged() {
        let fx = Fixture::new().await;
        let tip = fx.pushed_branch(GONE, &["m1"]).await;
        run_git(&fx.user, &["merge", "-q", "--ff-only", GONE]).await;
        run_git(&fx.user, &["push", "-q", "origin", "main"]).await;
        fx.delete_on_origin(GONE).await;

        let results = fx
            .cleanup_targets(vec![target(GONE, &tip, true, false)])
            .await
            .expect("cleanup");
        assert!(results[0].deleted, "{results:?}");
        assert_eq!(fx.local_tip(GONE).await, None);
    }

    #[tokio::test]
    async fn failed_prune_fetch_refuses_list_and_delete() {
        let fx = Fixture::new().await;
        let tip = fx.gone_branch(GONE, &["g1"]).await;
        let missing = tempfile::tempdir().expect("missing origin");
        let unreachable = slash_path(&missing.path().join("no-such-origin"));
        run_git(&fx.user, &["remote", "set-url", "origin", &unreachable]).await;

        assert!(fx.candidates().await.is_err());
        assert!(fx.cleanup_result(&[(GONE, &tip)]).await.is_err());
        assert!(fx
            .cleanup_targets(vec![target(GONE, &tip, true, false)])
            .await
            .is_err());
        assert_eq!(fx.local_tip(GONE).await, Some(tip));
        assert!(fx.has_upstream_config(GONE).await);
    }
}
