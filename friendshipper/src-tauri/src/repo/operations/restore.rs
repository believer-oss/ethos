use axum::extract::State;
use axum::Json;
use ethos_core::clients::git::{Opts, SaveSnapshotIndexOption};
use ethos_core::types::errors::CoreError;
use serde::{Deserialize, Serialize};
use tracing::instrument;

use super::{is_valid_sha, sanitize_repo_path};
use crate::engine::EngineProvider;
use crate::state::AppState;

fn default_snapshot() -> bool {
    true
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreFileToRevisionRequest {
    pub path: String,
    pub sha: String,
    #[serde(default = "default_snapshot")]
    pub take_snapshot: bool,
    #[serde(default)]
    pub skip_engine_check: bool,
}

#[instrument(skip(state))]
pub async fn restore_file_to_revision_handler<T>(
    State(state): State<AppState<T>>,
    Json(req): Json<RestoreFileToRevisionRequest>,
) -> Result<Json<String>, CoreError>
where
    T: EngineProvider,
{
    let path = sanitize_repo_path(&req.path)?;
    if path.is_empty() {
        return Err(CoreError::Input(anyhow::anyhow!("path is required")));
    }
    if !is_valid_sha(&req.sha) {
        return Err(CoreError::Input(anyhow::anyhow!(
            "invalid commit SHA: {}",
            req.sha
        )));
    }

    if !req.skip_engine_check {
        state.engine.check_ready_to_sync_repo().await?;
    }

    let git = state.git();

    // Only snapshot if the file actually differs from HEAD. `git stash create` with
    // `--pathspec-from-file` returns an empty SHA when there's nothing to stash (the typical
    // case when reverting an unmodified file to an older revision), and a subsequent
    // `git stash store` on an empty SHA fails with "Cannot update refs/stash".
    if req.take_snapshot {
        let status_output = git
            .run_and_collect_output(
                &["status", "--porcelain", "--", &path],
                Opts {
                    skip_notify_frontend: true,
                    should_log_stdout: false,
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| CoreError::Internal(anyhow::anyhow!("git status failed: {}", e)))?;

        if !status_output.trim().is_empty() {
            git.save_snapshot(
                "pre-restore-to-previous-version-of-file",
                vec![path.clone()],
                SaveSnapshotIndexOption::DiscardIndex,
            )
            .await
            .map_err(|e| CoreError::Internal(anyhow::anyhow!("failed to save snapshot: {}", e)))?;
        }
    }

    git.run(&["checkout", &req.sha, "--", &path], Opts::default())
        .await
        .map_err(|e| CoreError::Internal(anyhow::anyhow!("git checkout failed: {}", e)))?;

    Ok(Json(String::new()))
}
