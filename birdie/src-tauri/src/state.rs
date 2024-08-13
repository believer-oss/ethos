use std::{path::PathBuf, sync::mpsc::Sender as STDSender, sync::Arc};

use anyhow::Result;
use chrono::TimeZone;
use parking_lot::{RwLock as ParkingLotRwLock, RwLock};
use tokio::sync::mpsc::Sender as MPSCSender;
use tokio::sync::RwLock as TokioRwLock;
use tracing::{error, info};

use ethos_core::clients::{git, github};
use ethos_core::types::config::AppConfigRef;
use ethos_core::types::repo::{RepoStatus, RepoStatusRef};
use ethos_core::worker::TaskSequence;

use crate::repo::{LockCache, LockCacheRef};

pub struct AppState {
    pub app_config: AppConfigRef,
    pub config_file: PathBuf,

    pub repo_status: RepoStatusRef,
    //
    pub operation_tx: MPSCSender<TaskSequence>,

    pub github_client: Arc<ParkingLotRwLock<Option<github::GraphQLClient>>>,

    pub version: String,
    pub log_path: PathBuf,

    pub lock_cache: LockCacheRef,

    pub git_tx: STDSender<String>,
}

impl AppState {
    #[allow(clippy::too_many_arguments)]
    pub async fn new(
        app_config: AppConfigRef,
        config_file: PathBuf,
        operation_tx: MPSCSender<TaskSequence>,
        version: String,
        log_path: PathBuf,
        git_tx: STDSender<String>,
    ) -> Result<Self> {
        let repo_status = Arc::new(RwLock::new(RepoStatus {
            last_updated: chrono::Utc
                .with_ymd_and_hms(1970, 1, 1, 0, 0, 0)
                .single()
                .unwrap(),
            ..Default::default()
        }));

        let github_client = {
            let github_pat = app_config.read().github_pat.clone();

            match github_pat {
                Some(pat) => {
                    let client = github::GraphQLClient::new(pat).await?;
                    Arc::new(ParkingLotRwLock::new(Some(client)))
                }
                None => Arc::new(ParkingLotRwLock::new(None)),
            }
        };

        let lock_cache = Arc::new(TokioRwLock::new(LockCache::new(
            app_config.read().repo_path.clone(),
            git_tx.clone(),
        )));

        let writable_cache = lock_cache.clone();
        tokio::spawn(async move {
            loop {
                info!("Populating lock cache");
                {
                    let mut cache = writable_cache.write().await;
                    match cache.populate_cache().await {
                        Ok(_) => {}
                        Err(e) => {
                            error!("Error populating lock cache: {}", e);
                        }
                    }
                }

                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            }
        });

        Ok(Self {
            app_config,
            config_file,
            repo_status,
            operation_tx,
            github_client,
            version,
            log_path,
            lock_cache,
            git_tx,
        })
    }

    pub fn git(&self) -> git::Git {
        let repo_path = PathBuf::from(self.app_config.read().repo_path.clone());
        git::Git::new(repo_path, self.git_tx.clone())
    }

    pub fn send_git_output(&self, message: &str) {
        self.git_tx
            .send(message.to_string())
            .expect("error forwarding git log");
    }

    pub fn github_username(&self) -> String {
        self.github_client
            .read()
            .clone()
            .map_or(String::default(), |x| x.username.clone())
    }
}
