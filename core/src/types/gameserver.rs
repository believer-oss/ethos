use k8s_openapi::apimachinery::pkg::apis::meta::v1::Time;
use kube_derive::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(CustomResource, Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[kube(
    group = "game.believer.dev",
    version = "v1alpha1",
    kind = "GameServer",
    namespaced
)]
#[kube(status = "GameServerStatus")]
#[serde(rename_all = "camelCase")]
pub struct GameServerSpec {
    pub display_name: Option<String>,
    pub version: String,
    pub map: Option<String>,

    pub include_readiness_probe: bool,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub cmd_args: Option<Vec<String>>,
}

#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GameServerStatus {
    pub ip: Option<String>,
    pub port: i32,
    pub netimgui_port: i32,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub ready: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GameServerResults {
    pub name: String,
    pub display_name: String,
    pub ip: Option<String>,
    pub port: i32,
    pub netimgui_port: i32,
    pub version: String,
    pub creation_timestamp: Time,
    pub ready: bool,
}

impl GameServerResults {
    pub fn format_server_name(&self) -> String {
        if self.display_name.is_empty() {
            return self.name.clone();
        }
        format!("{} | {}", self.display_name, self.name)
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchRequest {
    pub commit: String,
    pub check_for_existing: bool,
    pub display_name: String,
    pub map: Option<String>,
    pub include_readiness_probe: bool,
    pub cmd_args: Vec<String>,
}
