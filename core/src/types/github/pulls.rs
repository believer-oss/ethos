use graphql_client::GraphQLQuery;

type DateTime = String;
#[allow(clippy::upper_case_acronyms)]
type URI = String;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/types/github/graphql/schema.graphql",
    query_path = "src/types/github/graphql/get_pull_request_id.graphql",
    response_derives = "Debug"
)]

pub struct GetPullRequestId;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/types/github/graphql/schema.graphql",
    query_path = "src/types/github/graphql/get_pull_request.graphql",
    response_derives = "Clone, Debug, Serialize"
)]
pub struct GetPullRequest;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/types/github/graphql/schema.graphql",
    query_path = "src/types/github/graphql/get_pull_requests.graphql",
    response_derives = "Clone, Debug, Serialize"
)]
pub struct GetPullRequests;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/types/github/graphql/schema.graphql",
    query_path = "src/types/github/graphql/enqueue_pull_request.graphql",
    response_derives = "Debug"
)]
pub struct EnqueuePullRequest;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/types/github/graphql/schema.graphql",
    query_path = "src/types/github/graphql/is_branch_pr_open.graphql",
    response_derives = "Debug"
)]
pub struct IsBranchPrOpen;
