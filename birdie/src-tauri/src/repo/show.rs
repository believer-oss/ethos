use std::sync::Arc;

use axum::extract::{Query, State};
use axum::{debug_handler, Json};
use serde::{Deserialize, Serialize};
use tracing::info;

use ethos_core::types::errors::CoreError;
use ethos_core::types::repo::{CommitFileInfo, ShowCommitFilesResponse};

use crate::state::AppState;

#[derive(Deserialize, Serialize)]
pub struct ShowCommitFilesParams {
    commit: String,

    #[serde(default)]
    stash: bool,
}

#[debug_handler]
pub async fn show_commit_files(
    State(state): State<Arc<AppState>>,
    params: Query<ShowCommitFilesParams>,
) -> Result<Json<ShowCommitFilesResponse>, CoreError> {
    let args = if params.stash {
        vec![
            "stash",
            "show",
            "--oneline",
            "--name-status",
            &params.commit,
        ]
    } else {
        vec!["show", "--oneline", "--name-status", &params.commit]
    };

    let output = state
        .git()
        .run_and_collect_output(&args, Default::default())
        .await?;

    let files: Vec<CommitFileInfo> = output
        .lines()
        .skip(match params.stash {
            true => 0,
            false => 1,
        })
        .map(|line| {
            info!("line: {}", line);
            let parts = line.split_whitespace().collect::<Vec<&str>>();
            let action = parts.first().unwrap_or(&"").to_string();
            let file = parts.get(1).unwrap_or(&"").to_string();
            CommitFileInfo { action, file }
        })
        .collect();

    Ok(Json(files))
}
