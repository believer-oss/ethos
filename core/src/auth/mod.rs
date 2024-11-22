use serde::{Deserialize, Serialize};

pub mod sso;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OIDCTokens {
    pub access_token: String,
    pub id_token: String,
    pub refresh_token: String,
}
