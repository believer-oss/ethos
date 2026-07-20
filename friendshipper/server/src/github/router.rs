use axum::{
    extract::State,
    routing::{get, post},
    Json, Router,
};
use ethos_core::types::errors::CoreError;
use ethos_core::types::github::auth::{
    GithubAppConfig, GithubOAuthTokenResponse, GithubTokenExchangeRequest,
    GithubTokenRefreshRequest, GithubTokens,
};
use tracing::{error, info};

use crate::ServerConfig;

const GITHUB_OAUTH_TOKEN_URL: &str = "https://github.com/login/oauth/access_token";

pub fn create_router() -> Router<ServerConfig> {
    Router::new()
        .route("/config", get(get_github_app_config))
        .route("/oauth/token", post(exchange_code))
        .route("/oauth/refresh", post(refresh_token))
}

fn require_app_auth(config: &ServerConfig) -> Result<crate::GithubAppAuthConfig, CoreError> {
    config.github_app_auth.clone().ok_or_else(|| {
        CoreError::Internal(anyhow::anyhow!(
            "GitHub App auth is not configured on this server"
        ))
    })
}

async fn get_github_app_config(
    State(config): State<ServerConfig>,
) -> Result<Json<GithubAppConfig>, CoreError> {
    let app_auth = require_app_auth(&config)?;
    Ok(Json(GithubAppConfig {
        client_id: app_auth.client_id,
    }))
}

async fn exchange_code(
    State(config): State<ServerConfig>,
    Json(payload): Json<GithubTokenExchangeRequest>,
) -> Result<Json<GithubTokens>, CoreError> {
    let app_auth = require_app_auth(&config)?;

    info!("Exchanging GitHub OAuth authorization code for tokens");

    let params = [
        ("client_id", app_auth.client_id.as_str()),
        ("client_secret", app_auth.client_secret.as_str()),
        ("code", payload.code.as_str()),
    ];

    request_tokens(&params).await
}

async fn refresh_token(
    State(config): State<ServerConfig>,
    Json(payload): Json<GithubTokenRefreshRequest>,
) -> Result<Json<GithubTokens>, CoreError> {
    let app_auth = require_app_auth(&config)?;

    info!("Refreshing GitHub user access token");

    let params = [
        ("client_id", app_auth.client_id.as_str()),
        ("client_secret", app_auth.client_secret.as_str()),
        ("grant_type", "refresh_token"),
        ("refresh_token", payload.refresh_token.as_str()),
    ];

    request_tokens(&params).await
}

async fn request_tokens(params: &[(&str, &str)]) -> Result<Json<GithubTokens>, CoreError> {
    let client = reqwest::Client::new();
    let response = client
        .post(GITHUB_OAUTH_TOKEN_URL)
        .header("Accept", "application/json")
        .form(params)
        .send()
        .await
        .map_err(|e| {
            error!("Failed to reach GitHub OAuth endpoint: {}", e);
            CoreError::Internal(anyhow::anyhow!("Failed to reach GitHub: {}", e))
        })?;

    let token_response: GithubOAuthTokenResponse = response.json().await.map_err(|e| {
        error!("Failed to parse GitHub OAuth response: {}", e);
        CoreError::Internal(anyhow::anyhow!("Failed to parse GitHub response: {}", e))
    })?;

    match token_response.into_tokens() {
        Ok(tokens) => Ok(Json(tokens)),
        Err(e) => {
            error!("GitHub OAuth token exchange failed: {}", e);
            Err(CoreError::Internal(anyhow::anyhow!(
                "GitHub token exchange failed: {}",
                e
            )))
        }
    }
}
