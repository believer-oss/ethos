use std::fs;

use anyhow::Context;
use axum::extract::{Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Local, Utc};
use ethos_core::storage::{
    ArtifactBuildConfig, ArtifactConfig, ArtifactEntry, ArtifactKind, ArtifactList, Platform,
};
use ethos_core::utils::junit::JunitOutput;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Time;
use serde::{Deserialize, Serialize};
use tracing::{error, info, instrument, warn};

use crate::engine::EngineProvider;
use ethos_core::clients::argo::{
    ARGO_WORKFLOW_COMMIT_LABEL_KEY, ARGO_WORKFLOW_COMPARE_ANNOTATION_KEY,
    ARGO_WORKFLOW_MESSAGE_ANNOTATION_KEY, ARGO_WORKFLOW_PUSHER_LABEL_KEY,
};
use ethos_core::clients::aws::ensure_aws_client;
use ethos_core::clients::kube::ensure_kube_client;
use ethos_core::clients::obs;
use ethos_core::types::argo::workflow::{Workflow, WorkflowStatus};
use ethos_core::types::builds::SyncClientRequest;
use ethos_core::types::errors::CoreError;
use ethos_core::types::gameserver::GameServerResults;

use crate::state::AppState;

const UNKNOWN_PUSHER: &str = "unknown";

pub fn router<T>() -> Router<AppState<T>>
where
    T: EngineProvider,
{
    Router::new().route("/engine/notify-state", post(notify_state))
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct NotifyStateParams {
    in_slow_task: bool,
}

pub async fn notify_state<T>(
    State(state): State<AppState<T>>,
    params: Query<NotifyStateParams>,
) -> Result<String, CoreError>
where
    T: EngineProvider,
{
    state.engine.set_state(params.in_slow_task);
    Ok("ok".to_string())
}
