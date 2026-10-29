//! Channels: the places a shard serves a client build from, and their keys.

use std::fmt;
use std::str::FromStr;

use kube::KubeSchema;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Pattern for a launcher `metadataPath` or a Steam `branch`. Keeping both label-safe means a channel
/// key maps to a label value and back without loss (see [`ChannelKey::label_value`]).
pub(crate) const CHANNEL_NAME_PATTERN: &str = r"^[A-Za-z0-9]([-A-Za-z0-9_.]*[A-Za-z0-9])?$";

/// Longest `metadataPath` or `branch`: `launcher.` plus this many characters fits a 63-character label.
pub(crate) const CHANNEL_NAME_MAX_LEN: usize = 54;

/// The CEL rule that a channel reference names exactly the field its kind needs.
pub(crate) const CHANNEL_FIELDS_RULE: &str = "self.kind == 'launcher' ? (has(self.metadataPath) && !has(self.branch)) : (has(self.branch) && !has(self.metadataPath))";
pub(crate) const CHANNEL_FIELDS_MESSAGE: &str =
    "a launcher channel needs metadataPath and no branch; a steam channel needs branch and no metadataPath";

/// How a channel distributes the client.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum ChannelKind {
    /// The launcher reads a metadata pointer that names the build.
    Launcher,
    /// A Steam branch.
    Steam,
}

impl ChannelKind {
    /// The kind as it appears in keys, labels and the promote workflow's `distribution` parameter.
    pub fn as_str(self) -> &'static str {
        match self {
            ChannelKind::Launcher => "launcher",
            ChannelKind::Steam => "steam",
        }
    }
}

impl fmt::Display for ChannelKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The client build configuration a channel serves.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum GameConfig {
    Development,
    Test,
    Shipping,
}

impl GameConfig {
    /// The value the promote workflow's `game_config` parameter takes: the name unchanged
    /// (`Development`, `Test`, `Shipping`). The Steam upload writes it verbatim into the build
    /// description, which is parsed back as-is, so it must not be lowercased here; the workflow
    /// template lowercases where it needs to.
    pub fn workflow_value(self) -> &'static str {
        match self {
            GameConfig::Development => "Development",
            GameConfig::Test => "Test",
            GameConfig::Shipping => "Shipping",
        }
    }

    /// Whether a channel of `kind` may serve this config. Steam takes only `Test` and `Shipping`.
    pub fn allowed_on(self, kind: ChannelKind) -> bool {
        match kind {
            ChannelKind::Launcher => true,
            ChannelKind::Steam => matches!(self, GameConfig::Test | GameConfig::Shipping),
        }
    }
}

/// A reference to one channel of a shard, as written in a `Promotion` spec:
/// `{kind: launcher, metadataPath: …}` or `{kind: steam, branch: …}`.
///
/// It is a flat struct rather than a Rust enum so the CRD schema stays structural; a CEL rule enforces
/// that exactly the right field is set. Use [`ChannelRef::key`] to get the typed form.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, KubeSchema)]
#[serde(rename_all = "camelCase")]
#[x_kube(validation = Rule::new(CHANNEL_FIELDS_RULE).message(CHANNEL_FIELDS_MESSAGE))]
#[cfg_attr(test, x_kube(cel))]
pub struct ChannelRef {
    pub kind: ChannelKind,

    /// Launcher only: the key of the metadata pointer the launcher reads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = CHANNEL_NAME_MAX_LEN), regex(pattern = CHANNEL_NAME_PATTERN))]
    pub metadata_path: Option<String>,

    /// Steam only: the branch name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = CHANNEL_NAME_MAX_LEN), regex(pattern = CHANNEL_NAME_PATTERN))]
    pub branch: Option<String>,
}

impl ChannelRef {
    /// The channel's key, or an error if the fields don't match the kind.
    pub fn key(&self) -> Result<ChannelKey, ParseChannelKeyError> {
        ChannelKey::from_parts(
            self.kind,
            self.metadata_path.as_deref(),
            self.branch.as_deref(),
        )
    }
}

impl From<&ChannelKey> for ChannelRef {
    fn from(key: &ChannelKey) -> Self {
        match key {
            ChannelKey::Launcher { metadata_path } => ChannelRef {
                kind: ChannelKind::Launcher,
                metadata_path: Some(metadata_path.clone()),
                branch: None,
            },
            ChannelKey::Steam { branch } => ChannelRef {
                kind: ChannelKind::Steam,
                metadata_path: None,
                branch: Some(branch.clone()),
            },
        }
    }
}

/// The identity of a channel within a shard.
///
/// - Key form ([`Display`](fmt::Display) / [`FromStr`]): `launcher/<metadataPath>` or `steam/<branch>`.
///   Used in `Shard.status.channels[].key` and `promotions[].channel`.
/// - Label form ([`label_value`](Self::label_value) / [`from_label_value`](Self::from_label_value)):
///   `launcher.<metadataPath>` or `steam.<branch>`. Used for the [`LABEL_CHANNEL`](crate::LABEL_CHANNEL)
///   label, since label values can't contain `/`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ChannelKey {
    Launcher { metadata_path: String },
    Steam { branch: String },
}

impl ChannelKey {
    pub fn launcher(metadata_path: impl Into<String>) -> Self {
        ChannelKey::Launcher {
            metadata_path: metadata_path.into(),
        }
    }

    pub fn steam(branch: impl Into<String>) -> Self {
        ChannelKey::Steam {
            branch: branch.into(),
        }
    }

    pub fn kind(&self) -> ChannelKind {
        match self {
            ChannelKey::Launcher { .. } => ChannelKind::Launcher,
            ChannelKey::Steam { .. } => ChannelKind::Steam,
        }
    }

    /// The `metadataPath` or `branch`.
    pub fn name(&self) -> &str {
        match self {
            ChannelKey::Launcher { metadata_path } => metadata_path,
            ChannelKey::Steam { branch } => branch,
        }
    }

    /// The label-safe form, e.g. `steam.test` for `steam/test`.
    pub fn label_value(&self) -> String {
        format!("{}.{}", self.kind(), self.name())
    }

    /// Parses the label form produced by [`label_value`](Self::label_value).
    pub fn from_label_value(value: &str) -> Result<Self, ParseChannelKeyError> {
        Self::split(value, '.')
    }

    /// Builds a key from a channel's fields, checking that exactly the field the kind needs is set.
    pub(crate) fn from_parts(
        kind: ChannelKind,
        metadata_path: Option<&str>,
        branch: Option<&str>,
    ) -> Result<Self, ParseChannelKeyError> {
        let key = match (kind, metadata_path, branch) {
            (ChannelKind::Launcher, Some(path), None) => ChannelKey::launcher(path),
            (ChannelKind::Steam, None, Some(branch)) => ChannelKey::steam(branch),
            _ => return Err(ParseChannelKeyError::FieldsMismatch(kind)),
        };
        key.check_name()?;
        Ok(key)
    }

    fn split(value: &str, separator: char) -> Result<Self, ParseChannelKeyError> {
        let (kind, name) = value
            .split_once(separator)
            .ok_or_else(|| ParseChannelKeyError::Malformed(value.to_string()))?;
        let key = match kind {
            "launcher" => ChannelKey::launcher(name),
            "steam" => ChannelKey::steam(name),
            other => return Err(ParseChannelKeyError::UnknownKind(other.to_string())),
        };
        key.check_name()?;
        Ok(key)
    }

    fn check_name(&self) -> Result<(), ParseChannelKeyError> {
        let name = self.name();
        let first_last_ok = |c: Option<char>| c.is_some_and(|c| c.is_ascii_alphanumeric());
        let valid = name.len() <= CHANNEL_NAME_MAX_LEN
            && first_last_ok(name.chars().next())
            && first_last_ok(name.chars().next_back())
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
        if valid {
            Ok(())
        } else {
            Err(ParseChannelKeyError::InvalidName(name.to_string()))
        }
    }
}

impl fmt::Display for ChannelKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.kind(), self.name())
    }
}

impl FromStr for ChannelKey {
    type Err = ParseChannelKeyError;

    /// Parses the key form, `launcher/<metadataPath>` or `steam/<branch>`.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::split(s, '/')
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ParseChannelKeyError {
    #[error("channel key {0:?} has no kind separator")]
    Malformed(String),
    #[error("unknown channel kind {0:?}")]
    UnknownKind(String),
    #[error("channel name {0:?} is empty, too long, or not label-safe")]
    InvalidName(String),
    #[error("a {0} channel needs exactly its own name field set")]
    FieldsMismatch(ChannelKind),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_round_trips_through_both_forms() {
        for key in [
            ChannelKey::launcher("stable-1.0"),
            ChannelKey::steam("test"),
            ChannelKey::steam("beta_x"),
        ] {
            assert_eq!(key.to_string().parse::<ChannelKey>().unwrap(), key);
            assert_eq!(
                ChannelKey::from_label_value(&key.label_value()).unwrap(),
                key
            );
        }
    }

    #[test]
    fn key_forms() {
        let key = ChannelKey::launcher("stable-1.0");
        assert_eq!(key.to_string(), "launcher/stable-1.0");
        assert_eq!(key.label_value(), "launcher.stable-1.0");
        assert_eq!(ChannelKey::steam("test").label_value(), "steam.test");
    }

    #[test]
    fn label_form_splits_on_the_first_dot_only() {
        assert_eq!(
            ChannelKey::from_label_value("launcher.v1.2").unwrap(),
            ChannelKey::launcher("v1.2")
        );
    }

    #[test]
    fn rejects_bad_keys() {
        assert_eq!(
            "launcher".parse::<ChannelKey>(),
            Err(ParseChannelKeyError::Malformed("launcher".into()))
        );
        assert_eq!(
            "ftp/x".parse::<ChannelKey>(),
            Err(ParseChannelKeyError::UnknownKind("ftp".into()))
        );
        for bad in ["steam/", "steam/-x", "steam/x-", "steam/a/b", "steam/a b"] {
            assert!(
                matches!(
                    bad.parse::<ChannelKey>(),
                    Err(ParseChannelKeyError::InvalidName(_))
                ),
                "{bad}"
            );
        }
        let too_long = format!("launcher/{}", "a".repeat(CHANNEL_NAME_MAX_LEN + 1));
        assert!(too_long.parse::<ChannelKey>().is_err());
    }

    #[test]
    fn longest_name_fits_a_label() {
        let key = ChannelKey::launcher("a".repeat(CHANNEL_NAME_MAX_LEN));
        assert!(key.label_value().len() <= 63);
    }

    #[test]
    fn channel_ref_key_checks_fields() {
        let launcher = ChannelRef::from(&ChannelKey::launcher("latest"));
        assert_eq!(launcher.key().unwrap(), ChannelKey::launcher("latest"));

        let both = ChannelRef {
            kind: ChannelKind::Steam,
            metadata_path: Some("latest".into()),
            branch: Some("test".into()),
        };
        assert_eq!(
            both.key(),
            Err(ParseChannelKeyError::FieldsMismatch(ChannelKind::Steam))
        );
    }

    #[test]
    fn steam_takes_only_test_and_shipping() {
        assert!(!GameConfig::Development.allowed_on(ChannelKind::Steam));
        assert!(GameConfig::Test.allowed_on(ChannelKind::Steam));
        assert!(GameConfig::Shipping.allowed_on(ChannelKind::Steam));
        assert!(GameConfig::Development.allowed_on(ChannelKind::Launcher));
    }

    #[test]
    fn workflow_value_keeps_the_case() {
        for config in [
            GameConfig::Development,
            GameConfig::Test,
            GameConfig::Shipping,
        ] {
            let serialized = serde_json::to_value(config).unwrap();
            assert_eq!(serialized, config.workflow_value());
        }
        assert_eq!(GameConfig::Test.workflow_value(), "Test");
    }

    #[test]
    fn channel_ref_cel_rule() {
        let ok = serde_json::json!({"kind": "steam", "branch": "test"});
        assert!(ChannelRef::validate_cel(&ok, None).is_ok());
        let ok = serde_json::json!({"kind": "launcher", "metadataPath": "latest"});
        assert!(ChannelRef::validate_cel(&ok, None).is_ok());

        for bad in [
            serde_json::json!({"kind": "steam"}),
            serde_json::json!({"kind": "steam", "metadataPath": "latest"}),
            serde_json::json!({"kind": "launcher", "branch": "test"}),
            serde_json::json!({"kind": "launcher", "metadataPath": "latest", "branch": "test"}),
        ] {
            assert!(ChannelRef::validate_cel(&bad, None).is_err(), "{bad}");
        }
    }
}
