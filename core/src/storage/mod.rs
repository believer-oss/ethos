use std::str::FromStr;
use std::sync::Arc;

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tracing::error;

pub use crate::storage::config::ArtifactBuildConfig;
pub use crate::storage::config::ArtifactConfig;
pub use crate::storage::config::ArtifactKind;
pub use crate::storage::config::Platform;
pub use crate::storage::entry::ArtifactEntry;
pub use crate::storage::list::ArtifactList;
pub use crate::storage::list::MethodPrefix;
pub use crate::storage::s3::S3ArtifactProvider;
use crate::types::errors::CoreError;

pub mod config;
pub mod entry;
pub mod list;
pub mod mock;
pub mod s3;

// Trait implementing artifact lookup different storage providers
#[async_trait]
pub trait ArtifactProvider: std::fmt::Debug + Send + Sync {
    fn get_method_prefix(&self) -> MethodPrefix;

    // Pulls the artifact listing for the given path from the provider
    async fn get_artifact_list(
        &self,
        path: &str,
    ) -> Result<(MethodPrefix, Vec<ArtifactEntry>), CoreError>;

    // Get a single artifact matching the given prefix, erroring if there is more than one match
    async fn get_artifact_by_prefix(&self, prefix: &str) -> Result<String, CoreError>;
}

// Storage version implementations
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum StorageSchemaVersion {
    #[default]
    V1,
}

impl FromStr for StorageSchemaVersion {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "v1" => Ok(StorageSchemaVersion::V1),
            _ => Err(anyhow!("Unknown storage schema version: {}", s)),
        }
    }
}

// Wrapper pairing an artifact provider with the schema to use
#[derive(Clone, Debug)]
pub struct ArtifactStorage {
    provider: Arc<dyn ArtifactProvider>,
    schema_version: StorageSchemaVersion,
}

impl ArtifactStorage {
    pub fn new(provider: Arc<dyn ArtifactProvider>, schema_version: StorageSchemaVersion) -> Self {
        Self {
            provider,
            schema_version,
        }
    }

    // Use the provider to get the artifact with the given short sha prefix. This allows
    // for the v0 schema, where it'll match the filename without the .json extension,
    // and the v1 schema where any number of characters in the prefix of the 40-character sha
    // can be used for the engine lookup.
    pub async fn get_from_short_sha(
        &self,
        artifact_config: ArtifactConfig,
        short_sha: &str,
    ) -> Result<String, CoreError> {
        let path = self.resolve_path(&artifact_config);

        self.provider
            .get_artifact_by_prefix(&format!("{}{}", path, short_sha))
            .await
    }

    // Use the provider to get an artifact listing by constructing the lookup path.
    pub async fn artifact_list(&self, artifact_config: ArtifactConfig) -> ArtifactList {
        let artifact_list_result = self
            .provider
            .get_artifact_list(&self.resolve_path(&artifact_config))
            .await;

        match artifact_list_result {
            Err(e) => {
                let method_prefix: MethodPrefix = self.provider.get_method_prefix();
                error!("Caught error retrieving artifact list with artifact config {:?} and method prefix {:?}. {}", artifact_config, method_prefix, e);
                ArtifactList::new(artifact_config, method_prefix)
            }
            Ok((method_prefix, entries)) => {
                let mut list = ArtifactList::new(artifact_config, method_prefix);
                list.entries = entries;
                list.sort_by_last_modified();
                list
            }
        }
    }

    pub async fn get_artifact_for_commit(
        &self,
        artifact_config: ArtifactConfig,
        commit: &str,
    ) -> Result<ArtifactEntry, CoreError> {
        let artifact_list = self.artifact_list(artifact_config).await;
        let artifact_entry = artifact_list
            .entries
            .iter()
            .find(|entry| entry.commit == Some(commit.to_string()))
            .ok_or(anyhow!("Artifact not found for commit {}", commit))?;
        Ok(artifact_entry.clone())
    }

    fn resolve_path(&self, artifact_config: &ArtifactConfig) -> String {
        match &self.schema_version {
            StorageSchemaVersion::V1 => Self::resolve_path_v1(artifact_config),
        }
    }
    // This is the v1 path resolver with full support for the new enums
    fn resolve_path_v1(artifact_config: &ArtifactConfig) -> String {
        let path = format!(
            "v1/{}/{}/{}/{}/",
            artifact_config.project,
            artifact_config.artifact_kind.to_path_string(),
            artifact_config.platform.to_path_string(),
            artifact_config.artifact_build_config.to_path_string(),
        );
        path
    }
}

mod test {
    #[allow(unused_imports)]
    use super::*;

    #[test]
    fn test_resolve_path() {
        let ac = ArtifactConfig::new(
            "believerco-gameprototypemp".into(),
            ArtifactKind::Client,
            ArtifactBuildConfig::Development,
            Platform::Win64,
        );
        assert_eq!(
            ArtifactStorage::resolve_path_v1(&ac),
            "v1/believerco-gameprototypemp/client/win64/development/"
        );

        let ac = ArtifactConfig::new(
            "believerco-gameprototypemp".into(),
            ArtifactKind::Engine,
            ArtifactBuildConfig::Development,
            Platform::Win64,
        );

        assert_eq!(
            ArtifactStorage::resolve_path_v1(&ac),
            "v1/believerco-gameprototypemp/engine/win64/development/"
        );
    }
}
