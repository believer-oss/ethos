use std::sync::Arc;

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

pub type BirdieConfigRef = Arc<RwLock<BirdieConfig>>;

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct BirdieConfig {
    #[serde(default, rename = "repoPath", alias = "repo_path")]
    pub repo_path: String,

    #[serde(default, rename = "repoUrl", alias = "repo_url")]
    pub repo_url: String,

    #[serde(default, rename = "toolsPath", alias = "tools_path")]
    pub tools_path: String,

    #[serde(default, rename = "toolsUrl", alias = "tools_url")]
    pub tools_url: String,

    #[serde(default, rename = "userDisplayName", alias = "user_display_name")]
    pub user_display_name: String,

    #[serde(default, rename = "githubPAT", skip_serializing_if = "Option::is_none")]
    pub github_pat: Option<String>,

    #[serde(default)]
    pub initialized: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BirdieRepoConfig {
    #[serde(skip_serializing_if = "Option::is_none", rename = "otlp_endpoint")]
    pub otlp_endpoint: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none", rename = "otlp_headers")]
    pub otlp_headers: Option<String>,
}
