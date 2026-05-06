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

#[derive(Debug, Clone)]
pub struct GraphQLClient {
    pub username: String,
    client: Client,
}

pub type CommitStatusMap = HashMap<String, String>;
pub type MergeTimestampMap = HashMap<String, String>;

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

        match post_graphql_with_retry::<GetUsername>(
            &client,
            "get_username",
            RETRY_DEFAULT_ATTEMPTS,
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
        match post_graphql::<EnqueuePullRequest, _>(
            &self.client,
            GITHUB_GRAPHQL_URL,
            enqueue_pull_request::Variables { id },
        )
        .await
        {
            Ok(_) => Ok(()),
            Err(e) => Err(anyhow!("Error enqueuing PR: {}", e)),
        }
    }

    #[instrument(skip(self))]
    pub async fn dequeue_pull_request(&self, id: String) -> Result<()> {
        match post_graphql::<DequeuePullRequest, _>(
            &self.client,
            GITHUB_GRAPHQL_URL,
            dequeue_pull_request::Variables { id },
        )
        .await
        {
            Ok(_) => Ok(()),
            Err(e) => Err(anyhow!("Error dequeueing PR: {}", e)),
        }
    }

    #[instrument(skip(self))]
    pub async fn is_branch_pr_open(
        &self,
        owner: &str,
        repo: &str,
        branch: &str,
        limit: i64,
    ) -> Result<bool> {
        let res = match post_graphql_with_retry::<IsBranchPrOpen>(
            &self.client,
            "is_branch_pr_open",
            RETRY_DEFAULT_ATTEMPTS,
            || is_branch_pr_open::Variables {
                owner: owner.to_string(),
                name: repo.to_string(),
                branch: branch.to_string(),
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
            None => return Err(missing_data_error("is_branch_pr_open", &res)),
        };

        let repo = match &data.repository {
            Some(repo) => repo,
            None => {
                error!("Failed to get valid repository: {:?}", data);
                return Err(anyhow!("Failed to get valid repository"));
            }
        };

        let nodes = match &repo.pull_requests.nodes {
            Some(pr) => pr,
            None => {
                error!("Failed to get valid PR: {:?}", data);
                return Err(anyhow!("Failed to get valid PR"));
            }
        };

        Ok(!nodes.is_empty())
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
