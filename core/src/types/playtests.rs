use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use kube_derive::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug)]
pub struct GroupFullError;
impl IntoResponse for GroupFullError {
    fn into_response(self) -> Response {
        (StatusCode::BAD_REQUEST, "Error: Group is full").into_response()
    }
}

impl std::fmt::Display for GroupFullError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Group is full.")
    }
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
pub struct Group {
    pub name: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub users: Option<Vec<String>>,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
pub struct LocalObjectReference {
    pub name: String,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
pub struct GroupStatus {
    pub name: String,

    #[serde(rename = "serverRef")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_ref: Option<LocalObjectReference>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub users: Option<Vec<String>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub ready: Option<bool>,
}

#[derive(CustomResource, Default, Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[kube(
    group = "game.believer.dev",
    version = "v1alpha1",
    kind = "Playtest",
    namespaced
)]
#[kube(status = "PlaytestStatus")]
pub struct PlaytestSpec {
    pub version: String,
    pub map: Option<String>,

    #[serde(rename = "displayName")]
    pub display_name: String,

    #[serde(rename = "minGroups")]
    pub min_groups: i32,

    #[serde(rename = "playersPerGroup")]
    pub players_per_group: i32,

    #[serde(rename = "startTime")]
    pub start_time: String,

    #[serde(rename = "feedbackURL")]
    pub feedback_url: String,

    #[serde(rename = "usersToAutoAssign")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub users_to_auto_assign: Option<Vec<String>>,

    #[serde(rename = "includeReadinessProbe")]
    pub include_readiness_probe: bool,

    pub groups: Vec<Group>,
}

#[derive(Deserialize, Serialize, Clone, Debug, Default, JsonSchema)]
pub struct PlaytestStatus {
    pub groups: Vec<GroupStatus>,
}

pub type GetPlaytestsResponse = Vec<Playtest>;

#[derive(Debug, Deserialize, Serialize)]
pub struct CreatePlaytestRequest {
    pub name: String,
    pub project: String,
    pub do_not_prune: bool,
    pub spec: PlaytestSpec,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct UpdatePlaytestRequest {
    pub project: String,
    pub do_not_prune: bool,
    pub spec: PlaytestSpec,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct AssignUserRequest {
    pub playtest: String,
    pub user: String,
    pub group: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct UnassignUserRequest {
    pub playtest: String,
    pub user: String,
}

#[derive(Clone, Debug)]
pub struct PlaytestAssignment {
    pub server: String,
    pub version: String,
}
