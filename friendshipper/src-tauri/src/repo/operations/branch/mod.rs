use anyhow::anyhow;

use crate::engine::EngineProvider;
use crate::repo::operations::gh::submit::is_branch_mode;
use crate::state::AppState;
use ethos_core::clients::aws::ensure_aws_client;
use ethos_core::storage::ArtifactStorage;
use ethos_core::types::config::RepoConfig;
use ethos_core::types::errors::CoreError;
use ethos_core::worker::{Task, TaskSequence};
use ethos_core::AWSClient;

mod analyze;
mod cleanup;
mod create;
mod list;
mod post_head_change;
mod pull_request;
mod submit;
mod switch;

pub use analyze::{branch_analysis_handler, BranchAnalysisOp};
pub use cleanup::{branch_cleanup_candidates_handler, branch_cleanup_handler};
pub use create::{create_branch_handler, CreateBranchOp};
pub use list::list_branches_handler;
pub use pull_request::{
    branch_pull_request_enqueue_handler, branch_pull_request_handler, BranchPullRequestOp,
};
pub use submit::{branch_submit_handler, BranchSubmitOp};
pub use switch::{switch_branch_handler, SwitchBranchOp};

const MAX_SLUG_LEN: usize = 48;

pub(crate) const GITHUB_PAT_MISSING: &str =
    "GitHub PAT is not configured. Please configure it in the settings.";

/// Whether `trunk` merges through the merge queue; a trunk with no target entry does.
pub(crate) fn trunk_uses_merge_queue(repo_config: &RepoConfig, trunk: &str) -> bool {
    repo_config
        .target_branches
        .iter()
        .find(|target| target.name == trunk)
        .map(|target| target.uses_merge_queue)
        .unwrap_or(true)
}

/// The trunk for Branch Management on `branch`, or the Input error that refuses it.
pub(crate) fn branch_management_trunk(
    branch: &str,
    repo_config: &RepoConfig,
) -> Result<String, CoreError> {
    if branch.is_empty() {
        return Err(CoreError::Input(anyhow!(
            "No branch is checked out (detached HEAD or rebase in progress)."
        )));
    }
    let trunk = repo_config.trunk_branch.clone();
    if trunk.is_empty() {
        return Err(CoreError::Input(anyhow!(
            "The repo config does not define trunkBranch."
        )));
    }
    if !is_branch_mode(repo_config, branch) {
        return Err(CoreError::Input(anyhow!(
            "Branch Management actions are not available on {branch}. Create or select a feature branch."
        )));
    }
    Ok(trunk)
}

/// Normalizes a user-entered slug (lowercase; whitespace and `_` become `-`; `-` runs collapse;
/// edge `-` trimmed) and validates the result. Invalid characters are rejected, never stripped.
pub fn normalize_and_validate_slug(input: &str) -> Result<String, CoreError> {
    let mut slug = String::with_capacity(input.len());
    for c in input.trim().to_lowercase().chars() {
        let c = if c.is_whitespace() || c == '_' {
            '-'
        } else {
            c
        };
        if c == '-' && slug.ends_with('-') {
            continue;
        }
        slug.push(c);
    }
    let slug = slug.trim_matches('-').to_string();

    if slug.is_empty() {
        return Err(CoreError::Input(anyhow!("Branch name is required.")));
    }
    if !slug
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err(CoreError::Input(anyhow!(
            "Branch name can only contain letters, numbers, spaces, '-' and '_'."
        )));
    }
    if slug.chars().count() > MAX_SLUG_LEN {
        return Err(CoreError::Input(anyhow!(
            "Branch name must be 48 characters or fewer."
        )));
    }

    Ok(slug)
}

/// The AWS client and artifact storage a switch needs, checked before anything is queued.
pub(crate) async fn require_aws_and_storage<T>(
    state: &AppState<T>,
) -> Result<(AWSClient, ArtifactStorage), CoreError>
where
    T: EngineProvider,
{
    let aws_client = ensure_aws_client(state.aws_client.read().await.clone())?;
    let storage = state.storage.read().clone().ok_or_else(|| {
        CoreError::Internal(anyhow!(
            "Storage not configured. AWS may still be initializing."
        ))
    })?;
    Ok((aws_client, storage))
}

/// Queues `op` as its own worker sequence and waits for it, returning the op's error.
pub(crate) async fn run_sequence<T>(
    state: &AppState<T>,
    op: Box<dyn Task + Send + Sync>,
) -> Result<(), CoreError>
where
    T: EngineProvider,
{
    let (tx, rx) = tokio::sync::oneshot::channel::<Option<CoreError>>();
    let mut sequence = TaskSequence::new().with_completion_tx(tx);
    sequence.push(op);
    state
        .operation_tx
        .send(sequence)
        .await
        .map_err(|_| CoreError::Internal(anyhow!("Failed to queue repo operation")))?;
    match rx.await {
        Ok(Some(e)) => Err(e),
        Ok(None) => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// Rejects names git would refuse as a branch, and anything that could be read as a
/// command-line option (e.g. `--upload-pack=…`) when passed to `git fetch origin <branch>`.
pub fn is_valid_branch_name(name: &str) -> bool {
    if name.is_empty() || name == "@" || name == "HEAD" {
        return false;
    }
    if name.starts_with('-') || name.starts_with('/') {
        return false;
    }
    if name.ends_with('/') || name.ends_with('.') || name.ends_with(".lock") {
        return false;
    }
    if name.contains("..") || name.contains("//") || name.contains("@{") {
        return false;
    }
    !name.chars().any(|c| {
        c.is_whitespace()
            || c.is_ascii_control()
            || matches!(c, '~' | '^' | ':' | '?' | '*' | '[' | '\\')
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slug_error(input: &str) -> String {
        match normalize_and_validate_slug(input) {
            Err(CoreError::Input(e)) => e.to_string(),
            other => panic!("expected Input error for {input:?}, got {other:?}"),
        }
    }

    #[test]
    fn slug_normalization() {
        assert_eq!(
            normalize_and_validate_slug("  My Feature_X  ").unwrap(),
            "my-feature-x"
        );
        assert_eq!(normalize_and_validate_slug("a--b").unwrap(), "a-b");
        assert_eq!(normalize_and_validate_slug("-a-").unwrap(), "a");
        assert_eq!(normalize_and_validate_slug("a _\tb").unwrap(), "a-b");
        assert_eq!(normalize_and_validate_slug("ABC123").unwrap(), "abc123");
    }

    #[test]
    fn slug_normalization_is_idempotent() {
        for input in ["  My Feature_X  ", "a--b", "-a-", "LLM Test_20261021 1"] {
            let once = normalize_and_validate_slug(input).unwrap();
            assert_eq!(normalize_and_validate_slug(&once).unwrap(), once);
        }
    }

    #[test]
    fn slug_required() {
        for input in ["", "   ", "---", "_ _"] {
            assert_eq!(slug_error(input), "Branch name is required.");
        }
    }

    #[test]
    fn slug_invalid_characters() {
        for input in ["a.b", "\u{e9}", "a/b", "a@b", "caf\u{e9}"] {
            assert_eq!(
                slug_error(input),
                "Branch name can only contain letters, numbers, spaces, '-' and '_'."
            );
        }
    }

    #[test]
    fn slug_length() {
        let ok = "a".repeat(48);
        assert_eq!(normalize_and_validate_slug(&ok).unwrap(), ok);
        assert_eq!(
            slug_error(&"a".repeat(49)),
            "Branch name must be 48 characters or fewer."
        );
        // Length is measured after normalization.
        assert_eq!(
            normalize_and_validate_slug(&format!("  {ok}--")).unwrap(),
            ok
        );
    }

    #[test]
    fn valid_branch_names() {
        assert!(is_valid_branch_name("main"));
        assert!(is_valid_branch_name("release/1.2"));
        assert!(is_valid_branch_name("f11r-branch-x"));
        assert!(is_valid_branch_name("f11r-main-Jane-1761000000"));
        assert!(is_valid_branch_name("content-main"));
    }

    #[test]
    fn invalid_branch_names() {
        for name in [
            "",
            "--upload-pack=x",
            "-b",
            "a..b",
            "a b",
            "x.lock",
            "a~1",
            "a^",
            "a:b",
            "a?",
            "a*",
            "a[b",
            "a\\b",
            "a\tb",
            "a\nb",
            "a\u{7f}b",
            "/a",
            "a/",
            "a.",
            "a//b",
            "a@{1}",
            "@",
            "HEAD",
        ] {
            assert!(!is_valid_branch_name(name), "{name:?} should be invalid");
        }
    }
}
