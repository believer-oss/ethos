use crate::engine;
use crate::engine::unreal::ofpa::OFPANameCache;
use crate::engine::unreal::ofpa::OFPANameCacheRef;
use crate::engine::EngineProvider;
use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use ethos_core::types::config::AppConfig;
use ethos_core::types::config::RepoConfig;
use ethos_core::types::gameserver::GameServerResults;
use ethos_core::types::repo::RepoStatus;
use std::path::Path;
use std::path::PathBuf;
use std::time::SystemTime;
use sysinfo::{ProcessRefreshKind, System, UpdateKind};
use tracing::instrument;
use tracing::warn;

#[derive(Clone)]
pub struct UnrealEngineProvider {
    pub repo_path: PathBuf,
    pub uproject_path: PathBuf,
    pub ofpa_cache: OFPANameCacheRef,
}

#[async_trait]
impl EngineProvider for UnrealEngineProvider {
    #[instrument(skip(app_config, repo_config))]
    fn new_from_config(app_config: AppConfig, repo_config: RepoConfig) -> Self {
        Self {
            repo_path: PathBuf::from(app_config.repo_path),
            uproject_path: PathBuf::from(repo_config.uproject_path),
            ofpa_cache: std::sync::Arc::new(parking_lot::RwLock::new(OFPANameCache::new())),
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

    #[instrument(skip(self))]
    async fn send_status_update(&self, status: &RepoStatus) {
        let client = reqwest::Client::new();
        _ = client
            .post("http://localhost:8091/friendshipper-ue/status/update".to_string())
            .json(status)
            .send()
            .await;
    }

    async fn check_ready_to_sync_repo(&self) -> Result<()> {
        if self.is_editor_process_running() {
            bail!("Close Unreal Editor and re-run operation.");
        }

        Ok(())
    }

    async fn open_project(&self) -> Result<()> {
        let path_absolute: PathBuf = self.repo_path.join(self.uproject_path.clone());

        if !self.is_editor_process_running() {
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
}

impl UnrealEngineProvider {
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
                if arg.contains(&repo_path) {
                    return true;
                }
            }
        }

        false
    }
}
