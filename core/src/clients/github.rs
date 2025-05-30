use crate::types::github::commits::get_commit_statuses::GetCommitStatusesRepositoryDefaultBranchRefTarget;
use crate::types::github::commits::{get_commit_statuses, GetCommitStatuses};
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
use reqwest::Client;
use std::collections::HashMap;
use tracing::error;
use tracing::instrument;

pub const GITHUB_GRAPHQL_URL: &str = "https://api.github.com/graphql";

#[derive(Debug, Clone)]
pub struct GraphQLClient {
    pub username: String,
    client: Client,
}

pub type CommitStatusMap = HashMap<String, String>;

impl GraphQLClient {
    #[instrument(skip(token))]
    pub async fn new(token: String) -> Result<Self> {
        let client = Client::builder()
            .user_agent("graphql-rust/0.10.0")
            .default_headers(
                std::iter::once((
                    reqwest::header::AUTHORIZATION,
                    reqwest::header::HeaderValue::from_str(&format!("Bearer {token}"))?,
                ))
                .collect(),
            )
            .build()?;

        match post_graphql::<GetUsername, _>(
            &client,
            GITHUB_GRAPHQL_URL,
            get_username::Variables {},
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
        let res = match post_graphql::<GetPullRequestId, _>(
            &self.client,
            GITHUB_GRAPHQL_URL,
            get_pull_request_id::Variables {
                owner: owner.clone().to_string(),
                name: repo.clone().to_string(),
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
            None => {
                error!("Response data was empty: {:?}", res);
                return Err(anyhow!("Failed to get valid response data"));
            }
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
        let res = match post_graphql::<GetPullRequest, _>(
            &self.client,
            GITHUB_GRAPHQL_URL,
            get_pull_request::Variables {
                owner: owner.clone().to_string(),
                name: repo.clone().to_string(),
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
            None => {
                error!("Response data was empty: {:?}", res);
                return Err(anyhow!("Failed to get valid response data"));
            }
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
        let res = match post_graphql::<GetPullRequests, _>(
            &self.client,
            GITHUB_GRAPHQL_URL,
            get_pull_requests::Variables { query, limit },
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
            None => {
                error!("Response data was empty: {:?}", res);
                return Err(anyhow!("Failed to get valid response data"));
            }
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
        let res = match post_graphql::<IsBranchPrOpen, _>(
            &self.client,
            GITHUB_GRAPHQL_URL,
            is_branch_pr_open::Variables {
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
            None => {
                error!("Response data was empty: {:?}", res);
                return Err(anyhow!("Failed to get valid response data"));
            }
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
        let res = match post_graphql::<GetMergeQueue, _>(
            &self.client,
            GITHUB_GRAPHQL_URL,
            get_merge_queue::Variables {
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
            None => {
                error!("Response data was empty: {:?}", res);
                return Err(anyhow!("Failed to get valid response data"));
            }
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
        let res = match post_graphql::<GetCommitStatuses, _>(
            &self.client,
            GITHUB_GRAPHQL_URL,
            get_commit_statuses::Variables {
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
            None => {
                error!("Response data was empty: {:?}", res);
                return Err(anyhow!("Failed to get valid response data"));
            }
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
}
