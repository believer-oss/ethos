use crate::engine::EngineProvider;
use axum::routing::{get, post};
use axum::Router;

use crate::repo::operations;
use crate::state::AppState;

pub fn router<T>() -> Router<AppState<T>>
where
    T: EngineProvider,
{
    Router::new()
        .route("/clone", post(operations::clone_handler))
        .route("/diff", get(operations::diff_handler))
        .route("/log", get(operations::log_handler))
        .route("/pull", post(operations::pull_handler))
        .route("/show", get(operations::show_commit_files))
        .route(
            "/snapshots",
            get(operations::list_snapshots).delete(operations::delete_snapshot),
        )
        .route("/snapshots/restore", post(operations::restore_snapshot))
        .route("/snapshots/save", post(operations::save_snapshot))
        .route("/status", get(operations::status_handler))
        .route(
            "/diagnostics/rebase",
            get(operations::diagnostics::rebase_status_handler)
                .post(operations::diagnostics::rebase_handler),
        )
        .route(
            "/diagnostics/rebase/fix",
            post(operations::diagnostics::remediate_rebase_handler),
        )
        .route("/download-dlls", post(operations::download_dlls_handler))
        .route("/download-engine", post(operations::update_engine_handler))
        .route("/checkout/trunk", post(operations::checkout_trunk_handler))
        .route(
            "/checkout/commit",
            post(operations::checkout_commit_handler),
        )
        .route("/reset", post(operations::reset_repo))
        .route("/revert", post(operations::revert_files_handler::<T>))
        .route("/locks/lock", post(operations::acquire_locks_handler))
        .route("/locks/unlock", post(operations::release_locks_handler))
        .route("/gh/queue", get(operations::gh::get_merge_queue))
        .route("/gh/submit", post(operations::gh::submit_handler))
        .route("/gh/pulls", get(operations::gh::get_pull_requests))
        .route("/gh/pulls/:id", get(operations::gh::get_pull_request))
        .route("/gh/user", get(operations::gh::get_user))
}
