use graphql_client::GraphQLQuery;

type DateTime = String;
#[allow(clippy::upper_case_acronyms)]
type URI = String;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/types/github/graphql/schema.graphql",
    query_path = "src/types/github/graphql/get_merge_queue.graphql",
    response_derives = "Clone, Debug, Serialize"
)]
pub struct GetMergeQueue;
