use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::Sender as STDSender;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Result};
use config::Config;
use directories_next::BaseDirs;
use ethos_core::clients::git::Git;
use ethos_core::clients::GitMaintenanceRunner;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use notify_debouncer_full::{new_debouncer, DebounceEventResult, Debouncer, FileIdMap};
use parking_lot::RwLock;
use tokio::sync::mpsc;
use tokio::sync::oneshot::error::RecvError;
use tracing::{debug, error, info, warn};

use ethos_core::msg::LongtailMsg;
use ethos_core::storage::ArtifactStorage;
use ethos_core::types::config::{AppConfig, DynamicConfig};
use ethos_core::types::errors::CoreError;
use ethos_core::types::repo::RepoStatus;
use ethos_core::types::repo::RepoStatusRef;
use ethos_core::utils::logging::OtelReloadHandle;
use ethos_core::worker::{RepoWorker, TaskSequence};

use crate::engine::{EngineProvider, UnrealEngineProvider};
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
    otel_reload_handle: OtelReloadHandle,
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
        otel_reload_handle: OtelReloadHandle,
    ) -> Self {
        Server {
            port,
            longtail_tx,
            notification_tx,
            frontend_op_tx,
            log_path,
            git_tx,
            gameserver_log_tx,
            otel_reload_handle,
        }
    }

    pub async fn run(
        &self,
        startup_tx: STDSender<String>,
        refresh_tx: STDSender<()>,
        shutdown_rx: mpsc::Receiver<()>,
    ) -> Result<(), CoreError> {
        let mut shutdown_rx = shutdown_rx;

        startup_tx.send("Initializing application config".to_string())?;

        let pause_file_watcher = Arc::new(AtomicBool::new(false));

        if let (Some(config_file), Some(config)) = self.initialize_app_config()? {
            let app_config = Arc::new(RwLock::new(config.clone()));
            let repo_config = Arc::new(RwLock::new(app_config.read().initialize_repo_config()?));

            // start the operation worker
            startup_tx.send("Starting operation worker".to_string())?;
            let (op_tx, op_rx) = mpsc::channel(32);
            let mut worker = RepoWorker::new(app_config.clone(), op_rx, pause_file_watcher.clone());
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
                Some(self.otel_reload_handle.clone()),
                self.git_tx.clone(),
                self.gameserver_log_tx.clone(),
            )
            .await?;

            // configure file watcher
            let watcher_status = shared_state.repo_status.clone();
            let watcher_git = shared_state.git().clone();
            let mut debouncer = self.create_file_watcher(
                watcher_status,
                watcher_git,
                shared_state.engine.clone(),
                pause_file_watcher.clone(),
                refresh_tx,
            )?;

            // start the maintenance runner if we have a repo path
            let repo_path = shared_state.app_config.read().repo_path.clone();
            let tx = shared_state.git_tx.clone();
            if !repo_path.is_empty() {
                let maintenance_runner = GitMaintenanceRunner::new(repo_path, tx)
                    .with_fetch_interval(Duration::from_secs(5));
                tokio::spawn(async move {
                    match maintenance_runner.run().await {
                        Ok(_) => {}
                        Err(e) => {
                            error!("Failed to run maintenance runner: {:?}", e);
                        }
                    };
                });

                // start file watcher
                let content_dir = PathBuf::from(shared_state.app_config.read().repo_path.clone())
                    .join(shared_state.engine.get_default_content_subdir());
                debouncer
                    .watcher()
                    .watch(content_dir.as_path(), RecursiveMode::Recursive)?;
                debouncer
                    .cache()
                    .add_root(content_dir.as_path(), RecursiveMode::Recursive);
            }

            // install git hooks + set initial git config
            {
                let hooks_state = shared_state.clone();

                let repo_path: String = hooks_state.app_config.read().repo_path.clone();
                let git_hooks_path: Option<String> =
                    hooks_state.repo_config.read().git_hooks_path.clone();

                // avoids spamming a notification if repo/hooks paths are not configured
                if !repo_path.is_empty() {
                    let git = hooks_state.git().clone();
                    git.set_config("gc.auto", "0").await?;

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
                .layer(ethos_core::utils::tracing::new_tracing_layer(
                    APP_NAME.to_lowercase(),
                ));

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

    fn create_file_watcher<T>(
        &self,
        status: RepoStatusRef,
        git_client: Git,
        engine: T,
        pause_rx: Arc<AtomicBool>,
        refresh_tx: STDSender<()>,
    ) -> Result<Debouncer<RecommendedWatcher, FileIdMap>, CoreError>
    where
        T: EngineProvider,
    {
        let (engine_update_tx, engine_update_rx) = std::sync::mpsc::channel::<RepoStatus>();

        tokio::spawn(async move {
            while let Ok(repo_status) = engine_update_rx.recv() {
                engine.send_status_update(&repo_status).await;
            }
        });

        new_debouncer(
            Duration::from_secs(2),
            None,
            move |result: DebounceEventResult| {
                if let Ok(event) = result {
                    // get unique paths in events
                    let modified = event
                        .iter()
                        .flat_map(|e| e.paths.iter())
                        .filter(|p| p.is_file())
                        .collect::<HashSet<_>>();

                    {
                        // if we're paused, return
                        if pause_rx.load(std::sync::atomic::Ordering::Relaxed) {
                            debug!("File watcher paused, skipping this event");
                            return;
                        }

                        let mut status = status.write();
                        let rt = tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()
                            .unwrap();
                        match rt.block_on(async {
                            git_client
                                .status(
                                    modified
                                        .iter()
                                        .map(|p| {
                                            p.strip_prefix(&git_client.repo_path)
                                                .unwrap()
                                                .to_str()
                                                .unwrap()
                                                .to_string()
                                        })
                                        .collect(),
                                )
                                .await
                        }) {
                            Ok(output) => {
                                for line in output.lines() {
                                    status.parse_file_line(line);
                                }

                                if let Err(e) = engine_update_tx.send(status.clone()) {
                                    warn!("Failed to signal engine update channel, engine update will be delayed: {}", e);
                                }
                            }
                            Err(e) => {
                                error!("Failed to get git status: {}", e);
                            }
                        }
                    }

                    if refresh_tx.send(()).is_err() {
                        error!("Failed to send refresh message");
                    }
                }
            },
        )
        .map_err(CoreError::from)
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

                match serde_yaml::to_writer(file, &AppConfig::new(APP_NAME)) {
                    Ok(_) => {
                        info!("Initialized config file at {}", &config_file_str);
                    }
                    Err(e) => {
                        bail!("Failed to initialize config file: {:?}", e);
                    }
                }
            }

            let default_config: AppConfig = AppConfig::new(APP_NAME);

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
