use std::fmt::Display;
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::anyhow;
use axum::extract::State;
use axum::{async_trait, Json};
use octocrab::models::pulls::{MergeableState, PullRequest};
use octocrab::Octocrab;
use tracing::{info, instrument, warn};

use super::analyze::{collect_branch_facts, merged_pr_covers_tip, BranchFacts};
use super::{
    branch_management_trunk, guard_state, run_guarded_sequence, trunk_uses_merge_queue,
    GITHUB_PAT_MISSING,
};
use crate::engine::EngineProvider;
use crate::repo::operations::gh::submit::create_pull_request;
use crate::repo::operations::validate::validate_repo_state;
use crate::repo::RepoStatusRef;
use crate::state::{AppState, Notification};
use ethos_core::clients::{git, github};
use ethos_core::operations::parse_log_output;
use ethos_core::types::commits::Commit;
use ethos_core::types::config::{AppConfigRef, RedactedString, RepoConfig, RepoConfigRef};
use ethos_core::types::errors::CoreError;
use ethos_core::types::github::TokenNotFoundError;
use ethos_core::types::repo::{
    BranchLocalSync, BranchPullRequest, BranchPullRequestResult, BranchUpstream,
    CreateBranchPullRequestRequest, RepoStatus,
};
use ethos_core::worker::Task;

const BODY_COMMIT_LIMIT: usize = 50;
const BODY_SUBJECT_MAX_CHARS: usize = 120;
const TITLE_MAX_CHARS: usize = 256;
const MERGEABLE_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const MERGEABLE_POLL_INTERVAL: Duration = Duration::from_secs(2);
const GITHUB_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// The first failed PR gate, in the order the page lists its blockers. Behind trunk is not a gate.
pub(crate) fn pr_gate_error(facts: &BranchFacts, trunk: &str, branch: &str) -> Option<String> {
    if !facts.git.supported {
        let (major, minor) = git::MIN_BRANCH_GIT;
        return Some(format!(
            "Update git to use Branch Management (found {}, need {major}.{minor} or newer).",
            facts.git.version
        ));
    }
    match facts.upstream {
        BranchUpstream::Gone => {
            return Some(format!(
                "{branch} no longer exists on origin; it was merged or deleted."
            ))
        }
        BranchUpstream::NotPushed => {
            return Some(format!(
                "{branch} has not been pushed to origin. Push it first."
            ))
        }
        BranchUpstream::Present => {}
    }
    match facts.local_sync {
        BranchLocalSync::Ahead => {
            return Some(format!(
                "{} local commit(s) are not pushed. Submit to the branch first.",
                facts.local_ahead
            ))
        }
        BranchLocalSync::Behind => {
            return Some("Your local branch is behind origin. Sync first.".to_string())
        }
        BranchLocalSync::Diverged => {
            return Some("Your local branch has diverged from origin. Sync first.".to_string())
        }
        BranchLocalSync::InSync | BranchLocalSync::Unknown => {}
    }
    if facts.commits_ahead == 0 {
        return Some(format!("{branch} has no commits ahead of {trunk}."));
    }
    if !facts.predicted_conflicts.is_empty() {
        return Some(format!(
            "{branch} is predicted to conflict with {trunk} in {} file(s). Rebase onto {trunk} first.",
            facts.predicted_conflicts.len()
        ));
    }
    None
}

/// `## Commits` with one line per commit (at most 50), then a "Created by Friendshipper" footer.
pub(crate) fn build_pr_body(commits: &[Commit], total: u32) -> String {
    let shown = commits.len().min(BODY_COMMIT_LIMIT);
    let mut body = String::from("## Commits\n\n");
    for commit in &commits[..shown] {
        let sha: String = commit.sha.chars().take(8).collect();
        let subject: String = commit
            .message
            .as_deref()
            .unwrap_or_default()
            .trim_start()
            .lines()
            .next()
            .unwrap_or_default()
            .trim()
            .chars()
            .take(BODY_SUBJECT_MAX_CHARS)
            .collect();
        body.push_str(format!("- `{sha}` {subject}").trim_end());
        body.push('\n');
    }
    let total = total as usize;
    if total > shown {
        body.push_str(&format!("- ...and {} more\n", total - shown));
    }
    body.push_str("\nCreated by Friendshipper");
    body
}

/// The trimmed title, or an Input error when it is empty or longer than 256 characters.
pub(crate) fn validate_pr_title(title: &str) -> Result<String, CoreError> {
    let title = title.trim();
    if title.is_empty() {
        return Err(CoreError::Input(anyhow!("Pull request title is required.")));
    }
    if title.chars().count() > TITLE_MAX_CHARS {
        return Err(CoreError::Input(anyhow!(
            "Pull request title must be 256 characters or fewer."
        )));
    }
    Ok(title.to_string())
}

pub(crate) fn queue_dirty_message(number: i64, trunk: &str) -> String {
    format!(
        "GitHub reports PR #{number} cannot be merged cleanly into {trunk}. It was not added to the merge queue."
    )
}

pub(crate) fn queue_poll_error_message(number: i64, error: impl Display) -> String {
    format!(
        "Couldn't check whether PR #{number} can merge: {error}. Use Add to merge queue to retry."
    )
}

pub(crate) fn queue_timeout_message(number: i64) -> String {
    format!(
        "PR #{number} is still not mergeable after 30 minutes (required checks may be failing). It was not added to the merge queue; use Add to merge queue once the checks pass."
    )
}

pub(crate) fn no_open_pr_message(branch: &str) -> String {
    format!("{branch} has no open pull request. Create one first.")
}

pub(crate) fn queue_lookup_message(number: i64, error: impl Display) -> String {
    format!("Could not look up PR #{number} to add it to the merge queue: {error}")
}

pub(crate) fn queue_enqueue_failed_message(number: i64, error: impl Display) -> String {
    format!("PR #{number} is open but could not be added to the merge queue: {error}")
}

pub(crate) fn queue_success_message(number: i64) -> String {
    format!("PR #{number} was added to the merge queue.")
}

/// Where the PR goes, read from the repo status and config.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PrTarget {
    pub owner: String,
    pub repo: String,
    pub branch: String,
    pub trunk: String,
}

pub(crate) fn pr_target(
    status: &RepoStatus,
    repo_config: &RepoConfig,
) -> Result<PrTarget, CoreError> {
    if status.repo_owner.is_empty() || status.repo_name.is_empty() {
        return Err(CoreError::Internal(anyhow!(
            "Repository information not yet available - please wait for repo status to initialize"
        )));
    }

    let branch = status.branch.clone();
    let trunk = branch_management_trunk(&branch, repo_config)?;
    if !trunk_uses_merge_queue(repo_config, &trunk) {
        return Err(CoreError::Input(anyhow!(
            "Branch PRs require the merge queue, which is disabled for {trunk} in this repo's config."
        )));
    }

    Ok(PrTarget {
        owner: status.repo_owner.clone(),
        repo: status.repo_name.clone(),
        branch,
        trunk,
    })
}

/// What the caller asked for. Only `Enqueue` adds a PR to the merge queue.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PrRequest {
    /// Create a PR into the trunk, or return the branch's open one.
    Create { title: String },
    /// Add the branch's open PR to the merge queue once GitHub reports it mergeable.
    Enqueue,
}

/// What the branch's existing pull request leaves to do.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PrPlan {
    /// Open and already in the merge queue: nothing to do.
    AlreadyQueued(BranchPullRequest),
    /// Open but not queued.
    Existing(BranchPullRequest),
    /// No PR that describes the branch tip: create one once the gates pass.
    Create,
}

/// Decides from the lookup result alone; a merged PR that covers the branch tip is refused.
pub(crate) async fn plan_from_lookup(
    git: &git::Git,
    branch: &str,
    found: Option<BranchPullRequest>,
) -> Result<PrPlan, CoreError> {
    let Some(pr) = found else {
        return Ok(PrPlan::Create);
    };
    match pr.state.as_str() {
        "OPEN" if pr.merge_queue_entry.is_some() => Ok(PrPlan::AlreadyQueued(pr)),
        "OPEN" => Ok(PrPlan::Existing(pr)),
        "MERGED" => {
            if merged_pr_covers_tip(git, branch, &pr.head_ref_oid).await {
                Err(CoreError::Input(anyhow!(
                    "Pull request #{} for {branch} was already merged.",
                    pr.number
                )))
            } else {
                Ok(PrPlan::Create)
            }
        }
        state => Err(CoreError::Internal(anyhow!(
            "Pull request #{} for {branch} has unexpected state {state}.",
            pr.number
        ))),
    }
}

/// What to do once every gate has passed.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PrAction {
    /// Open and already in the merge queue: nothing to send.
    AlreadyQueued(BranchPullRequest),
    Create {
        title: String,
        body: String,
    },
    /// A create request found an open PR: return it without queueing.
    Reuse(BranchPullRequest),
    Enqueue(BranchPullRequest),
}

/// Fetches, plans from the lookup result `found`, runs the gates, and for a new PR validates the
/// title and builds the body. Writes nothing to GitHub or to the local branch, index or
/// working tree.
pub(crate) async fn gate_pull_request(
    git: &git::Git,
    trunk: &str,
    branch: &str,
    request: &PrRequest,
    found: Option<BranchPullRequest>,
) -> Result<PrAction, CoreError> {
    let facts = collect_branch_facts(git, trunk, branch, git::Opts::default()).await?;
    // After the fetch, so the merged check reads the current origin/<branch>.
    let existing = match plan_from_lookup(git, branch, found).await? {
        PrPlan::AlreadyQueued(pr) => return Ok(PrAction::AlreadyQueued(pr)),
        PrPlan::Existing(pr) => Some(pr),
        PrPlan::Create => None,
    };
    if let Some(message) = pr_gate_error(&facts, trunk, branch) {
        return Err(CoreError::Input(anyhow!(message)));
    }

    let title = match (request, existing) {
        (PrRequest::Enqueue, Some(pr)) => return Ok(PrAction::Enqueue(pr)),
        (PrRequest::Enqueue, None) => {
            return Err(CoreError::Input(anyhow!(no_open_pr_message(branch))))
        }
        (PrRequest::Create { .. }, Some(pr)) => return Ok(PrAction::Reuse(pr)),
        (PrRequest::Create { title }, None) => title,
    };

    let title = validate_pr_title(title)?;
    let range = format!("refs/remotes/origin/{trunk}..refs/remotes/origin/{branch}");
    let log = git.log(BODY_COMMIT_LIMIT, &range).await?;
    let body = build_pr_body(&parse_log_output(&log), facts.commits_ahead);
    Ok(PrAction::Create { title, body })
}

/// After a failed create, an open PR found by a fresh lookup (e.g. a concurrent create) is
/// used instead; anything else fails with the create error.
pub(crate) fn existing_after_create_failure(
    found: anyhow::Result<Option<BranchPullRequest>>,
    create_error: impl Display,
) -> Result<BranchPullRequest, CoreError> {
    match found {
        Ok(Some(pr)) if pr.state == "OPEN" => Ok(pr),
        _ => Err(CoreError::Internal(anyhow!(
            "Failed to create pull request: {create_error}"
        ))),
    }
}

/// An existing PR is enqueued only when it merges this repo's branch into the trunk the gates
/// checked; a PR into another base or from a fork with the same branch name is refused.
pub(crate) fn check_existing_pr(
    number: i64,
    base: &str,
    head_repo: Option<&str>,
    target: &PrTarget,
) -> Result<(), CoreError> {
    let trunk = &target.trunk;
    if base != trunk {
        return Err(CoreError::Input(anyhow!(
            "Pull request #{number} targets {base}, not {trunk}. Retarget or close it on GitHub first."
        )));
    }
    let expected = format!("{}/{}", target.owner, target.repo);
    match head_repo {
        Some(head_repo) if head_repo.eq_ignore_ascii_case(&expected) => Ok(()),
        head_repo => Err(CoreError::Input(anyhow!(
            "Pull request #{number} comes from {}, not {expected}.",
            head_repo.unwrap_or("an unknown repository")
        ))),
    }
}

/// The result for an existing open PR that is returned as is, once it passes `check_existing_pr`.
fn existing_result(
    pr: &BranchPullRequest,
    target: &PrTarget,
) -> Result<BranchPullRequestResult, CoreError> {
    check_existing_pr(
        pr.number,
        &pr.base_ref_name,
        pr.head_repository.as_deref(),
        target,
    )?;
    Ok(BranchPullRequestResult {
        number: pr.number,
        url: pr.url.clone(),
        created: false,
        already_queued: pr.merge_queue_entry.is_some(),
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mergeability {
    Ready,
    Wait,
    Dirty,
}

/// Whether the merge queue can take a PR in `state`. The queue rebuilds the PR on the trunk,
/// so being behind is ready; blocked and unknown wait, and a missing state is ready.
pub(crate) fn queue_mergeability(state: Option<&MergeableState>) -> Mergeability {
    match state {
        Some(MergeableState::Blocked | MergeableState::Unknown) => Mergeability::Wait,
        Some(MergeableState::Dirty) => Mergeability::Dirty,
        _ => Mergeability::Ready,
    }
}

#[derive(Debug)]
pub(crate) enum MergeableWaitError {
    Dirty,
    /// GitHub could not be asked, so the PR's state is unknown.
    Check(String),
}

/// Re-reads the PR every 2 s until `queue_mergeability` is not `Wait`. It has no timeout of
/// its own.
async fn wait_until_queueable(
    octocrab: &Octocrab,
    pr: PullRequest,
    owner: &str,
    repo: &str,
) -> Result<(), MergeableWaitError> {
    let mut state = pr.mergeable_state;
    loop {
        match queue_mergeability(state.as_ref()) {
            Mergeability::Ready => return Ok(()),
            Mergeability::Dirty => return Err(MergeableWaitError::Dirty),
            Mergeability::Wait => {
                tokio::time::sleep(MERGEABLE_POLL_INTERVAL).await;
                state = octocrab
                    .pulls(owner, repo)
                    .get(pr.number)
                    .await
                    .map_err(|e| MergeableWaitError::Check(e.to_string()))?
                    .mergeable_state;
            }
        }
    }
}

/// Waits up to `timeout` for `poll`, then looks up the PR id and enqueues it. Returns the one
/// notification for the outcome; nothing is enqueued unless the poll succeeded in time.
pub(crate) async fn enqueue_when_mergeable<P, L, LF, E, EF>(
    number: i64,
    trunk: &str,
    timeout: Duration,
    poll: P,
    lookup_id: L,
    enqueue: E,
) -> Notification
where
    P: Future<Output = Result<(), MergeableWaitError>>,
    L: FnOnce() -> LF,
    LF: Future<Output = anyhow::Result<String>>,
    E: FnOnce(String) -> EF,
    EF: Future<Output = anyhow::Result<()>>,
{
    match tokio::time::timeout(timeout, poll).await {
        Err(_) => {
            let message = queue_timeout_message(number);
            warn!("{message}");
            return Notification::Error(message);
        }
        Ok(Err(MergeableWaitError::Dirty)) => {
            let message = queue_dirty_message(number, trunk);
            warn!("{message}");
            return Notification::Error(message);
        }
        Ok(Err(MergeableWaitError::Check(e))) => {
            let message = queue_poll_error_message(number, e);
            warn!("{message}");
            return Notification::Error(message);
        }
        Ok(Ok(())) => {}
    }

    let id = match lookup_id().await {
        Ok(id) => id,
        Err(e) => {
            let message = queue_lookup_message(number, e);
            warn!("{message}");
            return Notification::Error(message);
        }
    };

    match enqueue(id).await {
        Ok(()) => {
            let message = queue_success_message(number);
            info!("{message}");
            Notification::Success(message)
        }
        Err(e) => {
            let message = queue_enqueue_failed_message(number, e);
            warn!("{message}");
            Notification::Error(message)
        }
    }
}

/// The PAT and GraphQL client the op needs: no PAT is a 400, no client a 401.
fn require_github<C>(
    pat: Option<RedactedString>,
    client: Option<C>,
) -> Result<(String, C), CoreError> {
    let token = pat.ok_or(CoreError::Input(anyhow!(GITHUB_PAT_MISSING)))?;
    let client = client.ok_or_else(|| CoreError::Internal(anyhow!(TokenNotFoundError)))?;
    Ok((token.to_string(), client))
}

#[derive(Clone)]
pub struct BranchPullRequestOp {
    pub request: PrRequest,

    pub app_config: AppConfigRef,
    pub repo_config: RepoConfigRef,
    pub repo_status: RepoStatusRef,

    pub git_client: git::Git,
    pub token: String,
    pub github_client: github::GraphQLClient,
    pub sync_phase_tx: std::sync::mpsc::Sender<String>,
    pub notification_tx: std::sync::mpsc::Sender<Notification>,
    pub result: Arc<parking_lot::Mutex<Option<BranchPullRequestResult>>>,
}

impl BranchPullRequestOp {
    fn emit_phase(&self, phase: impl Into<String>) {
        let _ = self.sync_phase_tx.send(phase.into());
    }

    async fn get_open_pull_request(
        &self,
        octocrab: &Octocrab,
        target: &PrTarget,
        number: i64,
    ) -> Result<PullRequest, CoreError> {
        let pr = octocrab
            .pulls(target.owner.as_str(), target.repo.as_str())
            .get(number as u64)
            .await
            .map_err(|e| {
                CoreError::Internal(anyhow!("Failed to get pull request #{number}: {e}"))
            })?;
        let head_repo = pr
            .head
            .repo
            .as_ref()
            .and_then(|repo| repo.full_name.as_deref());
        check_existing_pr(number, &pr.base.ref_field, head_repo, target)?;
        Ok(pr)
    }
}

#[async_trait]
impl Task for BranchPullRequestOp {
    #[instrument(name = "BranchPullRequestOp::execute", skip(self))]
    async fn execute(&self) -> Result<(), CoreError> {
        let repo_status = self.repo_status.read().clone();
        let target = pr_target(&repo_status, &self.repo_config.read())?;
        let repo_path = PathBuf::from(self.app_config.read().repo_path.clone());
        validate_repo_state(&repo_path, &repo_status)?;
        let PrTarget {
            owner,
            repo,
            branch,
            trunk,
        } = target.clone();

        // A failed lookup never falls through to creating a PR.
        self.emit_phase("Checking pull requests");
        let found = self
            .github_client
            .get_branch_pull_request(&owner, &repo, &branch, &trunk)
            .await
            .map_err(CoreError::Internal)?;

        self.emit_phase("Checking branch");
        let action =
            gate_pull_request(&self.git_client, &trunk, &branch, &self.request, found).await?;

        let octocrab = Octocrab::builder()
            .set_connect_timeout(Some(GITHUB_REQUEST_TIMEOUT))
            .set_read_timeout(Some(GITHUB_REQUEST_TIMEOUT))
            .personal_token(self.token.clone())
            .build()?;

        let existing = match action {
            PrAction::AlreadyQueued(pr) | PrAction::Reuse(pr) => {
                *self.result.lock() = Some(existing_result(&pr, &target)?);
                return Ok(());
            }
            PrAction::Create { title, body } => {
                self.emit_phase("Creating pull request");
                let result = match create_pull_request(
                    &octocrab,
                    &owner,
                    &repo,
                    title,
                    branch.clone(),
                    trunk.clone(),
                    Some(body),
                )
                .await
                {
                    Ok(pr) => BranchPullRequestResult {
                        number: pr.number as i64,
                        url: pr
                            .html_url
                            .as_ref()
                            .map(|url| url.to_string())
                            .unwrap_or_default(),
                        created: true,
                        already_queued: false,
                    },
                    Err(e) => {
                        warn!("Failed to create pull request for {branch}: {e}");
                        let found = self
                            .github_client
                            .get_branch_pull_request(&owner, &repo, &branch, &trunk)
                            .await;
                        existing_result(&existing_after_create_failure(found, &e)?, &target)?
                    }
                };
                *self.result.lock() = Some(result);
                return Ok(());
            }
            PrAction::Enqueue(existing) => existing,
        };

        let pr = self
            .get_open_pull_request(&octocrab, &target, existing.number)
            .await?;
        let number = pr.number as i64;
        *self.result.lock() = Some(BranchPullRequestResult {
            number,
            url: existing.url,
            created: false,
            already_queued: false,
        });

        // The request returns now; mergeability and the enqueue are reported by notification.
        self.emit_phase("Adding to merge queue");
        let github_client = self.github_client.clone();
        let notification_tx = self.notification_tx.clone();
        tokio::spawn(async move {
            let notification = enqueue_when_mergeable(
                number,
                &trunk,
                MERGEABLE_TIMEOUT,
                wait_until_queueable(&octocrab, pr, &owner, &repo),
                || github_client.get_pull_request_id(owner.clone(), repo.clone(), number),
                |id| github_client.enqueue_pull_request(id),
            )
            .await;
            let _ = notification_tx.send(notification);
        });

        Ok(())
    }

    fn get_name(&self) -> String {
        String::from("BranchPullRequestOp")
    }
}

/// Creates the branch's PR, or returns its open one; never adds it to the merge queue.
#[instrument(skip(state, request))]
pub async fn branch_pull_request_handler<T>(
    State(state): State<AppState<T>>,
    Json(request): Json<CreateBranchPullRequestRequest>,
) -> Result<Json<BranchPullRequestResult>, CoreError>
where
    T: EngineProvider,
{
    run_branch_pull_request(
        &state,
        PrRequest::Create {
            title: request.title,
        },
    )
    .await
}

/// Adds the branch's open PR to the merge queue in the background.
#[instrument(skip(state))]
pub async fn branch_pull_request_enqueue_handler<T>(
    State(state): State<AppState<T>>,
) -> Result<Json<BranchPullRequestResult>, CoreError>
where
    T: EngineProvider,
{
    run_branch_pull_request(&state, PrRequest::Enqueue).await
}

async fn run_branch_pull_request<T>(
    state: &AppState<T>,
    request: PrRequest,
) -> Result<Json<BranchPullRequestResult>, CoreError>
where
    T: EngineProvider,
{
    guard_state(state).await?;

    let (token, github_client) = require_github(
        state.app_config.read().github_pat.clone(),
        state.github_client.read().clone(),
    )?;

    let result = Arc::new(parking_lot::Mutex::new(None));
    let op = BranchPullRequestOp {
        request,
        app_config: state.app_config.clone(),
        repo_config: state.repo_config.clone(),
        repo_status: state.repo_status.clone(),
        git_client: state.git(),
        token,
        github_client,
        sync_phase_tx: state.sync_phase_tx.clone(),
        notification_tx: state.notification_tx.clone(),
        result: result.clone(),
    };

    run_guarded_sequence(state, Box::new(op)).await?;

    let result = result
        .lock()
        .take()
        .ok_or_else(|| CoreError::Internal(anyhow!("Pull request operation produced no result")))?;
    Ok(Json(result))
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::fs;
    use std::path::Path;
    use std::sync::mpsc;

    use axum::http::StatusCode;
    use axum::response::IntoResponse;

    use super::*;
    use ethos_core::clients::git::GitVersionCheck;
    use ethos_core::types::config::TargetBranchConfig;
    use ethos_core::types::repo::BranchMergeQueueEntry;

    const BRANCH: &str = "f11r-branch-x";

    fn input_message<T: std::fmt::Debug>(result: Result<T, CoreError>) -> String {
        match result {
            Err(CoreError::Input(e)) => e.to_string(),
            other => panic!("expected Input error, got {other:?}"),
        }
    }

    fn internal_message<T: std::fmt::Debug>(result: Result<T, CoreError>) -> String {
        match result {
            Err(CoreError::Internal(e)) => e.to_string(),
            other => panic!("expected Internal error, got {other:?}"),
        }
    }

    // pr_gate_error

    fn ok_facts() -> BranchFacts {
        BranchFacts {
            git: GitVersionCheck {
                version: "2.47.1.windows.1".to_string(),
                supported: true,
            },
            upstream: BranchUpstream::Present,
            local_sync: BranchLocalSync::InSync,
            commits_ahead: 2,
            merge_base: Some("a".repeat(40)),
            ..Default::default()
        }
    }

    fn gate(facts: BranchFacts) -> Option<String> {
        pr_gate_error(&facts, "main", BRANCH)
    }

    #[test]
    fn gate_rows_in_order() {
        let cases: Vec<(BranchFacts, &str)> = vec![
            (
                BranchFacts {
                    git: GitVersionCheck {
                        version: "2.37.9".to_string(),
                        supported: false,
                    },
                    ..Default::default()
                },
                "Update git to use Branch Management (found 2.37.9, need 2.38 or newer).",
            ),
            (
                BranchFacts {
                    upstream: BranchUpstream::Gone,
                    local_sync: BranchLocalSync::Unknown,
                    ..ok_facts()
                },
                "f11r-branch-x no longer exists on origin; it was merged or deleted.",
            ),
            (
                BranchFacts {
                    upstream: BranchUpstream::NotPushed,
                    local_sync: BranchLocalSync::Unknown,
                    ..ok_facts()
                },
                "f11r-branch-x has not been pushed to origin. Push it first.",
            ),
            (
                BranchFacts {
                    local_sync: BranchLocalSync::Ahead,
                    local_ahead: 3,
                    ..ok_facts()
                },
                "3 local commit(s) are not pushed. Submit to the branch first.",
            ),
            (
                BranchFacts {
                    local_sync: BranchLocalSync::Behind,
                    local_behind: 1,
                    ..ok_facts()
                },
                "Your local branch is behind origin. Sync first.",
            ),
            (
                BranchFacts {
                    local_sync: BranchLocalSync::Diverged,
                    local_ahead: 1,
                    local_behind: 1,
                    ..ok_facts()
                },
                "Your local branch has diverged from origin. Sync first.",
            ),
            (
                BranchFacts {
                    commits_ahead: 0,
                    ..ok_facts()
                },
                "f11r-branch-x has no commits ahead of main.",
            ),
            (
                BranchFacts {
                    predicted_conflicts: vec!["a.txt".to_string(), "Content/b.uasset".to_string()],
                    ..ok_facts()
                },
                "f11r-branch-x is predicted to conflict with main in 2 file(s). Rebase onto main first.",
            ),
        ];
        for (facts, expected) in cases {
            assert_eq!(gate(facts.clone()).as_deref(), Some(expected), "{facts:?}");
        }
    }

    #[test]
    fn gate_passes_behind_trunk_without_conflicts() {
        assert_eq!(gate(ok_facts()), None);
        assert_eq!(
            gate(BranchFacts {
                commits_behind: 40,
                ..ok_facts()
            }),
            None
        );
    }

    #[test]
    fn gate_first_match_wins() {
        let unsupported_and_gone = BranchFacts {
            git: GitVersionCheck {
                version: "garbage".to_string(),
                supported: false,
            },
            upstream: BranchUpstream::Gone,
            ..Default::default()
        };
        assert!(gate(unsupported_and_gone)
            .unwrap()
            .starts_with("Update git to use Branch Management"));

        let ahead_and_conflicting = BranchFacts {
            local_sync: BranchLocalSync::Ahead,
            local_ahead: 1,
            commits_ahead: 0,
            predicted_conflicts: vec!["a.txt".to_string()],
            ..ok_facts()
        };
        assert_eq!(
            gate(ahead_and_conflicting).as_deref(),
            Some("1 local commit(s) are not pushed. Submit to the branch first.")
        );

        let nothing_ahead_and_conflicting = BranchFacts {
            commits_ahead: 0,
            predicted_conflicts: vec!["a.txt".to_string()],
            ..ok_facts()
        };
        assert_eq!(
            gate(nothing_ahead_and_conflicting).as_deref(),
            Some("f11r-branch-x has no commits ahead of main.")
        );
    }

    // build_pr_body

    fn commit(sha: &str, message: &str) -> Commit {
        Commit {
            sha: sha.to_string(),
            author: Some("User".to_string()),
            message: Some(message.to_string()),
            timestamp: None,
            status: None,
            merge_timestamp: None,
        }
    }

    fn numbered(n: usize) -> Vec<Commit> {
        (0..n)
            .map(|i| commit(&format!("{i:08x}"), &format!("commit {i}\n")))
            .collect()
    }

    fn commit_lines(body: &str) -> Vec<&str> {
        body.lines().filter(|l| l.starts_with("- ")).collect()
    }

    #[test]
    fn body_lists_commits() {
        let commits = vec![
            commit("aaaaaaaa", "first\n\nbody text\n"),
            commit("bbbbbbbbcccc", "  second  \n"),
            commit("cccccccc", ""),
        ];
        assert_eq!(
            build_pr_body(&commits, 3),
            "## Commits\n\n- `aaaaaaaa` first\n- `bbbbbbbb` second\n- `cccccccc`\n\nCreated by Friendshipper"
        );
    }

    #[test]
    fn body_with_exactly_fifty() {
        let body = build_pr_body(&numbered(50), 50);
        assert_eq!(commit_lines(&body).len(), 50);
        assert!(!body.contains("more"));
        assert!(body.ends_with("- `00000031` commit 49\n\nCreated by Friendshipper"));
    }

    #[test]
    fn body_over_fifty_shows_remainder() {
        let body = build_pr_body(&numbered(60), 60);
        let lines = commit_lines(&body);
        assert_eq!(lines.len(), 51);
        assert_eq!(lines[49], "- `00000031` commit 49");
        assert_eq!(lines[50], "- ...and 10 more");
    }

    #[test]
    fn body_remainder_uses_total_beyond_the_log_cap() {
        let body = build_pr_body(&numbered(50), 237);
        assert_eq!(commit_lines(&body).last(), Some(&"- ...and 187 more"));
        let body = build_pr_body(&numbered(3), 5);
        assert_eq!(commit_lines(&body).len(), 4);
        assert_eq!(commit_lines(&body).last(), Some(&"- ...and 2 more"));
    }

    #[test]
    fn body_truncates_long_subjects() {
        let message = format!("{}\nsecond line", "s".repeat(200));
        let body = build_pr_body(&[commit("aaaaaaaa", &message)], 1);
        let line = commit_lines(&body)[0];
        assert_eq!(line, format!("- `aaaaaaaa` {}", "s".repeat(120)));
        assert!(!body.contains("second line"));
    }

    #[test]
    fn body_truncates_non_ascii_on_char_boundary() {
        let subject = "\u{e9}\u{1F600}".repeat(100);
        let body = build_pr_body(&[commit("aaaaaaaa", &subject)], 1);
        let line = commit_lines(&body)[0];
        let kept = line.strip_prefix("- `aaaaaaaa` ").unwrap();
        assert_eq!(kept.chars().count(), 120);
        assert_eq!(kept, "\u{e9}\u{1F600}".repeat(60));
    }

    // validate_pr_title

    #[test]
    fn title_validation() {
        for blank in ["", "  ", "\t\n"] {
            assert_eq!(
                input_message(validate_pr_title(blank)),
                "Pull request title is required."
            );
        }
        assert_eq!(validate_pr_title("  My PR \n").unwrap(), "My PR");

        let max = "t".repeat(256);
        assert_eq!(validate_pr_title(&max).unwrap(), max);
        assert_eq!(validate_pr_title(&format!("  {max}  ")).unwrap(), max);
        assert_eq!(
            input_message(validate_pr_title(&"t".repeat(257))),
            "Pull request title must be 256 characters or fewer."
        );

        let wide = "\u{e9}".repeat(256);
        assert_eq!(validate_pr_title(&wide).unwrap(), wide);
        assert_eq!(
            input_message(validate_pr_title(&"\u{e9}".repeat(257))),
            "Pull request title must be 256 characters or fewer."
        );
    }

    // background messages

    #[test]
    fn queue_messages() {
        assert_eq!(
            queue_dirty_message(12, "main"),
            "GitHub reports PR #12 cannot be merged cleanly into main. It was not added to the merge queue."
        );
        assert_eq!(
            queue_poll_error_message(12, "GitHub is down"),
            "Couldn't check whether PR #12 can merge: GitHub is down. Use Add to merge queue to retry."
        );
        assert_eq!(
            queue_timeout_message(12),
            "PR #12 is still not mergeable after 30 minutes (required checks may be failing). It was not added to the merge queue; use Add to merge queue once the checks pass."
        );
        assert_eq!(
            queue_lookup_message(12, "Request to get PR failed"),
            "Could not look up PR #12 to add it to the merge queue: Request to get PR failed"
        );
        assert_eq!(
            queue_enqueue_failed_message(12, "Not allowed"),
            "PR #12 is open but could not be added to the merge queue: Not allowed"
        );
        assert_eq!(
            queue_success_message(12),
            "PR #12 was added to the merge queue."
        );
        assert_eq!(
            no_open_pr_message(BRANCH),
            "f11r-branch-x has no open pull request. Create one first."
        );
    }

    // enqueue_when_mergeable

    fn notification_text(notification: &Notification) -> (&'static str, &str) {
        match notification {
            Notification::Success(m) => ("success", m.as_str()),
            Notification::Error(m) => ("error", m.as_str()),
        }
    }

    #[tokio::test]
    async fn background_success_enqueues_the_looked_up_id() {
        let lookups = Cell::new(0);
        let enqueued = RefCell::new(vec![]);
        let notification = enqueue_when_mergeable(
            7,
            "main",
            Duration::from_secs(5),
            async { Ok::<(), MergeableWaitError>(()) },
            || {
                lookups.set(lookups.get() + 1);
                async { Ok("PR_node".to_string()) }
            },
            |id| {
                enqueued.borrow_mut().push(id);
                async { Ok(()) }
            },
        )
        .await;
        assert_eq!(
            notification_text(&notification),
            ("success", "PR #7 was added to the merge queue.")
        );
        assert_eq!(lookups.get(), 1);
        assert_eq!(*enqueued.borrow(), ["PR_node"]);
    }

    #[tokio::test]
    async fn background_timeout_never_enqueues() {
        let notification = enqueue_when_mergeable(
            7,
            "main",
            Duration::from_millis(20),
            std::future::pending::<Result<(), MergeableWaitError>>(),
            || async { panic!("looked up after a timeout") },
            |_| async { panic!("enqueued after a timeout") },
        )
        .await;
        assert_eq!(
            notification_text(&notification),
            ("error", queue_timeout_message(7).as_str())
        );
    }

    #[tokio::test]
    async fn background_dirty_never_enqueues() {
        let notification = enqueue_when_mergeable(
            7,
            "main",
            Duration::from_secs(5),
            async { Err(MergeableWaitError::Dirty) },
            || async { panic!("looked up after a dirty poll") },
            |_| async { panic!("enqueued after a dirty poll") },
        )
        .await;
        assert_eq!(
            notification_text(&notification),
            (
                "error",
                "GitHub reports PR #7 cannot be merged cleanly into main. It was not added to the merge queue."
            )
        );
    }

    #[tokio::test]
    async fn background_poll_error_is_not_reported_as_dirty() {
        let notification = enqueue_when_mergeable(
            7,
            "main",
            Duration::from_secs(5),
            async { Err(MergeableWaitError::Check("GitHub is down".to_string())) },
            || async { panic!("looked up after a failed poll") },
            |_| async { panic!("enqueued after a failed poll") },
        )
        .await;
        assert_eq!(
            notification_text(&notification),
            (
                "error",
                "Couldn't check whether PR #7 can merge: GitHub is down. Use Add to merge queue to retry."
            )
        );
    }

    #[test]
    fn behind_is_ready_for_the_merge_queue() {
        let cases = [
            (None, Mergeability::Ready),
            (Some(MergeableState::Clean), Mergeability::Ready),
            (Some(MergeableState::Behind), Mergeability::Ready),
            (Some(MergeableState::HasHooks), Mergeability::Ready),
            (Some(MergeableState::Unstable), Mergeability::Ready),
            (Some(MergeableState::Blocked), Mergeability::Wait),
            (Some(MergeableState::Unknown), Mergeability::Wait),
            (Some(MergeableState::Dirty), Mergeability::Dirty),
        ];
        for (state, expected) in cases {
            assert_eq!(queue_mergeability(state.as_ref()), expected, "{state:?}");
        }
    }

    #[tokio::test]
    async fn background_lookup_failure_never_enqueues() {
        let notification = enqueue_when_mergeable(
            7,
            "main",
            Duration::from_secs(5),
            async { Ok::<(), MergeableWaitError>(()) },
            || async { Err(anyhow!("Request to get PR failed")) },
            |_| async { panic!("enqueued without an id") },
        )
        .await;
        assert_eq!(
            notification_text(&notification),
            (
                "error",
                "Could not look up PR #7 to add it to the merge queue: Request to get PR failed"
            )
        );
    }

    #[tokio::test]
    async fn background_enqueue_failure_is_reported() {
        let notification = enqueue_when_mergeable(
            7,
            "main",
            Duration::from_secs(5),
            async { Ok::<(), MergeableWaitError>(()) },
            || async { Ok("PR_node".to_string()) },
            |_| async { Err(anyhow!("Error enqueuing PR: Pull request is not mergeable")) },
        )
        .await;
        assert_eq!(
            notification_text(&notification),
            (
                "error",
                "PR #7 is open but could not be added to the merge queue: Error enqueuing PR: Pull request is not mergeable"
            )
        );
    }

    // pr_target

    fn status(owner: &str, branch: &str) -> RepoStatus {
        let mut status = RepoStatus::new();
        status.repo_owner = owner.to_string();
        status.repo_name = if owner.is_empty() { "" } else { "repo" }.to_string();
        status.branch = branch.to_string();
        status
    }

    fn repo_config(trunk: &str, targets: &[(&str, bool)]) -> RepoConfig {
        RepoConfig {
            trunk_branch: trunk.to_string(),
            target_branches: targets
                .iter()
                .map(|(name, uses_merge_queue)| TargetBranchConfig {
                    name: name.to_string(),
                    uses_merge_queue: *uses_merge_queue,
                    blocked_file_globs: vec![],
                })
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn target_from_status_and_config() {
        let config = repo_config("main", &[("main", true), ("content-main", false)]);
        assert_eq!(
            pr_target(&status("BelieverCo", BRANCH), &config).unwrap(),
            PrTarget {
                owner: "BelieverCo".to_string(),
                repo: "repo".to_string(),
                branch: BRANCH.to_string(),
                trunk: "main".to_string(),
            }
        );
        // No target entry for the trunk defaults to the merge queue.
        let redirected = repo_config("f11r-branch-llm-test-pr3-base", &[("main", true)]);
        assert!(pr_target(&status("BelieverCo", BRANCH), &redirected).is_ok());
    }

    #[test]
    fn target_refusals() {
        let config = repo_config("main", &[("main", true), ("content-main", true)]);
        assert_eq!(
            internal_message(pr_target(&status("", BRANCH), &config)),
            "Repository information not yet available - please wait for repo status to initialize"
        );
        assert_eq!(
            input_message(pr_target(&status("BelieverCo", ""), &config)),
            "No branch is checked out (detached HEAD or rebase in progress)."
        );
        assert_eq!(
            input_message(pr_target(
                &status("BelieverCo", BRANCH),
                &repo_config("", &[])
            )),
            "The repo config does not define trunkBranch."
        );
        for branch in ["main", "content-main", "f11r-main-Jane-1761000000"] {
            assert_eq!(
                input_message(pr_target(&status("BelieverCo", branch), &config)),
                format!(
                    "Branch Management actions are not available on {branch}. Create or select a feature branch."
                )
            );
        }
        assert_eq!(
            input_message(pr_target(
                &status("BelieverCo", BRANCH),
                &repo_config("main", &[("main", false)])
            )),
            "Branch PRs require the merge queue, which is disabled for main in this repo's config."
        );
    }

    // create fallback, base check, handler pre-checks

    fn pr(number: i64, state: &str, queued: bool) -> BranchPullRequest {
        BranchPullRequest {
            number,
            id: format!("PR_{number}"),
            url: format!("https://github.com/o/r/pull/{number}"),
            head_ref_oid: "0".repeat(40),
            base_ref_name: "main".to_string(),
            head_repository: Some("o/r".to_string()),
            state: state.to_string(),
            mergeable: "MERGEABLE".to_string(),
            merge_queue_entry: queued.then(|| BranchMergeQueueEntry {
                state: "QUEUED".to_string(),
                position: 1,
            }),
        }
    }

    #[test]
    fn create_failure_uses_only_an_open_pr() {
        assert_eq!(
            existing_after_create_failure(Ok(Some(pr(5, "OPEN", false))), "422").unwrap(),
            pr(5, "OPEN", false)
        );
        assert_eq!(
            existing_after_create_failure(Ok(Some(pr(5, "OPEN", true))), "422").unwrap(),
            pr(5, "OPEN", true)
        );
        for found in [
            Ok(None),
            Ok(Some(pr(5, "MERGED", false))),
            Err(anyhow!("Request to get PR failed")),
        ] {
            assert_eq!(
                internal_message(existing_after_create_failure(found, "GitHub error 422")),
                "Failed to create pull request: GitHub error 422"
            );
        }
    }

    #[test]
    fn existing_pr_must_merge_this_repo_into_the_trunk() {
        let target = PrTarget {
            owner: "BelieverCo".to_string(),
            repo: "bv-unreal-sample".to_string(),
            branch: BRANCH.to_string(),
            trunk: "main".to_string(),
        };
        assert!(check_existing_pr(5, "main", Some("BelieverCo/bv-unreal-sample"), &target).is_ok());
        assert!(check_existing_pr(5, "main", Some("believerco/BV-unreal-sample"), &target).is_ok());
        assert_eq!(
            input_message(check_existing_pr(
                5,
                "content-main",
                Some("BelieverCo/bv-unreal-sample"),
                &target
            )),
            "Pull request #5 targets content-main, not main. Retarget or close it on GitHub first."
        );
        assert_eq!(
            input_message(check_existing_pr(
                5,
                "main",
                Some("someone/bv-unreal-sample"),
                &target
            )),
            "Pull request #5 comes from someone/bv-unreal-sample, not BelieverCo/bv-unreal-sample."
        );
        assert_eq!(
            input_message(check_existing_pr(5, "main", None, &target)),
            "Pull request #5 comes from an unknown repository, not BelieverCo/bv-unreal-sample."
        );
    }

    #[test]
    fn existing_pr_result_must_merge_this_repo_into_the_trunk() {
        let target = PrTarget {
            owner: "o".to_string(),
            repo: "r".to_string(),
            branch: BRANCH.to_string(),
            trunk: "main".to_string(),
        };
        let queued = pr(5, "OPEN", true);
        let result = existing_result(&queued, &target).unwrap();
        assert_eq!(
            (result.number, result.url.as_str()),
            (5, queued.url.as_str())
        );
        assert!(result.already_queued && !result.created);
        let open = existing_result(&pr(5, "OPEN", false), &target).unwrap();
        assert!(!open.already_queued && !open.created);
        let other_base = BranchPullRequest {
            base_ref_name: "release".to_string(),
            ..queued.clone()
        };
        assert_eq!(
            input_message(existing_result(&other_base, &target)),
            "Pull request #5 targets release, not main. Retarget or close it on GitHub first."
        );
        let fork = BranchPullRequest {
            head_repository: Some("fork/r".to_string()),
            ..queued
        };
        assert_eq!(
            input_message(existing_result(&fork, &target)),
            "Pull request #5 comes from fork/r, not o/r."
        );
    }

    #[test]
    fn github_prechecks() {
        let pat = || Some(RedactedString::from("token".to_string()));
        let (token, client) = require_github(pat(), Some(1)).unwrap();
        assert_eq!((token.as_str(), client), ("token", 1));

        let no_pat = require_github(None, Some(1)).unwrap_err();
        assert!(matches!(&no_pat, CoreError::Input(e) if e.to_string()
            == "GitHub PAT is not configured. Please configure it in the settings."));
        assert_eq!(no_pat.into_response().status(), StatusCode::BAD_REQUEST);
        assert!(matches!(
            require_github::<i32>(None, None),
            Err(CoreError::Input(_))
        ));

        let no_client = require_github::<i32>(pat(), None).unwrap_err();
        assert_eq!(no_client.into_response().status(), StatusCode::UNAUTHORIZED);
    }

    // Real repos: a bare origin with `main` and the pushed feature branch, a user clone on
    // the branch, and a collaborator clone. Hooks come only from a fixture dir.

    fn new_git(path: &Path) -> git::Git {
        let (tx, _rx) = mpsc::channel::<String>();
        git::Git::new(path.to_path_buf(), tx)
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

    fn write_file(dir: &Path, path: &str, contents: &[u8]) {
        let full = dir.join(path);
        fs::create_dir_all(full.parent().expect("parent dir")).unwrap();
        fs::write(full, contents).unwrap();
    }

    fn slash_path(path: &Path) -> String {
        path.to_str().expect("utf8 path").replace('\\', "/")
    }

    struct Fixture {
        _origin_dir: tempfile::TempDir,
        user_dir: tempfile::TempDir,
        collab_dir: tempfile::TempDir,
        _hooks: tempfile::TempDir,
        origin: git::Git,
        user: git::Git,
        collab: git::Git,
    }

    impl Fixture {
        async fn new() -> Self {
            let origin_dir = tempfile::tempdir().expect("origin tempdir");
            let user_dir = tempfile::tempdir().expect("user tempdir");
            let collab_dir = tempfile::tempdir().expect("collab tempdir");
            let hooks = tempfile::tempdir().expect("hooks tempdir");

            let origin = new_git(origin_dir.path());
            run_git(&origin, &["init", "-q", "--bare"]).await;
            let origin_url = slash_path(origin_dir.path());

            let user = new_git(user_dir.path());
            run_git(
                &user,
                &["clone", "-q", &origin_url, &slash_path(user_dir.path())],
            )
            .await;
            Self::configure(&user, "user@example.com", "User", hooks.path()).await;
            write_file(user_dir.path(), "seed.txt", b"one\ntwo\nthree\n");
            run_git(&user, &["checkout", "-q", "-b", "main"]).await;
            run_git(&user, &["add", "."]).await;
            run_git(&user, &["commit", "-qm", "seed"]).await;
            run_git(&user, &["push", "-q", "-u", "origin", "main"]).await;
            run_git(&user, &["checkout", "-q", "-b", BRANCH]).await;
            run_git(&user, &["push", "-q", "-u", "origin", BRANCH]).await;

            let collab = new_git(collab_dir.path());
            run_git(
                &collab,
                &["clone", "-q", &origin_url, &slash_path(collab_dir.path())],
            )
            .await;
            Self::configure(&collab, "collab@example.com", "Collaborator", hooks.path()).await;

            Self {
                _origin_dir: origin_dir,
                user_dir,
                collab_dir,
                _hooks: hooks,
                origin,
                user,
                collab,
            }
        }

        async fn configure(git: &git::Git, email: &str, name: &str, hooks: &Path) {
            run_git(git, &["config", "user.email", email]).await;
            run_git(git, &["config", "user.name", name]).await;
            run_git(git, &["config", "commit.gpgsign", "false"]).await;
            run_git(git, &["config", "core.autocrlf", "false"]).await;
            run_git(git, &["config", "core.hooksPath", &slash_path(hooks)]).await;
        }

        async fn git_supported(&self) -> bool {
            let check = self.user.check_min_version().await.expect("git version");
            if !check.supported {
                println!(
                    "skipping: git {} lacks merge-tree --write-tree",
                    check.version
                );
            }
            check.supported
        }

        async fn user_commit(&self, path: &str, contents: &[u8], message: &str) -> String {
            write_file(self.user_dir.path(), path, contents);
            run_git(&self.user, &["add", path]).await;
            run_git(&self.user, &["commit", "-qm", message]).await;
            git_output(&self.user, &["rev-parse", "HEAD"]).await
        }

        async fn user_push(&self) {
            let refspec = format!("HEAD:refs/heads/{BRANCH}");
            run_git(&self.user, &["push", "-q", "origin", &refspec]).await;
        }

        async fn collab_push(&self, branch: &str, path: &str, contents: &[u8]) {
            run_git(&self.collab, &["fetch", "-q", "origin"]).await;
            let tracking = format!("origin/{branch}");
            run_git(&self.collab, &["checkout", "-q", "-B", branch, &tracking]).await;
            write_file(self.collab_dir.path(), path, contents);
            run_git(&self.collab, &["add", path]).await;
            run_git(&self.collab, &["commit", "-qm", &format!("collab {path}")]).await;
            let refspec = format!("HEAD:refs/heads/{branch}");
            run_git(&self.collab, &["push", "-q", "origin", &refspec]).await;
        }

        async fn origin_sha(&self, branch: &str) -> String {
            let refname = format!("refs/heads/{branch}");
            git_output(&self.origin, &["rev-parse", &refname]).await
        }

        async fn facts(&self) -> BranchFacts {
            collect_branch_facts(&self.user, "main", BRANCH, git::Opts::default())
                .await
                .expect("facts")
        }

        async fn gate(
            &self,
            title: &str,
            existing: Option<BranchPullRequest>,
        ) -> Result<PrAction, CoreError> {
            let request = PrRequest::Create {
                title: title.to_string(),
            };
            gate_pull_request(&self.user, "main", BRANCH, &request, existing).await
        }

        async fn gate_enqueue(
            &self,
            existing: Option<BranchPullRequest>,
        ) -> Result<PrAction, CoreError> {
            gate_pull_request(&self.user, "main", BRANCH, &PrRequest::Enqueue, existing).await
        }

        /// HEAD, porcelain status, stash and local branches; the gates must not change them.
        async fn local_state(&self) -> Vec<String> {
            let mut state = vec![];
            for args in [
                &["rev-parse", "HEAD"][..],
                &["status", "--porcelain"],
                &["stash", "list"],
                &[
                    "for-each-ref",
                    "--format=%(refname) %(objectname)",
                    "refs/heads",
                ],
            ] {
                state.push(git_output(&self.user, args).await);
            }
            state
        }
    }

    // plan_from_lookup

    #[tokio::test]
    async fn plan_from_each_lookup_result() {
        let fx = Fixture::new().await;
        let tip = fx.origin_sha(BRANCH).await;
        let git = &fx.user;

        assert_eq!(
            plan_from_lookup(git, BRANCH, None).await.unwrap(),
            PrPlan::Create
        );
        assert_eq!(
            plan_from_lookup(git, BRANCH, Some(pr(3, "OPEN", true)))
                .await
                .unwrap(),
            PrPlan::AlreadyQueued(pr(3, "OPEN", true))
        );
        assert_eq!(
            plan_from_lookup(git, BRANCH, Some(pr(3, "OPEN", false)))
                .await
                .unwrap(),
            PrPlan::Existing(pr(3, "OPEN", false))
        );

        let merged_at_tip = BranchPullRequest {
            head_ref_oid: tip,
            ..pr(46, "MERGED", false)
        };
        assert_eq!(
            input_message(plan_from_lookup(git, BRANCH, Some(merged_at_tip)).await),
            "Pull request #46 for f11r-branch-x was already merged."
        );

        // A merged PR that does not contain the branch tip (reused name) allows a new PR.
        let stale = BranchPullRequest {
            head_ref_oid: "0123456789abcdef0123456789abcdef01234567".to_string(),
            ..pr(46, "MERGED", false)
        };
        assert_eq!(
            plan_from_lookup(git, BRANCH, Some(stale)).await.unwrap(),
            PrPlan::Create
        );

        assert!(internal_message(
            plan_from_lookup(git, BRANCH, Some(pr(3, "CLOSED", false))).await
        )
        .contains("unexpected state CLOSED"));
    }

    #[tokio::test]
    async fn plan_allows_new_pr_after_pushing_past_a_merged_head() {
        let fx = Fixture::new().await;
        let merged_oid = fx.origin_sha(BRANCH).await;
        fx.user_commit("after.txt", b"after\n", "after merge").await;
        fx.user_push().await;
        let merged = BranchPullRequest {
            head_ref_oid: merged_oid,
            ..pr(46, "MERGED", false)
        };
        assert_eq!(
            plan_from_lookup(&fx.user, BRANCH, Some(merged))
                .await
                .unwrap(),
            PrPlan::Create
        );
    }

    #[tokio::test]
    async fn gate_checks_merged_pr_against_the_fetched_tip() {
        let fx = Fixture::new().await;
        if !fx.git_supported().await {
            return;
        }
        // The PR merged the tip the user last fetched; a collaborator has pushed since.
        let merged = BranchPullRequest {
            head_ref_oid: fx.origin_sha(BRANCH).await,
            ..pr(46, "MERGED", false)
        };
        fx.collab_push(BRANCH, "after.txt", b"after\n").await;
        assert_eq!(
            input_message(fx.gate("t", Some(merged)).await),
            "Your local branch is behind origin. Sync first."
        );
    }

    #[tokio::test]
    async fn gate_returns_a_queued_pr_before_the_gates() {
        let fx = Fixture::new().await;
        if !fx.git_supported().await {
            return;
        }
        assert_eq!(
            fx.gate("", Some(pr(3, "OPEN", true))).await.unwrap(),
            PrAction::AlreadyQueued(pr(3, "OPEN", true))
        );
        assert_eq!(
            fx.gate_enqueue(Some(pr(3, "OPEN", true))).await.unwrap(),
            PrAction::AlreadyQueued(pr(3, "OPEN", true))
        );
    }

    // Gates on real facts

    #[tokio::test]
    async fn gate_fresh_branch_has_nothing_ahead() {
        let fx = Fixture::new().await;
        if !fx.git_supported().await {
            return;
        }
        let expected = "f11r-branch-x has no commits ahead of main.";
        assert_eq!(
            pr_gate_error(&fx.facts().await, "main", BRANCH).as_deref(),
            Some(expected)
        );
        // Gates run before the title check, and when an open PR is reused or enqueued.
        assert_eq!(input_message(fx.gate("", None).await), expected);
        assert_eq!(
            input_message(fx.gate("t", Some(pr(3, "OPEN", false))).await),
            expected
        );
        assert_eq!(
            input_message(fx.gate_enqueue(Some(pr(3, "OPEN", false))).await),
            expected
        );
    }

    #[tokio::test]
    async fn gate_diverged_branch() {
        let fx = Fixture::new().await;
        if !fx.git_supported().await {
            return;
        }
        fx.user_commit("pushed.txt", b"pushed\n", "pushed").await;
        fx.user_push().await;
        fx.collab_push(BRANCH, "collab.txt", b"collab\n").await;
        fx.user_commit("local.txt", b"local\n", "unpushed").await;

        let expected = "Your local branch has diverged from origin. Sync first.";
        assert_eq!(
            pr_gate_error(&fx.facts().await, "main", BRANCH).as_deref(),
            Some(expected)
        );
        assert_eq!(input_message(fx.gate("t", None).await), expected);
        assert_eq!(
            input_message(fx.gate_enqueue(Some(pr(3, "OPEN", false))).await),
            expected
        );
    }

    #[tokio::test]
    async fn gate_unpushed_local_commits() {
        let fx = Fixture::new().await;
        if !fx.git_supported().await {
            return;
        }
        fx.user_commit("pushed.txt", b"pushed\n", "pushed").await;
        fx.user_push().await;
        fx.user_commit("a.txt", b"a\n", "a").await;
        fx.user_commit("b.txt", b"b\n", "b").await;
        assert_eq!(
            input_message(fx.gate("t", None).await),
            "2 local commit(s) are not pushed. Submit to the branch first."
        );
    }

    #[tokio::test]
    async fn gate_predicted_conflict_against_fresh_trunk() {
        let fx = Fixture::new().await;
        if !fx.git_supported().await {
            return;
        }
        fx.user_commit("seed.txt", b"one\nbranch\nthree\n", "branch edit")
            .await;
        fx.user_commit("Content/x.uasset", b"\0branch\n", "asset")
            .await;
        fx.user_push().await;
        // The user never fetches main; only the gate's own fetch sees these commits.
        fx.collab_push("main", "seed.txt", b"one\nmain\nthree\n")
            .await;
        fx.collab_push("main", "Content/x.uasset", b"\0main\n")
            .await;

        let expected =
            "f11r-branch-x is predicted to conflict with main in 2 file(s). Rebase onto main first.";
        let before = fx.local_state().await;
        assert_eq!(input_message(fx.gate("t", None).await), expected);
        assert_eq!(fx.local_state().await, before);
        assert_eq!(
            pr_gate_error(&fx.facts().await, "main", BRANCH).as_deref(),
            Some(expected)
        );
    }

    #[tokio::test]
    async fn gate_passes_behind_trunk_and_builds_the_pr() {
        let fx = Fixture::new().await;
        if !fx.git_supported().await {
            return;
        }
        let first = fx
            .user_commit("a.txt", b"a\n", "Add a\n\nlonger body")
            .await;
        let second = fx.user_commit("b.txt", b"b\n", "Add b").await;
        fx.user_push().await;
        fx.collab_push("main", "main.txt", b"main\n").await;
        // Uncommitted work is not a gate.
        write_file(fx.user_dir.path(), "seed.txt", b"dirty\n");
        write_file(fx.user_dir.path(), "untracked.txt", b"new\n");

        let facts = fx.facts().await;
        assert_eq!((facts.commits_ahead, facts.commits_behind), (2, 1));
        assert_eq!(pr_gate_error(&facts, "main", BRANCH), None);

        let before = fx.local_state().await;
        let action = fx.gate("  Feature X  ", None).await.expect("gates pass");
        assert_eq!(fx.local_state().await, before);
        assert_eq!(
            action,
            PrAction::Create {
                title: "Feature X".to_string(),
                body: format!(
                    "## Commits\n\n- `{}` Add b\n- `{}` Add a\n\nCreated by Friendshipper",
                    &second[..8],
                    &first[..8]
                ),
            }
        );

        // The title is checked only after the gates, and only when creating.
        assert_eq!(
            input_message(fx.gate("   ", None).await),
            "Pull request title is required."
        );
        // Create returns an open PR without queueing it; only an enqueue request queues it.
        assert_eq!(
            fx.gate("", Some(pr(3, "OPEN", false))).await.unwrap(),
            PrAction::Reuse(pr(3, "OPEN", false))
        );
        assert_eq!(
            fx.gate_enqueue(Some(pr(3, "OPEN", false))).await.unwrap(),
            PrAction::Enqueue(pr(3, "OPEN", false))
        );
        assert_eq!(
            input_message(fx.gate_enqueue(None).await),
            "f11r-branch-x has no open pull request. Create one first."
        );
    }

    #[tokio::test]
    async fn gate_refuses_unpushed_and_gone_branches() {
        let fx = Fixture::new().await;
        if !fx.git_supported().await {
            return;
        }
        run_git(&fx.user, &["checkout", "-q", "-b", "local-only"]).await;
        fx.user_commit("l.txt", b"l\n", "local").await;
        assert_eq!(
            input_message(
                gate_pull_request(
                    &fx.user,
                    "main",
                    "local-only",
                    &PrRequest::Create {
                        title: "t".to_string()
                    },
                    None
                )
                .await
            ),
            "local-only has not been pushed to origin. Push it first."
        );

        run_git(&fx.user, &["checkout", "-q", BRANCH]).await;
        fx.user_commit("x.txt", b"x\n", "x").await;
        fx.user_push().await;
        run_git(&fx.origin, &["branch", "-D", BRANCH]).await;
        assert_eq!(
            input_message(fx.gate("t", None).await),
            "f11r-branch-x no longer exists on origin; it was merged or deleted."
        );
    }

    #[tokio::test]
    async fn gate_refuses_when_checkout_differs() {
        let fx = Fixture::new().await;
        if !fx.git_supported().await {
            return;
        }
        run_git(&fx.user, &["checkout", "-q", "main"]).await;
        assert_eq!(
            input_message(fx.gate("t", None).await),
            "The checked-out branch changed. Refresh."
        );
    }
}
