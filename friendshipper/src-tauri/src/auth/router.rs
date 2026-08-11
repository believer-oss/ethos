use anyhow::Result;
use axum::extract::{Query, State};
use axum::response::Html;
use axum::routing::{get, post};
use axum::{Json, Router};
use ethos_core::{
    AWSClient, AWS_ACCESS_KEY_ID, AWS_ARTIFACT_BUCKET_NAME, AWS_SECRET_ACCESS_KEY,
    PROMOTED_ARTIFACT_BUCKET_NAME,
};
use serde::{Deserialize, Serialize};
use tracing::{error, info};

use crate::client::FriendshipperClient;
use crate::engine::EngineProvider;
use ethos_core::clients::aws::ensure_aws_client;
use ethos_core::types::errors::CoreError;

use crate::state::AppState;
use crate::state::Notification;

pub fn router<T>() -> Router<AppState<T>>
where
    T: EngineProvider,
{
    Router::new()
        .route("/status", get(get_status))
        .route("/refresh", post(refresh_aws_credentials))
        .route("/logout", post(logout))
        .route("/github/status", get(github_status))
        .route("/github/connect", post(github_connect))
        .route("/github/refresh", post(github_refresh))
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
    let new_aws_client = if state.app_config.read().serverless {
        let access_key_id = AWS_ACCESS_KEY_ID;
        let secret_access_key = AWS_SECRET_ACCESS_KEY;
        let artifact_bucket_name = AWS_ARTIFACT_BUCKET_NAME;
        let promoted_artifact_bucket_name = PROMOTED_ARTIFACT_BUCKET_NAME;

        AWSClient::from_static_creds(
            access_key_id,
            secret_access_key,
            None,
            None,
            artifact_bucket_name.to_string(),
            promoted_artifact_bucket_name.to_string(),
        )
        .await
    } else {
        let client = FriendshipperClient::new(state.app_config.read().server_url.clone())?;
        let credentials = client.get_aws_credentials(&params.token).await?;

        // get config
        let friendshipper_config = client.get_config(&params.token).await?;

        AWSClient::from_static_creds(
            &credentials.access_key_id,
            &credentials.secret_access_key,
            credentials.session_token.as_deref(),
            credentials.expiration,
            friendshipper_config.artifact_bucket_name.clone(),
            friendshipper_config
                .promoted_artifact_bucket_name
                .clone()
                .unwrap_or_else(|| PROMOTED_ARTIFACT_BUCKET_NAME.to_string()),
        )
        .await
    };

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

/// The port the local backend is bound to, injected as an Extension so the
/// OAuth handlers can build the loopback redirect URI.
#[derive(Clone, Copy, Debug)]
pub struct LocalPort(pub u16);

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GithubAuthStatus {
    connected: bool,
    username: String,
    expires_at: Option<chrono::DateTime<chrono::Utc>>,
}

async fn github_status<T>(State(state): State<AppState<T>>) -> Json<GithubAuthStatus>
where
    T: EngineProvider,
{
    let tokens = state.github_token_manager.tokens();

    Json(GithubAuthStatus {
        connected: tokens.as_ref().is_some_and(|t| t.can_refresh()),
        username: state.github_username(),
        expires_at: tokens.map(|t| t.expires_at),
    })
}

#[derive(Deserialize)]
struct OktaTokenParams {
    pub token: String,
}

async fn github_connect<T>(
    State(state): State<AppState<T>>,
    axum::Extension(port): axum::Extension<LocalPort>,
    Query(params): Query<OktaTokenParams>,
) -> Result<(), CoreError>
where
    T: EngineProvider,
{
    let server_url = state.app_config.read().server_url.clone();
    let client = FriendshipperClient::new(server_url)?;
    let app_config = client.get_github_app_config(&params.token).await?;

    let oauth_state = state.github_token_manager.begin_flow(params.token);
    let redirect_uri = format!("http://127.0.0.1:{}/auth/github/callback", port.0);

    let authorize_url = format!(
        "https://github.com/login/oauth/authorize?client_id={}&redirect_uri={}&state={}",
        app_config.client_id,
        urlencoding::encode(&redirect_uri),
        oauth_state
    );

    info!("Opening browser for GitHub authorization");
    open::that(authorize_url)
        .map_err(|e| CoreError::Internal(anyhow::anyhow!("Failed to open browser: {}", e)))?;

    Ok(())
}

/// Refresh the GitHub access token if it's inside the refresh margin. Called
/// on a timer by the frontend alongside the AWS credential refresh, since the
/// server-side refresh endpoint requires a live Okta token.
async fn github_refresh<T>(
    State(state): State<AppState<T>>,
    Query(params): Query<OktaTokenParams>,
) -> Result<Json<bool>, CoreError>
where
    T: EngineProvider,
{
    if !state.github_token_manager.should_refresh() {
        return Ok(Json(false));
    }

    let tokens = match state.github_token_manager.tokens() {
        Some(tokens) => tokens,
        None => return Ok(Json(false)),
    };

    let server_url = state.app_config.read().server_url.clone();
    let client = FriendshipperClient::new(server_url)?;
    let new_tokens = client
        .refresh_github_token(&params.token, &tokens.refresh_token)
        .await?;

    let access_token = new_tokens.access_token.clone();
    state.github_token_manager.set_tokens(new_tokens)?;
    state.apply_github_access_token(access_token).await?;

    info!("GitHub access token refreshed");
    Ok(Json(true))
}

#[derive(Deserialize)]
pub struct GithubCallbackParams {
    pub code: Option<String>,
    pub state: Option<String>,
    pub error_description: Option<String>,
}

fn callback_page(title: &str, message: &str) -> Html<String> {
    Html(format!(
        "<html><head><title>Friendshipper</title></head>\
         <body style=\"font-family: sans-serif; text-align: center; padding-top: 4rem;\">\
         <h2>{title}</h2><p>{message}</p></body></html>"
    ))
}

/// Browser-facing OAuth redirect target. Mounted outside the nonce
/// middleware in lib.rs since GitHub's redirect can't carry the nonce header.
pub async fn github_oauth_callback<T>(
    State(state): State<AppState<T>>,
    Query(params): Query<GithubCallbackParams>,
) -> Html<String>
where
    T: EngineProvider,
{
    let (code, oauth_state) = match (params.code, params.state) {
        (Some(code), Some(oauth_state)) => (code, oauth_state),
        _ => {
            let detail = params
                .error_description
                .unwrap_or_else(|| "Missing code or state parameter.".to_string());
            error!("GitHub OAuth callback failed: {}", detail);
            return callback_page(
                "GitHub authorization failed",
                &format!("{detail} You can close this tab and try again from Friendshipper."),
            );
        }
    };

    let pending = match state.github_token_manager.take_pending(&oauth_state) {
        Ok(pending) => pending,
        Err(e) => {
            error!("GitHub OAuth callback rejected: {}", e);
            return callback_page(
                "GitHub authorization failed",
                "This login attempt is no longer valid. You can close this tab and try again from Friendshipper.",
            );
        }
    };

    let result: Result<(), CoreError> = async {
        let server_url = state.app_config.read().server_url.clone();
        let client = FriendshipperClient::new(server_url)?;
        let tokens = client
            .exchange_github_code(&pending.okta_token, &code)
            .await?;

        let access_token = tokens.access_token.clone();
        state.github_token_manager.set_tokens(tokens)?;
        state.apply_github_access_token(access_token).await?;

        Ok(())
    }
    .await;

    match result {
        Ok(()) => {
            info!("GitHub account connected via OAuth");

            // Take over credential handling for the repo right away so the
            // next git/LFS operation uses the new token instead of GCM.
            let repo_path = state.app_config.read().repo_path.clone();
            if !repo_path.is_empty() {
                if let Some(helper_path) = crate::credential_helper_path() {
                    if let Err(e) = state
                        .git()
                        .configure_credential_helper(&helper_path.to_string_lossy())
                        .await
                    {
                        error!("Failed to configure credential helper: {}", e);
                    }
                }
            }

            state.send_notification(Notification::Success(
                "GitHub account connected.".to_string(),
            ));
            callback_page(
                "GitHub connected",
                "You're all set. You can close this tab and return to Friendshipper.",
            )
        }
        Err(e) => {
            error!("GitHub OAuth token exchange failed: {}", e);
            callback_page(
                "GitHub authorization failed",
                "Something went wrong finishing the login. You can close this tab and try again from Friendshipper.",
            )
        }
    }
}
