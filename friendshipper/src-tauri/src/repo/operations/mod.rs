pub use branch_compare::branch_compare_handler;
pub use browse::list_directory_handler;
pub use changeset::{load_changeset, save_changeset, SaveChangeSetRequest};
pub use checkout::{checkout_target_branch_handler, checkout_trunk_handler, CheckoutOp};
pub use clone::clone_handler;
pub use commit_info::commit_info_handler;
pub use diff::{diff_handler, DiffOp};
pub use download_dlls::{download_dlls_handler, DownloadDllsOp};
pub use file_history::file_history_handler;
pub use install_git_hooks::{install_git_hooks_handler, InstallGitHooksOp};
pub use locks::{acquire_locks_handler, release_locks_handler};
pub use log::log_handler;
pub use pull::{pull_handler, PullOp};
pub use refetch::refetch_repo;
pub use reset::{reset_repo, reset_repo_to_commit};
pub use restore::{restore_file_to_revision_handler, RestoreFileToRevisionRequest};
pub use revert::{revert_files_handler, RevertFilesOp};
pub use show::show_commit_files;
pub use snapshot::{
    delete_snapshot, list_snapshots, restore_snapshot, save_snapshot, RestoreSnapshotRequest,
    SaveSnapshotRequest,
};
pub use status::{status_handler, RepoStatusRef, StatusOp};
pub use update_engine::{
    reset_engine_handler, update_engine_handler, UpdateEngineOp, WipeEngineOp,
};

pub use crate::repo::operations::gh::submit::GitHubSubmitOp;

use ethos_core::types::errors::CoreError;

/// Validates a repo-relative path supplied by the frontend. Normalizes backslashes to forward
/// slashes, strips leading/trailing slashes, and rejects empty segments, `.` / `..` traversal,
/// and absolute paths (anything containing `:`, which catches Windows drive letters).
///
/// Returns an empty string for empty input (meaning "repo root"), which callers can reject
/// further if they require a specific file.
pub(crate) fn sanitize_repo_path(input: &str) -> Result<String, CoreError> {
    let normalized = input.replace('\\', "/");
    let trimmed = normalized.trim_matches('/').to_string();

    if trimmed.is_empty() {
        return Ok(String::new());
    }

    for seg in trimmed.split('/') {
        if seg.is_empty() || seg == "." || seg == ".." {
            return Err(CoreError::Input(anyhow::anyhow!(
                "Invalid path segment in '{}'",
                trimmed
            )));
        }
    }

    if trimmed.contains(':') {
        return Err(CoreError::Input(anyhow::anyhow!(
            "Absolute paths are not allowed"
        )));
    }

    Ok(trimmed)
}

/// Validates a commit SHA: non-empty, hex-only, and no longer than a full SHA-256 id.
pub(crate) fn is_valid_sha(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.chars().all(|c| c.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_accepts_empty_as_repo_root() {
        assert_eq!(sanitize_repo_path("").unwrap(), "");
        assert_eq!(sanitize_repo_path("/").unwrap(), "");
        assert_eq!(sanitize_repo_path("///").unwrap(), "");
    }

    #[test]
    fn sanitize_normalizes_backslashes_and_trims_slashes() {
        assert_eq!(
            sanitize_repo_path("Content\\Maps\\Arena.umap").unwrap(),
            "Content/Maps/Arena.umap"
        );
        assert_eq!(
            sanitize_repo_path("/Content/Maps/").unwrap(),
            "Content/Maps"
        );
    }

    #[test]
    fn sanitize_accepts_ordinary_paths() {
        assert_eq!(sanitize_repo_path("Content").unwrap(), "Content");
        assert_eq!(
            sanitize_repo_path("Content/Characters/Hero.uasset").unwrap(),
            "Content/Characters/Hero.uasset"
        );
    }

    #[test]
    fn sanitize_rejects_traversal() {
        assert!(sanitize_repo_path("..").is_err());
        assert!(sanitize_repo_path("Content/..").is_err());
        assert!(sanitize_repo_path("../../etc/passwd").is_err());
        assert!(sanitize_repo_path("Content/../../Other").is_err());
    }

    #[test]
    fn sanitize_rejects_dot_segments() {
        assert!(sanitize_repo_path(".").is_err());
        assert!(sanitize_repo_path("Content/./Maps").is_err());
    }

    #[test]
    fn sanitize_rejects_empty_segments() {
        // Double slashes create empty segments after trimming; should fail.
        assert!(sanitize_repo_path("Content//Maps").is_err());
    }

    #[test]
    fn sanitize_rejects_windows_drive_letters() {
        assert!(sanitize_repo_path("C:/Users/me").is_err());
        assert!(sanitize_repo_path("C:\\Users\\me").is_err());
        assert!(sanitize_repo_path("anything:with:colons").is_err());
    }

    #[test]
    fn valid_sha_accepts_short_and_full_hex() {
        assert!(is_valid_sha("a1b2c3d"));
        assert!(is_valid_sha("a1b2c3d4e5f6789012345678901234567890abcd"));
        // SHA-256 length (64) is the upper bound.
        assert!(is_valid_sha(&"0".repeat(64)));
    }

    #[test]
    fn valid_sha_rejects_empty_and_non_hex() {
        assert!(!is_valid_sha(""));
        assert!(!is_valid_sha("not-hex"));
        assert!(!is_valid_sha("abcg")); // 'g' isn't hex
        assert!(!is_valid_sha("abc def"));
        assert!(!is_valid_sha("HEAD"));
        assert!(!is_valid_sha("../evil"));
    }

    #[test]
    fn valid_sha_rejects_over_64_chars() {
        assert!(!is_valid_sha(&"a".repeat(65)));
    }
}

mod branch_compare;
mod browse;
mod changeset;
mod checkout;
mod clone;
mod commit_info;
pub mod diagnostics;
mod diff;
mod download_dlls;
mod file_history;
pub mod gh;
mod install_git_hooks;
mod locks;
mod log;
mod pull;
mod refetch;
mod reset;
mod restore;
mod revert;
mod show;
mod snapshot;
mod status;
mod update_engine;
pub mod validate;
