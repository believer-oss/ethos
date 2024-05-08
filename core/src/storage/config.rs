use serde::{Deserialize, Serialize};
use std::fmt::Display;

// This is the project name, expected to be the name of the git org and repo,
// lowercased, separated by dashes
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Project(pub String);

impl Display for Project {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<&str> for Project {
    fn from(s: &str) -> Self {
        Self(s.to_string().to_lowercase())
    }
}

impl Project {
    pub fn new(org_name: &str, repo_name: &str) -> Self {
        Self(format!(
            "{}-{}",
            org_name.to_string().to_lowercase(),
            repo_name.to_string().to_lowercase()
        ))
    }
}

// This is a list of artifact types
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ArtifactKind {
    // UE client target artifacts
    Client,
    ClientSymbols,
    // UE editor target dll/so artifacts
    Editor,
    EditorSymbols,
    // UE installed engine artifacts
    Engine,
    EngineSymbols,
}

impl Display for ArtifactKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

impl ArtifactKind {
    pub fn new(kind: &str) -> Self {
        match kind {
            "client" => ArtifactKind::Client,
            "client-symbols" => ArtifactKind::ClientSymbols,
            "editor" => ArtifactKind::Editor,
            "editor-symbols" => ArtifactKind::EditorSymbols,
            "engine" => ArtifactKind::Engine,
            "engine-symbols" => ArtifactKind::EngineSymbols,
            _ => panic!("Unknown artifact kind: {}", kind),
        }
    }

    pub(super) fn to_path_string(&self) -> String {
        match self {
            ArtifactKind::Client => "client".to_owned(),
            ArtifactKind::ClientSymbols => "client-symbols".to_owned(),
            ArtifactKind::Editor => "editor".to_owned(),
            ArtifactKind::EditorSymbols => "editor-symbols".to_owned(),
            ArtifactKind::Engine => "engine".to_owned(),
            ArtifactKind::EngineSymbols => "engine-symbols".to_owned(),
        }
    }
}

// List of artifact build configurations
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ArtifactBuildConfig {
    Debug,
    DebugGame,
    Development,
    Shipping,
    Test,
}

impl Display for ArtifactBuildConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

impl ArtifactBuildConfig {
    pub fn new(config: &str) -> Self {
        match config {
            "debug" => ArtifactBuildConfig::Debug,
            "debug-game" => ArtifactBuildConfig::DebugGame,
            "development" => ArtifactBuildConfig::Development,
            "shipping" => ArtifactBuildConfig::Shipping,
            "test" => ArtifactBuildConfig::Test,
            _ => panic!("Unknown artifact kind config: {}", config),
        }
    }

    pub(super) fn to_path_string(&self) -> String {
        match self {
            ArtifactBuildConfig::Debug => "debug".to_owned(),
            ArtifactBuildConfig::DebugGame => "debug-game".to_owned(),
            ArtifactBuildConfig::Development => "development".to_owned(),
            ArtifactBuildConfig::Shipping => "shipping".to_owned(),
            ArtifactBuildConfig::Test => "test".to_owned(),
        }
    }
}

// List of potential platforms that could contain artifacts
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Platform {
    Win64,
    Mac,
    Ios,
    Android,
    Linux,
    LinuxArm64,
}

impl Display for Platform {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

impl Platform {
    pub fn new(platform: &str) -> Self {
        match platform {
            "win64" => Platform::Win64,
            "mac" => Platform::Mac,
            "ios" => Platform::Ios,
            "android" => Platform::Android,
            "linux" => Platform::Linux,
            "linux-arm64" => Platform::LinuxArm64,
            _ => panic!("Unknown platform: {}", platform),
        }
    }

    pub(super) fn to_path_string(&self) -> String {
        match self {
            Platform::Win64 => "win64".to_owned(),
            Platform::Mac => "mac".to_owned(),
            Platform::Ios => "ios".to_owned(),
            Platform::Android => "android".to_owned(),
            Platform::Linux => "linux".to_owned(),
            Platform::LinuxArm64 => "linux-arm64".to_owned(),
        }
    }
}

// Configuration for the artifacts being requested
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ArtifactConfig {
    // The project/repo name
    pub project: Project,

    // Game, Editor, etc
    pub artifact_kind: ArtifactKind,

    // Test, Development, Shipping, etc.
    pub artifact_build_config: ArtifactBuildConfig,

    // Linux, Mac, Win64, etc
    pub platform: Platform,
}

impl ArtifactConfig {
    pub fn new(
        project: Project,
        artifact_kind: ArtifactKind,
        artifact_build_config: ArtifactBuildConfig,
        platform: Platform,
    ) -> Self {
        Self {
            project,
            artifact_kind,
            artifact_build_config,
            platform,
        }
    }
}
