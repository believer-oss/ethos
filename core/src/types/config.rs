use std::collections::HashMap;
use std::fmt::Debug;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;

#[cfg(not(target_os = "windows"))]
use crate::fs::LocalDownloadPath;
use crate::storage::StorageSchemaVersion;
use crate::AWS_REGION;
use anyhow::{anyhow, bail, Result};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use config::Config;
use lazy_static::lazy_static;
use parking_lot::RwLock;
use regex::Regex;
use serde::{Deserialize, Serialize};

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
pub struct OktaConfig {
    pub client_id: String,
    pub issuer: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct RedactedString(String);

impl std::fmt::Debug for RedactedString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "********")
    }
}

impl From<String> for RedactedString {
    fn from(s: String) -> Self {
        RedactedString(s)
    }
}

#[allow(clippy::to_string_trait_impl)]
impl ToString for RedactedString {
    fn to_string(&self) -> String {
        self.0.clone()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectRepoConfig {
    #[serde(default, rename = "repoPath", alias = "repo_path")]
    pub repo_path: String,

    #[serde(default, rename = "repoUrl", alias = "repo_url")]
    pub repo_url: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub enum ConflictStrategy {
    #[default]
    Error,
    KeepOurs,
    KeepTheirs,
}

impl std::fmt::Display for ConflictStrategy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConflictStrategy::Error => write!(f, "Error"),
            ConflictStrategy::KeepOurs => write!(f, "KeepOurs"),
            ConflictStrategy::KeepTheirs => write!(f, "KeepTheirs"),
        }
    }
}

impl FromStr for ConflictStrategy {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "Error" => Ok(ConflictStrategy::Error),
            "KeepOurs" => Ok(ConflictStrategy::KeepOurs),
            "KeepTheirs" => Ok(ConflictStrategy::KeepTheirs),
            _ => Err(anyhow!("Invalid conflict strategy: {}", s)),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default, rename = "projects")]
    pub projects: HashMap<String, ProjectRepoConfig>,

    #[serde(default, rename = "repoPath", alias = "repo_path")]
    pub repo_path: String,

    #[serde(default, rename = "repoUrl", alias = "repo_url")]
    pub repo_url: String,

    #[serde(default, rename = "targetBranch", alias = "target_branch")]
    pub target_branch: String,

    #[serde(default, rename = "primaryBranch", alias = "primary_branch")]
    pub primary_branch: Option<String>,

    #[serde(default, rename = "contentBranch", alias = "content_branch")]
    pub content_branch: Option<String>,

    #[serde(default, rename = "conflictStrategy", alias = "conflict_strategy")]
    pub conflict_strategy: ConflictStrategy,

    #[serde(default, rename = "toolsPath", alias = "tools_path")]
    pub tools_path: String,

    #[serde(default, rename = "toolsUrl", alias = "tools_url")]
    pub tools_url: String,

    #[serde(default, rename = "userDisplayName", alias = "user_display_name")]
    pub user_display_name: String,

    #[serde(default, rename = "groupDownloadedBuildsByPlaytest")]
    pub group_downloaded_builds_by_playtest: bool,

    #[serde(default, rename = "gameClientDownloadSymbols")]
    pub game_client_download_symbols: bool,

    #[serde(default, rename = "pullDlls")]
    pub pull_dlls: bool,

    #[serde(default, rename = "editorDownloadSymbols")]
    pub editor_download_symbols: bool,

    #[serde(default, rename = "openUprojectAfterSync")]
    pub open_uproject_after_sync: bool,

    #[serde(default, rename = "githubPAT", skip_serializing_if = "Option::is_none")]
    pub github_pat: Option<RedactedString>,

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

    #[serde(default, rename = "engineAllowMultipleProcesses")]
    pub engine_allow_multiple_processes: bool,

    #[serde(default, rename = "maxClientCacheSizeGb")]
    pub max_client_cache_size_gb: u64,

    #[serde(default, rename = "recordPlay")]
    pub record_play: bool,

    #[serde(default, rename = "serverUrl")]
    pub server_url: String,

    #[serde(rename = "oktaConfig")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub okta_config: Option<OktaConfig>,

    #[serde(default)]
    pub serverless: bool,

    #[serde(default, rename = "selectedArtifactProject")]
    pub selected_artifact_project: Option<String>,

    #[serde(default = "default_playtest_region", rename = "playtestRegion")]
    pub playtest_region: String,

    #[serde(skip_serializing_if = "Option::is_none", rename = "otlp_endpoint")]
    pub otlp_endpoint: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none", rename = "otlp_headers")]
    pub otlp_headers: Option<String>,

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
            projects: HashMap::new(),
            repo_path: Default::default(),
            repo_url: Default::default(),
            target_branch: "main".to_string(),
            primary_branch: None,
            content_branch: None,
            conflict_strategy: Default::default(),
            tools_path: Default::default(),
            tools_url: Default::default(),
            user_display_name: Default::default(),
            group_downloaded_builds_by_playtest: false,
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
            engine_allow_multiple_processes: false,
            record_play: false,
            server_url: Default::default(),
            okta_config: None,
            serverless: false,
            selected_artifact_project: None,
            playtest_region: default_playtest_region(),
            otlp_endpoint: None,
            otlp_headers: None,
            max_client_cache_size_gb: 32,
            initialized: false,
        }
    }

    pub fn initialize_repo_config(&self) -> Result<RepoConfig> {
        if self.selected_artifact_project.is_none() {
            return Ok(Default::default());
        }

        let project_config = self
            .projects
            .get(self.selected_artifact_project.as_ref().unwrap());
        if project_config.is_none() {
            return Ok(Default::default());
        }

        let project_config = project_config.unwrap();

        let config_file =
            PathBuf::from(project_config.repo_path.clone()).join("friendshipper.yaml");

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
        let mut repo_config = settings
            .try_deserialize::<RepoConfig>()
            .map_err(|e| anyhow!(e))?;

        // add a no-op default profile if none exist
        let default_profile = PlaytestProfile {
            name: "Default".to_string(),
            description: String::new(),
            args: String::new(),
        };

        match repo_config.playtest_profiles {
            Some(ref mut profiles) => {
                if profiles.is_empty() {
                    profiles.push(default_profile);
                }
            }
            None => repo_config.playtest_profiles = Some(vec![default_profile]),
        };

        Ok(repo_config)
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

    pub fn get_primary_branch(&self, repo_config: &RepoConfig) -> String {
        self.primary_branch.clone().unwrap_or_else(|| {
            repo_config
                .target_branches
                .first()
                .map(|branch| branch.name.clone())
                .unwrap_or_else(|| "main".to_string())
        })
    }

    pub fn get_content_branch(&self, repo_config: &RepoConfig) -> Option<String> {
        self.content_branch.clone().or_else(|| {
            repo_config
                .target_branches
                .get(1)
                .map(|branch| branch.name.clone())
        })
    }

    pub fn initialize_branch_defaults(&mut self, repo_config: &RepoConfig) -> bool {
        let mut updated = false;

        if self.primary_branch.is_none() {
            if let Some(primary) = repo_config.target_branches.first() {
                self.primary_branch = Some(primary.name.clone());
                updated = true;
            }
        }

        if self.content_branch.is_none() {
            if let Some(content) = repo_config.target_branches.get(1) {
                self.content_branch = Some(content.name.clone());
                updated = true;
            }
        }

        updated
    }

    pub fn validate_configured_branches(&self, repo_config: &RepoConfig) -> Result<()> {
        let target_branch_names: Vec<&String> = repo_config
            .target_branches
            .iter()
            .map(|branch| &branch.name)
            .collect();

        // Validate primary branch
        if let Some(ref primary_branch) = self.primary_branch {
            if !target_branch_names.contains(&primary_branch) {
                bail!(
                    "Configured primary branch '{}' does not exist in repository target branches: {:?}",
                    primary_branch,
                    target_branch_names
                );
            }
        }

        // Validate content branch
        if let Some(ref content_branch) = self.content_branch {
            if !target_branch_names.contains(&content_branch) {
                bail!(
                    "Configured content branch '{}' does not exist in repository target branches: {:?}",
                    content_branch,
                    target_branch_names
                );
            }
        }

        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct ConfigValidationError(pub String);

impl IntoResponse for ConfigValidationError {
    fn into_response(self) -> Response {
        (StatusCode::BAD_REQUEST, format!("{self}")).into_response()
    }
}

impl std::fmt::Display for ConfigValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Failed to save preferences: {}", self.0)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaytestProfile {
    name: String,
    description: String,
    args: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetBranchConfig {
    pub name: String,

    #[serde(rename = "usesMergeQueue")]
    pub uses_merge_queue: bool,
}

impl Default for TargetBranchConfig {
    fn default() -> Self {
        TargetBranchConfig {
            name: "main".to_string(),
            uses_merge_queue: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoConfig {
    #[serde(default, rename = "uprojectPath")]
    pub uproject_path: String,

    #[serde(default, rename = "trunkBranch")]
    pub trunk_branch: String,

    #[serde(default, rename = "targetBranches")]
    pub target_branches: Vec<TargetBranchConfig>,

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

    #[serde(default, rename = "playtestProfiles")]
    pub playtest_profiles: Option<Vec<PlaytestProfile>>,

    #[serde(default, rename = "editorUrlScheme")]
    pub editor_url_scheme: Option<String>,

    #[serde(default, rename = "buildsEnabled")]
    pub builds_enabled: bool,

    #[serde(default, rename = "serversEnabled")]
    pub servers_enabled: bool,
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
            target_branches: vec![TargetBranchConfig::default()],
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
            playtest_profiles: Some(vec![]),
            editor_url_scheme: None,
            builds_enabled: false,
            servers_enabled: false,
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

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FriendshipperConfig {
    pub artifact_bucket_name: String,
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

    #[serde(default, rename = "profileDataPath")]
    pub profile_data_path: String,

    #[serde(default)]
    pub kubernetes_cluster_name: String,

    #[serde(skip_serializing_if = "Option::is_none", rename = "otlp_endpoint")]
    pub otlp_endpoint: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none", rename = "otlp_headers")]
    pub otlp_headers: Option<String>,

    #[serde(default, rename = "playtestRegions")]
    pub playtest_regions: Vec<String>,

    #[serde(default, rename = "mobileURLScheme")]
    pub mobile_url_scheme: String,
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
