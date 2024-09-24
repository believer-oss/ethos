use graphql_client::GraphQLQuery;

type GitObjectID = String;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/types/github/graphql/schema.graphql",
    query_path = "src/types/github/graphql/get_commit_statuses.graphql",
    response_derives = "Clone, Debug, Serialize"
)]
pub struct GetCommitStatuses;
