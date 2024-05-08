use anyhow::Result;
use async_trait::async_trait;

use crate::storage::anyhow;
use crate::storage::entry::ArtifactEntry;
use crate::storage::list::MethodPrefix;
use crate::storage::ArtifactProvider;
use crate::types::errors::CoreError;

// Do-nothing provider for tests
#[derive(Debug, Copy, Clone)]
pub struct MockArtifactProvider {}

impl MockArtifactProvider {
    #[allow(dead_code, clippy::new_without_default)]
    pub fn new() -> Self {
        Self {}
    }
}

#[async_trait]
impl ArtifactProvider for MockArtifactProvider {
    async fn get_artifact_by_prefix(&self, prefix: &str) -> Result<String, CoreError> {
        // TODO: Send back an actual longtail .json file to test with.
        match prefix.contains("fail") {
            true => Err(CoreError::from(anyhow!("fail".to_string()))),
            false => Ok("Fake successful file contents".to_string()),
        }
    }
    async fn get_artifact_list(
        &self,
        path: &str,
    ) -> Result<(MethodPrefix, Vec<ArtifactEntry>), CoreError> {
        let mut fake_entries = Vec::new();
        match path {
            "store/index/" => {
                fake_entries.push(ArtifactEntry::new(format!(
                    "{}{}",
                    path, "game-win64-01234567.json"
                )));
                fake_entries.push(ArtifactEntry::new(format!(
                    "{}{}",
                    path, "game-win64-11234567.json"
                )));
                fake_entries.push(ArtifactEntry::new(format!(
                    "{}{}",
                    path, "game-win64-21234567.json"
                )));
                fake_entries.push(ArtifactEntry::new(format!(
                    "{}{}",
                    path, "game-win64-31234567.json"
                )));
            }
            "store-editor/index/believerco-gameprototypemp/" => {
                fake_entries.push(ArtifactEntry::new(format!(
                    "{}{}",
                    path, "editor-win64-0deadbeef90deadbeef90deadbeef90deadbeef9.json"
                )));
                fake_entries.push(ArtifactEntry::new(format!(
                    "{}{}",
                    path, "editor-win64-1deadbeef90deadbeef90deadbeef90deadbeef9.json"
                )));
                fake_entries.push(ArtifactEntry::new(format!(
                    "{}{}",
                    path, "editor-win64-2deadbeef90deadbeef90deadbeef90deadbeef9.json"
                )));
                fake_entries.push(ArtifactEntry::new(format!(
                    "{}{}",
                    path, "editor-win64-3deadbeef90deadbeef90deadbeef90deadbeef9.json"
                )));
            }
            "v1/believerco-gameprototypemp/client/win64/development/" => {
                fake_entries.push(ArtifactEntry::new(format!(
                    "{}{}",
                    path, "0deadbeef90deadbeef90deadbeef90deadbeef9.json"
                )));
                fake_entries.push(ArtifactEntry::new(format!(
                    "{}{}",
                    path, "1deadbeef90deadbeef90deadbeef90deadbeef9.json"
                )));
                fake_entries.push(ArtifactEntry::new(format!(
                    "{}{}",
                    path, "2deadbeef90deadbeef90deadbeef90deadbeef9.json"
                )));
                fake_entries.push(ArtifactEntry::new(format!(
                    "{}{}",
                    path, "3deadbeef90deadbeef90deadbeef90deadbeef9.json"
                )));
            }
            "v1/believerco-gameprototypemp/editor/win64/development/" => {
                fake_entries.push(ArtifactEntry::new(format!(
                    "{}{}",
                    path, "0deadbeef90deadbeef90deadbeef90deadbeef9.json"
                )));
                fake_entries.push(ArtifactEntry::new(format!(
                    "{}{}",
                    path, "1deadbeef90deadbeef90deadbeef90deadbeef9.json"
                )));
                fake_entries.push(ArtifactEntry::new(format!(
                    "{}{}",
                    path, "2deadbeef90deadbeef90deadbeef90deadbeef9.json"
                )));
                fake_entries.push(ArtifactEntry::new(format!(
                    "{}{}",
                    path, "3deadbeef90deadbeef90deadbeef90deadbeef9.json"
                )));
            }
            _ => {
                fake_entries.push(ArtifactEntry::new(format!("{}{}", path, "test/file/1")));
                fake_entries.push(ArtifactEntry::new(format!("{}{}", path, "test/file/2")));
                fake_entries.push(ArtifactEntry::new(format!("{}{}", path, "test/file/3")));
                fake_entries.push(ArtifactEntry::new(format!("{}{}", path, "test/file/4")));
            }
        }
        Ok(("file://".into(), fake_entries))
    }
}

mod tests {
    #[allow(unused_imports)]
    use crate::storage::mock::MockArtifactProvider;
    #[allow(unused_imports)]
    use crate::storage::*;

    #[tokio::test]
    async fn test_artifact_list() {
        let mp = MockArtifactProvider::new();

        let ac = ArtifactConfig::new(
            "believerco-gameprototypemp".into(),
            ArtifactKind::Client,
            ArtifactBuildConfig::Development,
            Platform::Win64,
        );

        // v1 client win64 development
        let schema = "v1".parse().unwrap();
        let storage = ArtifactStorage::new(Arc::new(mp), schema);
        let al = storage.artifact_list(ac.clone()).await;
        assert_eq!(al.entries.len(), 4);
        assert_eq!(
            al.entries[3].key.0,
            "v1/believerco-gameprototypemp/client/win64/development/0deadbeef90deadbeef90deadbeef90deadbeef9.json"
        );
    }
}
