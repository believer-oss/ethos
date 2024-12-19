use serde::{Deserialize, Serialize};

pub mod sso;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OIDCTokens {
    pub access_token: String,
    pub id_token: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
}
