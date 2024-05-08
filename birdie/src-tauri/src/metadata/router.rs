use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use ethos_core::types::errors::CoreError;

use crate::metadata::character::CharacterMetadata;
use crate::state::AppState;

const METADATA_FILE_NAME: &str = ".birdie.json";

pub fn router(shared_state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(get_directory_metadata).post(update_metadata))
        .route("/class", post(update_metadata_class))
        .with_state(shared_state)
}

#[derive(Clone, Debug)]
pub enum DirectoryClass {
    None,
    Character,
}

impl DirectoryClass {
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> DirectoryClass {
        match s {
            "none" => DirectoryClass::None,
            "character" => DirectoryClass::Character,
            _ => panic!("Invalid directory class"),
        }
    }

    pub fn to_str(&self) -> &str {
        match self {
            DirectoryClass::None => "none",
            DirectoryClass::Character => "character",
        }
    }
}

impl Serialize for DirectoryClass {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::ser::Serializer,
    {
        serializer.serialize_str(self.to_str())
    }
}

impl<'de> Deserialize<'de> for DirectoryClass {
    fn deserialize<D>(deserializer: D) -> Result<DirectoryClass, D::Error>
    where
        D: serde::de::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Ok(DirectoryClass::from_str(&s))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectoryMetadata {
    pub directory_class: DirectoryClass,
    pub character: Option<CharacterMetadata>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateMetadataClassRequest {
    pub path: PathBuf,
    pub directory_class: DirectoryClass,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GetDirectoryMetadataParams {
    path: PathBuf,
}

pub async fn get_directory_metadata(
    State(state): State<Arc<AppState>>,
    params: Query<GetDirectoryMetadataParams>,
) -> Result<Json<DirectoryMetadata>, CoreError> {
    let repo_path = PathBuf::from(state.app_config.read().repo_path.clone());
    let metadata_path = repo_path.join(params.path.clone()).join(METADATA_FILE_NAME);
    if metadata_path.exists() {
        let metadata_json = std::fs::read_to_string(metadata_path)?;
        let metadata: DirectoryMetadata = serde_json::from_str(&metadata_json)?;
        Ok(Json(metadata))
    } else {
        Ok(Json(DirectoryMetadata {
            directory_class: DirectoryClass::None,
            character: None,
        }))
    }
}

pub async fn update_metadata_class(
    State(state): State<Arc<AppState>>,
    Json(request): Json<UpdateMetadataClassRequest>,
) -> Result<(), CoreError> {
    let repo_path = PathBuf::from(state.app_config.read().repo_path.clone());
    let metadata_path = repo_path.join(&request.path).join(METADATA_FILE_NAME);
    match request.directory_class {
        DirectoryClass::None => {
            if metadata_path.exists() {
                std::fs::remove_file(metadata_path)?;
            }
        }
        DirectoryClass::Character => {
            let metadata = DirectoryMetadata {
                directory_class: DirectoryClass::Character,
                character: Some(CharacterMetadata::default()),
            };
            let metadata_json = serde_json::to_string_pretty(&metadata)?;
            std::fs::write(metadata_path, metadata_json)?;
        }
    }

    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UpdateMetadataRequest {
    pub path: PathBuf,
    pub metadata: DirectoryMetadata,
}

pub async fn update_metadata(
    State(state): State<Arc<AppState>>,
    Json(request): Json<UpdateMetadataRequest>,
) -> Result<Json<DirectoryMetadata>, CoreError> {
    let repo_path = PathBuf::from(state.app_config.read().repo_path.clone());
    let metadata_path = repo_path.join(&request.path).join(METADATA_FILE_NAME);
    let metadata_json = serde_json::to_string_pretty(&request.metadata)?;
    std::fs::write(metadata_path, metadata_json)?;

    Ok(Json(request.metadata))
}
