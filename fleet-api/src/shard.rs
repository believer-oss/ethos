//! `Shard`: what a shard currently serves, as observed by the coordinator.

use std::collections::BTreeMap;

use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, Time};
use kube::{CustomResource, KubeSchema};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::backend::BackendParts;
use crate::channel::{ChannelKey, ChannelKind, GameConfig, ParseChannelKeyError};
use crate::fleet_release::FleetReleasePhase;
use crate::naming::{MAP_ID_MAX_LEN, MAP_ID_PATTERN};
use crate::promotion::PromotionPhase;
use crate::shard_config::{MAX_CLUSTERS, SHARD_NAME_PATTERN};
use crate::sizing::{Sizing, ABSOLUTE_BUFFER_RULE};

/// Upper bound on a shard's fleet maps, and on per-map sizing entries.
pub(crate) const MAX_FLEET_MAPS: usize = 32;

/// Upper bound on (map, cluster) sizing overrides.
pub(crate) const MAX_OVERRIDES: usize = MAX_FLEET_MAPS * MAX_CLUSTERS;

// Both rules are on `spec`, not the object root, so CEL ratcheting keeps an older spec that breaks
// them writable through status and metadata: a root rule sees every write as a change.

/// Fleets need at least one map: there is no default-map fallback.
const FLEET_MAPS_RULE: &str =
    "!has(self.fleetsEnabled) || !self.fleetsEnabled || (has(self.maps) && size(self.maps) > 0)";

/// Fleets need a warm buffer: there is no default, so a shard with Fleets on says how many servers
/// each Fleet keeps warm.
const FLEET_WARM_RULE: &str = "!has(self.fleetsEnabled) || !self.fleetsEnabled || (has(self.warm) && has(self.warm.bufferSize))";

/// No two fleet maps may share a slug, since they would name the same Fleet. `MAP_ID_PATTERN`
/// allows only `-`, `_` and `.` besides alphanumerics, so this is `map_slug` in CEL.
const UNIQUE_SLUGS_RULE: &str = "self.all(a, self.exists_one(b, b.lowerAscii().replace('_', '-').replace('.', '-') == a.lowerAscii().replace('_', '-').replace('.', '-')))";

/// What a shard serves. The coordinator creates one per `ShardConfig`, with no
/// owner reference so that deleting the config keeps the observed history, and is the only writer of
/// its status. Friendshipper edits the spec (the shard's Fleet settings), setting the `changed-by`
/// annotation.
///
/// Partial failure never blanks good data: each observed item carries its own `observedAt` and
/// `error`, and a failed observation changes only `error`. Readers show an item as stale once
/// `observedAt` is older than three poll intervals (`STALE_AFTER_POLL_INTERVALS`).
// Each printcolumn repeats `type_`, which clippy mistakes for a duplicated attribute.
#[allow(clippy::duplicated_attributes)]
#[derive(CustomResource, KubeSchema, Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[kube(
    group = "fleet.believer.dev",
    version = "v1alpha1",
    kind = "Shard",
    namespaced,
    shortname = "shard",
    status = "ShardStatus",
    doc = "What a shard currently serves: backend parts, channel versions, recent promotions and Fleet releases. The spec holds the shard's Fleet settings; status is written only by the coordinator.",
    printcolumn(name = "Fleets", type_ = "boolean", json_path = ".spec.fleetsEnabled"),
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
#[cfg_attr(test, kube(cel))]
#[x_kube(validation = Rule::new(FLEET_MAPS_RULE).message("spec.maps must name at least one map when spec.fleetsEnabled is true"))]
#[x_kube(validation = Rule::new(FLEET_WARM_RULE).message("spec.warm.bufferSize must be set when spec.fleetsEnabled is true"))]
#[serde(rename_all = "camelCase")]
pub struct ShardSpec {
    /// The master switch. Off: no new releases, and releases no channel serves retire (unless
    /// pinned); releases serving a channel keep their Fleets until the channel moves on. Requires
    /// `fleets` in the `ShardConfig`.
    #[serde(default)]
    pub fleets_enabled: bool,

    /// The map IDs that get Fleets: keys of the build manifest's `maps.levelAssets`. At least one
    /// when `fleetsEnabled`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(length(max = MAX_FLEET_MAPS), inner(length(min = 1, max = MAP_ID_MAX_LEN), regex(pattern = MAP_ID_PATTERN)))]
    #[schemars(extend("x-kubernetes-list-type" = "set"))]
    #[x_kube(validation = Rule::new(UNIQUE_SLUGS_RULE).message("spec.maps must not hold two map IDs that differ only in case or in '-', '_' and '.', as they would name the same Fleet"))]
    pub maps: Vec<String>,

    /// The buffer each (map, cluster) Fleet keeps while its release is Warming or Warm, before any
    /// channel serves it: an absolute `bufferSize`, never parked. Required, with `bufferSize`, when
    /// `fleetsEnabled`: there is no default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[x_kube(validation = Rule::new(ABSOLUTE_BUFFER_RULE).message("warm.bufferSize must be absolute, not a percentage"))]
    #[x_kube(validation = Rule::new("!has(self.parked) || !self.parked").message("warm sizing can't be parked; turn fleetsEnabled off instead"))]
    pub warm: Option<Sizing>,

    /// Sizing while a channel serves the release.
    #[serde(default, skip_serializing_if = "LiveSizing::is_empty")]
    pub live: LiveSizing,
}

/// Live sizing, most specific first: `overrides` (map and cluster) > `maps` (map) > `default`.
/// Each level may be partial; unset fields come from the next less specific level.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, KubeSchema)]
#[serde(rename_all = "camelCase")]
pub struct LiveSizing {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Sizing>,

    /// Per map ID.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[schemars(extend("maxProperties" = MAX_FLEET_MAPS))]
    #[x_kube(validation = Rule::new(format!("self.all(m, size(m) <= {MAP_ID_MAX_LEN} && m.matches('{MAP_ID_PATTERN}'))")).message("live.maps keys must be map IDs"))]
    pub maps: BTreeMap<String, Sizing>,

    /// Per (map, cluster), unique by both. `sizing: {parked: true}` parks that one Fleet.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(length(max = MAX_OVERRIDES))]
    #[schemars(extend("x-kubernetes-list-type" = "map", "x-kubernetes-list-map-keys" = ["cluster", "mapId"]))]
    pub overrides: Vec<SizingOverride>,
}

impl LiveSizing {
    pub fn is_empty(&self) -> bool {
        self.default.is_none() && self.maps.is_empty() && self.overrides.is_empty()
    }

    /// The override for this (map, cluster), if any.
    pub fn override_for(&self, map_id: &str, cluster: &str) -> Option<&SizingOverride> {
        self.overrides
            .iter()
            .find(|o| o.map_id == map_id && o.cluster == cluster)
    }
}

/// Sizing for one (map, cluster) Fleet, over the less specific levels. Used by `Shard.spec.live`
/// and `FleetRelease.spec.overrides`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SizingOverride {
    /// A cluster in the `ShardConfig`'s `fleets.clusters`.
    #[schemars(length(min = 1, max = 63), regex(pattern = SHARD_NAME_PATTERN))]
    pub cluster: String,

    #[schemars(length(min = 1, max = MAP_ID_MAX_LEN), regex(pattern = MAP_ID_PATTERN))]
    pub map_id: String,

    pub sizing: Sizing,
}

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

    /// The shard's `FleetRelease`s, newest first.
    #[serde(default)]
    pub releases: Vec<ReleaseSummary>,

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

/// A `FleetRelease` as listed on its shard.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseSummary {
    /// The `FleetRelease`'s name.
    pub name: String,

    pub version: String,

    #[serde(default)]
    pub phase: Option<FleetReleasePhase>,

    /// Channel keys serving the version now.
    #[serde(default)]
    pub served_by: Vec<String>,

    /// Ready servers across every map and cluster.
    #[serde(default)]
    pub ready: i32,

    /// Allocated servers across every map and cluster.
    #[serde(default)]
    pub allocated: i32,
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

    #[test]
    fn fleets_need_a_map_and_a_warm_buffer() {
        let warm = Sizing {
            buffer_size: Some(crate::BufferSize::Absolute(2)),
            ..Sizing::default()
        };
        let shard = |fleets_enabled: bool, maps: &[&str], warm: Option<Sizing>| {
            Shard::new(
                "test",
                ShardSpec {
                    fleets_enabled,
                    maps: maps.iter().map(|m| m.to_string()).collect(),
                    warm,
                    ..ShardSpec::default()
                },
            )
        };
        assert!(shard(false, &[], None).validate_cel().is_ok());
        assert!(shard(true, &["Arena"], Some(warm.clone()))
            .validate_cel()
            .is_ok());
        assert!(shard(true, &[], Some(warm.clone())).validate_cel().is_err());
        assert!(shard(true, &["Arena"], None).validate_cel().is_err());
        // A warm sizing without bufferSize isn't a buffer.
        let no_buffer = Sizing {
            max_replicas: Some(4),
            ..Sizing::default()
        };
        assert!(shard(true, &["Arena"], Some(no_buffer))
            .validate_cel()
            .is_err());
    }

    #[test]
    fn fleet_maps_must_not_share_a_slug() {
        let shard = |maps: &[&str]| {
            Shard::new(
                "test",
                ShardSpec {
                    maps: maps.iter().map(|m| m.to_string()).collect(),
                    ..ShardSpec::default()
                },
            )
        };
        shard(&["Arena", "Arena_Tutorial", "QA_BOX_Rotator"])
            .validate_cel()
            .unwrap();
        for colliding in [
            ["QA_BOX_Rotator", "QA-BOX-Rotator"],
            ["Arena", "arena"],
            ["Arena.v2", "Arena_v2"],
        ] {
            assert!(shard(&colliding).validate_cel().is_err(), "{colliding:?}");
            assert!(crate::check_map_slugs(colliding).is_err(), "{colliding:?}");
        }
    }
}
