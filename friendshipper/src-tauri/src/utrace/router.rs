use std::fs;
use std::path::PathBuf;
use std::process::Command;

use anyhow::anyhow;
use axum::extract::{Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tracing::{info, instrument};

use ethos_core::clients::aws::ensure_aws_client;
use ethos_core::fs::LocalDownloadPath;
use ethos_core::types::errors::CoreError;
use ethos_core::types::utrace::{
    DownloadTraceRequest, OpenTraceRequest, RecentTracesResponse, TraceEntry,
};

use crate::engine::EngineProvider;
use crate::state::AppState;
use crate::APP_NAME;

const UTRACE_PREFIX: &str = "friendshipper/utrace/";

fn unreal_insights_rel_path() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        "Engine/Binaries/Win64/UnrealInsights.exe"
    }
    #[cfg(target_os = "linux")]
    {
        "Engine/Binaries/Linux/UnrealInsights"
    }
    #[cfg(target_os = "macos")]
    {
        "Engine/Binaries/Mac/UnrealInsights.app/Contents/MacOS/UnrealInsights"
    }
}

pub fn router<T>() -> Router<AppState<T>>
where
    T: EngineProvider,
{
    Router::new()
        .route("/dates", get(get_dates))
        .route("/recent", get(get_recent))
        .route("/by-date", get(get_by_date))
        .route("/download", post(download_trace))
        .route("/open", post(open_trace))
}

/// Returns UTC dates that have at least one trace, newest first.
#[instrument(skip(state))]
async fn get_dates<T>(State(state): State<AppState<T>>) -> Result<Json<Vec<String>>, CoreError>
where
    T: EngineProvider,
{
    let aws_client = ensure_aws_client(state.aws_client.read().await.clone())?;
    aws_client.check_expiration().await?;

    let raw = aws_client.list_common_prefixes(UTRACE_PREFIX, "/").await?;
    let mut dates: Vec<String> = raw
        .into_iter()
        .filter_map(|p| strip_prefix_and_trailing_slash(&p, UTRACE_PREFIX))
        .collect();
    dates.sort();
    dates.reverse();

    Ok(Json(dates))
}

#[derive(Debug, Deserialize)]
pub struct GetRecentParams {
    pub limit: Option<u32>,
    pub before: Option<String>,
}

/// Walks the date list newest-first (skipping dates >= `before`) until at least `limit`
/// traces have been collected. Returns the traces sorted by `last_modified` desc plus a
/// cursor (the oldest date pulled from) that the frontend feeds back for "load more".
#[instrument(skip(state))]
async fn get_recent<T>(
    State(state): State<AppState<T>>,
    Query(params): Query<GetRecentParams>,
) -> Result<Json<RecentTracesResponse>, CoreError>
where
    T: EngineProvider,
{
    let aws_client = ensure_aws_client(state.aws_client.read().await.clone())?;
    aws_client.check_expiration().await?;

    let limit = params.limit.unwrap_or(25).max(1) as usize;

    let raw = aws_client.list_common_prefixes(UTRACE_PREFIX, "/").await?;
    let mut dates: Vec<String> = raw
        .into_iter()
        .filter_map(|p| strip_prefix_and_trailing_slash(&p, UTRACE_PREFIX))
        .collect();
    dates.sort();
    dates.reverse();

    if let Some(ref before) = params.before {
        dates.retain(|d| d.as_str() < before.as_str());
    }

    let mut traces: Vec<TraceEntry> = Vec::new();
    let mut last_date_pulled: Option<String> = None;
    for date in &dates {
        let prefix = format!("{UTRACE_PREFIX}{date}/");
        let objs = aws_client.list_objects_with_metadata(&prefix).await?;
        for o in objs {
            if let Some(entry) = parse_trace_entry(o, date) {
                traces.push(entry);
            }
        }
        last_date_pulled = Some(date.clone());
        if traces.len() >= limit {
            break;
        }
    }

    traces.sort_by_key(|t| std::cmp::Reverse(t.last_modified));

    let exhausted = last_date_pulled
        .as_ref()
        .map(|d| dates.last().is_some_and(|last| last == d))
        .unwrap_or(true);
    let next_cursor = if exhausted { None } else { last_date_pulled };

    Ok(Json(RecentTracesResponse {
        traces,
        next_cursor,
    }))
}

#[derive(Debug, Deserialize)]
pub struct GetByDateParams {
    pub date: String,
}

#[instrument(skip(state))]
async fn get_by_date<T>(
    State(state): State<AppState<T>>,
    Query(params): Query<GetByDateParams>,
) -> Result<Json<Vec<TraceEntry>>, CoreError>
where
    T: EngineProvider,
{
    let aws_client = ensure_aws_client(state.aws_client.read().await.clone())?;
    aws_client.check_expiration().await?;

    let prefix = format!("{UTRACE_PREFIX}{}/", params.date);
    let objs = aws_client.list_objects_with_metadata(&prefix).await?;
    let mut traces: Vec<TraceEntry> = objs
        .into_iter()
        .filter_map(|o| parse_trace_entry(o, &params.date))
        .collect();
    traces.sort_by_key(|t| std::cmp::Reverse(t.last_modified));

    Ok(Json(traces))
}

#[instrument(skip(state))]
async fn download_trace<T>(
    State(state): State<AppState<T>>,
    Json(req): Json<DownloadTraceRequest>,
) -> Result<(), CoreError>
where
    T: EngineProvider,
{
    let aws_client = ensure_aws_client(state.aws_client.read().await.clone())?;
    aws_client.check_expiration().await?;

    if let Some(parent) = std::path::Path::new(&req.dest_path).parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|e| {
                CoreError::Internal(anyhow!("Failed to create destination directory: {}", e))
            })?;
        }
    }

    info!(key = %req.key, dest = %req.dest_path, "Downloading trace");
    aws_client
        .download_object_to_path(&req.dest_path, &req.key)
        .await?;
    Ok(())
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenTraceResponse {
    pub cached_path: String,
}

#[instrument(skip(state))]
async fn open_trace<T>(
    State(state): State<AppState<T>>,
    Json(req): Json<OpenTraceRequest>,
) -> Result<Json<OpenTraceResponse>, CoreError>
where
    T: EngineProvider,
{
    let engine_prebuilt_path = state.app_config.read().engine_prebuilt_path.clone();
    if engine_prebuilt_path.trim().is_empty() {
        return Err(CoreError::Internal(anyhow!(
            "Engine path is not configured. Set 'engine_prebuilt_path' in Preferences."
        )));
    }
    let insights_exe = PathBuf::from(&engine_prebuilt_path).join(unreal_insights_rel_path());
    if !insights_exe.exists() {
        return Err(CoreError::Internal(anyhow!(
            "UnrealInsights not found at {}. Verify your engine path in Preferences.",
            insights_exe.display()
        )));
    }

    let cached_path = trace_cache_path(&req.key).ok_or_else(|| {
        CoreError::Internal(anyhow!(
            "Unexpected trace key shape (expected {UTRACE_PREFIX}<date>/<server>/<file>): {}",
            req.key
        ))
    })?;

    if !cached_path.exists() {
        if let Some(parent) = cached_path.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                CoreError::Internal(anyhow!("Failed to create cache directory: {}", e))
            })?;
        }

        let aws_client = ensure_aws_client(state.aws_client.read().await.clone())?;
        aws_client.check_expiration().await?;
        info!(key = %req.key, cache = %cached_path.display(), "Downloading trace to cache");
        aws_client
            .download_object_to_path(cached_path.to_string_lossy().as_ref(), &req.key)
            .await?;
    } else {
        info!(cache = %cached_path.display(), "Using cached trace");
    }

    info!(exe = %insights_exe.display(), trace = %cached_path.display(), "Launching UnrealInsights");
    Command::new(&insights_exe)
        .arg(&cached_path)
        .spawn()
        .map_err(|e| CoreError::Internal(anyhow!("Failed to launch UnrealInsights: {}", e)))?;

    Ok(Json(OpenTraceResponse {
        cached_path: cached_path.to_string_lossy().to_string(),
    }))
}

fn strip_prefix_and_trailing_slash(s: &str, prefix: &str) -> Option<String> {
    let rest = s.strip_prefix(prefix)?;
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    if rest.is_empty() {
        None
    } else {
        Some(rest.to_string())
    }
}

fn parse_trace_entry(
    obj: ethos_core::clients::aws::S3ObjectEntry,
    date: &str,
) -> Option<TraceEntry> {
    let date_prefix = format!("{UTRACE_PREFIX}{date}/");
    let rest = obj.key.strip_prefix(&date_prefix)?;
    let mut parts = rest.splitn(2, '/');
    let server_name = parts.next()?.to_string();
    let filename = parts.next()?.to_string();
    if server_name.is_empty() || filename.is_empty() {
        return None;
    }
    Some(TraceEntry {
        date: date.to_string(),
        server_name,
        filename,
        key: obj.key,
        size: obj.size,
        last_modified: obj.last_modified,
    })
}

fn trace_cache_path(key: &str) -> Option<PathBuf> {
    let rest = key.strip_prefix(UTRACE_PREFIX)?;
    let mut parts = rest.splitn(3, '/');
    let date = parts.next()?;
    let server = parts.next()?;
    let filename = parts.next()?;
    if date.is_empty() || server.is_empty() || filename.is_empty() {
        return None;
    }
    let mut path = LocalDownloadPath::new(APP_NAME).to_path_buf();
    path.push("utrace");
    path.push(date);
    path.push(server);
    path.push(filename);
    Some(path)
}
