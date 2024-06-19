use anyhow::{Context, Result};
use async_trait::async_trait;
use ethos_core::types::config::{AppConfig, RepoConfig};
use ethos_core::types::gameserver::GameServerResults;
use std::path::PathBuf;
use std::process::{Child, Command};
use tracing::info;

/// EngineProvider trait
#[async_trait]
pub trait EngineProvider: Clone + Send + Sync + 'static {
    /// Creates a new provider from app and repo config
    fn new_from_config(app_config: AppConfig, repo_config: RepoConfig) -> Self;

    /// Checks if the engine is in a state where many files can be synced.
    /// For example, if the Unreal editor is running, we should not sync, so this function
    /// should return an error.
    async fn check_ready_to_sync_repo(&self) -> Result<()>;

    /// Opens the project in the engine's editor
    /// For example, if the engine is Unreal, this function should open the .uproject file in the editor.
    async fn open_project(&self) -> Result<()>;

    /// Create arguments to launch the game client locally
    /// This assumes everything we need comes from self, the app config, the repo config,
    /// and a Game Server's networking info.
    fn create_launch_args(
        &self,
        app_config: AppConfig,
        repo_config: RepoConfig,
        game_server: GameServerResults,
    ) -> Vec<String>;

    /// Given a path, finds the appropriate client executable to launch and returns its full path.
    fn find_client_executable(&self, path: PathBuf) -> Result<PathBuf>;

    fn launch(&self, path: PathBuf, args: Vec<String>) -> Result<Option<Child>> {
        if cfg!(windows) {
            let exe = self.find_client_executable(path)?;
            let child = Command::new(exe)
                .args(args)
                .spawn()
                .context("Failed to spawn {exe}")?;

            return Ok(Some(child));
        } else {
            info!("Launch: {}:{:?}", path.to_str().unwrap(), args);
        }
        Ok(None)
    }
}
