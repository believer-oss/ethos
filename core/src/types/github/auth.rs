use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

/// Tokens minted for a user via the Friendshipper GitHub App.
/// Access tokens are short-lived (8 hours); the refresh token lasts ~6 months
/// and is used to silently mint new access tokens.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GithubTokens {
    pub access_token: String,
    pub expires_at: DateTime<Utc>,
    pub refresh_token: String,
    pub refresh_token_expires_at: DateTime<Utc>,
}

impl std::fmt::Debug for GithubTokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GithubTokens")
            .field("access_token", &"********")
            .field("expires_at", &self.expires_at)
            .field("refresh_token", &"********")
            .field("refresh_token_expires_at", &self.refresh_token_expires_at)
            .finish()
    }
}

impl GithubTokens {
    /// True once the access token is within the given margin of expiry and
    /// should be refreshed before use.
    pub fn needs_refresh(&self, margin: Duration) -> bool {
        self.expires_at - margin < Utc::now()
    }

    pub fn is_expired(&self) -> bool {
        self.expires_at < Utc::now()
    }

    pub fn can_refresh(&self) -> bool {
        self.refresh_token_expires_at > Utc::now()
    }
}

/// Request body for exchanging a GitHub OAuth authorization code for tokens.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GithubTokenExchangeRequest {
    pub code: String,
}

/// Request body for refreshing an expired access token.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GithubTokenRefreshRequest {
    pub refresh_token: String,
}

impl std::fmt::Debug for GithubTokenRefreshRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GithubTokenRefreshRequest")
            .field("refresh_token", &"********")
            .finish()
    }
}

/// Public app configuration the client needs to start the OAuth flow.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GithubAppConfig {
    pub client_id: String,
}

/// Raw response from GitHub's login/oauth/access_token endpoint.
#[derive(Clone, Deserialize)]
pub struct GithubOAuthTokenResponse {
    pub access_token: Option<String>,
    pub expires_in: Option<i64>,
    pub refresh_token: Option<String>,
    pub refresh_token_expires_in: Option<i64>,
    pub error: Option<String>,
    pub error_description: Option<String>,
}

impl GithubOAuthTokenResponse {
    pub fn into_tokens(self) -> Result<GithubTokens, String> {
        if let Some(error) = self.error {
            return Err(format!(
                "{}: {}",
                error,
                self.error_description.unwrap_or_default()
            ));
        }

        let now = Utc::now();
        match (
            self.access_token,
            self.expires_in,
            self.refresh_token,
            self.refresh_token_expires_in,
        ) {
            (Some(access_token), Some(expires_in), Some(refresh_token), Some(refresh_expires)) => {
                Ok(GithubTokens {
                    access_token,
                    expires_at: now + Duration::seconds(expires_in),
                    refresh_token,
                    refresh_token_expires_at: now + Duration::seconds(refresh_expires),
                })
            }
            _ => Err(
                "GitHub token response missing fields. Ensure the GitHub App has token \
                 expiration enabled."
                    .to_string(),
            ),
        }
    }
}
