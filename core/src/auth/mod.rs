use std::time::Duration;

use serde::{Deserialize, Serialize};

pub mod sso;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Token {
    pub token: String,
    pub expires_in: Duration,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OIDCTokens {
    pub access_token: Token,
    pub id_token: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<Token>,
}
