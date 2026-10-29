//! `Shard`: what a shard currently serves, as observed by the coordinator.

use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, Time};
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::backend::BackendParts;
use crate::channel::{ChannelKey, ChannelKind, GameConfig, ParseChannelKeyError};
use crate::promotion::PromotionPhase;

/// What a shard serves. The coordinator creates one per `ShardConfig`, with no
/// owner reference so that deleting the config keeps the observed history, and is the only writer of
/// its status.
///
/// Partial failure never blanks good data: each observed item carries its own `observedAt` and
/// `error`, and a failed observation changes only `error`. Readers show an item as stale once
/// `observedAt` is older than three poll intervals (`STALE_AFTER_POLL_INTERVALS`).
// Each printcolumn repeats `type_`, which clippy mistakes for a duplicated attribute.
#[allow(clippy::duplicated_attributes)]
#[derive(CustomResource, Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[kube(
    group = "fleet.believer.dev",
    version = "v1alpha1",
    kind = "Shard",
    namespaced,
    shortname = "shard",
    status = "ShardStatus",
    doc = "What a shard currently serves: backend parts, channel versions and recent promotions. Status is written only by the coordinator.",
    printcolumn(
        name = "Healthy",
        type_ = "boolean",
        json_path = ".status.backend.healthy"
    ),
    printcolumn(
        name = "Platform",
        type_ = "string",
        json_path = ".status.backend.parts.platform"
    ),
    printcolumn(
        name = "Content",
        type_ = "string",
        json_path = ".status.backend.parts.content"
    ),
    printcolumn(
        name = "Observed",
        type_ = "date",
        json_path = ".status.backend.observedAt"
    ),
    printcolumn(
        name = "Age",
        type_ = "date",
        json_path = ".metadata.creationTimestamp"
    )
)]
pub struct ShardSpec {}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ShardStatus {
    #[serde(default)]
    pub observed_generation: Option<i64>,

    #[serde(default)]
    pub backend: Option<BackendStatus>,

    /// One entry per channel of the `ShardConfig`.
    #[serde(default)]
    #[schemars(extend("x-kubernetes-list-type" = "map", "x-kubernetes-list-map-keys" = ["key"]))]
    pub channels: Vec<ChannelStatus>,

    /// Running promotions first, then the most recent finished ones per channel, as many as the
    /// coordinator keeps (its `--promotion-keep`, by default [`RETAINED_FINISHED_PROMOTIONS`](crate::RETAINED_FINISHED_PROMOTIONS)).
    #[serde(default)]
    pub promotions: Vec<PromotionSummary>,

    #[serde(default)]
    #[schemars(extend("x-kubernetes-list-type" = "map", "x-kubernetes-list-map-keys" = ["type"]))]
    pub conditions: Vec<Condition>,
}

impl ShardStatus {
    /// The status of the channel with this key, if observed.
    pub fn channel(&self, key: &ChannelKey) -> Option<&ChannelStatus> {
        self.channels
            .iter()
            .find(|c| c.channel_key().as_ref() == Ok(key))
    }
}

/// The shard's backend, from its healthcheck.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BackendStatus {
    #[serde(default)]
    pub healthy: bool,

    #[serde(default)]
    pub parts: BackendParts,

    /// The healthcheck's `launchTimeUnixMillis`: when the backend process last started. A deploy
    /// restarts it, but so do a crash, an OOM kill or a reschedule, so this is not a deploy time.
    #[serde(default)]
    pub launched_at: Option<Time>,

    /// When the healthcheck was last read successfully. A failed observation leaves it, and the
    /// other fields, as they were and sets `error`, so its age is how stale they are.
    #[serde(default)]
    pub observed_at: Option<Time>,

    /// The last observation's error; empty when it succeeded.
    #[serde(default)]
    pub error: String,
}

/// Where a channel's `version` came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ChannelSource {
    /// Read from the channel itself (the launcher pointer or the Steam Web API).
    Observed,
    /// The fallback: the last `Promotion` through the coordinator set it. Seen between a promotion
    /// succeeding and the next observation, or while Steam can't be observed.
    Promotion,
}

/// What one channel serves.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChannelStatus {
    /// The channel key, `launcher/<metadataPath>` or `steam/<branch>`.
    pub key: String,

    pub kind: ChannelKind,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata_path: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,

    /// The full 40-character commit SHA the channel serves, or empty if unknown.
    #[serde(default)]
    pub version: String,

    /// When `version` arrived on the channel. Changes only when `version` does, and means
    /// different things by kind:
    /// - launcher: the pointer object's S3 `LastModified` at that change, i.e. when the pointer
    ///   was written, on the first observation too;
    /// - Steam: when the coordinator first saw the build on the branch. It lags the real change by
    ///   up to a poll interval, and on the first observation (e.g. after the coordinator starts)
    ///   it is that observation's time, since no Steam API reports when a build went live.
    ///
    /// With `source: promotion`, it is when the coordinator recorded the Promotion's success, or
    /// that Promotion's `finishedAt` when the channel had no version to fall back from.
    #[serde(default)]
    pub since: Option<Time>,

    #[serde(default)]
    pub source: Option<ChannelSource>,

    /// When the channel was last observed successfully; a failed observation leaves it as it was.
    /// For Steam, a branch whose build was found counts as observed even if its description
    /// couldn't be parsed (`error` then says so).
    #[serde(default)]
    pub observed_at: Option<Time>,

    /// The last observation's error; empty when it succeeded. `version` keeps its previous value.
    #[serde(default)]
    pub error: String,

    /// The `Promotion` that last changed this channel, if any.
    #[serde(default)]
    pub promotion: Option<String>,

    /// Steam only: the build ID on the branch. With `version`, a cache: the build description is
    /// looked up only when this changes.
    #[serde(default)]
    pub build_id: Option<i64>,

    /// Steam only: parsed from the build description.
    #[serde(default)]
    pub game_config: Option<GameConfig>,

    /// Steam only: when the build was uploaded to Steam (`GetAppBuilds` `CreationTime`), not when it
    /// was set live on the branch.
    #[serde(default)]
    pub build_created_at: Option<Time>,
}

impl ChannelStatus {
    /// An entry for `key` with nothing observed yet.
    pub fn new(key: &ChannelKey) -> Self {
        let (metadata_path, branch) = match key {
            ChannelKey::Launcher { metadata_path } => (Some(metadata_path.clone()), None),
            ChannelKey::Steam { branch } => (None, Some(branch.clone())),
        };
        ChannelStatus {
            key: key.to_string(),
            kind: key.kind(),
            metadata_path,
            branch,
            version: String::new(),
            since: None,
            source: None,
            observed_at: None,
            error: String::new(),
            promotion: None,
            build_id: None,
            game_config: None,
            build_created_at: None,
        }
    }

    pub fn channel_key(&self) -> Result<ChannelKey, ParseChannelKeyError> {
        self.key.parse()
    }
}

/// A promotion as listed on its shard.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PromotionSummary {
    /// The `Promotion`'s name.
    pub name: String,

    /// The channel key.
    pub channel: String,

    pub version: String,

    #[serde(default)]
    pub phase: Option<PromotionPhase>,

    /// The Promotion's `status.startedAt`: when the coordinator accepted it.
    #[serde(default)]
    pub started_at: Option<Time>,

    /// The Promotion's `status.finishedAt`.
    #[serde(default)]
    pub finished_at: Option<Time>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn deserializes_a_full_example() {
        let status: ShardStatus = serde_json::from_value(json!({
            "observedGeneration": 1,
            "backend": {
                "healthy": true,
                "parts": {
                    "platform": "2026-01-02-03-04-05_1.2.3_main_aaaaaaa",
                    "content": "2026-01-02-03-04-05_1.2.3_main_aaaaaaa",
                },
                "launchedAt": "2026-01-02T03:04:05Z",
                "observedAt": "2026-01-02T03:05:00Z",
                "error": "",
            },
            "channels": [
                {
                    "key": "launcher/stable-1.0",
                    "kind": "launcher",
                    "metadataPath": "stable-1.0",
                    "version": "0123456789abcdef0123456789abcdef01234567",
                    "since": "2026-01-01T00:00:00Z",
                    "source": "observed",
                    "observedAt": "2026-01-02T03:05:00Z",
                    "error": "",
                },
                {
                    "key": "steam/test",
                    "kind": "steam",
                    "branch": "test",
                    "version": "",
                    "buildId": 1234,
                    "gameConfig": "Test",
                    "source": "promotion",
                    "promotion": "test-steam-abcde",
                },
            ],
            "promotions": [
                {"name": "test-steam-abcde", "channel": "steam/test",
                 "version": "0123456789abcdef0123456789abcdef01234567", "phase": "Running",
                 "startedAt": "2026-01-02T03:00:00Z"},
            ],
            "conditions": [
                {"type": "BackendObserved", "status": "True", "reason": "Ok", "message": "",
                 "lastTransitionTime": "2026-01-02T03:05:00Z"},
            ],
        }))
        .unwrap();

        let backend = status.backend.as_ref().unwrap();
        assert!(backend.healthy);

        let steam = status.channel(&ChannelKey::steam("test")).unwrap();
        assert_eq!(steam.build_id, Some(1234));
        assert_eq!(steam.source, Some(ChannelSource::Promotion));
        assert!(status.channel(&ChannelKey::steam("other")).is_none());
        assert_eq!(status.promotions[0].phase, Some(PromotionPhase::Running));
    }

    #[test]
    fn new_channel_status_matches_its_key() {
        let key = ChannelKey::launcher("stable-1.0");
        let status = ChannelStatus::new(&key);
        assert_eq!(status.key, "launcher/stable-1.0");
        assert_eq!(status.metadata_path.as_deref(), Some("stable-1.0"));
        assert_eq!(status.channel_key().unwrap(), key);
    }
}
