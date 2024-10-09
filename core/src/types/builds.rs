use crate::storage::{ArtifactEntry, MethodPrefix};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncClientRequest {
    pub artifact_entry: ArtifactEntry,
    pub method_prefix: MethodPrefix,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub launch_options: Option<LaunchOptions>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchOptions {
    pub name: String,
}
