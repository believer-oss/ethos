use crate::types::github::commits::get_commit_merge_timestamps::GetCommitMergeTimestampsRepositoryRefTarget;
use crate::types::github::commits::get_commit_statuses::GetCommitStatusesRepositoryDefaultBranchRefTarget;
use crate::types::github::commits::{
    get_commit_merge_timestamps, get_commit_statuses, GetCommitMergeTimestamps, GetCommitStatuses,
};
use crate::types::github::merge_queue::get_merge_queue::GetMergeQueueRepositoryMergeQueue;
use crate::types::github::merge_queue::{get_merge_queue, GetMergeQueue};
use crate::types::github::pulls::get_pull_request::GetPullRequestRepositoryPullRequest;
use crate::types::github::pulls::get_pull_requests::{
    GetPullRequestsSearchEdgesNode, GetPullRequestsSearchEdgesNodeOnPullRequest,
};
use crate::types::github::pulls::{dequeue_pull_request, DequeuePullRequest};
use crate::types::github::pulls::{
    enqueue_pull_request, get_pull_request, get_pull_request_id, get_pull_requests,
    is_branch_pr_open, EnqueuePullRequest, GetPullRequest, GetPullRequestId, GetPullRequests,
    IsBranchPrOpen,
};
use crate::types::github::user::{get_username, GetUsername};
use crate::types::repo::{BranchMergeQueueEntry, BranchPullRequest};
use anyhow::{anyhow, Result};
use graphql_client::reqwest::post_graphql;
use graphql_client::{GraphQLQuery, Response};
use rand::Rng;
use reqwest::Client;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tracing::{error, instrument, warn};

pub const GITHUB_GRAPHQL_URL: &str = "https://api.github.com/graphql";

const RETRY_DEFAULT_ATTEMPTS: u32 = 3;
const RETRY_NO_RETRY_ATTEMPTS: u32 = 1;
const RETRY_BASE_DELAY_MS: u64 = 500;
const REQUEST_TIMEOUT_SECS: u64 = 30;
const SLOW_CALL_WARN_SECS: u64 = 20;

fn is_transient_transport_error(e: &reqwest::Error) -> bool {
    // graphql_client::reqwest::post_graphql does not call error_for_status, so 5xx and
    // 429 responses surface either as a successful Response with errors[] populated or,
    // when the body is HTML (Cloudflare/Varnish error pages), as a JSON decode error.
    // is_decode() therefore covers the practical 5xx/429 cases.
    e.is_timeout() || e.is_connect() || e.is_decode()
}

async fn post_graphql_with_retry<Q>(
    client: &Client,
    op: &'static str,
    max_attempts: u32,
    mut variables: impl FnMut() -> Q::Variables,
) -> Result<Response<Q::ResponseData>, reqwest::Error>
where
    Q: GraphQLQuery,
{
    let mut attempt: u32 = 0;
    loop {
        attempt += 1;
        let started = Instant::now();
        let res = post_graphql::<Q, _>(client, GITHUB_GRAPHQL_URL, variables()).await;
        let elapsed = started.elapsed();
        if elapsed >= Duration::from_secs(SLOW_CALL_WARN_SECS) {
            warn!(
                "{op} attempt {attempt} took {elapsed:?} (approaching {REQUEST_TIMEOUT_SECS}s timeout)"
            );
        }
        let retry_reason: Option<String> = match &res {
            Ok(response)
                if response.data.is_none()
                    && response.errors.as_ref().is_none_or(|e| e.is_empty()) =>
            {
                Some("empty GraphQL response (likely transient GitHub outage)".to_string())
            }
            Err(e) if is_transient_transport_error(e) => Some(e.to_string()),
            _ => None,
        };
        match retry_reason {
            Some(reason) if attempt < max_attempts => {
                let base_ms = RETRY_BASE_DELAY_MS
                    .checked_shl(attempt - 1)
                    .unwrap_or(u64::MAX);
                // ±25% jitter centered on the nominal backoff to avoid thundering herds.
                let jitter_factor: f64 = rand::thread_rng().gen_range(0.75..=1.25);
                let delay = Duration::from_millis((base_ms as f64 * jitter_factor) as u64);
                warn!(
                    "{op} attempt {attempt}/{max_attempts} failed: {reason}; retrying in {delay:?}"
                );
                tokio::time::sleep(delay).await;
            }
            _ => return res,
        }
    }
}

#[track_caller]
fn missing_data_error<T: std::fmt::Debug>(op: &str, res: &Response<T>) -> anyhow::Error {
    let caller = std::panic::Location::caller();
    let errors_msg = res
        .errors
        .as_ref()
        .filter(|errs| !errs.is_empty())
        .map(|errs| {
            errs.iter()
                .map(|e| e.to_string())
                .collect::<Vec<_>>()
                .join("; ")
        })
        .unwrap_or_else(|| "no errors returned (likely a transient GitHub outage)".to_string());
    error!(
        "GraphQL response missing data for {op} at {caller}: errors=[{errors_msg}], response={res:?}"
    );
    anyhow!("Failed to get valid response data for {op}: {errors_msg}")
}

/// Maps a GraphQL mutation response to `Ok(())` only when GitHub returned data and no errors.
pub fn mutation_result<T>(op: &str, res: Result<Response<T>, reqwest::Error>) -> Result<()> {
    let res = res.map_err(|e| anyhow!("Error {op}: {e}"))?;
    if let Some(errors) = res.errors.filter(|errs| !errs.is_empty()) {
        let msgs = errors
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
            .join("; ");
        return Err(anyhow!("Error {op}: {msgs}"));
    }
    if res.data.is_none() {
        return Err(anyhow!("Error {op}: GitHub returned an empty response"));
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub struct GraphQLClient {
    pub username: String,
    client: Client,
}

pub type CommitStatusMap = HashMap<String, String>;
pub type MergeTimestampMap = HashMap<String, String>;

/// The first OPEN pull request, else the first MERGED one, keeping the query order.
fn first_open_else_merged(prs: Vec<BranchPullRequest>) -> Option<BranchPullRequest> {
    let mut merged = None;
    for pr in prs {
        match pr.state.as_str() {
            "OPEN" => return Some(pr),
            "MERGED" if merged.is_none() => merged = Some(pr),
            _ => {}
        }
    }
    merged
}

/// Picks from the PRs that merge `head_repo` (`owner/name`) into `base` first, then from the
/// rest; within each, the first OPEN, else the first MERGED, keeping the query order.
pub fn select_branch_pull_request(
    prs: Vec<BranchPullRequest>,
    base: &str,
    head_repo: &str,
) -> Option<BranchPullRequest> {
    let (matching, other): (Vec<_>, Vec<_>) = prs.into_iter().partition(|pr| {
        pr.base_ref_name == base
            && pr
                .head_repository
                .as_deref()
                .is_some_and(|repo| repo.eq_ignore_ascii_case(head_repo))
    });
    first_open_else_merged(matching).or_else(|| first_open_else_merged(other))
}

fn branch_pull_request_from_node(
    node: is_branch_pr_open::IsBranchPrOpenRepositoryPullRequestsNodes,
) -> Option<BranchPullRequest> {
    let state = match node.state {
        is_branch_pr_open::PullRequestState::OPEN => "OPEN",
        is_branch_pr_open::PullRequestState::MERGED => "MERGED",
        _ => return None,
    };
    let mergeable = match node.mergeable {
        is_branch_pr_open::MergeableState::MERGEABLE => "MERGEABLE".to_string(),
        is_branch_pr_open::MergeableState::CONFLICTING => "CONFLICTING".to_string(),
        is_branch_pr_open::MergeableState::UNKNOWN => "UNKNOWN".to_string(),
        is_branch_pr_open::MergeableState::Other(s) => s,
    };
    let merge_queue_entry = node.merge_queue_entry.map(|e| BranchMergeQueueEntry {
        state: match e.state {
            is_branch_pr_open::MergeQueueEntryState::AWAITING_CHECKS => {
                "AWAITING_CHECKS".to_string()
            }
            is_branch_pr_open::MergeQueueEntryState::LOCKED => "LOCKED".to_string(),
            is_branch_pr_open::MergeQueueEntryState::MERGEABLE => "MERGEABLE".to_string(),
            is_branch_pr_open::MergeQueueEntryState::QUEUED => "QUEUED".to_string(),
            is_branch_pr_open::MergeQueueEntryState::UNMERGEABLE => "UNMERGEABLE".to_string(),
            is_branch_pr_open::MergeQueueEntryState::Other(s) => s,
        },
        position: e.position,
    });
    Some(BranchPullRequest {
        number: node.number,
        id: node.id,
        url: node.url,
        head_ref_oid: node.head_ref_oid,
        base_ref_name: node.base_ref_name,
        head_repository: node.head_repository.map(|repo| repo.name_with_owner),
        state: state.to_string(),
        mergeable,
        merge_queue_entry,
    })
}

fn branch_pull_request_from_data(
    data: is_branch_pr_open::ResponseData,
    base: &str,
    head_repo: &str,
) -> Result<Option<BranchPullRequest>> {
    let Some(repo) = data.repository else {
        error!("Failed to get valid repository: repository is null");
        return Err(anyhow!("Failed to get valid repository"));
    };

    let Some(nodes) = repo.pull_requests.nodes else {
        return Ok(None);
    };

    let prs = nodes
        .into_iter()
        .flatten()
        .filter_map(branch_pull_request_from_node)
        .collect();
    Ok(select_branch_pull_request(prs, base, head_repo))
}

impl GraphQLClient {
    #[instrument(skip(token))]
    pub async fn new(token: String) -> Result<Self> {
        let client = Client::builder()
            .user_agent("graphql-rust/0.10.0")
            .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
            .connect_timeout(Duration::from_secs(10))
            .default_headers(
                std::iter::once((
                    reqwest::header::AUTHORIZATION,
                    reqwest::header::HeaderValue::from_str(&format!("Bearer {token}"))?,
                ))
                .collect(),
            )
            .build()?;

        // PAT validation is interactive (user is waiting on the settings save) and a bad
        // PAT is a permanent error, so don't burn the retry budget here — fail fast.
        match post_graphql_with_retry::<GetUsername>(
            &client,
            "get_username",
            RETRY_NO_RETRY_ATTEMPTS,
            || get_username::Variables {},
        )
        .await
        {
            Ok(res) => match res.data {
                Some(data) => Ok(GraphQLClient {
                    client,
                    username: data.viewer.login,
                }),
                None => Err(anyhow!("No data found. Check your Personal Access Token!")),
            },
            Err(e) => Err(anyhow!("Error getting username: {}", e)),
        }
    }

    #[instrument(skip(self))]
    pub async fn get_pull_request_id(
        &self,
        owner: String,
        repo: String,
        number: i64,
    ) -> Result<String> {
        let res = match post_graphql_with_retry::<GetPullRequestId>(
            &self.client,
            "get_pull_request_id",
            RETRY_DEFAULT_ATTEMPTS,
            || get_pull_request_id::Variables {
                owner: owner.clone(),
                name: repo.clone(),
                number,
            },
        )
        .await
        {
            Ok(res) => res,
            Err(e) => {
                error!("Request to get PR ID failed: {}", e);
                return Err(anyhow!("Request to get PR failed"));
            }
        };

        let data = match res.data {
            Some(data) => data,
            None => return Err(missing_data_error("get_pull_request_id", &res)),
        };

        let repo = match &data.repository {
            Some(repo) => repo,
            None => {
                error!("Failed to get valid repository: {:?}", data);
                return Err(anyhow!("Failed to get valid repository"));
            }
        };

        let pr = match &repo.pull_request {
            Some(pr) => pr,
            None => {
                error!("Failed to get valid PR: {:?}", data);
                return Err(anyhow!("Failed to get valid PR"));
            }
        };

        Ok(pr.id.clone())
    }

    #[instrument(skip(self))]
    pub async fn get_pull_request(
        &self,
        owner: String,
        repo: String,
        number: i64,
    ) -> Result<GetPullRequestRepositoryPullRequest> {
        let res = match post_graphql_with_retry::<GetPullRequest>(
            &self.client,
            "get_pull_request",
            RETRY_DEFAULT_ATTEMPTS,
            || get_pull_request::Variables {
                owner: owner.clone(),
                name: repo.clone(),
                number,
            },
        )
        .await
        {
            Ok(res) => res,
            Err(e) => {
                error!("Request to get PR failed: {}", e);
                return Err(anyhow!("Request to get PR failed"));
            }
        };

        let data = match res.data {
            Some(data) => data,
            None => return Err(missing_data_error("get_pull_request", &res)),
        };

        let repo = match &data.repository {
            Some(repo) => repo,
            None => {
                error!("Failed to get valid repository: {:?}", data);
                return Err(anyhow!("Failed to get valid repository"));
            }
        };

        let pr = match &repo.pull_request {
            Some(pr) => pr,
            None => {
                error!("Failed to get valid PR: {:?}", data);
                return Err(anyhow!("Failed to get valid PR"));
            }
        };

        Ok(pr.clone())
    }

    #[instrument(skip(self))]
    pub async fn get_pull_requests(
        &self,
        owner: String,
        repo: String,
        limit: i64,
    ) -> Result<Vec<GetPullRequestsSearchEdgesNodeOnPullRequest>> {
        let query = format!("is:pr author:{} repo:{}/{}", self.username, owner, repo);
        let res = match post_graphql_with_retry::<GetPullRequests>(
            &self.client,
            "get_pull_requests",
            RETRY_DEFAULT_ATTEMPTS,
            || get_pull_requests::Variables {
                query: query.clone(),
                limit,
            },
        )
        .await
        {
            Ok(res) => res,
            Err(e) => {
                error!("Request to get PR failed: {}", e);
                return Err(anyhow!("Request to get PR failed"));
            }
        };

        let data = match res.data {
            Some(data) => data,
            None => return Err(missing_data_error("get_pull_requests", &res)),
        };

        let search_edges = match data.search.edges {
            Some(repo) => repo,
            None => {
                error!("Failed to get valid valid search edges: {:?}", data);
                return Err(anyhow!("Failed to get valid search edges"));
            }
        };

        let prs: Vec<GetPullRequestsSearchEdgesNodeOnPullRequest> = search_edges
            .into_iter()
            .flatten()
            .filter_map(|edge| edge.node)
            .filter_map(|node| match node {
                GetPullRequestsSearchEdgesNode::PullRequest(pr) => Some(pr),
                _ => None,
            })
            .collect();
        Ok(prs)
    }

    #[instrument(skip(self))]
    pub async fn enqueue_pull_request(&self, id: String) -> Result<()> {
        mutation_result(
            "enqueuing PR",
            post_graphql::<EnqueuePullRequest, _>(
                &self.client,
                GITHUB_GRAPHQL_URL,
                enqueue_pull_request::Variables { id },
            )
            .await,
        )
    }

    #[instrument(skip(self))]
    pub async fn dequeue_pull_request(&self, id: String) -> Result<()> {
        mutation_result(
            "dequeueing PR",
            post_graphql::<DequeuePullRequest, _>(
                &self.client,
                GITHUB_GRAPHQL_URL,
                dequeue_pull_request::Variables { id },
            )
            .await,
        )
    }

    /// Returns the branch's newest open pull request, else its newest merged one, preferring
    /// PRs from `owner/repo` into `base`.
    #[instrument(skip(self))]
    pub async fn get_branch_pull_request(
        &self,
        owner: &str,
        repo: &str,
        branch: &str,
        base: &str,
    ) -> Result<Option<BranchPullRequest>> {
        let res = match post_graphql_with_retry::<IsBranchPrOpen>(
            &self.client,
            "get_branch_pull_request",
            RETRY_DEFAULT_ATTEMPTS,
            || is_branch_pr_open::Variables {
                owner: owner.to_string(),
                name: repo.to_string(),
                branch: branch.to_string(),
                limit: 10,
            },
        )
        .await
        {
            Ok(res) => res,
            Err(e) => {
                error!("Request to get PR failed: {}", e);
                return Err(anyhow!("Request to get PR failed: {e}"));
            }
        };

        let data = match res.data {
            Some(data) => data,
            None => return Err(missing_data_error("get_branch_pull_request", &res)),
        };

        branch_pull_request_from_data(data, base, &format!("{owner}/{repo}"))
    }

    #[instrument(skip(self))]
    pub async fn get_merge_queue(
        &self,
        owner: &str,
        repo: &str,
    ) -> Result<GetMergeQueueRepositoryMergeQueue> {
        self.get_merge_queue_inner(owner, repo, RETRY_DEFAULT_ATTEMPTS)
            .await
    }

    /// Fail-fast variant of `get_merge_queue` used in interactive paths (e.g. submit)
    /// where waiting through the full retry budget would feel like a hang to the user.
    #[instrument(skip(self))]
    pub async fn get_merge_queue_no_retry(
        &self,
        owner: &str,
        repo: &str,
    ) -> Result<GetMergeQueueRepositoryMergeQueue> {
        self.get_merge_queue_inner(owner, repo, RETRY_NO_RETRY_ATTEMPTS)
            .await
    }

    async fn get_merge_queue_inner(
        &self,
        owner: &str,
        repo: &str,
        max_attempts: u32,
    ) -> Result<GetMergeQueueRepositoryMergeQueue> {
        let res = match post_graphql_with_retry::<GetMergeQueue>(
            &self.client,
            "get_merge_queue",
            max_attempts,
            || get_merge_queue::Variables {
                owner: owner.to_string(),
                name: repo.to_string(),
            },
        )
        .await
        {
            Ok(res) => res,
            Err(e) => {
                error!("Request to get merge_queue failed: {}", e);
                return Err(anyhow!("Request to get merge_queue failed"));
            }
        };

        let data = match res.data {
            Some(data) => data,
            None => return Err(missing_data_error("get_merge_queue", &res)),
        };

        let repo = match &data.repository {
            Some(repo) => repo,
            None => {
                error!("Failed to get valid repository: {:?}", data);
                return Err(anyhow!("Failed to get valid repository"));
            }
        };

        let merge_queue = match &repo.merge_queue {
            Some(pr) => pr,
            None => {
                error!("Failed to get valid merge_queue: {:?}", data);
                return Err(anyhow!("Failed to get valid merge_queue"));
            }
        };

        Ok(merge_queue.clone())
    }

    #[instrument(skip(self))]
    pub async fn get_commit_statuses(
        &self,
        owner: &str,
        repo: &str,
        limit: i64,
    ) -> Result<CommitStatusMap> {
        let res = match post_graphql_with_retry::<GetCommitStatuses>(
            &self.client,
            "get_commit_statuses",
            RETRY_DEFAULT_ATTEMPTS,
            || get_commit_statuses::Variables {
                owner: owner.to_string(),
                name: repo.to_string(),
                limit,
            },
        )
        .await
        {
            Ok(res) => res,
            Err(e) => {
                error!("Request to get commit statuses failed: {}", e);
                return Err(anyhow!("Request to get commit statuses failed"));
            }
        };

        let data = match res.data {
            Some(data) => data,
            None => return Err(missing_data_error("get_commit_statuses", &res)),
        };

        let repo = match &data.repository {
            Some(repo) => repo,
            None => {
                error!("Failed to get valid repository: {:?}", data);
                return Err(anyhow!("Failed to get valid repository"));
            }
        };

        let default_branch_ref = match &repo.default_branch_ref {
            Some(pr) => pr,
            None => {
                error!("Failed to get valid default_branch_ref: {:?}", data);
                return Err(anyhow!("Failed to get valid default_branch_ref"));
            }
        };

        let target = match &default_branch_ref.target {
            Some(pr) => pr,
            None => {
                error!("Failed to get valid default_branch_ref target: {:?}", data);
                return Err(anyhow!("Failed to get valid default_branch_ref target"));
            }
        };

        let commit = match target {
            GetCommitStatusesRepositoryDefaultBranchRefTarget::Commit(commit) => commit,
            _ => {
                error!(
                    "Failed to get valid default_branch_ref is not a commit: {:?}",
                    data
                );
                return Err(anyhow!("Default branch ref is not a commit"));
            }
        };

        let history_nodes = match &commit.history.nodes {
            Some(nodes) => nodes,
            None => {
                error!("Failed to get valid history nodes: {:?}", data);
                return Err(anyhow!("Failed to get valid history nodes"));
            }
        };

        let mut map = HashMap::new();
        history_nodes.iter().for_each(|node| {
            if let Some(node) = node {
                let oid = node.oid.clone();
                if let Some(status) = &node.status {
                    let short_oid = oid.chars().take(8).collect::<String>();
                    let status_str: &str = match status.state {
                        get_commit_statuses::StatusState::SUCCESS => "success",
                        get_commit_statuses::StatusState::FAILURE => "failure",
                        get_commit_statuses::StatusState::ERROR => "error",
                        get_commit_statuses::StatusState::EXPECTED => "expected",
                        get_commit_statuses::StatusState::PENDING => "pending",
                        get_commit_statuses::StatusState::Other(_) => "other",
                    };

                    map.insert(short_oid, status_str.to_string());
                }
            }
        });
        Ok(map)
    }

    #[instrument(skip(self))]
    pub async fn get_commit_merge_timestamps(
        &self,
        owner: &str,
        repo: &str,
        branch: &str,
        limit: i64,
    ) -> Result<MergeTimestampMap> {
        let res = match post_graphql_with_retry::<GetCommitMergeTimestamps>(
            &self.client,
            "get_commit_merge_timestamps",
            RETRY_DEFAULT_ATTEMPTS,
            || get_commit_merge_timestamps::Variables {
                owner: owner.to_string(),
                name: repo.to_string(),
                qualified_name: branch.to_string(),
                limit,
            },
        )
        .await
        {
            Ok(res) => res,
            Err(e) => {
                error!("Request to get commit merge timestamps failed: {}", e);
                return Err(anyhow!("Request to get commit merge timestamps failed"));
            }
        };

        let data = match res.data {
            Some(data) => data,
            None => return Err(missing_data_error("get_commit_merge_timestamps", &res)),
        };

        let repo = match &data.repository {
            Some(repo) => repo,
            None => {
                error!("Failed to get valid repository: {:?}", data);
                return Err(anyhow!("Failed to get valid repository"));
            }
        };

        let branch_ref = match &repo.ref_ {
            Some(r) => r,
            None => {
                error!("Failed to get valid ref for branch {}: {:?}", branch, data);
                return Err(anyhow!("Failed to get valid ref for branch {}", branch));
            }
        };

        let target = match &branch_ref.target {
            Some(t) => t,
            None => {
                error!("Failed to get valid ref target: {:?}", data);
                return Err(anyhow!("Failed to get valid ref target"));
            }
        };

        let commit = match target {
            GetCommitMergeTimestampsRepositoryRefTarget::Commit(commit) => commit,
            _ => {
                error!("Ref target is not a commit: {:?}", data);
                return Err(anyhow!("Ref target is not a commit"));
            }
        };

        let history_nodes = match &commit.history.nodes {
            Some(nodes) => nodes,
            None => {
                error!("Failed to get valid history nodes: {:?}", data);
                return Err(anyhow!("Failed to get valid history nodes"));
            }
        };

        let mut map = HashMap::new();
        for node in history_nodes.iter().flatten() {
            let short_oid = node.oid.chars().take(8).collect::<String>();
            if let Some(prs) = &node.associated_pull_requests {
                if let Some(pr_nodes) = &prs.nodes {
                    // Find the PR whose merge commit matches this commit
                    let matching_pr = pr_nodes.iter().flatten().find(|pr| {
                        pr.merged_at.is_some()
                            && pr
                                .merge_commit
                                .as_ref()
                                .is_some_and(|mc| mc.oid == node.oid)
                    });
                    // Fall back to any merged PR if no exact match
                    let pr = matching_pr
                        .or_else(|| pr_nodes.iter().flatten().find(|pr| pr.merged_at.is_some()));
                    if let Some(pr) = pr {
                        if let Some(merged_at) = &pr.merged_at {
                            map.insert(short_oid, merged_at.clone());
                        }
                    }
                }
            }
        }
        Ok(map)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dequeue_response(json: &str) -> Response<dequeue_pull_request::ResponseData> {
        serde_json::from_str(json).expect("parse dequeue response")
    }

    fn enqueue_response(json: &str) -> Response<enqueue_pull_request::ResponseData> {
        serde_json::from_str(json).expect("parse enqueue response")
    }

    #[test]
    fn mutation_result_errors_on_graphql_errors() {
        let res = dequeue_response(
            r#"{"data":null,"errors":[{"message":"Pull request is already merged"}]}"#,
        );
        let err = mutation_result("dequeueing PR", Ok(res))
            .expect_err("graphql errors must fail")
            .to_string();
        assert!(err.starts_with("Error dequeueing PR:"), "{err}");
        assert!(err.contains("already merged"), "{err}");
    }

    #[test]
    fn mutation_result_ok_with_data() {
        let res = dequeue_response(r#"{"data":{"dequeuePullRequest":{"clientMutationId":null}}}"#);
        assert!(mutation_result("dequeueing PR", Ok(res)).is_ok());
    }

    #[test]
    fn mutation_result_errors_on_empty_response() {
        let res = dequeue_response(r#"{"data":null}"#);
        let err = mutation_result("dequeueing PR", Ok(res))
            .expect_err("missing data must fail")
            .to_string();
        assert!(err.contains("empty response"), "{err}");
    }

    #[test]
    fn mutation_result_errors_on_enqueue_graphql_errors() {
        let res = enqueue_response(
            r#"{"data":null,"errors":[{"message":"Pull request is already merged"}]}"#,
        );
        let err = mutation_result("enqueuing PR", Ok(res))
            .expect_err("graphql errors must fail")
            .to_string();
        assert!(err.starts_with("Error enqueuing PR:"), "{err}");
        assert!(err.contains("already merged"), "{err}");
    }

    #[test]
    fn mutation_result_errors_take_precedence_over_data() {
        let res = enqueue_response(
            r#"{"data":{"enqueuePullRequest":{"clientMutationId":null}},"errors":[{"message":"x"}]}"#,
        );
        let err = mutation_result("enqueuing PR", Ok(res))
            .expect_err("errors must fail even with data")
            .to_string();
        assert!(err.contains('x'), "{err}");
    }

    #[test]
    fn mutation_result_joins_multiple_errors() {
        let res = dequeue_response(
            r#"{"data":null,"errors":[{"message":"first"},{"message":"second"}]}"#,
        );
        let err = mutation_result("dequeueing PR", Ok(res))
            .expect_err("graphql errors must fail")
            .to_string();
        assert!(err.contains("first; "), "{err}");
        assert!(err.contains("second"), "{err}");
    }

    fn pr(number: i64, state: &str) -> BranchPullRequest {
        BranchPullRequest {
            number,
            state: state.to_string(),
            ..Default::default()
        }
    }

    fn numbers(pr: Option<BranchPullRequest>) -> Option<i64> {
        pr.map(|p| p.number)
    }

    #[test]
    fn select_prefers_open_over_newer_merged() {
        let prs = vec![pr(3, "MERGED"), pr(2, "OPEN"), pr(1, "MERGED")];
        assert_eq!(
            numbers(select_branch_pull_request(prs, "main", "o/r")),
            Some(2)
        );
    }

    #[test]
    fn select_merged_only() {
        assert_eq!(
            numbers(select_branch_pull_request(
                vec![pr(5, "MERGED")],
                "main",
                "o/r"
            )),
            Some(5)
        );
    }

    #[test]
    fn select_first_of_two_merged() {
        let prs = vec![pr(7, "MERGED"), pr(4, "MERGED")];
        assert_eq!(
            numbers(select_branch_pull_request(prs, "main", "o/r")),
            Some(7)
        );
    }

    #[test]
    fn select_empty_is_none() {
        assert_eq!(select_branch_pull_request(vec![], "main", "o/r"), None);
    }

    #[test]
    fn select_first_of_two_open() {
        let prs = vec![pr(9, "OPEN"), pr(8, "OPEN")];
        assert_eq!(
            numbers(select_branch_pull_request(prs, "main", "o/r")),
            Some(9)
        );
    }

    fn pr_into(number: i64, state: &str, base: &str, head_repo: Option<&str>) -> BranchPullRequest {
        BranchPullRequest {
            base_ref_name: base.to_string(),
            head_repository: head_repo.map(str::to_string),
            ..pr(number, state)
        }
    }

    #[test]
    fn select_prefers_this_repo_into_the_trunk() {
        let prs = vec![
            pr_into(9, "OPEN", "release", Some("o/r")),
            pr_into(8, "OPEN", "main", Some("fork/r")),
            pr_into(7, "OPEN", "main", None),
            pr_into(6, "MERGED", "main", Some("O/R")),
            pr_into(5, "OPEN", "main", Some("o/r")),
        ];
        assert_eq!(
            numbers(select_branch_pull_request(prs, "main", "o/r")),
            Some(5)
        );
    }

    #[test]
    fn select_matching_merged_beats_other_open() {
        let prs = vec![
            pr_into(9, "OPEN", "release", Some("o/r")),
            pr_into(6, "MERGED", "main", Some("o/r")),
        ];
        assert_eq!(
            numbers(select_branch_pull_request(prs, "main", "o/r")),
            Some(6)
        );
    }

    #[test]
    fn select_falls_back_to_other_prs() {
        let prs = vec![
            pr_into(9, "MERGED", "main", Some("fork/r")),
            pr_into(8, "OPEN", "release", Some("o/r")),
        ];
        assert_eq!(
            numbers(select_branch_pull_request(prs, "main", "o/r")),
            Some(8)
        );
    }

    #[test]
    fn select_ignores_other_states() {
        assert_eq!(
            select_branch_pull_request(vec![pr(1, "CLOSED")], "main", "o/r"),
            None
        );
    }

    fn branch_pr_data(json: &str) -> is_branch_pr_open::ResponseData {
        serde_json::from_str(json).expect("parse IsBranchPrOpen response data")
    }

    fn node(state: &str, mergeable: &str, queue: &str) -> String {
        format!(
            r#"{{"id":"PR_kw{state}","number":12,"url":"https://github.com/o/r/pull/12","headRefOid":"0123456789abcdef0123456789abcdef01234567","baseRefName":"main","headRepository":{{"nameWithOwner":"o/r"}},"state":"{state}","mergeable":"{mergeable}","mergeQueueEntry":{queue}}}"#
        )
    }

    fn nodes_json(nodes: &str) -> String {
        format!(r#"{{"repository":{{"pullRequests":{{"nodes":{nodes}}}}}}}"#)
    }

    #[test]
    fn from_data_converts_open_queued_node() {
        let json = nodes_json(&format!(
            "[{}]",
            node("OPEN", "MERGEABLE", r#"{"state":"QUEUED","position":2}"#)
        ));
        let pr = branch_pull_request_from_data(branch_pr_data(&json), "main", "o/r")
            .expect("ok")
            .expect("some");
        assert_eq!(
            pr,
            BranchPullRequest {
                number: 12,
                id: "PR_kwOPEN".to_string(),
                url: "https://github.com/o/r/pull/12".to_string(),
                head_ref_oid: "0123456789abcdef0123456789abcdef01234567".to_string(),
                base_ref_name: "main".to_string(),
                head_repository: Some("o/r".to_string()),
                state: "OPEN".to_string(),
                mergeable: "MERGEABLE".to_string(),
                merge_queue_entry: Some(BranchMergeQueueEntry {
                    state: "QUEUED".to_string(),
                    position: 2,
                }),
            }
        );
    }

    #[test]
    fn from_data_null_queue_entry() {
        let json = nodes_json(&format!("[{}]", node("MERGED", "UNKNOWN", "null")));
        let pr = branch_pull_request_from_data(branch_pr_data(&json), "main", "o/r")
            .expect("ok")
            .expect("some");
        assert_eq!(pr.state, "MERGED");
        assert_eq!(pr.mergeable, "UNKNOWN");
        assert_eq!(pr.merge_queue_entry, None);
    }

    #[test]
    fn from_data_drops_closed() {
        let json = nodes_json(&format!("[{}]", node("CLOSED", "CONFLICTING", "null")));
        assert_eq!(
            branch_pull_request_from_data(branch_pr_data(&json), "main", "o/r").expect("ok"),
            None
        );
    }

    #[test]
    fn from_data_passes_unknown_enum_values_through() {
        let json = nodes_json(&format!(
            "[{}]",
            node(
                "OPEN",
                "SOMETHING_NEW",
                r#"{"state":"SOMETHING_ELSE","position":0}"#
            )
        ));
        let pr = branch_pull_request_from_data(branch_pr_data(&json), "main", "o/r")
            .expect("ok")
            .expect("some");
        assert_eq!(pr.mergeable, "SOMETHING_NEW");
        assert_eq!(
            pr.merge_queue_entry.map(|e| e.state),
            Some("SOMETHING_ELSE".to_string())
        );
    }

    #[test]
    fn from_data_null_nodes_is_none() {
        let json = nodes_json("null");
        assert_eq!(
            branch_pull_request_from_data(branch_pr_data(&json), "main", "o/r").expect("ok"),
            None
        );
    }

    #[test]
    fn from_data_empty_nodes_is_none() {
        let json = nodes_json("[]");
        assert_eq!(
            branch_pull_request_from_data(branch_pr_data(&json), "main", "o/r").expect("ok"),
            None
        );
    }

    #[test]
    fn from_data_null_repository_errors() {
        let err =
            branch_pull_request_from_data(branch_pr_data(r#"{"repository":null}"#), "main", "o/r")
                .expect_err("null repository must fail")
                .to_string();
        assert!(err.contains("Failed to get valid repository"), "{err}");
    }

    #[test]
    fn from_data_skips_null_nodes() {
        let json = nodes_json(&format!("[null,{}]", node("OPEN", "CONFLICTING", "null")));
        let pr = branch_pull_request_from_data(branch_pr_data(&json), "main", "o/r")
            .expect("ok")
            .expect("some");
        assert_eq!(pr.state, "OPEN");
        assert_eq!(pr.mergeable, "CONFLICTING");
    }

    #[test]
    fn from_data_open_wins_over_newer_merged() {
        let json = nodes_json(&format!(
            "[{},{}]",
            node("MERGED", "UNKNOWN", "null"),
            node("OPEN", "MERGEABLE", "null")
        ));
        let pr = branch_pull_request_from_data(branch_pr_data(&json), "main", "o/r")
            .expect("ok")
            .expect("some");
        assert_eq!(pr.state, "OPEN");
        assert_eq!(pr.id, "PR_kwOPEN");
    }

    #[test]
    fn from_data_prefers_this_repo_into_the_trunk() {
        let fork = node("OPEN", "MERGEABLE", "null").replace("o/r", "fork/r");
        let other_base = node("OPEN", "MERGEABLE", "null")
            .replace(r#""baseRefName":"main""#, r#""baseRefName":"release""#);
        let ours = node("OPEN", "MERGEABLE", "null").replace("PR_kwOPEN", "PR_ours");
        let json = nodes_json(&format!("[{fork},{other_base},{ours}]"));
        let pr = branch_pull_request_from_data(branch_pr_data(&json), "main", "o/r")
            .expect("ok")
            .expect("some");
        assert_eq!(pr.id, "PR_ours");
    }

    #[test]
    fn from_data_null_head_repository() {
        let json = nodes_json(&format!(
            "[{}]",
            node("OPEN", "MERGEABLE", "null").replace(r#"{"nameWithOwner":"o/r"}"#, "null")
        ));
        let pr = branch_pull_request_from_data(branch_pr_data(&json), "main", "o/r")
            .expect("ok")
            .expect("some");
        assert_eq!(pr.head_repository, None);
    }
}
