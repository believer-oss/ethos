use anyhow::Result;
use axum::extract::{Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use openidconnect::core::{CoreClient, CoreProviderMetadata};
use openidconnect::{AuthorizationCode, ClientId, IssuerUrl, OAuth2TokenResponse, TokenResponse};
// use ethos_core::auth::OIDCTokens;
use ethos_core::AWSClient;
use serde::Deserialize;
use tracing::{debug, error};
use ethos_core::auth::OIDCTokens;
use crate::client::FriendshipperClient;
use crate::engine::EngineProvider;
use ethos_core::clients::aws::ensure_aws_client;
use ethos_core::types::errors::CoreError;

use crate::state::AppState;

pub fn router<T>() -> Router<AppState<T>>
where
    T: EngineProvider,
{
    Router::new()
        .route("/callback", get(oidc_callback))
        .route("/status", get(get_status))
        .route("/refresh", post(refresh_aws_credentials))
        .route("/logout", post(logout))
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
    // access_token: String,
    // id_token: String,
    // refresh_token: String,
    state: String,
    code: String,
}

async fn oidc_callback<T>(State(state): State<AppState<T>>, Query(params): Query<OIDCQueryParams>) -> Result<(), CoreError>
where
    T: EngineProvider,
{
    let http_client = reqwest::blocking::ClientBuilder::new()
        // Following redirects opens the client up to SSRF vulnerabilities.
        .redirect(reqwest::redirect::Policy::none())
        .build()?;

    let issuer_url_string = state.app_config.read().clone().okta_config.unwrap().issuer.clone();
    let issuer_url = IssuerUrl::new(issuer_url_string)?;

    // Fetch GitLab's OpenID Connect discovery document.
    let provider_metadata = CoreProviderMetadata::discover(&issuer_url, &http_client)?;

    let client_id_string = state.app_config.read().clone().okta_config.unwrap().client_id.clone();
    let client_id = ClientId::new(client_id_string);
    let client = CoreClient::from_provider_metadata(
        provider_metadata,
        client_id,
        None,
    );

    let code = AuthorizationCode::new(params.code.clone());
    let token_response = client.exchange_code(code).request(http_client)?;

    state.oidc_tx.send(OIDCTokens {
        access_token: token_response.access_token().secret().to_string(),
        id_token: token_response.id_token().unwrap().to_string(),
        refresh_token: token_response.refresh_token().unwrap().secret().to_string(),
    })?;

    Ok(())
}
