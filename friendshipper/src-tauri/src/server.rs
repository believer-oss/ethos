use std::fs;
use std::path::PathBuf;
use std::sync::mpsc::Sender as STDSender;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Result};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::response::Response;
use config::Config;
use directories_next::BaseDirs;
use ethos_core::middleware::uri::{uri_passthrough, RequestUri};
use parking_lot::RwLock;
use tokio::sync::mpsc;
use tokio::sync::oneshot::error::RecvError;
use tower_http::trace::TraceLayer;
use tracing::{debug, info, warn};
use tracing::{error, Span};

use ethos_core::msg::LongtailMsg;
use ethos_core::storage::ArtifactStorage;
use ethos_core::types::config::{AppConfig, DynamicConfig};
use ethos_core::worker::{RepoWorker, TaskSequence};

use crate::engine::UnrealEngineProvider;
use crate::repo::operations::InstallGitHooksOp;
use crate::state::FrontendOp;
use crate::APP_NAME;
use crate::{state::AppState, KEYRING_USER, VERSION};

pub struct Server {
    port: u16,
    longtail_tx: STDSender<LongtailMsg>,
    notification_tx: STDSender<String>,
    frontend_op_tx: STDSender<FrontendOp>,
    log_path: PathBuf,
    git_tx: STDSender<String>,
    gameserver_log_tx: STDSender<String>,
}

impl Server {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        port: u16,
        longtail_tx: STDSender<LongtailMsg>,
        notification_tx: STDSender<String>,
        frontend_op_tx: STDSender<FrontendOp>,
        log_path: PathBuf,
        git_tx: STDSender<String>,
        gameserver_log_tx: STDSender<String>,
    ) -> Self {
        Server {
            port,
            longtail_tx,
            notification_tx,
            frontend_op_tx,
            log_path,
            git_tx,
            gameserver_log_tx,
        }
    }

    pub async fn run(
        &self,
        startup_tx: STDSender<String>,
        shutdown_rx: mpsc::Receiver<()>,
    ) -> Result<()> {
        let mut shutdown_rx = shutdown_rx;

        startup_tx.send("Initializing application config".to_string())?;

        if let (Some(config_file), Some(config)) = self.initialize_app_config()? {
            let app_config = Arc::new(RwLock::new(config.clone()));
            let repo_config = Arc::new(RwLock::new(app_config.read().initialize_repo_config()?));

            // start the operation worker
            startup_tx.send("Starting operation worker".to_string())?;
            let (op_tx, op_rx) = mpsc::channel(32);
            let mut worker = RepoWorker::new(app_config.clone(), op_rx);
            tokio::spawn(async move {
                worker.run().await;
            });

            startup_tx.send("Checking for local dynamic config overrides".to_string())?;
            let storage: Option<ArtifactStorage> = None;
            let dynamic_config_override: Option<Result<DynamicConfig, anyhow::Error>> =
                BaseDirs::new().and_then(|b| {
                    let override_file = b.config_dir().join(APP_NAME).join("dynamic-config.json");
                    debug!(
                        "Checking if we should load dynamic config from {:?}",
                        override_file
                    );
                    override_file.exists().then(|| {
                        debug!("Loading dynamic config from {:?}", override_file);
                        let data = fs::read_to_string(override_file)?;
                        Ok(serde_json::from_str(&data)?)
                    })
                });

            let dynamic_config = match dynamic_config_override {
                Some(Ok(config)) => config,
                Some(Err(e)) => {
                    debug!("Failed to load dynamic config: {:?}", e);
                    startup_tx.send("Fetching dynamic config".to_string())?;

                    DynamicConfig::default()
                }
                None => DynamicConfig::default(),
            };

            startup_tx.send("Initializing application state".to_string())?;
            let shared_state: AppState<UnrealEngineProvider> = AppState::new(
                app_config.clone(),
                repo_config.clone(),
                Arc::new(RwLock::new(dynamic_config.clone())),
                config_file,
                storage,
                self.longtail_tx.clone(),
                op_tx.clone(),
                self.notification_tx.clone(),
                self.frontend_op_tx.clone(),
                VERSION.to_string(),
                None,
                self.log_path.clone(),
                self.git_tx.clone(),
                self.gameserver_log_tx.clone(),
            )
            .await?;

            // install git hooks
            {
                let hooks_state = shared_state.clone();

                let repo_path: String = hooks_state.app_config.read().repo_path.clone();
                let git_hooks_path: Option<String> =
                    hooks_state.repo_config.read().git_hooks_path.clone();

                // avoids spamming a notification if repo/hooks paths are not configured
                if !repo_path.is_empty() {
                    startup_tx.send("Installing git hooks".to_string())?;
                    if let Some(git_hooks_path) = git_hooks_path {
                        tokio::spawn(async move {
                            let op = InstallGitHooksOp {
                                repo_path,
                                git_hooks_path,
                            };

                            let (tx, rx) = tokio::sync::oneshot::channel::<Option<anyhow::Error>>();
                            let mut sequence = TaskSequence::new().with_completion_tx(tx);
                            sequence.push(Box::new(op));
                            let _ = hooks_state.operation_tx.send(sequence).await;

                            let res: Result<Option<anyhow::Error>, RecvError> = rx.await;
                            match res {
                                Ok(operation_error) => {
                                    if let Some(e) = operation_error {
                                        error!("Failed to install git hook: {}", e);
                                    }
                                }
                                Err(e) => {
                                    error!("Failed to install git hook: {}", e);
                                }
                            }
                        });
                    }
                }
            }

            let app = crate::router(&shared_state.log_path)?
                .with_state(shared_state.clone())
                .layer(axum::middleware::from_fn(uri_passthrough))
                .layer(
                TraceLayer::new_for_http()
                    .on_request(|request: &Request<Body>, _span: &Span| {
                        info!(method = %request.method(), path = %request.uri().path(), "request");
                    })
                    .on_response(|response: &Response, latency: Duration, _span: &Span| {
                        let path = response.extensions().get::<RequestUri>().map(|r| r.0.path()).unwrap_or("unknown");
                        match response.status() {
                            StatusCode::OK => {
                                info!(status = %response.status(), latency = ?latency, path, "response");
                            }
                            _ => {
                                warn!(status = %response.status(), latency = ?latency, path, "response");
                            }
                        }
                    }),
            );

            let address = format!("127.0.0.1:{}", self.port);

            info!("starting server at {}", address);
            startup_tx.send("Starting server".to_string())?;
            let listener = tokio::net::TcpListener::bind(address).await?;
            let result = axum::serve(listener, app.into_make_service())
                .with_graceful_shutdown(async move {
                    shutdown_rx.recv().await;

                    info!("Shutting down server");
                })
                .await;

            match result {
                Ok(_) => {
                    info!("server shut down gracefully");
                }
                Err(e) => info!("server shut down with error: {:?}", e),
            }
        }

        Ok(())
    }

    fn initialize_app_config(&self) -> Result<(Option<PathBuf>, Option<AppConfig>)> {
        if let Some(base_dirs) = BaseDirs::new() {
            let config_dir = base_dirs.config_dir().join(APP_NAME);

            match fs::create_dir_all(&config_dir) {
                Ok(_) => {
                    info!(
                        "Created config directory at {}",
                        config_dir.to_str().unwrap()
                    );
                }
                Err(e) => {
                    bail!("Failed to create config directory: {:?}", e);
                }
            }

            let config_file = config_dir.join("config.yaml");
            // Unwrap is safe because we checked Some(BaseDirs) and joins are all utf-8
            let config_file_str = config_file.to_str().unwrap();

            if !config_file.exists() {
                let file = match fs::OpenOptions::new()
                    .create(true)
                    .write(true)
                    .read(true)
                    .truncate(true)
                    .open(&config_file)
                {
                    Ok(file) => file,
                    Err(e) => {
                        bail!("Failed to create config file: {:?}", e);
                    }
                };

                match serde_yaml::to_writer(file, &AppConfig::default()) {
                    Ok(_) => {
                        info!("Initialized config file at {}", &config_file_str);
                    }
                    Err(e) => {
                        bail!("Failed to initialize config file: {:?}", e);
                    }
                }
            }

            let default_config: AppConfig = Default::default();

            let builder = Config::builder()
                .add_source(config::File::with_name(config_file_str))
                .set_default("pullDlls", true)
                .unwrap()
                .set_default("openUprojectAfterSync", true)
                .unwrap()
                .set_default(
                    "enginePrebuiltPath",
                    default_config.engine_prebuilt_path.clone(),
                )
                .unwrap()
                .set_default("initialized", true)
                .unwrap();

            match builder.build() {
                Ok(settings) => match settings.try_deserialize::<AppConfig>() {
                    Ok(mut config) => {
                        info!("Loaded config from {}", &config_file_str);
                        info!("Config: {:?}", config);

                        // if there's a PAT in the keyring, load it
                        if let Ok(pat) = keyring::Entry::new(APP_NAME, KEYRING_USER)?.get_password()
                        {
                            if !pat.is_empty() {
                                config.github_pat = Some(pat);
                            }
                        }

                        // Remove any existing selected artifact project. We want to discover this
                        // based on the repo status, not store this state.
                        config.selected_artifact_project = None;

                        return Ok((Some(config_file), Some(config)));
                    }
                    Err(e) => {
                        bail!("Failed to deserialize AppConfig: {:?}", e);
                    }
                },
                Err(e) => {
                    bail!("Failed to load config: {:?}", e);
                }
            }
        }

        Ok((None, None))
    }
}
