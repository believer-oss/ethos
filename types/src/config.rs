use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct OktaConfig {
    pub client_id: String,
    pub issuer: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FriendshipperConfig {
    pub artifact_bucket_name: String,
    pub promoted_artifact_bucket_name: Option<String>,
}
