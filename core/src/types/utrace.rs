use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceEntry {
    pub date: String,
    pub server_name: String,
    pub filename: String,
    pub key: String,
    pub size: i64,
    pub last_modified: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecentTracesResponse {
    pub traces: Vec<TraceEntry>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadTraceRequest {
    pub key: String,
    pub dest_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenTraceRequest {
    pub key: String,
}
