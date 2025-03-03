use anyhow::{Context, Result};
use async_trait::async_trait;
use ethos_core::types::config::{AppConfig, RepoConfig};
use ethos_core::types::gameserver::GameServerResults;
use ethos_core::types::repo::RepoStatus;
use std::fmt::Debug;
use std::path::Path;
use std::path::PathBuf;
use std::process::{Child, Command};
use tracing::info;

#[derive(Debug, Eq, PartialEq)]
pub enum CommunicationType {
    None, // Only in-memory cache lookups are allowed - use for situations where high performance is desired
    IpcOnly, // Only interprocess communication is allowed in this case, for example a HTTP request, pipes, etc.
    OfflineFallback, // Tries IPC first, but falls back to an offline approach if the host engine process isn't running, which can be much slower than IPC
}

#[derive(Debug, Eq, PartialEq)]
pub enum AllowMultipleProcesses {
    False,
    True,
}

/// EngineProvider trait
#[async_trait]
pub trait EngineProvider: Clone + Send + Sync + 'static {
    /// Creates a new provider from app and repo config
    fn new_from_config(app_config: AppConfig, repo_config: RepoConfig) -> Self;

    /// Loads any internal caches the provider needs from disk
    async fn load_caches(&mut self);

    /// Performs any post-download fixups necessary. Note that you must provide the path as
    /// the user could be downloading a packaged build as well as the engine.
    async fn post_download(path: &Path);

    /// Sends repo status updates to the engine
    async fn send_status_update(&self, status: &RepoStatus);

    /// Checks if the engine is in a state where many files can be synced.
    /// For example, if the Unreal editor is running, we should not sync, so this function
    /// should return an error.
    async fn check_ready_to_sync_repo(&self) -> Result<()>;

    /// Opens the project in the engine's editor
    /// For example, if the engine is Unreal, this function should open the .uproject file in the editor.
    async fn open_project(&self, allow_multiple: AllowMultipleProcesses) -> Result<()>;

    fn get_default_content_subdir(&self) -> String;

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
                .context("Failed to spawn exe")?;

            return Ok(Some(child));
        } else {
            info!("Launch: {}:{:?}", path.to_str().unwrap(), args);
        }
        Ok(None)
    }

    // Generates a parallel array to the passed-in asset_names slice with display names. Empty strings may be
    // present if a display name wasn't able to be determined.
    async fn get_asset_display_names(
        &self,
        communication: CommunicationType,
        engine_path: &Path,
        asset_paths: &[String],
    ) -> Vec<String>;

    fn is_lockable_file(&self, filepath: &str) -> bool;

    fn set_state(&self, in_slow_task: bool);

    // Given a file, returns the URL to view the file in the engine's editor.
    // If the file is not viewable in the engine, return None.
    fn get_url_for_path(&self, path: &str) -> Option<String>;
}
