use std::sync::Arc;

use anyhow::anyhow;
use axum::extract::State;
use axum::routing::post;
use axum::Json;
use axum::Router;
use base64::prelude::BASE64_STANDARD;
use base64::Engine;
use reqwest::Response;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde::Serialize;

use ethos_core::types::errors::CoreError;

use crate::state::AppState;

#[derive(Serialize, Deserialize)]
pub struct GetPayload {
    #[serde(default)]
    pub key: String,
}

#[derive(Serialize, Deserialize)]
pub struct GetResponse {
    #[serde(default)]
    pub key: String,

    #[serde(default)]
    pub format: String,

    #[serde(default)]
    pub data: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PutPayload {
    #[serde(default)]
    pub key: String,

    #[serde(default)]
    pub data: String,

    #[serde(default)]
    pub format: String,
}

#[derive(Serialize, Deserialize)]
pub struct PutResponse {}

#[derive(Debug, Serialize, Deserialize)]
pub struct ListPayload {
    #[serde(default)]
    pub filter: String,
}

#[derive(Serialize, Deserialize)]
pub struct ListResponseItem {
    #[serde(default)]
    pub key: String,

    #[serde(default)]
    pub format: String,
}

#[derive(Serialize, Deserialize)]
pub struct ListResponse {
    #[serde(default)]
    pub objects: Vec<ListResponseItem>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DeletePayload {
    #[serde(default)]
    pub keys: Vec<String>,
}

const JSON_STR: &str = "json";
const LUDOS_ACCESS_KEY: &str = "x-ludos-access-key";

pub fn router(shared_state: Arc<AppState>) -> Router {
    Router::new()
        .route("/get", post(objects_get))
        .route("/put", post(objects_put))
        .route("/list", post(objects_list))
        .route("/delete", post(objects_delete))
        .with_state(shared_state)
}

async fn objects_get(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<GetPayload>,
) -> Result<Json<GetResponse>, CoreError> {
    let dynamic_config = state.dynamic_config.read().clone();
    if dynamic_config.ludos_access_secret.is_empty() {
        return Err(CoreError(anyhow!("Ludos access secret is not set")));
    };

    let endpoint = state.app_config.read().ludos_endpoint();
    let client = reqwest::Client::new();
    let res = client
        .post(format!("{}/prototype/objects/v1/get", endpoint))
        .header(LUDOS_ACCESS_KEY, &dynamic_config.ludos_access_secret)
        .json(&payload)
        .send()
        .await;

    let mut get_data = handle_result::<GetResponse>(res).await?;
    let maybe_decoded: Option<String> = match BASE64_STANDARD.decode(&get_data.data) {
        Err(e) => {
            tracing::error!("Got invalid data from Ludos. base64 decode error: {}", e);
            None
        }
        Ok(decoded_bytes) => {
            match String::from_utf8(decoded_bytes) {
                Ok(decoded_string) => Some(decoded_string),
                Err(e) => {
                    tracing::error!("Got invalid data from Ludos. Data was base64 encoded, but not valid UTF8: {}", e);
                    None
                }
            }
        }
    };

    if let Some(decoded) = maybe_decoded {
        get_data.data = decoded;
    } else {
        return Err(CoreError(anyhow!(
            "Got invalid data from Ludos server. Check log for details."
        )));
    }

    Ok(get_data)
}

async fn objects_put(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<PutPayload>,
) -> Result<Json<PutResponse>, CoreError> {
    let dynamic_config = state.dynamic_config.read().clone();
    if dynamic_config.ludos_access_secret.is_empty() {
        return Err(CoreError(anyhow!("Ludos access secret is not set")));
    };

    let data: String = BASE64_STANDARD.encode(&payload.data);
    let body = PutPayload {
        key: payload.key.clone(),
        data,
        format: JSON_STR.to_string(),
    };

    let endpoint = state.app_config.read().ludos_endpoint();
    let client = reqwest::Client::new();
    let res = client
        .post(format!("{}/prototype/objects/v1/put", endpoint))
        .header(LUDOS_ACCESS_KEY, &dynamic_config.ludos_access_secret)
        .json(&body)
        .send()
        .await;

    handle_result::<PutResponse>(res).await
}

async fn objects_list(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<ListPayload>,
) -> Result<Json<ListResponse>, CoreError> {
    let dynamic_config = state.dynamic_config.read().clone();
    if dynamic_config.ludos_access_secret.is_empty() {
        return Err(CoreError(anyhow!("Ludos access secret is not set")));
    };

    let endpoint = state.app_config.read().ludos_endpoint();
    let client = reqwest::Client::new();
    let res = client
        .post(format!("{}/prototype/objects/v1/list", endpoint))
        .header(LUDOS_ACCESS_KEY, &dynamic_config.ludos_access_secret)
        .json(&payload)
        .send()
        .await;

    handle_result::<ListResponse>(res).await
}

async fn objects_delete(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<DeletePayload>,
) -> Result<Json<DeletePayload>, CoreError> {
    let dynamic_config = state.dynamic_config.read().clone();
    if dynamic_config.ludos_access_secret.is_empty() {
        return Err(CoreError(anyhow!("Ludos access secret is not set")));
    };

    let endpoint = state.app_config.read().ludos_endpoint();
    let client = reqwest::Client::new();
    let res = client
        .post(format!("{}/prototype/objects/v1/delete", endpoint))
        .header(LUDOS_ACCESS_KEY, &dynamic_config.ludos_access_secret)
        .json(&payload)
        .send()
        .await;

    handle_result::<DeletePayload>(res).await
}

async fn handle_result<T>(
    maybe_result: Result<Response, reqwest::Error>,
) -> Result<Json<T>, CoreError>
where
    T: DeserializeOwned,
{
    if let Err(e) = maybe_result {
        return Err(CoreError(anyhow!("Failed sending request: {}", e)));
    }

    let res = maybe_result.unwrap();

    if res.status().is_client_error() {
        let body = res.text().await?;
        return Err(CoreError(anyhow!("Failed request: {}", body)));
    }

    match res.json::<T>().await {
        Err(e) => Err(CoreError(anyhow!("Failed reading response body: {}", e))),
        Ok(body) => Ok(axum::Json(body)),
    }
}
