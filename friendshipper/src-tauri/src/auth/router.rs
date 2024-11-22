use std::sync::atomic::Ordering;

use anyhow::{anyhow, Result};
use axum::extract::{Query, State};
use axum::response::Html;
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use ethos_core::tauri::TauriState;
use openidconnect::core::{CoreClient, CoreProviderMetadata};
use openidconnect::{
    AuthorizationCode, ClientId, CsrfToken, IssuerUrl, OAuth2TokenResponse, PkceCodeVerifier,
    RedirectUrl, RefreshToken, TokenResponse,
};

use crate::client::FriendshipperClient;
use crate::engine::EngineProvider;
use ethos_core::auth::OIDCTokens;
use ethos_core::clients::aws::ensure_aws_client;
use ethos_core::types::errors::CoreError;
use ethos_core::AWSClient;
use serde::Deserialize;
use tauri::{AppHandle, Manager};
use tracing::{error, instrument};

use crate::state::AppState;

static REDIRECT_HTML: &str = r#"<!DOCTYPE html>
<html>
<head>
    <title>Friendshipper Authentication</title>
    <style>
        body {
            font-family: system-ui, -apple-system, sans-serif;
            display: flex;
            justify-content: center;
            align-items: center;
            height: 100vh;
            margin: 0;
            background-color: #2f3537;
        }
        .message {
            text-align: center;
            padding: 2rem;
            background: #475457;
            border-radius: 8px;
            box-shadow: 0 2px 4px rgba(0,0,0,0.1);
        }
        h1 {
            color: #f6c55c;
            margin-bottom: 1rem;
        }
        p {
            color: #f9da8e;
            margin: 0;
        }
    </style>
</head>
<body>
    <div class="message">
        <h1>Authentication Complete</h1>
        <p>You can now close this window and return to Friendshipper.</p>
    </div>
</body>
</html>"#;

pub fn router<T>() -> Router<AppState<T>>
where
    T: EngineProvider,
{
    Router::new()
        .route("/callback", get(authorize))
        .route("/status", get(get_status))
        .route("/aws/refresh", post(refresh_aws_credentials))
        .route("/logout", post(logout))
        .route("/refresh", post(refresh))
}

async fn get_status<T>(State(state): State<AppState<T>>) -> Json<bool>
where
    T: EngineProvider,
{
    let aws_client = match state.aws_client.read().await.clone() {
        Some(client) => client,
        None => {
            return Json(true);
        }
    };

    Json(aws_client.login_required().await)
}

#[derive(Deserialize)]
struct RefreshParams {
    pub token: String,
}

async fn refresh_aws_credentials<T>(
    State(state): State<AppState<T>>,
    Query(params): Query<RefreshParams>,
) -> Result<(), CoreError>
where
    T: EngineProvider,
{
    let client = FriendshipperClient::new(state.app_config.read().server_url.clone())?;
    let credentials = client.get_aws_credentials(&params.token).await?;

    // get config
    let friendshipper_config = client.get_config(&params.token).await?;

    let new_aws_client = AWSClient::from_static_creds(
        &credentials.access_key_id,
        &credentials.secret_access_key,
        credentials.session_token.as_deref(),
        credentials.expiration,
        friendshipper_config.artifact_bucket_name.clone(),
    )
    .await;

    let username = state.app_config.read().user_display_name.clone();
    let playtest_region = state.app_config.read().playtest_region.clone();
    match state
        .replace_aws_client(new_aws_client, playtest_region, &username)
        .await
    {
        Ok(_) => (),
        Err(e) => {
            error!("Failed to replace AWS client: {}", e);
        }
    }

    Ok(())
}

async fn logout<T>(State(state): State<AppState<T>>) -> Result<(), CoreError>
where
    T: EngineProvider,
{
    let aws_client = ensure_aws_client(state.aws_client.read().await.clone())?;
    aws_client.logout().await?;
    Ok(())
}
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct OIDCQueryParams {
    state: CsrfToken,
    code: AuthorizationCode,
}

#[instrument(skip_all, err)]
async fn authorize<T>(
    handle: Extension<AppHandle>,
    State(state): State<AppState<T>>,
    query: Query<OIDCQueryParams>,
) -> Result<Html<String>, CoreError>
where
    T: EngineProvider,
{
    let tauri_state = handle.state::<TauriState>();
    let auth = tauri_state
        .auth_state
        .clone()
        .ok_or(CoreError::Internal(anyhow!("Auth state not found")))?;
    let http_client = reqwest::Client::new();

    let okta_config = state
        .app_config
        .read()
        .clone()
        .okta_config
        .ok_or(CoreError::Internal(anyhow!("Okta config not found")))?;

    let issuer_url_string = okta_config.issuer;
    let issuer_url = IssuerUrl::new(issuer_url_string)?;

    let provider_metadata = CoreProviderMetadata::discover_async(issuer_url, &http_client).await?;

    let client_id_string = okta_config.client_id;
    let client_id = ClientId::new(client_id_string);

    let redirect_url = RedirectUrl::new("http://localhost:8484/auth/callback".to_string())?;
    let client = CoreClient::from_provider_metadata(provider_metadata, client_id, None)
        .set_redirect_uri(redirect_url);

    let token_response = client
        .exchange_code(query.code.clone())?
        .set_pkce_verifier(PkceCodeVerifier::new(auth.pkce.1.clone()))
        .request_async(&http_client)
        .await?;

    let id_token = token_response
        .id_token()
        .ok_or(CoreError::Internal(anyhow!(
            "No ID token found in response"
        )))?
        .to_string();

    let refresh_token: Option<String> = token_response
        .refresh_token()
        .map(|token| token.secret().to_string());

    state.oidc_tx.send(OIDCTokens {
        access_token: token_response.access_token().secret().to_string(),
        id_token,
        refresh_token,
    })?;

    state.login_in_flight.store(false, Ordering::Relaxed);

    Ok(Html(REDIRECT_HTML.to_string()))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RefreshOktaTokensParams {
    pub refresh_token: String,
}

#[instrument(skip_all, err)]
pub async fn refresh<T>(
    State(state): State<AppState<T>>,
    Query(params): Query<RefreshOktaTokensParams>,
) -> Result<(), CoreError>
where
    T: EngineProvider,
{
    let http_client = reqwest::Client::new();

    let okta_config = state
        .app_config
        .read()
        .clone()
        .okta_config
        .ok_or(CoreError::Internal(anyhow!("Okta config not found")))?;

    let issuer_url_string = okta_config.issuer;
    let issuer_url = IssuerUrl::new(issuer_url_string)?;

    let provider_metadata = CoreProviderMetadata::discover_async(issuer_url, &http_client).await?;

    let client_id_string = okta_config.client_id;
    let client_id = ClientId::new(client_id_string);

    let redirect_url = RedirectUrl::new("http://localhost:8484/auth/callback".to_string())?;
    let client = CoreClient::from_provider_metadata(provider_metadata, client_id, None)
        .set_redirect_uri(redirect_url);

    let refresh_token = RefreshToken::new(params.refresh_token.clone());
    let token_response = client
        .exchange_refresh_token(&refresh_token)?
        .request_async(&http_client)
        .await?;

    let id_token = token_response
        .id_token()
        .ok_or(CoreError::Internal(anyhow!(
            "No ID token found in response"
        )))?
        .to_string();

    let refresh_token: Option<String> = token_response
        .refresh_token()
        .map(|token| token.secret().to_string());

    state.oidc_tx.send(OIDCTokens {
        access_token: token_response.access_token().secret().to_string(),
        id_token,
        refresh_token,
    })?;

    Ok(())
}
