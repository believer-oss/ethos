use std::collections::BTreeMap;
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use futures::{AsyncBufReadExt, TryStreamExt};
use json_patch::Patch as JsonPatch;
use k8s_openapi::api::core::v1::{ConfigMap, Pod};
use kube::api::{LogParams, Patch, PatchParams};
use kube::{
    api::{DeleteParams, ListParams, PostParams},
    Api, Client,
};
use rand::{distributions::Alphanumeric, Rng};
use serde_json::json;
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tracing::{debug, error, info, instrument};

use crate::clients::argo::ArgoClient;
use crate::types::argo::workflow::Workflow;
use crate::types::errors::CoreError;
use crate::types::gameserver::{GameServer, GameServerResults, GameServerSpec};
use crate::types::playtests::{
    CreatePlaytestRequest, Group, GroupFullError, Playtest, UpdatePlaytestRequest,
};
use crate::types::project::ProjectConfig;
use crate::utils::junit::JunitOutput;
use crate::{AWSClient, KUBE_SHA_LABEL_KEY};

static SHA_LABEL_KEY: &str = KUBE_SHA_LABEL_KEY;

#[derive(Clone, Debug)]
pub struct KubeClient {
    aws_creds: AWSClient,
    aws_creds_expire_at: Arc<RwLock<Option<DateTime<Utc>>>>,
    kubeconfig: Arc<RwLock<KubeConfig>>,
    argo_client: Arc<RwLock<ArgoClient>>,

    default_project: ProjectConfig,

    log_tail_handle: Arc<RwLock<Option<JoinHandle<()>>>>,
    log_tx: Option<Sender<String>>,
    region: String,
    cluster_name: String,
}

#[derive(Debug, Clone)]
pub struct KubeConfig {
    kubeconfig: kube::Config,
    expires_at: Instant,
    last_retry_time: Option<Instant>,
}

impl KubeClient {
    #[instrument(skip(aws_creds, log_tx))]
    pub async fn new(
        aws_creds: &AWSClient,
        cluster_name: String,
        region: String,
        log_tx: Option<Sender<String>>,
    ) -> Result<Self> {
        debug!("Checking AWS credentials");
        let _ = aws_creds.check_expiration().await;

        let aws_creds_expire_at = aws_creds.get_credential_expiration().await;
        debug!("AWS credentials expire at: {:?}", aws_creds_expire_at);

        let aws_creds = aws_creds.clone();

        debug!("Getting EKS cluster info");
        let (eks_cluster_url, eks_cluster_cert) =
            match aws_creds.eks_k8s_cluster_info(&cluster_name, &region).await {
                Ok(v) => v,
                Err(e) => {
                    return Err(anyhow!("Error getting EKS cluster info: {:?}", e));
                }
            };

        debug!("Generating k8s token");
        let token = match aws_creds.generate_k8s_token(&cluster_name, &region).await {
            Ok(token) => token,
            Err(e) => {
                return Err(anyhow!("Error generating k8s token: {:?}", e));
            }
        };

        debug!("Creating kubeconfig");
        let kubeconfig = kube::Config {
            cluster_url: eks_cluster_url,
            default_namespace: "game-servers".to_string(),
            auth_info: kube::config::AuthInfo {
                token: Some(token.clone().into()),
                ..Default::default()
            },
            root_cert: Some(eks_cluster_cert),
            accept_invalid_certs: false,
            connect_timeout: Some(Duration::from_secs(30)),
            read_timeout: Some(Duration::from_secs(295)),
            write_timeout: None,
            proxy_url: None,
            tls_server_name: None,
        };

        let project = initialize_project_config(kubeconfig.clone()).await?;

        debug!("Creating Argo client");
        let argo_client = match project.argo.clone() {
            Some(argo) => ArgoClient::new(argo.server_url, token, argo.namespace),
            None => {
                return Err(anyhow!("Argo configuration not found"));
            }
        };

        debug!("Creating KubeClient");
        Ok(KubeClient {
            aws_creds,
            aws_creds_expire_at: Arc::new(RwLock::new(aws_creds_expire_at)),
            kubeconfig: Arc::new(RwLock::new(KubeConfig {
                kubeconfig,
                expires_at: Instant::now() + Duration::from_secs(600),
                last_retry_time: None,
            })),
            argo_client: Arc::new(RwLock::new(argo_client)),
            default_project: project,
            log_tail_handle: Arc::new(RwLock::new(None)),
            log_tx,
            region,
            cluster_name,
        })
    }

    #[instrument(skip_all)]
    pub async fn kubeconfig(&self) -> Result<kube::Config, CoreError> {
        self.aws_creds.check_expiration().await?;

        let new_aws_creds_expire_at = self.aws_creds.get_credential_expiration().await;

        let now = Instant::now();

        let mut expired = false;
        {
            let lock = self.kubeconfig.read().await;
            debug!(
                "Checking expires_at:[{:?}] >= now:[{:?}]",
                lock.expires_at, now
            );
            if now >= lock.expires_at {
                expired = true;
            }
        }

        if expired || (new_aws_creds_expire_at != *self.aws_creds_expire_at.read().await) {
            let mut aws_creds_expire_at = self.aws_creds_expire_at.write().await;

            info!("Refreshing credentials, kubeconfig expired: {}, last aws_creds_expire_at: {:?}, new aws_creds_expire_at: {:?}", expired, *aws_creds_expire_at, new_aws_creds_expire_at);

            self.refresh_token(false).await;

            if new_aws_creds_expire_at != *aws_creds_expire_at {
                info!("Caching new AWS creds expiration for kubeconfig.");
                *aws_creds_expire_at = new_aws_creds_expire_at;
            }
        }

        Ok(self.kubeconfig.read().await.kubeconfig.clone())
    }

    #[instrument(skip(self))]
    pub async fn refresh_token(&self, retrying: bool) {
        info!(
            "Attempting to acquire lock on kubeconfig. Retry: {}",
            retrying
        );

        let mut kubeconfig = self.kubeconfig.write().await;

        if retrying {
            if let Some(last_auth_error_time) = kubeconfig.last_retry_time {
                let now = Instant::now();
                let elapsed = now - last_auth_error_time;
                if elapsed < Duration::from_secs(60) {
                    debug!(
                        "Last auth error was less than 60 seconds ago, not retrying. elapsed: {:?}",
                        elapsed
                    );
                    return;
                }
            }
            info!("Attempting to refresh auth token due to 401");
            kubeconfig.last_retry_time = Some(Instant::now());
        }

        let token = match self
            .aws_creds
            .generate_k8s_token(&self.cluster_name, &self.region)
            .await
        {
            Ok(token) => token,
            Err(e) => {
                error!("Error generating k8s token: {:?}", e);
                return;
            }
        };

        info!("Refreshed token");
        debug!("token: {:#?}", token);

        {
            let mut argo_client = self.argo_client.write().await;
            argo_client.set_auth(token.clone());
        }

        kubeconfig.kubeconfig.auth_info.token = Some(token.into());
        kubeconfig.expires_at = Instant::now() + Duration::from_secs(600);
    }

    pub async fn handle_kube_error(&self, e: kube::Error) -> CoreError {
        match e {
            kube::Error::Api(ae) => {
                info!("API error {}: {}", ae.code, ae.message);

                if ae.code == 401 {
                    info!("Got 401, refreshing token");
                    self.refresh_token(true).await;

                    return CoreError::Unauthorized;
                }

                CoreError::Internal(anyhow::Error::new(ae))
            }
            _ => CoreError::Internal(anyhow::Error::new(e)),
        }
    }

    #[instrument(skip(self))]
    pub async fn list_gameservers(
        &self,
        sha: Option<String>,
    ) -> Result<Vec<GameServerResults>, CoreError> {
        let client = Client::try_from(self.kubeconfig().await?).unwrap();
        let mut lp = ListParams::default();
        let api: Api<GameServer> = Api::default_namespaced(client);

        if let Some(sha) = sha {
            lp = lp.labels(format!("{SHA_LABEL_KEY}={sha}").as_str());
        }

        match api.list(&lp).await {
            Ok(res) => Ok(res
                .items
                .iter()
                .map(|i| {
                    let (ip, port, netimgui_port) = match &i.status {
                        Some(v) => (v.ip.clone(), v.port, v.netimgui_port),
                        None => (Some("Missing".to_string()), 0, 0),
                    };

                    GameServerResults {
                        name: i.metadata.name.clone().unwrap(),
                        display_name: match i.spec.display_name.clone() {
                            Some(name) => name,
                            None => i.metadata.name.clone().unwrap(),
                        },
                        ip,
                        port,
                        netimgui_port,
                        version: i.spec.version.clone(),
                        creation_timestamp: i.metadata.creation_timestamp.clone().unwrap(),
                    }
                })
                .collect::<Vec<GameServerResults>>()),
            Err(e) => Err(self.handle_kube_error(e).await),
        }
    }

    #[instrument(skip(self))]
    pub async fn create_gameserver_for_sha(
        &self,
        sha: String,
        check_for_existing: bool,
        display_name: &str,
        map: Option<String>,
    ) -> Result<GameServer, CoreError> {
        let suffix: String = rand::thread_rng()
            .sample_iter(&Alphanumeric)
            .take(8)
            .map(char::from)
            .collect();

        let name = format!("{sha}-{suffix}");
        // v1 uses the full 40-char sha and no linux-server- prepend.
        let tag = match sha.len() {
            40 => sha.clone(),
            _ => format!("linux-server-{sha}"),
        };

        let gameserver: GameServer = GameServer {
            metadata: kube::api::ObjectMeta {
                name: Some(name.to_lowercase()),
                namespace: Some("game-servers".to_string()),
                labels: Some({
                    let mut labels = BTreeMap::new();
                    labels.insert(SHA_LABEL_KEY.to_string(), sha.clone());
                    labels
                }),
                ..Default::default()
            },
            spec: GameServerSpec {
                display_name: Some(display_name.to_string()),
                version: tag,
                map,
            },
            status: None,
        };

        let client = Client::try_from(self.kubeconfig().await?).unwrap();
        let api: Api<GameServer> = Api::default_namespaced(client);

        if check_for_existing {
            let lp = ListParams::default().labels(format!("{SHA_LABEL_KEY}={sha}").as_str());

            match api.list(&lp).await {
                Ok(res) => {
                    let empty = &res.items.is_empty();
                    if !empty {
                        info!("Found an existing gameserver. Not creating a new one.");
                        return Ok(res.items.first().unwrap().to_owned());
                    }
                }
                Err(e) => return Err(self.handle_kube_error(e).await),
            }
        }

        match api.create(&PostParams::default(), &gameserver).await {
            Ok(_) => Ok(gameserver),
            Err(e) => Err(self.handle_kube_error(e).await),
        }
    }

    #[instrument(skip(self))]
    pub async fn get_gameserver(&self, name: &str) -> Result<GameServer, CoreError> {
        let client = Client::try_from(self.kubeconfig().await?).unwrap();
        let api: Api<GameServer> = Api::default_namespaced(client);

        match api.get(name).await {
            Ok(res) => Ok(res),
            Err(e) => Err(self.handle_kube_error(e).await),
        }
    }

    #[instrument(skip(self))]
    pub async fn delete_gameserver(&self, name: &str) -> Result<(), CoreError> {
        let params = DeleteParams::default();

        let client = Client::try_from(self.kubeconfig().await?).unwrap();
        let api: Api<GameServer> = Api::default_namespaced(client);

        match api.delete(name, &params).await {
            Ok(_) => Ok(()),
            Err(e) => Err(self.handle_kube_error(e).await),
        }
    }

    #[instrument(skip(self))]
    pub async fn get_logs_for_gameserver(
        &self,
        name: &str,
        previous: bool,
    ) -> Result<Option<String>, CoreError> {
        let client = Client::try_from(self.kubeconfig().await?).unwrap();
        let api: Api<Pod> = Api::default_namespaced(client);

        let lp = match previous {
            true => LogParams {
                previous: true,
                ..Default::default()
            },
            false => LogParams::default(),
        };

        match api.logs(name, &lp).await {
            Ok(res) => Ok(Some(res)),
            Err(e) => {
                if previous
                    && !e.to_string().contains(
                        format!(
                            "previous terminated container \"game-server\" in pod \"{}\" not found",
                            &name
                        )
                        .as_str(),
                    )
                {
                    Err(self.handle_kube_error(e).await)
                } else {
                    Ok(None)
                }
            }
        }
    }

    #[instrument(skip(self))]
    pub async fn tail_logs_for_gameserver(&self, name: &str) -> Result<(), CoreError> {
        let client = Client::try_from(self.kubeconfig().await?).unwrap();
        let api: Api<Pod> = Api::default_namespaced(client);

        let lp = LogParams {
            follow: true,
            since_seconds: Some(30),
            ..Default::default()
        };

        let mut logs = api.log_stream(name, &lp).await?.lines();

        let tx = self.log_tx.clone();
        let handle = tokio::spawn(async move {
            while let Some(line) = logs.try_next().await.unwrap() {
                if let Some(tx) = &tx {
                    tx.send(line).unwrap();
                }
            }
        });

        let mut lock = self.log_tail_handle.write().await;
        *lock = Some(handle);

        Ok(())
    }

    #[instrument(skip(self))]
    pub async fn stop_tail(&self) {
        let mut lock = self.log_tail_handle.write().await;
        if let Some(handle) = lock.take() {
            handle.abort();
        }
    }

    #[instrument(skip(self))]
    pub async fn get_playtests(&self) -> Result<Vec<Playtest>, CoreError> {
        let client = Client::try_from(self.kubeconfig().await?)?;
        let api: Api<Playtest> = Api::default_namespaced(client);

        let lp = ListParams::default();

        match api.list(&lp).await {
            Ok(res) => {
                // sort by creation timestamp
                let mut items = res.items;
                items.sort_by(|a, b| {
                    b.metadata
                        .creation_timestamp
                        .cmp(&a.metadata.creation_timestamp)
                });
                Ok(items)
            }
            Err(e) => Err(self.handle_kube_error(e).await),
        }
    }

    #[instrument(skip(self))]
    pub async fn create_playtest(
        &self,
        input: CreatePlaytestRequest,
        owner: String,
    ) -> Result<Playtest, CoreError> {
        let client = Client::try_from(self.kubeconfig().await?)?;
        let api: Api<Playtest> = Api::default_namespaced(client);

        let pp = PostParams::default();

        let mut playtest = Playtest::new(&input.name, input.spec);
        let mut annotations = BTreeMap::from([
            (String::from("believer.dev/project"), input.project),
            (String::from("believer.dev/owner"), owner),
        ]);
        if input.do_not_prune {
            annotations.insert(
                String::from("believer.dev/do-not-prune"),
                "true".to_string(),
            );
        }
        playtest.metadata.annotations = Some(annotations);

        match api.create(&pp, &playtest).await {
            Ok(res) => Ok(res),
            Err(e) => Err(self.handle_kube_error(e).await),
        }
    }

    #[instrument(skip(self))]
    pub async fn update_playtest(
        &self,
        name: &str,
        input: UpdatePlaytestRequest,
        owner: String,
    ) -> Result<Playtest, CoreError> {
        let client = Client::try_from(self.kubeconfig().await?)?;
        let api: Api<Playtest> = Api::default_namespaced(client);

        match api.get(name).await {
            Ok(existing) => {
                let pp = PostParams::default();

                let mut playtest = Playtest::new(name, input.spec);
                playtest.metadata.resource_version = existing.metadata.resource_version;
                let mut annotations =
                    BTreeMap::from([(String::from("believer.dev/project"), input.project)]);
                if let Some(existing_annotations) = existing.metadata.annotations {
                    match existing_annotations.get("believer.dev/owner") {
                        Some(o) => {
                            annotations.insert(String::from("believer.dev/owner"), o.to_string())
                        }
                        None => annotations.insert(String::from("believer.dev/owner"), owner),
                    };
                } else {
                    annotations.insert(String::from("believer.dev/owner"), owner);
                }
                if input.do_not_prune {
                    annotations.insert(
                        String::from("believer.dev/do-not-prune"),
                        "true".to_string(),
                    );
                }
                playtest.metadata.annotations = Some(annotations);
                playtest.spec.groups = existing.spec.groups;

                match api.replace(name, &pp, &playtest).await {
                    Ok(res) => Ok(res),
                    Err(e) => Err(self.handle_kube_error(e).await),
                }
            }
            Err(e) => Err(self.handle_kube_error(e).await),
        }
    }

    #[instrument(skip(self))]
    pub async fn delete_playtest(&self, name: &str) -> Result<(), CoreError> {
        let params = DeleteParams::default();

        let client = Client::try_from(self.kubeconfig().await?).unwrap();
        let api: Api<Playtest> = Api::default_namespaced(client);

        match api.delete(name, &params).await {
            Ok(_) => Ok(()),
            Err(e) => Err(self.handle_kube_error(e).await),
        }
    }

    #[instrument(skip(self))]
    pub async fn assign_user_to_playtest(
        &self,
        playtest_name: &str,
        user: &str,
        group: Option<String>,
    ) -> Result<(), CoreError> {
        let client = Client::try_from(self.kubeconfig().await?)?;
        let api: Api<Playtest> = Api::default_namespaced(client);

        let mut playtest: Playtest;
        match api.get(playtest_name).await {
            Ok(_) => {}
            Err(e) => return Err(self.handle_kube_error(e).await),
        }

        // remove the user from the playtests if they're already in it
        playtest = self.remove_user_from_playtest(playtest_name, user).await?;

        match group {
            Some(group) => {
                let existing_group = playtest
                    .spec
                    .groups
                    .iter_mut()
                    .position(|g| g.name == group);
                match existing_group {
                    Some(i) => match playtest.spec.groups[i].users {
                        Some(ref mut users) => {
                            if users.len() as i32 >= playtest.spec.players_per_group {
                                return Err(anyhow!(GroupFullError).into());
                            }

                            users.push(user.to_string());

                            let json_patch: JsonPatch = serde_json::from_value(json!([ {
                                    "op": "replace",
                                    "path": format!("/spec/groups/{}/users", i),
                                    "value": serde_json::to_value(users).unwrap()
                                }
                            ]))
                            .unwrap();

                            self.patch_playtest(playtest_name, json_patch).await?;
                        }
                        None => {
                            let json_patch: JsonPatch = serde_json::from_value(json!([ {
                                    "op": "add",
                                    "path": format!("/spec/groups/{}/users", i),
                                    "value": serde_json::to_value(vec![user.to_string()]).unwrap()
                                }
                            ]))
                            .unwrap();

                            self.patch_playtest(playtest_name, json_patch).await?;

                            return Ok(());
                        }
                    },
                    None => {
                        let group = Group {
                            name: group,
                            users: Some(vec![user.to_string()]),
                        };

                        playtest.spec.groups.push(group);

                        let pp = PostParams::default();

                        return match api.replace(playtest_name, &pp, &playtest).await {
                            Ok(_) => Ok(()),
                            Err(e) => Err(self.handle_kube_error(e).await),
                        };
                    }
                }
            }
            None => {
                let mut json_patch: JsonPatch = serde_json::from_value(json!([
                    {
                        "op": "add",
                        "path": "/spec/usersToAutoAssign/-",
                        "value": user
                    }
                ]))
                .unwrap();

                // Initialize the array with a patch OP if necessary
                if playtest.spec.users_to_auto_assign.is_none() {
                    json_patch.0.insert(
                        0,
                        serde_json::from_value(json!({
                            "op": "add",
                            "path": "/spec/usersToAutoAssign",
                            "value": []
                        }))
                        .unwrap(),
                    );
                }

                self.patch_playtest(playtest_name, json_patch).await?;
            }
        }

        Ok(())
    }

    #[instrument(skip(self))]
    pub async fn remove_user_from_playtest(
        &self,
        playtest_name: &str,
        user: &str,
    ) -> Result<Playtest, CoreError> {
        let client = Client::try_from(self.kubeconfig().await?)?;
        let api: Api<Playtest> = Api::default_namespaced(client);

        let mut playtest: Playtest;
        match api.get(playtest_name).await {
            Ok(res) => playtest = res,
            Err(e) => return Err(self.handle_kube_error(e).await),
        }

        playtest.spec.groups.iter_mut().for_each(|g| {
            if let Some(users) = &mut g.users {
                users.retain(|u| u != user);
            }
        });

        let pp = PostParams::default();

        match api.replace(playtest_name, &pp, &playtest).await {
            Ok(res) => Ok(res),
            Err(e) => Err(self.handle_kube_error(e).await),
        }
    }

    #[instrument(skip(self))]
    async fn patch_playtest(&self, playtest_name: &str, patch: JsonPatch) -> Result<(), CoreError> {
        let client = Client::try_from(self.kubeconfig().await?)?;
        let api: Api<Playtest> = Api::default_namespaced(client);

        let pp = PatchParams::default();
        let patch = Patch::Json::<()>(patch);

        match api.patch(playtest_name, &pp, &patch).await {
            Ok(_) => Ok(()),
            Err(e) => Err(self.handle_kube_error(e).await),
        }
    }

    #[instrument(skip(self))]
    pub async fn get_workflows(
        &self,
        selected_artifact_project: &str,
    ) -> Result<Vec<Workflow>, CoreError> {
        self.kubeconfig().await?;

        let argo_client = self.argo_client.read().await;
        argo_client.get_workflows(selected_artifact_project).await
    }

    #[instrument(skip(self))]
    pub async fn get_logs_for_workflow_node(
        &self,
        uid: &str,
        node_id: &str,
    ) -> Result<String, CoreError> {
        self.kubeconfig().await?;

        let argo_client = self.argo_client.read().await;
        argo_client.get_logs_for_workflow_node(uid, node_id).await
    }

    #[instrument(skip(self))]
    pub async fn get_junit_artifact_for_workflow_node(
        &self,
        uid: &str,
        node_id: &str,
    ) -> Result<Option<JunitOutput>, CoreError> {
        self.kubeconfig().await?;

        let argo_client = self.argo_client.read().await;
        argo_client
            .get_junit_artifact_for_workflow_node(uid, node_id)
            .await
    }

    #[instrument(skip(self))]
    pub async fn stop_workflow(&self, workflow: &str) -> Result<String, CoreError> {
        self.kubeconfig().await?;

        let argo_client = self.argo_client.read().await;
        argo_client.stop_workflow(workflow).await
    }

    #[instrument(skip(self))]
    pub async fn get_project_configs(&self) -> Result<Vec<ProjectConfig>, CoreError> {
        // load configmap with name `projects` in `game-servers` namespace
        let client = Client::try_from(self.kubeconfig().await?)?;
        let api: Api<ConfigMap> = Api::namespaced(client, "game-servers");

        let configmap_name = "projects";

        let configmap = api.get(configmap_name).await?;

        match configmap.data {
            Some(data) => {
                info!("ConfigMap data: {:?}", data);
                let project_configs: Vec<ProjectConfig> =
                    serde_yaml::from_str(data.get("projects").unwrap())?;
                Ok(project_configs)
            }
            None => Err(CoreError::Internal(anyhow!("ConfigMap data is empty"))),
        }
    }

    #[instrument(skip(self))]
    pub fn default_project(&self) -> ProjectConfig {
        self.default_project.clone()
    }
}

async fn initialize_project_config(config: kube::Config) -> Result<ProjectConfig> {
    let client = Client::try_from(config)?;
    let api: Api<ConfigMap> = Api::namespaced(client, "game-servers");

    let configmap_name = "projects";

    let configmap = api.get(configmap_name).await?;

    match configmap.data {
        Some(data) => {
            info!("ConfigMap data: {:?}", data);
            let project_configs: Vec<ProjectConfig> =
                serde_yaml::from_str(data.get("projects").unwrap())?;

            // return the first project config with default: true, or the first project config
            for project in project_configs.clone() {
                if project.default {
                    return Ok(project);
                }
            }

            match project_configs.first() {
                Some(project) => Ok(project.clone()),
                None => Err(anyhow!("No project configs found")),
            }
        }
        None => Err(anyhow!("ConfigMap data is empty")),
    }
}

pub fn ensure_kube_client(client: Option<KubeClient>) -> Result<KubeClient> {
    match client {
        Some(c) => Ok(c),
        None => {
            error!("Kubernetes client not initialized. Please check your AWS configuration.");
            Err(anyhow!("Kubernetes client not initialized"))
        }
    }
}
