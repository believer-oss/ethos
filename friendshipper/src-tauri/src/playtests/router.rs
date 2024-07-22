use crate::engine::EngineProvider;
use axum::extract::{Path, State};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use ethos_core::clients::kube::ensure_kube_client;
use ethos_core::types::errors::CoreError;
use ethos_core::types::playtests::{
    AssignUserRequest, CreatePlaytestRequest, GetPlaytestsResponse, Playtest, UnassignUserRequest,
    UpdatePlaytestRequest,
};
use tracing::instrument;

use crate::state::AppState;

pub fn router<T>() -> Router<AppState<T>>
where
    T: EngineProvider,
{
    Router::new()
        .route("/", get(get_playtests).post(create_playtest))
        .route("/:name", put(update_playtest).delete(delete_playtest))
        .route("/assign", post(assign_user))
        .route("/unassign", post(unassign_user))
}

#[instrument(skip(state))]
async fn get_playtests<T>(
    State(state): State<AppState<T>>,
) -> Result<Json<GetPlaytestsResponse>, CoreError>
where
    T: EngineProvider,
{
    let kube_client = ensure_kube_client(state.kube_client.read().clone())?;
    let playtests = kube_client.get_playtests().await?;

    Ok(Json(playtests))
}

#[instrument(skip(state))]
async fn create_playtest<T>(
    State(state): State<AppState<T>>,
    Json(request): Json<CreatePlaytestRequest>,
) -> Result<Json<Playtest>, CoreError>
where
    T: EngineProvider,
{
    let kube_client = ensure_kube_client(state.kube_client.read().clone())?;
    let playtest = kube_client.create_playtest(request).await?;

    Ok(Json(playtest))
}

#[instrument(skip(state))]
async fn update_playtest<T>(
    Path(name): Path<String>,
    State(state): State<AppState<T>>,
    Json(request): Json<UpdatePlaytestRequest>,
) -> Result<Json<Playtest>, CoreError>
where
    T: EngineProvider,
{
    let kube_client = ensure_kube_client(state.kube_client.read().clone())?;
    let playtest = kube_client.update_playtest(&name, request).await?;

    Ok(Json(playtest))
}

#[instrument(skip(state))]
async fn delete_playtest<T>(
    Path(name): Path<String>,
    State(state): State<AppState<T>>,
) -> Result<(), CoreError>
where
    T: EngineProvider,
{
    let kube_client = ensure_kube_client(state.kube_client.read().clone())?;
    kube_client.delete_playtest(&name).await?;

    Ok(())
}

#[instrument(skip(state))]
async fn assign_user<T>(
    State(state): State<AppState<T>>,
    Json(request): Json<AssignUserRequest>,
) -> Result<(), CoreError>
where
    T: EngineProvider,
{
    let kube_client = ensure_kube_client(state.kube_client.read().clone())?;
    kube_client
        .assign_user_to_playtest(&request.playtest, &request.user, request.group)
        .await?;

    Ok(())
}

#[instrument(skip(state))]
async fn unassign_user<T>(
    State(state): State<AppState<T>>,
    Json(request): Json<UnassignUserRequest>,
) -> Result<(), CoreError>
where
    T: EngineProvider,
{
    let kube_client = ensure_kube_client(state.kube_client.read().clone())?;
    kube_client
        .remove_user_from_playtest(&request.playtest, &request.user)
        .await?;

    Ok(())
}
