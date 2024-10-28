use crate::ServerConfig;
use axum::{extract::State, routing::get, Json, Router};

pub fn create_router() -> Router<ServerConfig> {
    Router::new().route("/", get(get_friendshipper_config))
}

async fn get_friendshipper_config(
    State(config): State<ServerConfig>,
) -> Json<crate::FriendshipperConfig> {
    Json(config.friendshipper_config)
}
