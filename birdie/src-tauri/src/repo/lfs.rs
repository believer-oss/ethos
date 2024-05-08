use std::sync::Arc;

use axum::extract::State;
use axum::Json;
use serde::{Deserialize, Serialize};

use ethos_core::types::errors::CoreError;

use crate::state::AppState;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DownloadFilesRequest {
    pub files: Vec<String>,
}

pub async fn download_files(
    State(state): State<Arc<AppState>>,
    Json(request): Json<DownloadFilesRequest>,
) -> Result<(), CoreError> {
    // join file paths by comma
    let include_arg = request.files.join(",");

    state
        .git()
        .run(
            &["lfs", "pull", "--include", &include_arg, "--exclude", ""],
            Default::default(),
        )
        .await?;

    Ok(())
}
