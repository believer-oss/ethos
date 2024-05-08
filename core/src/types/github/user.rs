use graphql_client::GraphQLQuery;

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/types/github/graphql/schema.graphql",
    query_path = "src/types/github/graphql/get_username.graphql",
    response_derives = "Clone, Debug, Serialize"
)]
pub struct GetUsername;
