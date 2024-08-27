use std::sync::Arc;

use axum::routing::{get, post};
use axum::Router;

use crate::repo::clone::clone_handler;
use crate::repo::config::{del_fetch_include, get_fetch_include};
use crate::repo::diagnostics;
use crate::repo::file::{get_all_files, get_file_history, get_files};
use crate::repo::lfs::download_files;
use crate::repo::locks::{lock_files, unlock_files, verify_locks_handler};
use crate::repo::log::log_handler;
use crate::repo::pull::pull_handler;
use crate::repo::push::push_handler;
use crate::repo::revert::revert_files_handler;
use crate::repo::show::show_commit_files;
use crate::repo::status::status_handler;
use crate::state::AppState;

pub fn router(shared_state: Arc<AppState>) -> Router {
    Router::new()
        .route("/clone", post(clone_handler))
        .route(
            "/config/fetchinclude",
            get(get_fetch_include).delete(del_fetch_include),
        )
        .route("/status", get(status_handler))
        .route("/files", get(get_files))
        .route("/files/all", get(get_all_files))
        .route("/files/history", get(get_file_history))
        .route("/lfs/download", post(download_files))
        .route("/lfs/locks/lock", post(lock_files))
        .route("/lfs/locks/unlock", post(unlock_files))
        .route("/lfs/locks/verify", get(verify_locks_handler))
        .route("/log", get(log_handler))
        .route("/pull", post(pull_handler))
        .route("/push", post(push_handler))
        .route("/revert", post(revert_files_handler))
        .route("/show", get(show_commit_files))
        .route(
            "/diagnostics/rebase",
            get(diagnostics::rebase_status_handler).post(diagnostics::rebase_handler),
        )
        .route(
            "/diagnostics/rebase/fix",
            post(diagnostics::remediate_rebase_handler),
        )
        .with_state(shared_state)
}
