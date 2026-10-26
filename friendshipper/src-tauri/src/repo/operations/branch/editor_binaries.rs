use anyhow::anyhow;
use axum::extract::State;
use axum::Json;
use tracing::instrument;

use super::rebase_state::{parse_commit_info, read_git, COMMIT_INFO_FORMAT, CPP_PATHSPECS};
use crate::engine::EngineProvider;
use crate::repo::operations::is_valid_sha;
use crate::state::AppState;
use ethos_core::clients::git;
use ethos_core::types::errors::CoreError;
use ethos_core::types::repo::{EditorBinariesState, EditorBinariesStatus, RebaseCommitInfo};

/// Whether the editor binaries built at `dll_commit` are still current, given the latest
/// commit after it that changed C++.
pub(crate) fn editor_binaries_status(
    dll_commit: &str,
    latest_cpp: Option<RebaseCommitInfo>,
) -> EditorBinariesStatus {
    if !is_valid_sha(dll_commit) {
        return EditorBinariesStatus {
            state: EditorBinariesState::Unknown,
            dll_commit: String::new(),
            latest_cpp_commit: None,
        };
    }
    EditorBinariesStatus {
        state: match latest_cpp {
            Some(_) => EditorBinariesState::Mismatch,
            None => EditorBinariesState::Match,
        },
        dll_commit: dll_commit.to_string(),
        latest_cpp_commit: latest_cpp,
    }
}

/// The latest commit in `dll_commit..HEAD` that changed C++.
async fn latest_cpp_commit(
    git: &git::Git,
    dll_commit: &str,
) -> Result<Option<RebaseCommitInfo>, CoreError> {
    let range = format!("{dll_commit}..HEAD");
    let mut args = vec!["log", "-1", COMMIT_INFO_FORMAT, &range, "--"];
    args.extend(CPP_PATHSPECS);
    let output = read_git(git, &args).await?;
    if output.exit_code != 0 {
        return Err(CoreError::Internal(anyhow!(
            "Could not list C++ changes since {dll_commit}: {}",
            output.stderr.trim()
        )));
    }
    Ok(parse_commit_info(output.stdout.trim()))
}

/// [`editor_binaries_status`] of HEAD against `dll_commit`.
pub(crate) async fn editor_binaries(
    git: &git::Git,
    dll_commit: &str,
) -> Result<EditorBinariesStatus, CoreError> {
    let latest_cpp = match is_valid_sha(dll_commit) {
        true => latest_cpp_commit(git, dll_commit).await?,
        false => None,
    };
    Ok(editor_binaries_status(dll_commit, latest_cpp))
}

#[instrument(skip_all)]
pub async fn editor_binaries_handler<T>(
    State(state): State<AppState<T>>,
) -> Result<Json<EditorBinariesStatus>, CoreError>
where
    T: EngineProvider,
{
    let dll_commit = state.repo_status.read().dll_commit_local.clone();
    if state.app_config.read().repo_path.is_empty() {
        return Ok(Json(editor_binaries_status("", None)));
    }
    Ok(Json(editor_binaries(&state.git(), &dll_commit).await?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::operations::branch::rebase_state::tests::{
        engine, info, Change, Fixture, BRANCH,
    };
    use crate::repo::operations::branch::rebase_state::{
        build_rebase_state, derive_rebase_state, GuidedRebaseFile,
    };
    use ethos_core::types::repo::RebaseStateKind;

    fn unknown() -> EditorBinariesStatus {
        EditorBinariesStatus {
            state: EditorBinariesState::Unknown,
            dll_commit: String::new(),
            latest_cpp_commit: None,
        }
    }

    #[test]
    fn classifies_the_dll_commit() {
        assert_eq!(editor_binaries_status("", None), unknown());
        assert_eq!(editor_binaries_status("not-a-sha", None), unknown());
        assert_eq!(
            editor_binaries_status("--all", Some(info("abc", "x"))),
            unknown()
        );
        assert_eq!(
            editor_binaries_status("abc123", None),
            EditorBinariesStatus {
                state: EditorBinariesState::Match,
                dll_commit: "abc123".to_string(),
                latest_cpp_commit: None,
            }
        );
        assert_eq!(
            editor_binaries_status("abc123", Some(info("def456", "cpp"))),
            EditorBinariesStatus {
                state: EditorBinariesState::Mismatch,
                dll_commit: "abc123".to_string(),
                latest_cpp_commit: Some(info("def456", "cpp")),
            }
        );
    }

    async fn status(fx: &Fixture, dll_commit: &str) -> EditorBinariesStatus {
        editor_binaries(&fx.git, dll_commit)
            .await
            .expect("editor binaries")
    }

    #[tokio::test]
    async fn compares_head_with_the_dll_commit() {
        let fx = Fixture::new().await;
        let dll = fx.head().await;
        assert_eq!(status(&fx, &dll).await.state, EditorBinariesState::Match);

        fx.commit_file("Content/Sourceful/x.uasset", b"x\n", "content")
            .await;
        assert_eq!(status(&fx, &dll).await.state, EditorBinariesState::Match);

        let cpp = fx
            .commit_file("Game/Source/G/a.cpp", b"int a;\n", "cpp change")
            .await;
        fx.commit_file("docs.txt", b"d\n", "docs").await;
        let mismatch = status(&fx, &dll).await;
        assert_eq!(
            mismatch,
            EditorBinariesStatus {
                state: EditorBinariesState::Mismatch,
                dll_commit: dll.clone(),
                latest_cpp_commit: Some(info(&cpp, "cpp change")),
            }
        );
        assert_eq!(status(&fx, "").await, unknown());
    }

    #[tokio::test]
    async fn rebased_cpp_commits_come_after_main_binaries() {
        let fx = Fixture::new().await;
        let d = fx
            .diverge(
                &[],
                &[Change::Write("m.txt", b"main\n")],
                &[Change::Write("Source/G/a.cpp", b"int a;\n")],
            )
            .await;
        let file: GuidedRebaseFile = fx.guided_file("main").await;
        fx.write_file_json(&file);
        assert_eq!(fx.start_raw_rebase(&file.onto).await, 0);
        let derived = derive_rebase_state(&fx.git, fx.path()).await.unwrap();
        let state = build_rebase_state(
            &fx.git,
            fx.path(),
            &engine(fx.path()),
            std::path::Path::new(""),
            derived,
            None,
        )
        .await
        .unwrap();
        assert_eq!(state.state, RebaseStateKind::CompletedUnpushed);

        // Binaries exist for main's tip; the replayed C++ commit has a new sha after it.
        let head = fx.head().await;
        assert_ne!(head, d.branch);
        assert_eq!(
            status(&fx, &d.main).await,
            EditorBinariesStatus {
                state: EditorBinariesState::Mismatch,
                dll_commit: d.main.clone(),
                latest_cpp_commit: Some(info(&head, "branch change")),
            }
        );
        assert_eq!(fx.out(&["symbolic-ref", "--short", "HEAD"]).await, BRANCH);
    }
}
