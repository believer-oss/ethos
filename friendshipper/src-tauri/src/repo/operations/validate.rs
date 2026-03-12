use std::path::Path;

use anyhow::anyhow;
use tracing::info;

use ethos_core::types::errors::CoreError;
use ethos_core::types::repo::RepoStatus;

/// Validates git repository state before starting sync or submit operations.
/// Checks for rebase-in-progress, merge-in-progress, and detached HEAD states
/// that would cause confusing failures mid-operation.
pub fn validate_repo_state(repo_path: &Path, repo_status: &RepoStatus) -> Result<(), CoreError> {
    let git_dir = repo_path.join(".git");

    // Check for rebase in progress in the main working tree
    if git_dir.join("rebase-merge").exists() || git_dir.join("rebase-apply").exists() {
        info!("Rebase in progress detected at {:?}", repo_path);
        return Err(CoreError::Input(anyhow!(
            "A rebase is in progress. Resolve it in the Diagnostics tab before continuing."
        )));
    }

    // Check for rebase in progress in any worktree
    let worktrees_dir = git_dir.join("worktrees");
    if worktrees_dir.exists() {
        if let Ok(entries) = std::fs::read_dir(&worktrees_dir) {
            for entry in entries.flatten() {
                let wt_path = entry.path();
                if wt_path.join("rebase-merge").exists() || wt_path.join("rebase-apply").exists() {
                    let wt_name = entry.file_name();
                    info!(
                        "Rebase in progress detected in worktree {:?} at {:?}",
                        wt_name, repo_path
                    );
                    return Err(CoreError::Input(anyhow!(
                        "A rebase is in progress in a worktree. This is likely left over from a previous submit. \
                        Try submitting again (it will be cleaned up automatically), or manually delete: {}",
                        wt_path.join("rebase-merge").display()
                    )));
                }
            }
        }
    }

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
    fn test_validate_worktree_rebase_in_progress() {
        let (dir, status) = create_test_repo();
        fs::create_dir_all(dir.path().join(".git/worktrees/test-wt/rebase-merge")).unwrap();
        let result = validate_repo_state(dir.path(), &status);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("worktree"));
    }

    #[test]
    fn test_validate_worktree_rebase_apply_in_progress() {
        let (dir, status) = create_test_repo();
        fs::create_dir_all(dir.path().join(".git/worktrees/test-wt/rebase-apply")).unwrap();
        let result = validate_repo_state(dir.path(), &status);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("worktree"));
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
