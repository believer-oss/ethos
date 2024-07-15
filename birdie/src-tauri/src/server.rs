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
use parking_lot::RwLock;
use tokio::sync::mpsc;
use tower_http::trace::TraceLayer;
use tracing::{error, Span};
use tracing::{info, warn};

use ethos_core::types::config::AppConfig;
use ethos_core::types::repo::RepoStatus;
use ethos_core::worker::RepoWorker;

use ethos_core::middleware::uri::{uri_passthrough, RequestUri};
#[cfg(windows)]
use {crate::DEFAULT_DRIVE_MOUNT, ethos_core::utils, std::path::Path};

use crate::repo::StatusOp;
use crate::{state::AppState, APP_NAME, KEYRING_USER, VERSION};

pub struct Server {
    port: u16,
    log_path: PathBuf,
    git_tx: STDSender<String>,
}

impl Server {
    #[allow(clippy::too_many_arguments)]
    pub fn new(port: u16, log_path: PathBuf, git_tx: STDSender<String>) -> Self {
        Server {
            port,
            log_path,
            git_tx,
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

            startup_tx.send("Initializing application state".to_string())?;
            let shared_state = Arc::new(
                AppState::new(
                    app_config.clone(),
                    repo_config.clone(),
                    config_file,
                    op_tx.clone(),
                    VERSION.to_string(),
                    self.log_path.clone(),
                    self.git_tx.clone(),
                )
                .await?,
            );

            // get initial repo status
            {
                if !shared_state.app_config.read().repo_path.is_empty() {
                    #[cfg(windows)]
                    if !Path::new(DEFAULT_DRIVE_MOUNT).exists() {
                        // Mount drive at repo location. For now assume this is not configurable.
                        startup_tx.send("Confirming drive mount exists".to_string())?;
                        utils::windows::mount_drive(
                            DEFAULT_DRIVE_MOUNT,
                            &shared_state.app_config.read().repo_path,
                        )?;
                    }

                    startup_tx.send("Fetching initial repo status".to_string())?;
                    let status_op = {
                        StatusOp {
                            repo_status: shared_state.repo_status.clone(),
                            git_client: shared_state.git(),
                            skip_fetch: false,
                        }
                    };

                    let res: Result<RepoStatus, anyhow::Error> = status_op.run().await;
                    match res {
                        Ok(status) => {
                            let mut lock = shared_state.repo_status.write();
                            *lock = status;
                        }
                        error => error!("Failed initial status operation: {:?}", error),
                    }
                } else {
                    info!("Repo path not configured, skipping initial status operation");
                }
            }

            let app = crate::router(shared_state.clone())?
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
                    error!("Failed to create config directory: {:?}", e);
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
                        bail!("Failed to open config file: {:?}", e);
                    }
                };

                match serde_yaml::to_writer(file, &AppConfig::new(crate::APP_NAME)) {
                    Ok(_) => {
                        info!("Initialized config file at {}", &config_file_str);
                    }
                    Err(e) => {
                        bail!("Failed to initialize config file: {:?}", e);
                    }
                }
            }

            let default_config: AppConfig = AppConfig::new(crate::APP_NAME);

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

                        // if there's a PAT in the keyring, load it
                        if let Ok(pat) = keyring::Entry::new(APP_NAME, KEYRING_USER)?.get_password()
                        {
                            if !pat.is_empty() {
                                config.github_pat = Some(pat);
                            }
                        }

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
