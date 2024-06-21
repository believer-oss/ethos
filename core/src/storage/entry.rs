use semver::Version;
use serde::{Deserialize, Serialize};
use serde_with::{serde_as, TimestampSeconds};
use std::convert::TryFrom;
use std::fmt::Display;
use std::time::SystemTime;

use crate::{storage::ArtifactStorage, types::errors::CoreError};

use super::ArtifactConfig;

// This is the path inside the underlying storage, without the prefix or bucket
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectLocation(pub String);

impl ObjectLocation {
    // This produces the filename at the end of the key
    //   v1: v1/testproj/client/win64/shipping/abcdef0123abcdef0123abcdef0123abcdef0123.json
    //     -> abcdef0123abcdef0123abcdef0123abcdef0123.json
    pub fn filename(&self) -> String {
        let parts: Vec<&str> = self.0.split('/').collect();
        if parts.len() > 1 {
            parts[parts.len() - 1].to_string()
        } else {
            // If we're passed an invalid path, return an empty string
            "".to_string()
        }
    }

    // This produces the suffix of the filename, assuming dash separated parts, examples:
    //   v1: v1/testproj/client/win64/shipping/abcdef0123abcdef0123abcdef0123abcdef0123.json
    //     -> abcdef0123abcdef0123abcdef0123abcdef0123
    pub fn suffix(&self) -> Option<String> {
        let filename = self.filename();
        let name = filename.split('.').next();
        name.map(|s| {
            let parts: Vec<&str> = s.split('-').collect();
            parts[parts.len() - 1].to_string()
        })
    }

    // eg. game-win64, stripping off the sha if commit is defined
    // This is used to construct a stable local directory name to sync the artifact to
    //   v1: v1/testproj/client/win64/shipping/abcdef0123abcdef0123abcdef0123abcdef0123.json
    //     -> client-win64
    pub fn base_name(&self) -> String {
        // v1
        // Use the full path to derive the directory name
        // NOTE: We may want to add a path element for keeping clients from multiple repos
        // separate in the future.
        let parts: Vec<&str> = self.0.split('/').collect();
        if parts.len() < 4 {
            return "".to_string();
        }

        let kind = parts.get(parts.len() - 4);
        let platform = parts.get(parts.len() - 3);
        if let (Some(kind), Some(platform)) = (kind, platform) {
            format!("{}-{}", kind, platform)
        } else {
            "".to_string()
        }
    }

    // Return the commit if the suffix is a valid git sha
    pub fn full_commit(&self) -> Option<String> {
        self.suffix()
            .filter(|s| s.len() == 40)
            .filter(|s| s.chars().all(|c| c.is_ascii_hexdigit()))
    }

    // Return the commit if the suffix is a valid 8 character git sha
    pub fn short_commit(&self) -> Option<String> {
        self.suffix()
            .filter(|s| s.len() == 8)
            .filter(|s| s.chars().all(|c| c.is_ascii_hexdigit()))
    }

    // Return the commit, potentially truncating to 8 characters
    pub fn commit(&self, short: bool) -> Option<String> {
        self.short_commit().or(self.full_commit()).map(|mut s| {
            if short {
                s.truncate(8)
            };
            s
        })
    }

    // This produces the filename without the extension
    //   v1: v1/testproj/client/win64/shipping/abcdef0123abcdef0123abcdef0123abcdef0123.json
    //     -> abcdef0123abcdef0123abcdef0123abcdef0123
    pub fn display_name(&self) -> String {
        format!(
            "{}-{}",
            self.base_name(),
            self.commit(true).unwrap_or_default()
        )
    }

    // This assumes that the key contains a version string as the last element before the basename, examples:
    //   friendshipper/v1.2.3/friendshipper.exe
    //
    pub fn semver(&self) -> Option<Version> {
        let parts: Vec<&str> = self.0.split('/').collect();

        if parts.len() < 2 {
            return None;
        }

        let version = parts[parts.len() - 2].replace('v', "");
        match Version::parse(&version) {
            Ok(v) => Some(v),
            Err(_) => None,
        }
    }
}

impl From<&str> for ObjectLocation {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl Display for ObjectLocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

// Contains the object location and the last modified time
#[serde_as]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactEntry {
    // Path inside the storage system
    pub key: ObjectLocation,
    // Partial key suitable for UX
    pub display_name: String,
    // Used to sort multiple entries into a known order
    #[serde_as(as = "TimestampSeconds<f64>")]
    pub last_modified: SystemTime,
    // If possible, derived commit sha
    pub commit: Option<String>,
}

impl Default for ArtifactEntry {
    fn default() -> Self {
        Self {
            key: ObjectLocation(String::new()),
            display_name: String::new(),
            last_modified: SystemTime::now(),
            commit: None,
        }
    }
}

impl From<aws_sdk_s3::types::Object> for ArtifactEntry {
    fn from(object: aws_sdk_s3::types::Object) -> Self {
        let key = ObjectLocation(object.key.unwrap_or_default());
        let last_modified = if let Some(dt) = object.last_modified {
            SystemTime::try_from(dt).unwrap()
        } else {
            SystemTime::now()
        };

        let display_name = key.display_name();

        let commit = key.commit(false);

        Self {
            key,
            display_name,
            last_modified,
            commit,
        }
    }
}

impl ArtifactEntry {
    pub fn new(key: String) -> Self {
        let key = ObjectLocation(key);
        let display_name = key.display_name();
        let commit = key.commit(false);

        Self {
            key,
            display_name,
            last_modified: SystemTime::now(),
            commit,
        }
    }

    pub fn get_semver(&self) -> Option<Version> {
        self.key.semver()
    }

    pub fn base_name(&self) -> String {
        tracing::info!("In base_name: {:?}", self);
        self.key.base_name()
    }

    // Convert this entry to a path from another config
    pub fn convert_to_config(
        &self,
        config: &ArtifactConfig,
        storage: &ArtifactStorage,
    ) -> Result<Self, CoreError> {
        let commit = self.commit.clone();
        let path = storage.resolve_path(config);
        // This only works for V1 schema
        Ok(Self::new(format!(
            "{}{}.json",
            path,
            commit.unwrap_or_default()
        )))
    }
}

mod tests {
    #[allow(unused_imports)]
    use crate::storage::{
        mock::MockArtifactProvider, ArtifactBuildConfig, ArtifactConfig, ArtifactKind,
        ArtifactStorage, Platform,
    };
    #[allow(unused_imports)]
    use std::{collections::HashMap, sync::Arc};

    use super::{ArtifactEntry, ObjectLocation};

    // ObjectLocation tests

    lazy_static::lazy_static! {
        static ref FAKE_OBJS: HashMap<String, String> = HashMap::from([
            ("friendshipper_ver".into(), "friendshipper/v1.2.3/friendshipper.exe".into()),
            ("friendshipper_no_ver".into(), "friendshipper/test/friendshipper.exe".into()),
            ("game_short_commit".into(), "testproj/game/win64/shipping/game-win64-abcdef01.json".into()),
            ("game_full_commit".into(), "testproj/game/win64/shipping/game-win64-abcdef01abcdef01abcdef01abcdef01abcdef01.json".into()),
            ("game_path".into(), "testproj/client/win64/shipping/".into()),
            ("game_v1".into(), "v1/believerco-testproj/client/win64/shipping/abcdef01abcdef01abcdef01abcdef01abcdef01.json".into()),
            ("invalid_short_path".into(), "path/without-enough-elements".into()),
        ]);
    }

    #[allow(dead_code)]
    fn get_objectlocation(s: &str) -> ObjectLocation {
        ObjectLocation(FAKE_OBJS.get(s).unwrap().clone())
    }

    #[allow(dead_code)]
    fn get_entry(s: &str) -> ArtifactEntry {
        ArtifactEntry::new(FAKE_OBJS.get(s).unwrap().clone())
    }

    #[test]
    fn filename() {
        // successful condition
        let ol = get_objectlocation("game_v1");
        assert_eq!(
            ol.filename(),
            String::from("abcdef01abcdef01abcdef01abcdef01abcdef01.json")
        );

        // unsuccessful condition
        let ol = get_objectlocation("game_path");
        assert_eq!(ol.filename(), String::from(""));
    }

    #[test]
    fn suffix() {
        // successful condition
        let ol = get_objectlocation("game_short_commit").suffix().unwrap();
        assert_eq!(ol, String::from("abcdef01"));
        let ol = get_objectlocation("game_v1").suffix().unwrap();
        assert_eq!(ol, String::from("abcdef01abcdef01abcdef01abcdef01abcdef01"));

        // unsuccessful condition
        let ol = get_objectlocation("game_path").suffix().unwrap();
        assert_eq!(ol, String::from(""));
    }

    #[test]
    fn base_name() {
        // successful condition
        let ol = get_objectlocation("game_v1");
        assert_eq!(ol.base_name(), String::from("client-win64"));

        // unsuccessful condition
        let ol = get_objectlocation("invalid_short_path");
        assert_eq!(ol.base_name(), String::from(""));
    }

    #[test]
    fn full_commit() {
        // successful condition
        let ol = get_objectlocation("game_full_commit");
        assert_eq!(
            ol.full_commit(),
            Some(String::from("abcdef01abcdef01abcdef01abcdef01abcdef01"))
        );

        // unsuccessful condition
        let ol = get_objectlocation("game_path");
        assert_eq!(ol.full_commit(), None);
    }

    #[test]
    fn short_commit() {
        // successful condition
        let ol = get_objectlocation("game_short_commit");
        assert_eq!(ol.short_commit(), Some(String::from("abcdef01")));

        // unsuccessful condition
        let ol = get_objectlocation("game_path");
        assert_eq!(ol.short_commit(), None);
        let ol = get_objectlocation("game_v1");
        assert_eq!(ol.short_commit(), None);
    }

    #[test]
    fn display_name() {
        // successful condition
        let ol = get_objectlocation("game_v1");
        assert_eq!(ol.display_name(), String::from("client-win64-abcdef01"));

        // unsuccessful condition
        // Since we don't find a base_name or commit, it'll just be "-"
        let ol = get_objectlocation("invalid_short_path");
        assert_eq!(ol.display_name(), String::from("-"));
    }

    #[test]
    fn semver() {
        // successful condition
        let ol = get_objectlocation("friendshipper_ver");
        assert_eq!(ol.semver().unwrap().to_string(), String::from("1.2.3"));

        // unsuccessful condition
        let ol = get_objectlocation("friendshipper_no_ver");
        assert_eq!(ol.semver(), None);
    }

    #[test]
    fn commit() {
        // successful condition
        let ol = get_objectlocation("game_full_commit");
        assert_eq!(ol.commit(true), Some(String::from("abcdef01")));
        assert_eq!(
            ol.commit(false),
            Some(String::from("abcdef01abcdef01abcdef01abcdef01abcdef01"))
        );
        // unsuccessful condition
        let ol = get_objectlocation("game_path");
        assert_eq!(ol.commit(true), None);
        assert_eq!(ol.commit(false), None);
    }

    #[test]
    fn convert_to_config() {
        // successful condition
        let entry = get_entry("game_v1");
        let mp = MockArtifactProvider::new();
        let config = ArtifactConfig::new(
            "believerco-testproj".into(),
            ArtifactKind::ClientSymbols,
            ArtifactBuildConfig::Shipping,
            Platform::Win64,
        );
        let schema = "v1".parse().unwrap();
        let storage = ArtifactStorage::new(Arc::new(mp), schema);
        let converted = entry.convert_to_config(&config, &storage).unwrap();
        assert_eq!(
            converted.key.0,
            "v1/believerco-testproj/client-symbols/win64/shipping/abcdef01abcdef01abcdef01abcdef01abcdef01.json"
        );
        let config = ArtifactConfig::new(
            "believerco-testproj".into(),
            ArtifactKind::EditorSymbols,
            ArtifactBuildConfig::Development,
            Platform::Win64,
        );
        let converted = entry.convert_to_config(&config, &storage).unwrap();
        assert_eq!(
            converted.key.0,
            "v1/believerco-testproj/editor-symbols/win64/development/abcdef01abcdef01abcdef01abcdef01abcdef01.json"
        );
    }
}
