use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArgoConfig {
    pub server_url: String,
    pub namespace: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectConfig {
    pub name: String,
    pub maps: Vec<String>,

    #[serde(default)]
    pub default: bool,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub argo: Option<ArgoConfig>,
}
