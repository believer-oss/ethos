use crate::engine;
use crate::engine::provider::AllowMultipleProcesses;
use crate::engine::unreal::ofpa::OFPANameCache;
use crate::engine::unreal::ofpa::OFPANameCacheRef;
use crate::engine::EngineProvider;
use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use ethos_core::types::config::AppConfig;
use ethos_core::types::config::RepoConfig;
use ethos_core::types::gameserver::GameServerResults;
use ethos_core::types::repo::RepoStatus;
use futures::FutureExt;
use std::path::Path;
use std::path::PathBuf;
use std::time::SystemTime;
use sysinfo::{ProcessRefreshKind, System, UpdateKind};
use tracing::info;
use tracing::instrument;
use tracing::warn;

#[derive(Clone)]
pub struct UnrealEngineProvider {
    pub repo_path: PathBuf,
    pub uproject_path: PathBuf,
    pub ofpa_cache: OFPANameCacheRef,
    pub can_handle_requests: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

#[async_trait]
impl EngineProvider for UnrealEngineProvider {
    #[instrument(skip(app_config, repo_config))]
    fn new_from_config(app_config: AppConfig, repo_config: RepoConfig) -> Self {
        Self {
            repo_path: PathBuf::from(app_config.repo_path),
            uproject_path: PathBuf::from(repo_config.uproject_path),
            ofpa_cache: std::sync::Arc::new(parking_lot::RwLock::new(OFPANameCache::new())),
            can_handle_requests: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
        }
    }

    #[instrument(skip(self))]
    async fn load_caches(&mut self) {
        let now = SystemTime::now();

        let mut ofpa_cache = self.ofpa_cache.write();
        if let Err(e) = ofpa_cache.load_cache() {
            warn!("Failed to load OFPA cache: {}", e);
        }

        if let Ok(elapsed) = now.elapsed() {
            let elapsed_secs = elapsed.as_secs_f32();
            if elapsed_secs > 0.1 {
                warn!("Took {} seconds to load the OFPA name cache", elapsed_secs);
            }
        }
    }

    #[instrument]
    async fn post_download(path: &Path) {
        // Create the sentinel file Engine/Restricted/NotForLicensees/Build/EpicInternal.txt, which
        // signals to Unreal that the build can contain PII in crash uploads. Since Friendshipper
        // is only used in dev contexts, this is a safe thing to do and helps engineers debug
        // crashes and understand who is experiencing them.
        if path.exists() {
            let mut dest_path = PathBuf::from(path);
            dest_path.push("Engine/Restricted/NotForLicensees/Build/");

            if !dest_path.exists() {
                if let Err(e) = std::fs::create_dir_all(&dest_path) {
                    warn!("Failed to create path '{:?}': {}", &dest_path, e);
                }
            }

            dest_path.push("EpicInternal.txt");
            if !dest_path.exists() {
                if let Err(e) = std::fs::File::create(&dest_path) {
                    warn!("Failed to create file '{:?}': {}", &dest_path, e);
                }
            }
        }
    }

    #[instrument(skip(self, status))]
    async fn send_status_update(&self, status: &RepoStatus) {
        if self.is_editor_process_running()
            && self
                .can_handle_requests
                .load(std::sync::atomic::Ordering::Relaxed)
        {
            let client = reqwest::Client::new();
            let future = client
                .post("http://localhost:8091/friendshipper-ue/status/update".to_string())
                .json(status)
                .send();

            // Because Unreal can get stuck in blocking slow tasks on the main thread and not answer requests
            // for multiple minutes, the idea here is to manually poll the future to see if it's done or not.
            // If at any point during the request Unreal goes into a slow task, we can drop it. This is
            // important because this request could be part of a larger operation that blocks other operations
            // like StatusOp or LockOp from running, rendering the Friendshipper UI essentially useless and
            // even causing deadlocks in situations where Unreal is blocking waiting for a commandlet to finish,
            // but the commandlet is waiting for a Friendshipper request to come back before continuing.

            // Bit of an implementation detail here: pin!() allows us to call now_or_never() on the future
            // without consuming it. See this forum post for excellent details on how it works:
            // https://users.rust-lang.org/t/how-to-check-if-a-future-is-immediately-ready/86401
            tokio::pin!(future);

            while self
                .can_handle_requests
                .load(std::sync::atomic::Ordering::Relaxed)
            {
                if future.as_mut().now_or_never().is_some() {
                    break;
                } else {
                    // while the task isn't finished, let the task runtime run
                    tokio::task::yield_now().await;
                }
            }
            warn!("Canceling status update request due to Unreal being busy");
        }
    }

    async fn check_ready_to_sync_repo(&self) -> Result<()> {
        if self.is_editor_process_running() {
            bail!("Close Unreal Editor and re-run operation.");
        }

        Ok(())
    }

    async fn open_project(&self, allow_multiple: AllowMultipleProcesses) -> Result<()> {
        let path_absolute: PathBuf = self.repo_path.join(self.uproject_path.clone());

        let mut can_launch_editor = allow_multiple == AllowMultipleProcesses::True;
        if !can_launch_editor {
            can_launch_editor = !self.is_editor_process_running();
        }

        if can_launch_editor {
            open::that(path_absolute)?
        } else {
            return Err(anyhow!(
                "Attempted to open uproject - Unreal Editor is already running."
            ));
        }

        Ok(())
    }

    fn get_default_content_subdir(&self) -> String {
        "Content".to_string()
    }

    fn create_launch_args(
        &self,
        app_config: AppConfig,
        _repo_config: RepoConfig,
        game_server: GameServerResults,
    ) -> Vec<String> {
        vec![
            format!(
                "{}:{}",
                game_server.ip.clone().unwrap_or_default(),
                game_server.port
            ),
            format!("-NetImguiClientPort={}", game_server.netimgui_port),
            format!("-PlayerName={}", app_config.user_display_name.clone()),
        ]
    }

    fn find_client_executable(&self, path: PathBuf) -> Result<PathBuf> {
        for file in path.read_dir().context("Could not read launch directory")? {
            let file = file.context("Invalid file")?;
            let name = file.file_name();
            let name = name.to_str().context("Invalid launch filename")?;
            if name.ends_with("Client.exe") {
                return Ok(file.path());
            }
        }

        bail!("No client found in path!");
    }

    #[instrument(skip(self, asset_names))]
    async fn get_asset_display_names(
        &self,
        communication: engine::provider::CommunicationType,
        engine_path: &Path,
        asset_names: &[String],
    ) -> Vec<String> {
        OFPANameCache::get_names(self.clone(), communication, engine_path, asset_names).await
    }

    fn is_lockable_file(&self, filepath: &str) -> bool {
        filepath.ends_with(".uasset") || filepath.ends_with(".umap") || filepath.ends_with(".dll")
    }

    fn set_state(&self, in_slow_task: bool) {
        self.can_handle_requests
            .store(!in_slow_task, std::sync::atomic::Ordering::Relaxed);
    }
}

impl UnrealEngineProvider {
    #[instrument(skip(self))]
    pub fn is_editor_process_running(&self) -> bool {
        let mut system = System::new();
        let refresh_kind = ProcessRefreshKind::new().with_cmd(UpdateKind::OnlyIfNotSet);
        system.refresh_processes_specifics(refresh_kind);

        let repo_path: String = self
            .repo_path
            .to_str()
            .unwrap_or_default()
            .to_lowercase()
            .replace('\\', "/");

        for process in system.processes_by_name("UnrealEditor") {
            for arg in process.cmd() {
                let arg: String = arg.to_lowercase().replace('\\', "/");
                let is_editor_running = arg.contains(&repo_path);

                info!(
                    "Checked if process '{}' arg '{}' contains '{}': {}",
                    process.name(),
                    arg,
                    repo_path,
                    is_editor_running
                );

                if is_editor_running {
                    return true;
                }
            }
        }

        false
    }
}
