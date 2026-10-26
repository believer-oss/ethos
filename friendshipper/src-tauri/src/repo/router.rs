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
        .route("/browse", get(operations::list_directory_handler))
        .route("/commit-info", get(operations::commit_info_handler))
        .route(
            "/snapshots",
            get(operations::list_snapshots).delete(operations::delete_snapshot),
        )
        .route("/snapshots/preview", get(operations::preview_snapshot))
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
        .route(
            "/diagnostics/artifacts",
            get(operations::diagnostics::artifact_status_handler),
        )
        .route(
            "/diagnostics/incoming-engine",
            get(operations::diagnostics::incoming_engine_handler),
        )
        .route(
            "/diagnostics/verify/:kind",
            post(operations::diagnostics::verify_handler),
        )
        .route(
            "/diagnostics/object-count",
            get(operations::diagnostics::object_count_handler),
        )
        .route(
            "/diagnostics/gc",
            post(operations::diagnostics::run_gc_handler),
        )
        .route(
            "/diagnostics/github-status",
            get(operations::diagnostics::github_status_handler),
        )
        .route("/download-dlls", post(operations::download_dlls_handler))
        .route("/reset-engine", post(operations::reset_engine_handler))
        .route("/download-engine", post(operations::update_engine_handler))
        .route("/checkout/trunk", post(operations::checkout_trunk_handler))
        .route(
            "/checkout/target-branch",
            post(operations::checkout_target_branch_handler),
        )
        .route("/branch/list", get(operations::list_branches_handler))
        .route("/branch/create", post(operations::create_branch_handler))
        .route("/branch/switch", post(operations::switch_branch_handler))
        .route("/branch/submit", post(operations::branch_submit_handler))
        .route("/branch/analysis", get(operations::branch_analysis_handler))
        .route("/branch/pr", post(operations::branch_pull_request_handler))
        .route(
            "/branch/pr/enqueue",
            post(operations::branch_pull_request_enqueue_handler),
        )
        .route(
            "/branch/cleanup/candidates",
            get(operations::branch_cleanup_candidates_handler),
        )
        .route("/branch/cleanup", post(operations::branch_cleanup_handler))
        .route(
            "/branch/rebase/state",
            get(operations::guided_rebase_state_handler),
        )
        .route(
            "/branch/rebase/preflight",
            get(operations::guided_rebase_preflight_handler),
        )
        .route(
            "/branch/rebase/start",
            post(operations::guided_rebase_start_handler),
        )
        .route(
            "/branch/rebase/resolve",
            post(operations::guided_rebase_resolve_handler),
        )
        .route(
            "/branch/rebase/unresolve",
            post(operations::guided_rebase_unresolve_handler),
        )
        .route(
            "/branch/rebase/open-file",
            post(operations::guided_rebase_open_file_handler),
        )
        .route(
            "/branch/rebase/continue",
            post(operations::guided_rebase_continue_handler),
        )
        .route(
            "/branch/rebase/abort",
            post(operations::guided_rebase_abort_handler),
        )
        .route(
            "/branch/rebase/finish",
            post(operations::guided_rebase_finish_handler),
        )
        .route(
            "/branch/rebase/redo",
            post(operations::guided_rebase_redo_handler),
        )
        .route(
            "/branch/rebase/reset-to-backup",
            post(operations::guided_rebase_reset_to_backup_handler),
        )
        .route(
            "/branch/rebase/dismiss",
            post(operations::guided_rebase_dismiss_handler),
        )
        .route(
            "/branch/rebuild-editor",
            post(operations::editor_rebuild_handler),
        )
        .route(
            "/branch/editor-binaries",
            get(operations::editor_binaries_handler),
        )
        .route("/reset", post(operations::reset_repo))
        .route("/refetch", post(operations::refetch_repo))
        .route("/maintenance", post(operations::run_maintenance_handler))
        .route("/reset/:commit", post(operations::reset_repo_to_commit))
        .route("/revert", post(operations::revert_files_handler::<T>))
        .route(
            "/restore-file",
            post(operations::restore_file_to_revision_handler::<T>),
        )
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
        .route(
            "/zip/local-changes",
            post(operations::zip_local_changes_handler),
        )
        .route(
            "/zip/import/preview",
            get(operations::preview_import_zip_handler),
        )
        .route(
            "/zip/import",
            post(operations::import_zipped_changes_handler::<T>),
        )
}
