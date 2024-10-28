use crate::ServerConfig;
use axum::{extract::State, routing::get, Json, Router};

pub fn create_router() -> Router<ServerConfig> {
    Router::new().route("/", get(get_okta_config))
}

async fn get_okta_config(State(config): State<ServerConfig>) -> Json<crate::OktaConfig> {
    Json(config.okta_config)
}
