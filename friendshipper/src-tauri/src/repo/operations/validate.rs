use std::path::Path;

use anyhow::anyhow;
use tracing::info;

use ethos_core::types::errors::CoreError;
use ethos_core::types::repo::RepoStatus;

use super::branch::{read_rebase_file, resolve_git_dir, RebaseFileRead, GUARD_IN_PROGRESS};

/// Whether git's rebase in `git_dir` is the one Friendshipper's rebase record describes.
fn is_guided_rebase(git_dir: &Path) -> bool {
    let RebaseFileRead::Ok(file) = read_rebase_file(git_dir) else {
        return false;
    };
    std::fs::read_to_string(git_dir.join("rebase-merge/head-name"))
        .is_ok_and(|head| head.trim() == format!("refs/heads/{}", file.branch))
}

/// Validates git repository state before starting sync or submit operations.
/// Checks for rebase-in-progress, merge-in-progress, and detached HEAD states
/// that would cause confusing failures mid-operation.
pub fn validate_repo_state(repo_path: &Path, repo_status: &RepoStatus) -> Result<(), CoreError> {
    let git_dir = resolve_git_dir(repo_path);

    // Check for rebase in progress in this working tree
    if git_dir.join("rebase-merge").exists() || git_dir.join("rebase-apply").exists() {
        info!("Rebase in progress detected at {:?}", repo_path);
        if is_guided_rebase(&git_dir) {
            return Err(CoreError::Input(anyhow!(GUARD_IN_PROGRESS)));
        }
        return Err(CoreError::Input(anyhow!(
            "A rebase is in progress. Resolve it in the Diagnostics tab before continuing."
        )));
    }

    // Note: we intentionally do NOT check for rebase state in worktrees here.
    // Worktree rebase state is an implementation detail of the submit flow, which
    // has its own cleanup (rebase --abort + reset --hard). Blocking here would
    // prevent that cleanup from ever running.

    // Check for merge in progress
    if git_dir.join("MERGE_HEAD").exists() {
        info!("Merge in progress detected at {:?}", repo_path);
        return Err(CoreError::Input(anyhow!(
            "A merge is in progress. Resolve or abort it before continuing."
        )));
    }

    // Check for detached HEAD
    if repo_status.detached_head {
        info!("Detached HEAD detected at {:?}", repo_path);
        return Err(CoreError::Input(anyhow!(
            "Repository is in a detached HEAD state. Check out a branch before continuing."
        )));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::operations::branch::rebase_fixture::sample_file_for;
    use crate::repo::operations::branch::write_rebase_file;
    use std::fs;
    use tempfile::TempDir;

    fn create_test_repo() -> (TempDir, RepoStatus) {
        let dir = TempDir::new().unwrap();
        fs::create_dir_all(dir.path().join(".git")).unwrap();
        let status = RepoStatus::new();
        (dir, status)
    }

    #[test]
    fn test_validate_clean_state() {
        let (dir, status) = create_test_repo();
        assert!(validate_repo_state(dir.path(), &status).is_ok());
    }

    #[test]
    fn test_validate_rebase_in_progress() {
        let (dir, status) = create_test_repo();
        fs::create_dir_all(dir.path().join(".git/rebase-merge")).unwrap();
        let result = validate_repo_state(dir.path(), &status);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("rebase"));
    }

    #[test]
    fn test_validate_rebase_apply_in_progress() {
        let (dir, status) = create_test_repo();
        fs::create_dir_all(dir.path().join(".git/rebase-apply")).unwrap();
        let result = validate_repo_state(dir.path(), &status);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("rebase"));
    }

    #[test]
    fn test_validate_merge_in_progress() {
        let (dir, status) = create_test_repo();
        fs::write(dir.path().join(".git/MERGE_HEAD"), "abc123").unwrap();
        let result = validate_repo_state(dir.path(), &status);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("merge"));
    }

    #[test]
    fn test_validate_worktree_rebase_does_not_block() {
        let (dir, status) = create_test_repo();
        fs::create_dir_all(dir.path().join(".git/worktrees/test-wt/rebase-merge")).unwrap();
        let result = validate_repo_state(dir.path(), &status);
        assert!(
            result.is_ok(),
            "worktree rebase should not block validation"
        );
    }

    #[test]
    fn test_validate_worktree_rebase_apply_does_not_block() {
        let (dir, status) = create_test_repo();
        fs::create_dir_all(dir.path().join(".git/worktrees/test-wt/rebase-apply")).unwrap();
        let result = validate_repo_state(dir.path(), &status);
        assert!(
            result.is_ok(),
            "worktree rebase-apply should not block validation"
        );
    }

    const BRANCH: &str = "f11r-branch-x";

    /// A rebase record for `BRANCH` and git's rebase of `head_name` in `git_dir`.
    fn fake_rebase(git_dir: &Path, head_name: &str) {
        write_rebase_file(git_dir, &sample_file_for(BRANCH)).unwrap();
        fs::create_dir_all(git_dir.join("rebase-merge")).unwrap();
        fs::write(
            git_dir.join("rebase-merge/head-name"),
            format!("{head_name}\n"),
        )
        .unwrap();
    }

    #[test]
    fn test_validate_guided_rebase_in_progress() {
        let (dir, status) = create_test_repo();
        fake_rebase(&dir.path().join(".git"), "refs/heads/f11r-branch-x");
        let result = validate_repo_state(dir.path(), &status);
        assert_eq!(result.unwrap_err().to_string(), GUARD_IN_PROGRESS);
    }

    #[test]
    fn test_validate_other_rebase_with_guided_record() {
        let (dir, status) = create_test_repo();
        fake_rebase(&dir.path().join(".git"), "refs/heads/f11r-branch-other");
        let result = validate_repo_state(dir.path(), &status);
        assert!(result.unwrap_err().to_string().contains("Diagnostics"));
    }

    #[test]
    fn test_validate_unreadable_record_keeps_rebase_message() {
        let (dir, status) = create_test_repo();
        fs::create_dir_all(dir.path().join(".git/rebase-merge")).unwrap();
        fs::write(dir.path().join(".git/f11r-rebase.json"), "{").unwrap();
        let result = validate_repo_state(dir.path(), &status);
        assert!(result.unwrap_err().to_string().contains("Diagnostics"));
    }

    #[test]
    fn test_validate_guided_record_without_rebase_does_not_block() {
        let (dir, status) = create_test_repo();
        write_rebase_file(&dir.path().join(".git"), &sample_file_for(BRANCH)).unwrap();
        assert!(validate_repo_state(dir.path(), &status).is_ok());
    }

    #[test]
    fn test_validate_rebase_through_gitdir_file() {
        let dir = TempDir::new().unwrap();
        let git_dir = TempDir::new().unwrap();
        let gitdir_line = format!("gitdir: {}\n", git_dir.path().display());
        fs::write(dir.path().join(".git"), gitdir_line).unwrap();
        let status = RepoStatus::new();
        assert!(validate_repo_state(dir.path(), &status).is_ok());

        fs::create_dir_all(git_dir.path().join("rebase-merge")).unwrap();
        let result = validate_repo_state(dir.path(), &status);
        assert!(result.unwrap_err().to_string().contains("Diagnostics"));

        fake_rebase(git_dir.path(), "refs/heads/f11r-branch-x");
        let result = validate_repo_state(dir.path(), &status);
        assert_eq!(result.unwrap_err().to_string(), GUARD_IN_PROGRESS);
    }

    #[test]
    fn test_validate_detached_head() {
        let (dir, mut status) = create_test_repo();
        status.detached_head = true;
        let result = validate_repo_state(dir.path(), &status);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("detached HEAD"));
    }
}
