use std::sync::Arc;

use axum::extract::{Path, State};
use axum::routing::{get, post, put};
use axum::{Json, Router};

use ethos_core::clients::kube::ensure_kube_client;
use ethos_core::types::errors::CoreError;
use ethos_core::types::playtests::{
    AssignUserRequest, CreatePlaytestRequest, GetPlaytestsResponse, Playtest, UnassignUserRequest,
    UpdatePlaytestRequest,
};

use crate::state::AppState;

pub fn router(shared_state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(get_playtests).post(create_playtest))
        .route("/:name", put(update_playtest).delete(delete_playtest))
        .route("/assign", post(assign_user))
        .route("/unassign", post(unassign_user))
        .with_state(shared_state)
}

async fn get_playtests(
    State(state): State<Arc<AppState>>,
) -> Result<Json<GetPlaytestsResponse>, CoreError> {
    let kube_client = ensure_kube_client(state.kube_client.read().clone())?;
    let playtests = kube_client.get_playtests().await?;

    Ok(Json(playtests))
}

async fn create_playtest(
    State(state): State<Arc<AppState>>,
    Json(request): Json<CreatePlaytestRequest>,
) -> Result<Json<Playtest>, CoreError> {
    let kube_client = ensure_kube_client(state.kube_client.read().clone())?;
    let playtest = kube_client.create_playtest(request).await?;

    Ok(Json(playtest))
}

async fn update_playtest(
    Path(name): Path<String>,
    State(state): State<Arc<AppState>>,
    Json(request): Json<UpdatePlaytestRequest>,
) -> Result<Json<Playtest>, CoreError> {
    let kube_client = ensure_kube_client(state.kube_client.read().clone())?;
    let playtest = kube_client.update_playtest(&name, request).await?;

    Ok(Json(playtest))
}

async fn delete_playtest(
    Path(name): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Result<(), CoreError> {
    let kube_client = ensure_kube_client(state.kube_client.read().clone())?;
    kube_client.delete_playtest(&name).await?;

    Ok(())
}

async fn assign_user(
    State(state): State<Arc<AppState>>,
    Json(request): Json<AssignUserRequest>,
) -> Result<(), CoreError> {
    let kube_client = ensure_kube_client(state.kube_client.read().clone())?;
    kube_client
        .assign_user_to_playtest(&request.playtest, &request.user, request.group)
        .await?;

    Ok(())
}

async fn unassign_user(
    State(state): State<Arc<AppState>>,
    Json(request): Json<UnassignUserRequest>,
) -> Result<(), CoreError> {
    let kube_client = ensure_kube_client(state.kube_client.read().clone())?;
    kube_client
        .remove_user_from_playtest(&request.playtest, &request.user)
        .await?;

    Ok(())
}
