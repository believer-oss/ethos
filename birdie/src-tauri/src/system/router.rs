use std::sync::Arc;

use axum::routing::{get, post};
use axum::Router;

use crate::state::AppState;
use crate::system::git::{configure_user, install};
use crate::system::logs::{get_logs, open_system_logs_folder};
use crate::system::update::{get_latest_version, run_update};

pub fn router(shared_state: Arc<AppState>) -> Router {
    Router::new()
        .route("/git/configure", post(configure_user))
        .route("/git/install", post(install))
        .route("/status", get(status))
        .route("/update", get(get_latest_version).post(run_update))
        .route("/logs", get(get_logs))
        .route("/open-logs", post(open_system_logs_folder))
        .with_state(shared_state)
}

async fn status() -> String {
    String::from("OK")
}
