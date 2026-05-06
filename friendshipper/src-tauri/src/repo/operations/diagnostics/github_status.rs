use anyhow::anyhow;
use axum::Json;
use serde::Deserialize;
use std::time::Duration;

use ethos_core::types::errors::CoreError;
use ethos_core::types::github::status::GitHubStatusResponse;

const GITHUB_STATUS_URL: &str = "https://www.githubstatus.com/api/v2/status.json";
const GITHUB_STATUS_PAGE_URL: &str = "https://www.githubstatus.com";

#[derive(Deserialize)]
struct StatuspageResponse {
    status: StatuspageStatus,
}

#[derive(Deserialize)]
struct StatuspageStatus {
    indicator: String,
    description: String,
}

pub async fn github_status_handler() -> Result<Json<GitHubStatusResponse>, CoreError> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .connect_timeout(Duration::from_secs(5))
        .build()
        .map_err(|e| CoreError::Internal(anyhow!("Failed to build status client: {}", e)))?;

    let resp =
        client.get(GITHUB_STATUS_URL).send().await.map_err(|e| {
            CoreError::Internal(anyhow!("Failed to reach GitHub status page: {}", e))
        })?;

    let body: StatuspageResponse = resp.json().await.map_err(|e| {
        CoreError::Internal(anyhow!("Failed to parse GitHub status response: {}", e))
    })?;

    Ok(Json(GitHubStatusResponse {
        indicator: body.status.indicator,
        description: body.status.description,
        url: GITHUB_STATUS_PAGE_URL.to_string(),
    }))
}
