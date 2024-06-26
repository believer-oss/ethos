use std::{path::PathBuf, sync::mpsc::Sender as STDSender, sync::Arc};

use anyhow::{anyhow, Result};
use chrono::TimeZone;
use parking_lot::RwLock;
use tokio::sync::mpsc::Sender as MPSCSender;
use tokio::sync::RwLock as TokioRwLock;
use tracing::{debug, info, warn};

use crate::engine::EngineProvider;
use ethos_core::clients::git;
use ethos_core::clients::github;
use ethos_core::clients::kube::KubeClient;
use ethos_core::longtail::Longtail;
use ethos_core::msg::LongtailMsg;
use ethos_core::storage::ArtifactStorage;
use ethos_core::types::config::AppConfigRef;
use ethos_core::types::errors::CoreError;
use ethos_core::types::repo::RepoStatus;
use ethos_core::worker::TaskSequence;
use ethos_core::AWSClient;

use crate::config::{DynamicConfigRef, RepoConfigRef};
use crate::repo::RepoStatusRef;
use crate::system::unreal::OFPANameCache;

pub enum FrontendOp {
    ShowUI,
}

#[derive(Clone)]
pub struct AppState<T> {
    pub app_config: AppConfigRef,
    pub repo_config: RepoConfigRef,
    pub dynamic_config: DynamicConfigRef,
    pub config_file: PathBuf,
    pub storage: Arc<RwLock<Option<ArtifactStorage>>>,

    pub ofpa_cache: Arc<RwLock<OFPANameCache>>,

    pub repo_status: RepoStatusRef,

    pub longtail: Longtail,
    pub longtail_tx: STDSender<LongtailMsg>,

    pub operation_tx: MPSCSender<TaskSequence>,
    pub notification_tx: STDSender<String>,
    pub frontend_op_tx: STDSender<FrontendOp>,

    pub aws_client: Arc<TokioRwLock<Option<AWSClient>>>,
    pub kube_client: Arc<RwLock<Option<KubeClient>>>,

    pub github_client: Arc<RwLock<Option<github::GraphQLClient>>>,

    pub version: String,
    pub log_path: PathBuf,

    pub gameserver_log_tx: STDSender<String>,
    pub git_tx: STDSender<String>,

    pub engine: T,
}

impl<T> AppState<T>
where
    T: EngineProvider,
{
    #[allow(clippy::too_many_arguments)]
    pub async fn new(
        app_config: AppConfigRef,
        repo_config: RepoConfigRef,
        dynamic_config: DynamicConfigRef,
        config_file: PathBuf,
        storage: Option<ArtifactStorage>,
        longtail_tx: STDSender<LongtailMsg>,
        operation_tx: MPSCSender<TaskSequence>,
        notification_tx: STDSender<String>,
        frontend_op_tx: STDSender<FrontendOp>,
        version: String,
        aws_client: Option<AWSClient>,
        log_path: PathBuf,
        git_tx: STDSender<String>,
        server_log_tx: STDSender<String>,
    ) -> Result<Self> {
        let mut longtail = Longtail::new();

        debug!("Checking longtail");
        if longtail.exec_path.is_none() && longtail.update_exec().is_err() {
            match longtail.get_longtail(longtail_tx.clone()) {
                Ok(_) => {
                    longtail.update_exec()?;
                }
                Err(e) => {
                    return Err(anyhow!("Failed to get longtail exe. Any operations depending on longtail will fail. Reason: {}", e));
                }
            }
        }

        debug!("Creating repo status");
        let repo_status = Arc::new(RwLock::new(RepoStatus {
            last_updated: chrono::Utc
                .with_ymd_and_hms(1970, 1, 1, 0, 0, 0)
                .single()
                .unwrap(),
            ..Default::default()
        }));

        debug!("Creating GitHub client");
        let github_client = {
            let github_pat = app_config.read().github_pat.clone();

            match github_pat {
                Some(pat) => {
                    let client = match github::GraphQLClient::new(pat).await {
                        Ok(client) => Some(client),
                        Err(e) => {
                            warn!("Failed to create GitHub client: {}", e);
                            None
                        }
                    };

                    Arc::new(RwLock::new(client))
                }
                None => Arc::new(RwLock::new(None)),
            }
        };

        debug!("Creating kube client");
        let cluster_name = dynamic_config.read().clone().kubernetes_cluster_name;

        let kube_client = match aws_client.clone() {
            Some(aws_client) => {
                let kube_client =
                    KubeClient::new(&aws_client, cluster_name, Some(server_log_tx.clone())).await?;
                Arc::new(RwLock::new(Some(kube_client)))
            }
            None => Arc::new(RwLock::new(None)),
        };

        // Always initialize to the defaults, will be replaced in StatusOp if repo is set.
        debug!("Selecting default artifact project");
        let selected_artifact_project: Option<String> = match kube_client.read().clone() {
            Some(kube_client) => {
                let project = kube_client.default_project();
                Some(project.name.clone())
            }
            None => None,
        };

        {
            let mut config = app_config.write();
            config.selected_artifact_project = selected_artifact_project;
        }

        let engine = T::new_from_config(app_config.read().clone(), repo_config.read().clone());

        debug!("AppState preparation complete.");
        Ok(Self {
            app_config,
            repo_config,
            dynamic_config,
            config_file,
            storage: Arc::new(RwLock::new(storage)),
            ofpa_cache: Arc::new(RwLock::new(OFPANameCache::new())),
            repo_status,
            longtail,
            longtail_tx,
            operation_tx,
            notification_tx,
            frontend_op_tx,
            aws_client: Arc::new(TokioRwLock::new(aws_client)),
            kube_client,
            github_client,
            version,
            log_path,
            git_tx,
            gameserver_log_tx: server_log_tx,
            engine,
        })
    }

    pub fn git(&self) -> git::Git {
        let repo_path = PathBuf::from(self.app_config.read().repo_path.clone());
        git::Git::new(repo_path, self.git_tx.clone())
    }

    pub fn send_notification(&self, message: &str) {
        self.notification_tx
            .send(message.to_string())
            .expect("error sending notification");
    }

    pub fn send_git_output(&self, message: &str) {
        self.git_tx
            .send(message.to_string())
            .expect("error forwarding git log");
    }

    pub async fn replace_aws_client(&self, client: AWSClient) -> Result<(), CoreError> {
        let mut aws_client = self.aws_client.write().await;

        info!("Replacing AWS client");
        aws_client.replace(client.clone());

        let new_dynamic_config = client.get_dynamic_config().await?;

        {
            let mut dynamic_config = self.dynamic_config.write();
            *dynamic_config = new_dynamic_config.clone();
        }

        let new_kube_client = KubeClient::new(
            &client.clone(),
            new_dynamic_config.kubernetes_cluster_name,
            Some(self.gameserver_log_tx.clone()),
        )
        .await?;

        let project = new_kube_client.default_project();

        {
            let mut config = self.app_config.write();
            if config.selected_artifact_project.is_none() {
                config.selected_artifact_project = Some(project.name.clone());
            }
        }

        {
            info!("Replacing kube client");
            let mut kube_client = self.kube_client.write();
            kube_client.replace(new_kube_client);
        }

        let artifact_bucket = match &self.app_config.read().aws_config {
            Some(aws_config) => aws_config.artifact_bucket_name.clone(),
            None => {
                return Err(CoreError::from(anyhow!(
                    "No AWS config found in app config"
                )));
            }
        };

        // TODO Hardcoding for now, but this will move to dynamic_config
        let s3ap = ethos_core::storage::S3ArtifactProvider::new(&client, &artifact_bucket);
        let new_storage = Some(ArtifactStorage::new(
            Arc::new(s3ap),
            new_dynamic_config.storage_schema.clone(),
        ));

        {
            let mut storage = self.storage.write();
            *storage = new_storage;
        }

        Ok(())
    }
}
