use anyhow::Result;
use axum::Router;
use ethos_core::types::config::{FriendshipperConfig, OktaConfig};
use jwt_authorizer::{Authorizer, IntoLayer, JwtAuthorizer, Refresh, RefreshStrategy, Validation};
use serde::Deserialize;
use tracing::info;
pub mod aws;
pub mod config;
pub mod discovery;
pub const APP_NAME: &str = "friendshipper-server";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Debug)]
pub struct ServerConfig {
    pub role_to_assume: String,

    pub friendshipper_config: FriendshipperConfig,
    pub okta_config: OktaConfig,
}

#[derive(Debug, Deserialize, Clone)]
struct User {
    sub: String,
}

pub async fn router(oidc_endpoint: String) -> Result<Router<ServerConfig>> {
    // claims checker function
    fn claim_checker(u: &User) -> bool {
        info!("checking claims: {} -> {}", u.sub, u.sub.contains('@'));

        u.sub.contains('@') // must be an email
    }

    let validation = Validation::new().aud(&[oidc_endpoint.clone()]);

    // Set up JWT authorizer
    let jwt_authorizer: Authorizer<User> = JwtAuthorizer::from_oidc(&oidc_endpoint)
        .refresh(Refresh {
            strategy: RefreshStrategy::Interval,
            ..Default::default()
        })
        .check(claim_checker)
        .validation(validation)
        .build()
        .await?;

    let authed_router = Router::new()
        .nest("/aws", aws::router::create_router())
        .nest("/config", config::router::create_router())
        .layer(jwt_authorizer.into_layer());

    Ok(Router::new()
        .nest("/discovery", discovery::router::create_router())
        .route("/health", axum::routing::get(|| async { "ok" }))
        .merge(authed_router))
}
