use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::anyhow;
use axum::extract::State;
use axum::{debug_handler, Json};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock as TokioRwLock;
use tracing::{error, info, instrument, warn};
use walkdir::WalkDir;

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

    #[instrument(name = "LockCache::populate_cache", skip(self))]
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
#[instrument(skip(state))]
pub async fn lock_files(
    State(state): State<Arc<AppState>>,
    Json(request): Json<LockRequest>,
) -> Result<Json<LockResponse>, CoreError> {
    // repopulate lock cache
    {
        let mut lock_cache = state.lock_cache.write().await;
        lock_cache.populate_cache().await?;
    }
    info!("lock request: {:?}", request);

    let mut paths: Vec<String> = Vec::new();
    for path in request.paths.clone() {
        if let Some(lock) = state
            .repo_status
            .read()
            .locks_theirs
            .iter()
            .find(|l| l.path == path)
        {
            // do not attempt to lock any files owned by other users, instead log an error and abort
            if let Some(owner) = &lock.owner {
                warn!(
                    "Locking failed: file {} is already checked out by {}",
                    path, owner.name
                );
                return Err(CoreError::Internal(anyhow!(
                    "Failed to lock a file checked out by {}. Check the log for more details.",
                    owner.name,
                )));
            }
        } else if state
            .repo_status
            .read()
            .modified_upstream
            .iter()
            .any(|p| p == &path)
        {
            // do not attempt to lock any files modified upstream by other users, instead log an error and abort
            warn!(
                "Locking failed: files are modified upstream by other users. Sync and try again."
            );
            return Err(CoreError::Internal(anyhow!(
                "Files are modified upstream by other users. Sync and try again."
            )));
        } else {
            paths.push(path);
        }
    }

    let request_data = LockRequest {
        paths: paths.clone(),
        force: request.force,
    };

    let resp = internal_lock_handler(state.clone(), request_data, LockOperation::Lock).await?;

    let repo_path = PathBuf::from(state.app_config.read().repo_path.clone());
    for path in paths {
        // append path to repo dir
        let path = PathBuf::from(path);
        let full_path = repo_path.join(path);

        // set readonly
        match full_path.try_exists() {
            Ok(exists) => {
                if exists {
                    let mut perms = full_path.metadata()?.permissions();
                    #[allow(clippy::permissions_set_readonly_false)]
                    perms.set_readonly(false);

                    fs::set_permissions(full_path, perms)?;
                }
            }
            Err(e) => {
                error!(
                    "Failed to check existence of path {:?} for readonly flag: {}",
                    &full_path, e
                );
            }
        }
    }

    // repopulate lock cache
    {
        let mut lock_cache = state.lock_cache.write().await;
        lock_cache.populate_cache().await?;
    }

    Ok(resp)
}

#[debug_handler]
#[instrument(skip(state))]
pub async fn unlock_files(
    State(state): State<Arc<AppState>>,
    Json(request): Json<LockRequest>,
) -> Result<Json<LockResponse>, CoreError> {
    // repopulate lock cache
    {
        let mut lock_cache = state.lock_cache.write().await;
        lock_cache.populate_cache().await?;
    }

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

#[instrument(skip(state))]
async fn internal_lock_handler(
    state: Arc<AppState>,
    request: LockRequest,
    op: LockOperation,
) -> Result<Json<LockResponse>, CoreError> {
    let github_pat = state
        .app_config
        .read()
        .github_pat
        .clone()
        .ok_or(CoreError::Internal(anyhow!(
            "GitHub PAT is not configured. Please configure it in the settings."
        )))?;

    // make a new vec of paths
    // for each path in self.paths, if path is a directory, add all files in the directory recursively to the vec
    // if path is a file, add the file to the vec
    let git_client = state.git();
    let repo_path = git_client
        .repo_path
        .to_str()
        .expect("was the git client passed an invalid repo path?");
    let mut paths = vec![];
    for path in &request.paths {
        let full_path = std::path::Path::new(repo_path).join(path);
        if full_path.is_dir() {
            for entry in WalkDir::new(full_path) {
                let entry = entry?;
                if entry.file_type().is_file() {
                    // remove repo dir from path
                    let entry = entry.path().strip_prefix(repo_path)?;
                    paths.push(entry.to_string_lossy().to_string().replace('\\', "/"));
                }
            }
        } else {
            paths.push(path.to_string());
        }
    }

    // if we're locking, retain any paths that are unlocked
    // if we're unlocking, retain any paths that are locked by us
    {
        let lock_cache = state.lock_cache.read().await;
        paths.retain(|path| {
            if let Some(entry) = lock_cache.get(path) {
                // Unlocking and ours
                if op == LockOperation::Unlock {
                    return entry.ours;
                }

                false
            } else {
                // Locking and unlocked
                op == LockOperation::Lock
            }
        });
    }

    let github_username = state.github_username();

    let lock_op = {
        LockOp {
            git_client: state.git(),
            paths,
            op,
            response_tx: None,
            github_pat: github_pat.to_string(),
            repo_status: state.repo_status.clone(),
            github_username,
            force: request.force,
        }
    };

    match lock_op.run().await {
        Ok(response) => Ok(Json(response)),
        Err(e) => Err(CoreError::Internal(anyhow!(
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
        Err(e) => Err(CoreError::Internal(anyhow!(
            "Error fetching locks: {}",
            e.to_string()
        ))),
    }
}
