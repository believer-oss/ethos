use graphql_client::GraphQLQuery;

type GitObjectID = String;
type DateTime = String;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/types/github/graphql/schema.graphql",
    query_path = "src/types/github/graphql/get_commit_statuses.graphql",
    response_derives = "Clone, Debug, Serialize"
)]
pub struct GetCommitStatuses;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/types/github/graphql/schema.graphql",
    query_path = "src/types/github/graphql/get_commit_merge_timestamps.graphql",
    response_derives = "Clone, Debug, Serialize"
)]
pub struct GetCommitMergeTimestamps;
