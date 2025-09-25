use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::Sender as STDSender;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use axum::Router;
use config::Config;
use directories_next::BaseDirs;
use ethos_core::clients::git::Git;
use ethos_core::clients::GitMaintenanceRunner;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use notify_debouncer_full::{new_debouncer, DebounceEventResult, Debouncer, FileIdMap};
use parking_lot::RwLock;
use tokio::sync::mpsc;
use tokio::sync::oneshot::error::RecvError;
use tracing::{debug, error, info, instrument, warn};

use ethos_core::msg::LongtailMsg;
use ethos_core::storage::ArtifactStorage;
use ethos_core::types::config::{AppConfig, DynamicConfig, ProjectRepoConfig};
use ethos_core::types::errors::CoreError;
use ethos_core::types::repo::RepoStatus;
use ethos_core::types::repo::RepoStatusRef;
use ethos_core::utils::logging::OtelReloadHandle;
use ethos_core::worker::{RepoWorker, TaskSequence};

use crate::client::FriendshipperClient;
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
        config: AppConfig,
        config_file: PathBuf,
        startup_tx: STDSender<String>,
        refresh_tx: STDSender<()>,
        mut shutdown_rx: mpsc::Receiver<()>,
    ) -> Result<(), CoreError> {
        let pause_background_tasks = Arc::new(AtomicBool::new(false));

        let (app, address, shared_state, app_config_error) = self
            .initialize_server(
                config,
                config_file,
                startup_tx.clone(),
                pause_background_tasks.clone(),
            )
            .await?;

        // configure file watcher
        let watcher_status = shared_state.repo_status.clone();
        let watcher_git = shared_state.git().clone();

        // this debouncer must stay in scope for the duration of the server run
        let mut debouncer = self.create_file_watcher(
            watcher_status,
            watcher_git,
            shared_state.engine.clone(),
            pause_background_tasks.clone(),
            refresh_tx,
        )?;

        let repo_path = shared_state.app_config.read().repo_path.clone();
        if !repo_path.is_empty() {
            let mut content_dir = PathBuf::from(shared_state.app_config.read().repo_path.clone());
            content_dir.push(shared_state.repo_config.read().uproject_path.clone());
            content_dir.pop(); // pop off uproject filename
            content_dir.push(shared_state.engine.get_default_content_subdir());

            let inner_span = tracing::info_span!("watcher_start_watch").entered();
            debouncer
                .watcher()
                .watch(content_dir.as_path(), RecursiveMode::Recursive)?;
            inner_span.exit();
        }

        info!("starting server at {}", address);
        startup_tx.send("Starting server".to_string())?;

        // Send any app configuration error after the server has started
        if let Some(error_msg) = app_config_error {
            startup_tx.send(error_msg)?;
        }

        let listener = tokio::net::TcpListener::bind(address).await?;
        let result = axum::serve(listener, app.into_make_service())
            .with_graceful_shutdown(async move {
                shutdown_rx.recv().await;

                info!("Shutting down server");

                // cancel any longtail downloads
                let longtail = shared_state.longtail.clone();
                let child = longtail.child_process.lock().take();
                if let Some(mut child) = child {
                    child.kill().unwrap();
                }

                // Wait up to 30 seconds for index.lock to go away
                let repo_path = shared_state.app_config.read().repo_path.clone();
                if !repo_path.is_empty() {
                    info!("Waiting for index.lock to be removed");
                    let index_lock_path = PathBuf::from(repo_path).join(".git").join("index.lock");
                    let mut attempts = 0;
                    while index_lock_path.exists() && attempts < 30 {
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        attempts += 1;
                    }

                    if index_lock_path.exists() {
                        warn!("index.lock still present after 30 seconds");
                    } else {
                        info!("index.lock removed after {} seconds", attempts);
                    }
                }
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

    #[instrument(
        level = "info",
        skip(self, config, config_file, startup_tx, pause_background_tasks)
    )]
    async fn initialize_server(
        &self,
        config: AppConfig,
        config_file: PathBuf,
        startup_tx: STDSender<String>,
        pause_background_tasks: Arc<AtomicBool>,
    ) -> Result<
        (
            Router,
            String,
            AppState<UnrealEngineProvider>,
            Option<String>,
        ),
        CoreError,
    > {
        startup_tx.send("Initializing application config".to_string())?;

        let app_config = Arc::new(RwLock::new(config.clone()));
        let repo_config = Arc::new(RwLock::new(app_config.read().initialize_repo_config()?));
        let dynamic_config = Arc::new(RwLock::new(DynamicConfig::default()));
        let storage: Option<ArtifactStorage> = None;

        // Initialize branch defaults if not set and save config if updated
        let app_config_error = {
            let mut app_config_write = app_config.write();
            let repo_config_read = repo_config.read();
            if app_config_write.initialize_branch_defaults(&repo_config_read) {
                // Save the updated config to disk
                if let Ok(file) = fs::OpenOptions::new()
                    .write(true)
                    .truncate(true)
                    .open(&config_file)
                {
                    let mut config_to_save = app_config_write.clone();
                    // Remove PAT before saving
                    config_to_save.github_pat = None;
                    if let Err(e) = serde_yaml::to_writer(file, &config_to_save) {
                        warn!(
                            "Failed to save config with initialized branch defaults: {}",
                            e
                        );
                    } else {
                        info!("Initialized and saved branch defaults to config file");
                    }
                }
            }

            // Validate that configured branches exist in repo target branches
            // We'll store the error and send it after server initialization to ensure UI can display it
            if let Err(e) = app_config_write.validate_configured_branches(&repo_config_read) {
                error!("App configuration validation failed: {}", e);
                let error_msg = format!(
                    "App configuration error: {e}. Please check your friendshipper.yaml target branches or update your app config."
                );
                Some(error_msg)
            } else {
                None
            }
            // Don't return error here - continue with server initialization
        };

        // start the operation worker
        startup_tx.send("Starting operation worker".to_string())?;
        let (op_tx, op_rx) = mpsc::channel(32);
        let mut worker = RepoWorker::new(op_rx, pause_background_tasks.clone());
        tokio::spawn(async move {
            worker.run().await;
        });

        startup_tx.send("Initializing application state".to_string())?;
        let shared_state: AppState<UnrealEngineProvider> = AppState::new(
            app_config.clone(),
            repo_config.clone(),
            dynamic_config,
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

        // start the maintenance runner if we have a repo path
        let span = tracing::info_span!("acquire_config_lock").entered();
        let repo_path = shared_state.app_config.read().repo_path.clone();
        span.exit();

        let tx = shared_state.git_tx.clone();
        if !repo_path.is_empty() {
            // Check for and remove index.lock if it exists
            let span = tracing::info_span!("remove_index_lock").entered();

            let index_lock_path = PathBuf::from(&repo_path).join(".git").join("index.lock");
            if index_lock_path.exists() {
                match fs::remove_file(&index_lock_path) {
                    Ok(_) => {
                        info!("Removed existing index.lock file");
                    }
                    Err(e) => {
                        warn!("Failed to remove index.lock file: {:?}", e);
                    }
                }
            }

            span.exit();

            let maintenance_runner =
                GitMaintenanceRunner::new(repo_path, pause_background_tasks, tx)
                    .with_fetch_interval(Duration::from_secs(5));
            tokio::spawn(async move {
                match maintenance_runner.run().await {
                    Ok(_) => {}
                    Err(e) => {
                        error!("Failed to run maintenance runner: {:?}", e);
                    }
                };
            });
        }

        // install git hooks + set initial git config
        {
            let hooks_state = shared_state.clone();

            let span = tracing::info_span!("acquire_config_lock").entered();
            let repo_path: String = hooks_state.app_config.read().repo_path.clone();
            span.exit();

            let git_hooks_path: Option<String> =
                hooks_state.repo_config.read().git_hooks_path.clone();

            // avoids spamming a notification if repo/hooks paths are not configured
            if !repo_path.is_empty() {
                let git = hooks_state.git().clone();

                // ensure important git configs are set
                git.set_config("gc.auto", "0").await?;
                git.set_config("maintenance.auto", "0").await?;
                git.set_config("lfs.setlockablereadonly", "false").await?;
                git.set_config("http.postBuffer", "524288000").await?;
                git.configure_untracked_cache().await?;

                // Check for and fix partial clone filters that may prevent full history access
                startup_tx.send("Checking repository configuration".to_string())?;
                if let Ok(has_filter) = git.has_partial_clone_filter().await {
                    if has_filter {
                        info!("Detected partial clone filter, converting to full repository");
                        startup_tx.send("Converting partial clone to full repository (this may take a few minutes)".to_string())?;

                        match git.remove_partial_clone_filter_and_refetch().await {
                            Ok(_) => {
                                info!("Successfully converted partial clone to full repository");
                                shared_state.send_notification(
                                    "Repository converted to full clone with complete history",
                                );
                            }
                            Err(e) => {
                                warn!("Failed to convert partial clone to full repository: {}", e);
                                shared_state.send_notification("Warning: Failed to convert repository to full clone. Some history operations may be limited.");
                            }
                        }
                    }
                }

                startup_tx.send("Performing git repo maintenance".to_string())?;
                git.expire_reflog().await?;

                startup_tx.send("Installing git hooks".to_string())?;
                if let Some(git_hooks_path) = git_hooks_path {
                    tokio::spawn(async move {
                        let op = InstallGitHooksOp {
                            repo_path,
                            git_hooks_path,
                        };

                        let (tx, rx) = tokio::sync::oneshot::channel::<Option<CoreError>>();
                        let mut sequence = TaskSequence::new().with_completion_tx(tx);
                        sequence.push(Box::new(op));
                        let _ = hooks_state.operation_tx.send(sequence).await;

                        let res: Result<Option<CoreError>, RecvError> = rx.await;
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

        let span = tracing::info_span!("create_router").entered();
        let app = crate::router(&shared_state.log_path, self.port)?
            .with_state(shared_state.clone())
            .layer(ethos_core::utils::tracing::new_tracing_layer(
                APP_NAME.to_lowercase(),
            ));
        span.exit();

        let address = format!("127.0.0.1:{}", self.port);

        Ok((app, address, shared_state, app_config_error))
    }

    #[instrument(
        level = "info",
        skip(self, status, git_client, engine, pause_rx, refresh_tx)
    )]
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
            .map_err(|e| CoreError::Internal(anyhow!(e)))
    }

    pub fn initialize_app_config() -> Result<(Option<PathBuf>, Option<AppConfig>), CoreError> {
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
                    return Err(CoreError::Internal(anyhow!(
                        "Failed to create config directory: {:?}",
                        e
                    )));
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
                        return Err(CoreError::Internal(anyhow!(
                            "Failed to create config file: {:?}",
                            e
                        )));
                    }
                };

                match serde_yaml::to_writer(file, &AppConfig::new(APP_NAME)) {
                    Ok(_) => {
                        info!("Initialized config file at {}", &config_file_str);
                    }
                    Err(e) => {
                        return Err(CoreError::Internal(anyhow!(
                            "Failed to write default config to file: {:?}",
                            e
                        )));
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
                .unwrap()
                .set_default("maxClientCacheSizeGb", 32)
                .unwrap()
                .set_default("targetBranch", "main".to_string())
                .unwrap();

            return match builder.build() {
                Ok(settings) => match settings.try_deserialize::<AppConfig>() {
                    Ok(mut config) => {
                        info!("Loaded config from {}", &config_file_str);
                        info!("Config: {:?}", config);

                        // If we have a repo path and repo url but no selected project,
                        // construct owner-repo from github url
                        if !config.repo_path.is_empty()
                            && !config.repo_url.is_empty()
                            && config.selected_artifact_project.is_none()
                        {
                            if let Some(repo_name) = config.repo_url.split('/').next_back() {
                                let owner =
                                    config.repo_url.split('/').nth_back(1).unwrap_or_default();

                                let project_key = format!(
                                    "{}-{}",
                                    owner.to_lowercase(),
                                    repo_name.trim_end_matches(".git").to_lowercase()
                                );
                                config.selected_artifact_project = Some(project_key.clone());

                                // Add project to map if not present
                                if let std::collections::hash_map::Entry::Vacant(e) =
                                    config.projects.entry(project_key)
                                {
                                    e.insert(ProjectRepoConfig {
                                        repo_path: config.repo_path.clone(),
                                        repo_url: config.repo_url.clone(),
                                    });
                                }

                                // Write updated config back to disk
                                let file = fs::OpenOptions::new()
                                    .write(true)
                                    .truncate(true)
                                    .open(&config_file)?;

                                serde_yaml::to_writer(file, &config).map_err(|e| {
                                    CoreError::Internal(anyhow!(
                                        "Failed to write updated config to file: {:?}",
                                        e
                                    ))
                                })?;
                            }
                        }

                        // if selected project is none, set it to the first project in the map
                        if config.selected_artifact_project.is_none() && !config.projects.is_empty()
                        {
                            let project_key = config.projects.keys().next().unwrap().clone();
                            config.selected_artifact_project = Some(project_key.clone());

                            config.repo_url = config.projects[&project_key].repo_url.clone();
                            config.repo_path = config.projects[&project_key].repo_path.clone();

                            // Write updated config back to disk
                            let file = fs::OpenOptions::new()
                                .write(true)
                                .truncate(true)
                                .open(&config_file)?;

                            serde_yaml::to_writer(file, &config).map_err(|e| {
                                CoreError::Internal(anyhow!(
                                    "Failed to write updated config to file: {:?}",
                                    e
                                ))
                            })?;
                        }

                        // if there's a PAT in the keyring, load it
                        if let Ok(pat) = keyring::Entry::new(APP_NAME, KEYRING_USER)?.get_password()
                        {
                            if !pat.is_empty() {
                                config.github_pat = Some(pat.into());
                            }
                        }

                        // if we've been compiled with AWS credentials, use them
                        config.serverless = !(ethos_core::AWS_ACCESS_KEY_ID.is_empty());

                        // if we have a server_url but no okta_config, fetch the okta config
                        if !config.server_url.is_empty() && config.okta_config.is_none() {
                            info!("Fetching Okta config");
                            let client = FriendshipperClient::new(config.server_url.clone())?;

                            // can't await in here as we haven't started tokio yet
                            match tauri::async_runtime::block_on(client.get_okta_config()) {
                                Ok(okta_config) => {
                                    config.okta_config = Some(okta_config);
                                }
                                Err(e) => {
                                    error!("Failed to fetch Okta config: {}", e);
                                    return Err(CoreError::Internal(anyhow!(
                                        "Failed to fetch Okta config: {}",
                                        e
                                    )));
                                }
                            }
                        }

                        Ok((Some(config_file), Some(config)))
                    }
                    Err(e) => Err(CoreError::Internal(anyhow!(
                        "Failed to deserialize AppConfig: {:?}",
                        e
                    ))),
                },
                Err(e) => Err(CoreError::Internal(anyhow!(
                    "Failed to load AppConfig: {:?}",
                    e
                ))),
            };
        }

        Ok((None, None))
    }
}
