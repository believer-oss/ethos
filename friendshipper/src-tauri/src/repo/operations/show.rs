use std::path::PathBuf;

use axum::extract::{Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use tracing::info;

use crate::engine::EngineProvider;
use ethos_core::types::errors::CoreError;
use ethos_core::types::repo::{CommitFileInfo, ShowCommitFilesResponse};

use crate::state::AppState;
use crate::system::unreal::CanUseCommandlet;
use crate::system::unreal::OFPANameCache;

#[derive(Deserialize, Serialize)]
pub struct ShowCommitFilesParams {
    commit: String,

    #[serde(default)]
    stash: bool,
}

pub async fn show_commit_files<T>(
    State(state): State<AppState<T>>,
    params: Query<ShowCommitFilesParams>,
) -> Result<Json<ShowCommitFilesResponse>, CoreError>
where
    T: EngineProvider,
{
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

    let mut files: Vec<CommitFileInfo> = output
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
            CommitFileInfo {
                action,
                file,
                display_name: String::new(),
            }
        })
        .collect();

    let file_paths: Vec<String> = files.iter().map(|v| v.file.clone()).collect();

    let repo_path = state.app_config.read().repo_path.clone();
    let uproject_path = state
        .app_config
        .read()
        .get_uproject_path(&state.repo_config.read());
    let engine_path = state
        .app_config
        .read()
        .load_engine_path_from_repo(&state.repo_config.read())
        .unwrap_or_default();

    let ofpa_names = OFPANameCache::get_names(
        state.ofpa_cache.clone(),
        &PathBuf::from(repo_path),
        &uproject_path,
        &engine_path,
        &file_paths,
        CanUseCommandlet::Never,
    )
    .await;

    assert_eq!(files.len(), ofpa_names.len());

    for i in 0..files.len() {
        files[i].display_name.clone_from(&ofpa_names[i]);
    }

    Ok(Json(files))
}
