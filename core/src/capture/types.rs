//! Capture ledger schema, session interface types and UI status types.

use std::collections::BTreeMap;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

fn one() -> u32 {
    1
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CaptureLedger {
    #[serde(default = "one")]
    pub version: u32,
    #[serde(default)]
    pub sessions: Vec<CaptureSession>,
    /// Key is the lowercase hex SHA-256 of the file.
    #[serde(default)]
    pub uploaded: BTreeMap<String, UploadedEntry>,
    #[serde(default)]
    pub inflight: Vec<InflightUpload>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CaptureSession {
    pub id: String,
    pub install_dir: PathBuf,
    pub sha: Option<String>,
    pub user: String,
    pub playtest: String,
    pub launched_at: DateTime<Utc>,
    pub state: SessionState,
    pub exited_at: Option<DateTime<Utc>>,
    pub closed_at: Option<DateTime<Utc>>,
    pub close_reason: Option<CloseReason>,
    #[serde(skip)]
    pub empty_polls: u8,
    #[serde(skip)]
    pub first_empty_at: Option<DateTime<Utc>>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    Launched,
    Running,
    Exited,
    Closed,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloseReason {
    Uploaded,
    Cancelled,
    Expired,
    Wiped,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UploadedEntry {
    pub key: String,
    pub bucket: String,
    pub local_path: PathBuf,
    pub size: u64,
    /// File mtime in ms since epoch.
    pub mtime_ms: i64,
    pub uploaded_at: DateTime<Utc>,
    pub session_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InflightUpload {
    pub session_id: String,
    pub local_path: PathBuf,
    pub size: u64,
    pub mtime_ms: i64,
    pub sha256: String,
    pub bucket: String,
    /// Fixed at creation so the UTC date in it never changes on resume.
    pub key: String,
    /// None for single-PutObject files.
    pub upload_id: Option<String>,
    pub part_size: u64,
    pub started_at: DateTime<Utc>,
    pub attempts: u32,
    pub next_attempt_at: Option<DateTime<Utc>>,
}

pub struct RegisterSession {
    pub install_dir: PathBuf,
    pub sha: Option<String>,
    pub user: String,
    pub playtest: String,
    pub launched_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingSummary {
    pub session_ids: Vec<String>,
    pub playtests: Vec<String>,
    pub files: u32,
    pub bytes: u64,
    pub game_running: bool,
    pub uploading: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureStatus {
    pub sessions: Vec<SessionStatus>,
    pub pause: PauseLevelStatus,
    pub blocked_reason: Option<String>,
    pub persist_failed: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionStatus {
    pub id: String,
    pub playtest: String,
    pub state: SessionState,
    pub launched_at: DateTime<Utc>,
    pub exited_at: Option<DateTime<Utc>>,
    pub files: Vec<FileStatus>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileStatus {
    pub name: String,
    pub kind: FileKind,
    pub size: u64,
    pub state: FileState,
    pub uploaded_bytes: u64,
    pub bytes_per_sec: Option<u64>,
    pub message: Option<String>,
    pub next_attempt_at: Option<DateTime<Utc>>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FileKind {
    Trace,
    Log,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FileState {
    Waiting,
    Queued,
    Hashing,
    Uploading,
    Paused,
    Retrying,
    Uploaded,
    Failed,
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PauseLevelStatus {
    #[default]
    None,
    Soft,
    Hard,
    Blocked,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> CaptureSession {
        CaptureSession {
            id: "1700000000000-abcdef".to_string(),
            install_dir: PathBuf::from("install"),
            sha: Some("abc123".to_string()),
            user: "tester".to_string(),
            playtest: "pt".to_string(),
            launched_at: DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
            state: SessionState::Running,
            exited_at: None,
            closed_at: None,
            close_reason: None,
            empty_polls: 3,
            first_empty_at: None,
        }
    }

    #[test]
    fn ledger_round_trip() {
        let mut ledger = CaptureLedger {
            version: 1,
            sessions: vec![session()],
            ..Default::default()
        };
        ledger.uploaded.insert(
            "deadbeef".to_string(),
            UploadedEntry {
                key: "k".to_string(),
                bucket: "b".to_string(),
                local_path: PathBuf::from("f.log"),
                size: 10,
                mtime_ms: 5,
                uploaded_at: DateTime::from_timestamp(1_700_000_100, 0).unwrap(),
                session_id: "s".to_string(),
            },
        );
        ledger.inflight.push(InflightUpload {
            session_id: "s".to_string(),
            local_path: PathBuf::from("f.utrace"),
            size: 99,
            mtime_ms: 7,
            sha256: "ff".to_string(),
            bucket: "b".to_string(),
            key: "k2".to_string(),
            upload_id: Some("u".to_string()),
            part_size: 8 << 20,
            started_at: DateTime::from_timestamp(1_700_000_200, 0).unwrap(),
            attempts: 2,
            next_attempt_at: None,
        });

        let json = serde_json::to_string_pretty(&ledger).unwrap();
        assert!(json.contains("\"launched_at\""));
        let back: CaptureLedger = serde_json::from_str(&json).unwrap();
        assert_eq!(back.version, 1);
        assert_eq!(back.sessions.len(), 1);
        assert_eq!(back.sessions[0].id, ledger.sessions[0].id);
        assert_eq!(back.sessions[0].state, SessionState::Running);
        assert_eq!(back.sessions[0].empty_polls, 0);
        assert_eq!(back.uploaded["deadbeef"].size, 10);
        assert_eq!(back.inflight[0].upload_id.as_deref(), Some("u"));
        assert_eq!(back.inflight[0].attempts, 2);
    }

    #[test]
    fn missing_version_defaults_to_one() {
        let ledger: CaptureLedger = serde_json::from_str("{}").unwrap();
        assert_eq!(ledger.version, 1);
        assert!(ledger.sessions.is_empty());
    }

    #[test]
    fn session_state_is_snake_case() {
        assert_eq!(
            serde_json::to_string(&SessionState::Launched).unwrap(),
            "\"launched\""
        );
        assert_eq!(
            serde_json::to_string(&CloseReason::Cancelled).unwrap(),
            "\"cancelled\""
        );
    }
}
