use crate::storage::entry::ArtifactEntry;
use serde::{Deserialize, Serialize};
use std::fmt::Display;

use super::config::ArtifactConfig;

// This is the prefix for the storage, ex: s3://$bucketname/
//   Combining this with the ObjectLocation gives the full key
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MethodPrefix(pub String);

impl MethodPrefix {
    // Since we only store the path in the objectlocation, this constructs the full URL
    pub fn get_storage_url(&self, artifact_entry: &ArtifactEntry) -> String {
        format!("{}{}", self.0, artifact_entry.key)
    }
}

impl Display for MethodPrefix {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<&str> for MethodPrefix {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl From<String> for MethodPrefix {
    fn from(s: String) -> Self {
        Self(s)
    }
}

// Container for the list of S3 entries and associated metadata
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactList {
    // The sorted list of artifacts
    pub entries: Vec<ArtifactEntry>,

    // Configuration of the artifacts in the list
    pub artifact_config: ArtifactConfig,

    // URL method and prefix, ex. s3://$bucketname/
    pub method_prefix: MethodPrefix,
}

impl ArtifactList {
    pub fn new(artifact_config: ArtifactConfig, method_prefix: MethodPrefix) -> Self {
        Self {
            entries: Vec::new(),
            artifact_config,
            method_prefix,
        }
    }

    // Since we only store the path in the objectlocation, this constructs the full URL
    pub fn get_storage_url(&self, artifact_entry: &ArtifactEntry) -> String {
        format!("{}{}", self.method_prefix, artifact_entry.key)
    }

    pub fn sort_by_last_modified(&mut self) -> &Self {
        self.entries
            .sort_by_key(|b| std::cmp::Reverse(b.last_modified));
        self
    }

    pub fn iter(&self) -> impl Iterator<Item = &ArtifactEntry> {
        self.entries.iter()
    }
}

mod test {
    #[allow(unused_imports)]
    use std::time::{Duration, SystemTime};

    #[allow(unused_imports)]
    use super::*;

    #[test]
    fn test_sort_by_last_modified() {
        let base = SystemTime::now();
        let ac = ArtifactConfig::new(
            "fake-project".into(),
            crate::storage::ArtifactKind::Client,
            crate::storage::ArtifactBuildConfig::Development,
            crate::storage::Platform::Win64,
        );
        let mut list = ArtifactList::new(ac, "test:///".into());
        let mut entry = ArtifactEntry::new("v1/test-project/dir/older".to_string());
        entry.last_modified = base - Duration::from_secs(10);
        list.entries.push(entry);
        let mut entry = ArtifactEntry::new("v1/test-project/dir/new".to_string());
        entry.last_modified = base - Duration::from_secs(5);
        list.entries.push(entry);
        let mut entry = ArtifactEntry::new("v1/test-project/dir/oldest".to_string());
        entry.last_modified = base - Duration::from_secs(30);
        list.entries.push(entry);
        list.sort_by_last_modified();

        assert_eq!(
            list.iter().map(|e| e.key.to_string()).collect::<Vec<_>>(),
            vec![
                "v1/test-project/dir/new",
                "v1/test-project/dir/older",
                "v1/test-project/dir/oldest"
            ]
        );
    }

    #[test]
    fn test_get_storage_url() {
        let ac = ArtifactConfig::new(
            "fake-project".into(),
            crate::storage::ArtifactKind::Client,
            crate::storage::ArtifactBuildConfig::Development,
            crate::storage::Platform::Win64,
        );
        let mut list = ArtifactList::new(ac, "test:///".into());
        list.entries
            .push(ArtifactEntry::new(String::from("v1/test-project/dir/a")));

        assert_eq!(
            list.get_storage_url(&list.entries[0]),
            "test:///v1/test-project/dir/a"
        );
    }
}
