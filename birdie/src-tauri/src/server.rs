use std::fs;
use std::path::PathBuf;
use std::sync::mpsc::Sender as STDSender;
use std::sync::Arc;

use anyhow::{bail, Result};
use config::Config;
use directories_next::BaseDirs;
use parking_lot::RwLock;
use tokio::sync::mpsc;
use tracing::error;
use tracing::info;

use ethos_core::types::repo::RepoStatus;
use ethos_core::worker::RepoWorker;

#[cfg(windows)]
use {crate::DEFAULT_DRIVE_MOUNT, ethos_core::utils, std::path::Path};

use crate::repo::StatusOp;
use crate::types::config::BirdieConfig;
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
        config: BirdieConfig,
        config_file: PathBuf,
        startup_tx: STDSender<String>,
        shutdown_rx: mpsc::Receiver<()>,
    ) -> Result<()> {
        let mut shutdown_rx = shutdown_rx;

        startup_tx.send("Initializing application config".to_string())?;

        let app_config = Arc::new(RwLock::new(config.clone()));

        let pause_file_watcher = Arc::new(std::sync::atomic::AtomicBool::new(false));

        // start the operation worker
        startup_tx.send("Starting operation worker".to_string())?;
        let (op_tx, op_rx) = mpsc::channel(32);
        let mut worker = RepoWorker::new(op_rx, pause_file_watcher);
        tokio::spawn(async move {
            worker.run().await;
        });

        startup_tx.send("Initializing application state".to_string())?;
        let shared_state = Arc::new(
            AppState::new(
                app_config.clone(),
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
                let status_op = StatusOp {
                    repo_status: shared_state.repo_status.clone(),
                    git_client: shared_state.git(),
                    skip_fetch: false,
                    github_username: shared_state.github_username(),
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

        let app = crate::router(shared_state.clone())?.layer(
            ethos_core::utils::tracing::new_tracing_layer(APP_NAME.to_lowercase()),
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
        Ok(())
    }

    pub fn initialize_app_config() -> Result<(Option<PathBuf>, Option<BirdieConfig>)> {
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

                match serde_yaml::to_writer(file, &BirdieConfig::default()) {
                    Ok(_) => {
                        info!("Initialized config file at {}", &config_file_str);
                    }
                    Err(e) => {
                        bail!("Failed to initialize config file: {:?}", e);
                    }
                }
            }

            let builder = Config::builder()
                .add_source(config::File::with_name(config_file_str))
                .set_default("initialized", true)
                .unwrap();

            match builder.build() {
                Ok(settings) => match settings.try_deserialize::<BirdieConfig>() {
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
