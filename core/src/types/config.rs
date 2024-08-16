use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use crate::AWS_REGION;
use anyhow::{anyhow, bail, Result};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use config::Config;
use lazy_static::lazy_static;
use parking_lot::RwLock;
use regex::Regex;
use serde::{Deserialize, Serialize};

#[cfg(not(target_os = "windows"))]
use crate::fs::LocalDownloadPath;
use crate::storage::StorageSchemaVersion;

lazy_static! {
    // Attempts to match the format <branch>-<short sha>. See test_engine_association_regex() for examples.
    static ref CUSTOM_ENGINE_ASSOCIATION_REGEX: Regex =
        Regex::new(r"^(.+)-([0-9a-f]+)$").unwrap();
}

pub type RepoConfigRef = Arc<RwLock<RepoConfig>>;
pub type DynamicConfigRef = Arc<RwLock<DynamicConfig>>;

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, Eq, PartialEq)]
pub enum EngineType {
    #[default]
    Prebuilt,
    Source,
}

pub type AppConfigRef = Arc<RwLock<AppConfig>>;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AWSConfig {
    pub account_id: String,
    pub sso_start_url: String,
    pub role_name: String,
    pub artifact_bucket_name: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default, rename = "repoPath", alias = "repo_path")]
    pub repo_path: String,

    #[serde(default, rename = "repoUrl", alias = "repo_url")]
    pub repo_url: String,

    #[serde(default, rename = "toolsPath", alias = "tools_path")]
    pub tools_path: String,

    #[serde(default, rename = "toolsUrl", alias = "tools_url")]
    pub tools_url: String,

    #[serde(default, rename = "userDisplayName", alias = "user_display_name")]
    pub user_display_name: String,

    #[serde(default, rename = "gameClientDownloadSymbols")]
    pub game_client_download_symbols: bool,

    #[serde(default, rename = "pullDlls")]
    pub pull_dlls: bool,

    #[serde(default, rename = "editorDownloadSymbols")]
    pub editor_download_symbols: bool,

    #[serde(default, rename = "openUprojectAfterSync")]
    pub open_uproject_after_sync: bool,

    #[serde(default, rename = "githubPAT", skip_serializing_if = "Option::is_none")]
    pub github_pat: Option<String>,

    #[serde(default, rename = "engineType")]
    pub engine_type: EngineType,

    #[serde(default, rename = "enginePrebuiltPath")]
    pub engine_prebuilt_path: String,

    #[serde(default, rename = "engineSourcePath")]
    pub engine_source_path: String,

    #[serde(default, rename = "engineDownloadSymbols")]
    pub engine_download_symbols: bool,

    #[serde(default, rename = "engineRepoUrl")]
    pub engine_repo_url: String,

    #[serde(default, rename = "recordPlay")]
    pub record_play: bool,

    #[serde(rename = "awsConfig")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aws_config: Option<AWSConfig>,

    #[serde(default, rename = "selectedArtifactProject")]
    pub selected_artifact_project: Option<String>,

    #[serde(default = "default_playtest_region", rename = "playtestRegion")]
    pub playtest_region: String,

    #[serde(default)]
    pub initialized: bool,
}

fn default_playtest_region() -> String {
    AWS_REGION.to_string()
}

impl AppConfig {
    pub fn new(app_name: &str) -> Self {
        #[cfg(target_os = "windows")]
        let mut engine_prebuilt_path = {
            _ = app_name;
            PathBuf::from("C:\\")
        };

        #[cfg(not(target_os = "windows"))]
        let mut engine_prebuilt_path = LocalDownloadPath::new(app_name).to_path_buf();

        engine_prebuilt_path.push("f11r_engine_prebuilt");
        AppConfig {
            repo_path: Default::default(),
            repo_url: Default::default(),
            tools_path: Default::default(),
            tools_url: Default::default(),
            user_display_name: Default::default(),
            game_client_download_symbols: false,
            pull_dlls: true,
            editor_download_symbols: false,
            open_uproject_after_sync: true,
            github_pat: Default::default(),
            engine_type: Default::default(),
            engine_prebuilt_path: engine_prebuilt_path.to_string_lossy().to_string(),
            engine_source_path: Default::default(),
            engine_download_symbols: false,
            engine_repo_url: Default::default(),
            record_play: false,
            aws_config: None,
            selected_artifact_project: None,
            playtest_region: default_playtest_region(),
            initialized: false,
        }
    }

    pub fn initialize_repo_config(&self) -> Result<RepoConfig> {
        if self.repo_path.is_empty() {
            return Ok(Default::default());
        }

        let config_file = PathBuf::from(self.repo_path.clone()).join("friendshipper.yaml");

        if !config_file.exists() {
            return Ok(Default::default());
        }

        let settings = Config::builder()
            .add_source(config::File::with_name(config_file.to_str().unwrap()))
            .set_default("trunkBranch", "main")?
            .set_default("useConventionalCommits", false)?
            .build()?;

        // TODO: We'll need some better error handling here. Because the config file is stored
        // in the project repo itself, all users are impacted by bad config being committed to it.
        settings
            .try_deserialize::<RepoConfig>()
            .map_err(|e| anyhow!(e))
    }

    pub fn get_uproject_path(&self, repo_config: &RepoConfig) -> PathBuf {
        PathBuf::from(&self.repo_path).join(&repo_config.uproject_path)
    }

    pub fn get_engine_path(&self, uproject: &UProject) -> PathBuf {
        if uproject.is_custom_engine() {
            return if self.engine_type == EngineType::Prebuilt {
                (&self.engine_prebuilt_path).into()
            } else {
                (&self.engine_source_path).into()
            };
        }

        format!(
            "C:/Program Files/Epic Games/UE_{}",
            uproject.engine_association
        )
        .into()
    }

    pub fn load_engine_path_from_repo(&self, repo_config: &RepoConfig) -> Result<PathBuf> {
        let project_path = self.get_uproject_path(repo_config);
        let uproject = UProject::load(&project_path)?;
        Ok(self.get_engine_path(&uproject))
    }
}

#[derive(Debug, Clone)]
pub struct ConfigValidationError(pub String);

impl IntoResponse for ConfigValidationError {
    fn into_response(self) -> Response {
        (StatusCode::BAD_REQUEST, format!("{}", self)).into_response()
    }
}

impl std::fmt::Display for ConfigValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Failed to save preferences: {}", self.0)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoConfig {
    #[serde(default, rename = "uprojectPath")]
    pub uproject_path: String,

    #[serde(default, rename = "trunkBranch")]
    pub trunk_branch: String,

    #[serde(default, rename = "gitHooksPath")]
    pub git_hooks_path: Option<String>,

    #[serde(default, rename = "useConventionalCommits")]
    pub use_conventional_commits: bool,

    #[serde(default, rename = "conventionalCommitsAllowedTypes")]
    pub conventional_commits_allowed_types: Vec<String>,

    #[serde(
        default,
        rename = "commitGuidelinesUrl",
        skip_serializing_if = "Option::is_none"
    )]
    pub commit_guidelines_url: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LfsConfigFile {
    pub lfs: LfsConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LfsConfig {
    pub url: Option<String>,
    pub setlockablereadonly: Option<bool>,
}

impl Default for RepoConfig {
    fn default() -> Self {
        RepoConfig {
            uproject_path: String::default(),
            trunk_branch: "main".to_string(),
            git_hooks_path: None,
            commit_guidelines_url: None,
            use_conventional_commits: false,
            conventional_commits_allowed_types: vec![
                "feat".to_string(),
                "fix".to_string(),
                "docs".to_string(),
                "style".to_string(),
                "refactor".to_string(),
                "perf".to_string(),
                "test".to_string(),
                "chore".to_string(),
            ],
        }
    }
}

impl RepoConfig {
    pub fn get_project_name(uproject_path: &str) -> Option<String> {
        let uproject_path = Path::new(uproject_path);
        match uproject_path.file_stem() {
            Some(project_name) => project_name
                .to_str()
                .map(|project_name| project_name.to_string()),
            None => None,
        }
    }

    pub fn read_lfs_config(repo_path: &str) -> Result<LfsConfigFile, anyhow::Error> {
        let lfs_config_path = PathBuf::from(repo_path).join(".lfsconfig");
        let lfs_config_bytes = fs::read(lfs_config_path)?;

        let mut config: LfsConfigFile =
            toml::from_str(std::str::from_utf8(lfs_config_bytes.as_slice())?)?;
        config.lfs.url = config
            .lfs
            .url
            .map(|url| url.trim_end_matches('/').to_string());
        Ok(config)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct UProject {
    #[serde(rename = "EngineAssociation")]
    pub engine_association: String,
}

impl UProject {
    pub fn load(uproject_path: &Path) -> Result<UProject, anyhow::Error> {
        let data: String = match fs::read_to_string(uproject_path) {
            Ok(s) => s,
            Err(e) => bail!("Failed to read UProject file {:?}: {}", uproject_path, e),
        };

        let uproject: UProject = match serde_json::from_str(&data) {
            Ok(p) => p,
            Err(e) => bail!(
                "Failed to deserialize UProject json from path {:?}: {}",
                uproject_path,
                e
            ),
        };

        Ok(uproject)
    }

    pub fn is_custom_engine(&self) -> bool {
        CUSTOM_ENGINE_ASSOCIATION_REGEX.is_match(&self.engine_association)
    }

    pub fn get_custom_engine_sha(&self) -> Result<String> {
        let captures = CUSTOM_ENGINE_ASSOCIATION_REGEX
            .captures(&self.engine_association)
            .unwrap();
        // let branch: &str = &captures[1];
        let commit_sha_short: &str = &captures[2];
        Ok(commit_sha_short.to_string())
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DiscordChannelInfo {
    pub name: String,
    pub url: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DynamicConfig {
    #[serde(default, rename = "playtestDiscordChannels")]
    pub playtest_discord_channels: Vec<DiscordChannelInfo>,

    #[serde(default)]
    pub storage_schema: StorageSchemaVersion,

    #[serde(default)]
    pub kubernetes_cluster_name: String,

    #[serde(skip_serializing_if = "Option::is_none", rename = "otlp_endpoint")]
    pub otlp_endpoint: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none", rename = "otlp_headers")]
    pub otlp_headers: Option<String>,

    #[serde(default, rename = "playtestRegions")]
    pub playtest_regions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnrealVerSelDiagResponse {
    pub valid_version_selector: bool,
    pub version_selector_msg: String,
    pub uproject_file_assoc: bool,
    pub uproject_file_assoc_msg: Vec<String>,
}

#[cfg(test)]
mod tests {
    use crate::types::config::CUSTOM_ENGINE_ASSOCIATION_REGEX;

    #[test]
    fn test_engine_association_regex() {
        let caps = CUSTOM_ENGINE_ASSOCIATION_REGEX
            .captures("believer-5.2-63c321a2")
            .expect("Failed to match string");
        let branch: &str = &caps[1];
        let sha: &str = &caps[2];
        assert_eq!(branch, "believer-5.2");
        assert_eq!(sha, "63c321a2");

        let caps = CUSTOM_ENGINE_ASSOCIATION_REGEX
            .captures("rjd/believer-5.2-529a2e6c3863582a78bc434a1ec87b731b64d809")
            .expect("Failed to match string");
        let branch: &str = &caps[1];
        let sha: &str = &caps[2];
        assert_eq!(branch, "rjd/believer-5.2");
        assert_eq!(sha, "529a2e6c3863582a78bc434a1ec87b731b64d809");

        let caps = CUSTOM_ENGINE_ASSOCIATION_REGEX
            .captures("my-wacky-branch-with-lots-of-hyphens-63c321a2")
            .expect("Failed to match string");
        let branch: &str = &caps[1];
        let sha: &str = &caps[2];
        assert_eq!(branch, "my-wacky-branch-with-lots-of-hyphens");
        assert_eq!(sha, "63c321a2");

        let caps = CUSTOM_ENGINE_ASSOCIATION_REGEX
            .captures("believer-5.2-63c321a2-ab")
            .expect("Failed to match string");
        let branch: &str = &caps[1];
        let sha: &str = &caps[2];
        assert_eq!(branch, "believer-5.2-63c321a2");
        assert_eq!(sha, "ab");

        assert!(CUSTOM_ENGINE_ASSOCIATION_REGEX.is_match("looking for a dash-63c321a2"));

        assert!(!CUSTOM_ENGINE_ASSOCIATION_REGEX.is_match("looking for a dash 63c321a2"));
        assert!(!CUSTOM_ENGINE_ASSOCIATION_REGEX.is_match("believer-5.2-no_underscores"));
        assert!(!CUSTOM_ENGINE_ASSOCIATION_REGEX.is_match("believer-5.2-63c321a2!"));
    }
}
