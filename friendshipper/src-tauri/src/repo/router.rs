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
        .route("/branch-compare", get(operations::branch_compare_handler))
        .route("/pull", post(operations::pull_handler))
        .route("/show", get(operations::show_commit_files))
        .route("/file-history", get(operations::file_history_handler))
        .route(
            "/snapshots",
            get(operations::list_snapshots).delete(operations::delete_snapshot),
        )
        .route("/snapshots/restore", post(operations::restore_snapshot))
        .route("/snapshots/save", post(operations::save_snapshot))
        .route("/changeset/save", post(operations::save_changeset))
        .route("/changeset/load", get(operations::load_changeset))
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
        .route("/reset-engine", post(operations::reset_engine_handler))
        .route("/download-engine", post(operations::update_engine_handler))
        .route("/checkout/trunk", post(operations::checkout_trunk_handler))
        .route(
            "/checkout/target-branch",
            post(operations::checkout_target_branch_handler),
        )
        .route("/reset", post(operations::reset_repo))
        .route("/refetch", post(operations::refetch_repo))
        .route("/reset/:commit", post(operations::reset_repo_to_commit))
        .route("/revert", post(operations::revert_files_handler::<T>))
        .route("/locks/lock", post(operations::acquire_locks_handler))
        .route("/locks/unlock", post(operations::release_locks_handler))
        .route(
            "/gh/commit-statuses",
            get(operations::gh::get_commit_statuses),
        )
        .route("/gh/queue", get(operations::gh::get_merge_queue))
        .route("/gh/submit", post(operations::gh::submit_handler))
        .route("/gh/pulls", get(operations::gh::get_pull_requests))
        .route("/gh/pulls/:id", get(operations::gh::get_pull_request))
        .route("/gh/user", get(operations::gh::get_user))
}
