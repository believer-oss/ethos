use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::anyhow;
use axum::extract::State;
use axum::{debug_handler, Json};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock as TokioRwLock;
use tracing::warn;

use ethos_core::clients::git;
use ethos_core::clients::git::Opts;
use ethos_core::operations::LockOp;
use ethos_core::types::errors::CoreError;
use ethos_core::types::locks::{Lock, LockOperation, LockResponse, VerifyLocksResponse};
use ethos_core::types::repo::LockRequest;

use crate::state::AppState;

pub type LockCacheRef = Arc<TokioRwLock<LockCache>>;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LockCacheEntry {
    lock: Lock,
    ours: bool,
}

#[derive(Debug)]
pub struct LockCache {
    inner: HashMap<String, LockCacheEntry>,
    repo_path: String,
    git_tx: std::sync::mpsc::Sender<String>,
}

impl LockCache {
    pub fn new(repo_path: String, git_tx: std::sync::mpsc::Sender<String>) -> Self {
        Self {
            inner: HashMap::new(),
            repo_path,
            git_tx,
        }
    }

    pub fn git(&self) -> git::Git {
        let repo_path = PathBuf::from(self.repo_path.clone());
        git::Git::new(repo_path, self.git_tx.clone())
    }

    pub fn insert(&mut self, lock: Lock, ours: bool) {
        self.inner
            .insert(lock.path.clone(), LockCacheEntry { lock, ours });
    }

    pub fn get(&self, id: &str) -> Option<&LockCacheEntry> {
        self.inner.get(id)
    }

    pub fn remove(&mut self, id: &str) -> Option<LockCacheEntry> {
        self.inner.remove(id)
    }

    pub fn set_repo_path(&mut self, repo_path: String) {
        self.repo_path = repo_path;
    }

    pub async fn populate_cache(&mut self) -> Result<(), anyhow::Error> {
        if self.repo_path.is_empty() {
            warn!("No repo path currently configured, skipping lock cache population.");

            return Ok(());
        }

        let output = self
            .git()
            .run_and_collect_output(
                &["lfs", "locks", "--verify", "--json"],
                Opts::new_without_logs(),
            )
            .await?;

        let output: VerifyLocksResponse = serde_json::from_str(&output)?;

        output.ours.iter().for_each(|lock| {
            self.insert(lock.clone(), true);
        });

        output.theirs.iter().for_each(|lock| {
            self.insert(lock.clone(), false);
        });

        // remove any cache entry not in the output
        self.inner.retain(|k, _| {
            output.ours.iter().any(|lock| lock.path == *k)
                || output.theirs.iter().any(|lock| lock.path == *k)
        });

        Ok(())
    }
}

#[debug_handler]
pub async fn lock_files(
    State(state): State<Arc<AppState>>,
    Json(request): Json<LockRequest>,
) -> Result<Json<LockResponse>, CoreError> {
    let resp = internal_lock_handler(state.clone(), request.clone(), LockOperation::Lock).await?;

    let repo_path = PathBuf::from(state.app_config.read().repo_path.clone());
    for path in &request.paths {
        // append path to repo dir
        let path = PathBuf::from(path);
        let full_path = repo_path.join(path);

        // set readonly
        let mut perms = full_path.metadata()?.permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);

        fs::set_permissions(full_path, perms)?;
    }

    // repopulate lock cache
    {
        let mut lock_cache = state.lock_cache.write().await;
        lock_cache.populate_cache().await?;
    }

    Ok(resp)
}

#[debug_handler]
pub async fn unlock_files(
    State(state): State<Arc<AppState>>,
    Json(request): Json<LockRequest>,
) -> Result<Json<LockResponse>, CoreError> {
    let resp = internal_lock_handler(state.clone(), request.clone(), LockOperation::Unlock).await?;

    let repo_path = PathBuf::from(state.app_config.read().repo_path.clone());
    for path in &request.paths {
        // append path to repo dir
        let path = PathBuf::from(path);
        let full_path = repo_path.join(path);

        // set readonly
        let mut perms = full_path.metadata()?.permissions();
        perms.set_readonly(true);

        fs::set_permissions(full_path, perms)?;
    }

    // repopulate lock cache
    {
        let mut lock_cache = state.lock_cache.write().await;
        lock_cache.populate_cache().await?;
    }

    Ok(resp)
}

async fn internal_lock_handler(
    state: Arc<AppState>,
    request: LockRequest,
    op: LockOperation,
) -> Result<Json<LockResponse>, CoreError> {
    let github_pat = state.app_config.read().ensure_github_pat()?;

    let lock_op = {
        LockOp {
            git_client: state.git(),
            paths: request.paths,
            op,
            github_pat,
            force: request.force,
        }
    };

    match lock_op.run().await {
        Ok(response) => Ok(Json(response)),
        Err(e) => Err(CoreError(anyhow!(
            "Error executing lock op: {}",
            e.to_string()
        ))),
    }
}

pub async fn verify_locks_handler(
    State(state): State<Arc<AppState>>,
) -> Result<Json<VerifyLocksResponse>, CoreError> {
    match state.git().verify_locks().await {
        Ok(output) => Ok(Json(output)),
        Err(e) => Err(CoreError(anyhow!(
            "Error fetching locks: {}",
            e.to_string()
        ))),
    }
}
