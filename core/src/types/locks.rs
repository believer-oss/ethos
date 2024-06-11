use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct OwnerInfo {
    pub name: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RefInfo {
    pub name: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Lock {
    // git lfs locks --verify --json outputs an object with these fields
    pub id: String,
    pub path: String,
    pub locked_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<OwnerInfo>,

    // these fields are our own additions, so we have to wrap them in Option to make sure serialization from the json still works
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ListLocksResponse {
    pub locks: Vec<Lock>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct VerifyLocksRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    pub limit: Option<u64>,

    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "ref")]
    pub ref_info: Option<RefInfo>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VerifyLocksResponse {
    pub ours: Vec<Lock>,
    pub theirs: Vec<Lock>,

    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "nextCursor")]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum LockOperation {
    Lock,
    Unlock,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ForceUnlock {
    True,
    False,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockFailure {
    pub path: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockResponseInner {
    pub paths: Vec<String>,
    pub failures: Vec<LockFailure>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockResponse {
    pub batch: LockResponseInner,
}

impl Default for LockResponse {
    fn default() -> Self {
        LockResponse {
            batch: LockResponseInner {
                paths: vec![],
                failures: vec![],
            },
        }
    }
}
