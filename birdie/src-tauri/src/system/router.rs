use std::sync::Arc;

use axum::routing::{get, post};
use axum::Router;

use crate::state::AppState;
use crate::system::git::{configure_user, install};
use crate::system::logs::{get_logs, open_system_logs_folder};
use crate::system::terminal::open_terminal_to_path;

pub fn router(shared_state: Arc<AppState>) -> Router {
    Router::new()
        .route("/git/configure", post(configure_user))
        .route("/git/install", post(install))
        .route("/status", get(status))
        .route("/logs", get(get_logs))
        .route("/open-logs", post(open_system_logs_folder))
        .route("/terminal", post(open_terminal_to_path))
        .with_state(shared_state)
}

async fn status() -> String {
    String::from("OK")
}
