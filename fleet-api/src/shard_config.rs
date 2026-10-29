//! `ShardConfig`: the git-owned guardrails for one shard.

use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use kube::{CustomResource, KubeSchema};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::channel::{
    ChannelKey, ChannelKind, ChannelRef, GameConfig, ParseChannelKeyError, CHANNEL_FIELDS_MESSAGE,
    CHANNEL_FIELDS_RULE, CHANNEL_NAME_MAX_LEN, CHANNEL_NAME_PATTERN,
};

/// A DNS label: shard names appear in object names, labels and workflow parameters.
pub(crate) const SHARD_NAME_PATTERN: &str = "^[a-z0-9]([-a-z0-9]*[a-z0-9])?$";
pub(crate) const SHARD_NAME_MAX_LEN: usize = 63;

/// Upper bound on channels per shard. It also bounds the cost of the uniqueness rule below.
const MAX_CHANNELS: usize = 32;

const UNIQUE_CHANNELS_RULE: &str = "self.all(c, (c.kind == 'steam' ? !has(c.branch) : !has(c.metadataPath)) || self.exists_one(d, d.kind == c.kind && (c.kind == 'steam' ? has(d.branch) && d.branch == c.branch : has(d.metadataPath) && d.metadataPath == c.metadataPath)))";

/// The guardrails for one shard. Created and edited only by Flux from git.
///
/// `metadata.name` must equal `spec.shardName`. The coordinator creates a `Shard` of
/// the same name for each one, and reports problems it can't express in CEL (a Steam branch claimed
/// by two shards) as a `ConfigValid` condition.
// Each printcolumn repeats `type_`, which clippy mistakes for a duplicated attribute.
#[allow(clippy::duplicated_attributes)]
#[derive(CustomResource, KubeSchema, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[kube(
    group = "fleet.believer.dev",
    version = "v1alpha1",
    kind = "ShardConfig",
    namespaced,
    shortname = "shardcfg",
    status = "ShardConfigStatus",
    doc = "The git-owned guardrails for one shard: its backend healthcheck, channels and promotion policy.",
    validation = Rule::new("self.metadata.name == self.spec.shardName").message("metadata.name must equal spec.shardName"),
    printcolumn(name = "Display", type_ = "string", json_path = ".spec.displayName"),
    printcolumn(name = "Deploy backend", type_ = "boolean", json_path = ".spec.promotion.allowDeployBackend"),
    printcolumn(name = "Age", type_ = "date", json_path = ".metadata.creationTimestamp")
)]
#[cfg_attr(test, kube(cel))]
#[serde(rename_all = "camelCase")]
pub struct ShardConfigSpec {
    /// The backend shard name. Also the promote workflow's `shard` parameter.
    #[schemars(length(min = 1, max = SHARD_NAME_MAX_LEN), regex(pattern = SHARD_NAME_PATTERN))]
    pub shard_name: String,

    /// A human-friendly name for UIs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(max = 64))]
    pub display_name: Option<String>,

    /// The backend healthcheck the coordinator polls for the shard's backend version.
    #[schemars(length(max = 2048), regex(pattern = "^https?://"))]
    pub healthcheck_url: String,

    /// The channels this shard serves clients from. Keys must be unique within the shard.
    #[schemars(length(min = 1, max = MAX_CHANNELS))]
    #[x_kube(validation = Rule::new(UNIQUE_CHANNELS_RULE).message("channels must be unique by kind and metadataPath or branch"))]
    pub channels: Vec<ShardChannel>,

    #[serde(default)]
    pub promotion: PromotionPolicy,
}

impl ShardConfigSpec {
    /// The channel with this key, if the shard has it.
    pub fn channel(&self, key: &ChannelKey) -> Option<&ShardChannel> {
        self.channels.iter().find(|c| c.key().as_ref() == Ok(key))
    }
}

/// One channel of a shard, with the defaults promotions to it use.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, KubeSchema)]
#[serde(rename_all = "camelCase")]
#[x_kube(validation = Rule::new(CHANNEL_FIELDS_RULE).message(CHANNEL_FIELDS_MESSAGE))]
#[x_kube(validation = Rule::new("self.kind != 'steam' || self.gameConfig in ['Test', 'Shipping']").message("a steam channel's gameConfig must be Test or Shipping"))]
#[cfg_attr(test, x_kube(cel))]
pub struct ShardChannel {
    pub kind: ChannelKind,

    /// Launcher only: the key of the metadata pointer the launcher reads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = CHANNEL_NAME_MAX_LEN), regex(pattern = CHANNEL_NAME_PATTERN))]
    pub metadata_path: Option<String>,

    /// Steam only: the branch name. A branch may belong to at most one shard.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(min = 1, max = CHANNEL_NAME_MAX_LEN), regex(pattern = CHANNEL_NAME_PATTERN))]
    pub branch: Option<String>,

    /// The game config promotions to this channel use unless they name one.
    pub game_config: GameConfig,
}

impl ShardChannel {
    pub fn key(&self) -> Result<ChannelKey, ParseChannelKeyError> {
        ChannelKey::from_parts(
            self.kind,
            self.metadata_path.as_deref(),
            self.branch.as_deref(),
        )
    }

    pub fn channel_ref(&self) -> ChannelRef {
        ChannelRef {
            kind: self.kind,
            metadata_path: self.metadata_path.clone(),
            branch: self.branch.clone(),
        }
    }
}

/// What promotions to this shard may do.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PromotionPolicy {
    /// Whether a promotion may also deploy the backend. Off unless set.
    #[serde(default)]
    pub allow_deploy_backend: bool,
}

/// Written by the coordinator; conditions only.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ShardConfigStatus {
    #[serde(default)]
    #[schemars(extend("x-kubernetes-list-type" = "map", "x-kubernetes-list-map-keys" = ["type"]))]
    pub conditions: Vec<Condition>,
}

#[cfg(test)]
mod tests {
    use kube::core::ObjectMeta;
    use serde_json::json;

    use super::*;

    pub(crate) fn sample_spec() -> ShardConfigSpec {
        serde_json::from_value(json!({
            "shardName": "test",
            "displayName": "Test",
            "healthcheckUrl": "https://backend.example.com/v1/healthcheck",
            "channels": [
                {"kind": "launcher", "metadataPath": "stable-1.0", "gameConfig": "Development"},
                {"kind": "steam", "branch": "test", "gameConfig": "Test"},
            ],
            "promotion": {"allowDeployBackend": true},
        }))
        .unwrap()
    }

    fn shard_config(name: &str, spec: ShardConfigSpec) -> ShardConfig {
        ShardConfig {
            metadata: ObjectMeta {
                name: Some(name.into()),
                ..Default::default()
            },
            spec,
            status: None,
        }
    }

    #[test]
    fn finds_channels_by_key() {
        let spec = sample_spec();
        let steam = spec.channel(&ChannelKey::steam("test")).unwrap();
        assert_eq!(steam.game_config, GameConfig::Test);
        assert!(spec.channel(&ChannelKey::steam("other")).is_none());
        assert!(spec.channel(&ChannelKey::launcher("stable-1.0")).is_some());
    }

    #[test]
    fn promotion_policy_defaults_to_no_backend_deploys() {
        let spec: ShardConfigSpec = serde_json::from_value(json!({
            "shardName": "test",
            "healthcheckUrl": "https://backend.example.com/v1/healthcheck",
            "channels": [{"kind": "steam", "branch": "test", "gameConfig": "Test"}],
        }))
        .unwrap();
        assert!(!spec.promotion.allow_deploy_backend);
    }

    #[test]
    fn valid_sample_passes_cel() {
        shard_config("test", sample_spec()).validate_cel().unwrap();
    }

    #[test]
    fn name_must_match_shard_name() {
        assert!(shard_config("other", sample_spec()).validate_cel().is_err());
    }

    #[test]
    fn duplicate_channels_are_rejected() {
        // A second Steam channel on the same branch.
        let mut spec = sample_spec();
        spec.channels.push(spec.channels[1].clone());
        assert!(shard_config("test", spec).validate_cel().is_err());

        // A second launcher channel on the same metadata path, even with another game config.
        let mut spec = sample_spec();
        let mut launcher = spec.channels[0].clone();
        launcher.game_config = GameConfig::Shipping;
        spec.channels.push(launcher);
        assert!(shard_config("test", spec).validate_cel().is_err());

        // Same name, different kind: not a duplicate.
        let mut spec = sample_spec();
        spec.channels.push(ShardChannel {
            kind: ChannelKind::Steam,
            metadata_path: None,
            branch: Some("stable-1.0".into()),
            game_config: GameConfig::Test,
        });
        shard_config("test", spec).validate_cel().unwrap();
    }

    #[test]
    fn uniqueness_skips_channels_missing_their_field() {
        // A Steam channel without a branch fails the per-channel fields rule; the uniqueness rule
        // must not add a second, misleading error for it.
        let mut spec = sample_spec();
        spec.channels.push(ShardChannel {
            kind: ChannelKind::Steam,
            metadata_path: Some("latest".into()),
            branch: None,
            game_config: GameConfig::Test,
        });
        let errors = shard_config("test", spec).validate_cel().unwrap_err();
        assert!(
            errors
                .as_slice()
                .iter()
                .all(|e| e.rule != UNIQUE_CHANNELS_RULE),
            "{errors}"
        );
    }

    #[test]
    fn steam_channels_need_test_or_shipping() {
        let bad = json!({"kind": "steam", "branch": "test", "gameConfig": "Development"});
        assert!(ShardChannel::validate_cel(&bad, None).is_err());
        let ok = json!({"kind": "launcher", "metadataPath": "latest", "gameConfig": "Development"});
        ShardChannel::validate_cel(&ok, None).unwrap();
    }

    #[test]
    fn channel_fields_must_match_kind() {
        let bad = json!({"kind": "launcher", "branch": "test", "gameConfig": "Test"});
        assert!(ShardChannel::validate_cel(&bad, None).is_err());
    }
}
