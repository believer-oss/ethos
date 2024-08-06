use graphql_client::GraphQLQuery;
use serde::{Deserialize, Serialize};

#[derive(GraphQLQuery)]
#[graphql(
    schema_path = "src/types/github/graphql/schema.graphql",
    query_path = "src/types/github/graphql/get_username.graphql",
    response_derives = "Clone, Debug, Serialize"
)]
pub struct GetUsername;

#[derive(Default, Deserialize, Serialize)]
pub struct UserInfoResponse {
    pub username: String,
}
