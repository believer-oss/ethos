use axum::{extract::State, routing::post, Json, Router};
use ethos_core::{clients::aws::ensure_aws_client, types::errors::CoreError};
use serde::{Deserialize, Serialize};
use tracing::instrument;

use crate::{engine::EngineProvider, state::AppState};

pub fn router<T>() -> Router<AppState<T>>
where
    T: EngineProvider,
{
    Router::new()
        .route("/download", post(download_file))
        .route("/upload", post(upload_file))
        .route("/list", post(list_files))
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ListFilesRequest {
    pub prefix: String,
}

#[instrument(skip(state))]
pub async fn list_files<T>(
    State(state): State<AppState<T>>,
    Json(request): Json<ListFilesRequest>,
) -> Result<Json<Vec<String>>, CoreError> {
    ensure_aws_client(state.aws_client.read().await.clone())?;

    let aws_client = state.aws_client.read().await.clone().unwrap();
    let files = aws_client.list_all_objects(&request.prefix).await;
    Ok(Json(files?))
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DownloadFileRequest {
    pub path: String,
    pub key: String,
}

#[instrument(skip(state))]
pub async fn download_file<T>(
    State(state): State<AppState<T>>,
    Json(request): Json<DownloadFileRequest>,
) -> Result<String, CoreError> {
    ensure_aws_client(state.aws_client.read().await.clone())?;

    let aws_client = state.aws_client.read().await.clone().unwrap();
    aws_client
        .download_object_to_path(&request.path, &request.key)
        .await
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UploadFileRequest {
    pub path: String,
    pub prefix: String,
}

#[instrument(skip(state))]
pub async fn upload_file<T>(
    State(state): State<AppState<T>>,
    Json(request): Json<UploadFileRequest>,
) -> Result<String, CoreError> {
    ensure_aws_client(state.aws_client.read().await.clone())?;

    let aws_client = state.aws_client.read().await.clone().unwrap();
    aws_client
        .upload_object(&request.path, &request.prefix)
        .await
}
