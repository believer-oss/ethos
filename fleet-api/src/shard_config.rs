//! `ShardConfig`: the git-owned guardrails for one shard.

use std::time::Duration;

use k8s_openapi::apimachinery::pkg::apis::meta::v1::Condition;
use kube::{CustomResource, KubeSchema};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::channel::{
    ChannelKey, ChannelKind, ChannelRef, GameConfig, ParseChannelKeyError, CHANNEL_FIELDS_MESSAGE,
    CHANNEL_FIELDS_RULE, CHANNEL_NAME_MAX_LEN, CHANNEL_NAME_PATTERN,
};
use crate::naming::FLEET_SHARD_NAME_MAX_LEN;

/// A DNS label: shard names appear in object names, labels and workflow parameters.
pub(crate) const SHARD_NAME_PATTERN: &str = "^[a-z0-9]([-a-z0-9]*[a-z0-9])?$";
pub(crate) const SHARD_NAME_MAX_LEN: usize = 63;

/// Upper bound on channels per shard. It also bounds the cost of the uniqueness rule below.
const MAX_CHANNELS: usize = 32;

/// Upper bound on clusters a shard's Fleets may run in.
pub(crate) const MAX_CLUSTERS: usize = 64;

/// A positive Go-style duration such as `30m` or `1h30m`, as `metav1.Duration` writes them.
pub(crate) const DURATION_PATTERN: &str = r"^([0-9]+(\.[0-9]+)?(ns|us|µs|ms|s|m|h))+$";

/// A UUID, as the backend writes shard IDs.
const UUID_PATTERN: &str =
    "^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$";

/// Defaults the coordinator applies when a shard's `fleets` (or, for [`LIVE`](fleet_defaults::LIVE),
/// its `Shard.spec.live`) leaves a field unset.
pub mod fleet_defaults {
    use std::time::Duration;

    use crate::sizing::{BufferSize, Sizing};

    /// `gate.minReady`: Ready servers each non-parked map needs in every cluster.
    pub const GATE_MIN_READY: i32 = 1;
    /// `gate.stableFor`: how long the release must stay healthy with `minReady` met.
    pub const GATE_STABLE_FOR: Duration = Duration::from_secs(2 * 60);
    /// `gate.timeout`: how long a promotion waits for the gate before failing.
    pub const GATE_TIMEOUT: Duration = Duration::from_secs(20 * 60);
    /// `retireAfterIdle`: how long a Draining release may have no Allocated servers before it retires.
    pub const RETIRE_AFTER_IDLE: Duration = Duration::from_secs(30 * 60);
    /// `warmTtl`: how long a release no channel has served may stay Warming or Warm.
    pub const WARM_TTL: Duration = Duration::from_secs(24 * 60 * 60);
    /// Live sizing's last level, under `Shard.spec.live.default`: it fills whatever the shard's live
    /// levels leave unset. The coordinator caps `maxReplicas` at `limits.maxReplicasPerFleet` and fills
    /// `minReplicas` as 0 when the resolved `bufferSize` is absolute (Agones wants it 0 or at least the
    /// buffer).
    pub const LIVE: Sizing = Sizing {
        buffer_size: Some(BufferSize::Percent(20)),
        min_replicas: Some(1),
        max_replicas: Some(10),
        parked: None,
    };
}

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
// On `spec`, not the root, so CEL ratcheting keeps an older config writable through status.
#[x_kube(validation = Rule::new(format!("!has(self.fleets) || size(self.shardName) <= {FLEET_SHARD_NAME_MAX_LEN}")).message(format!("a shard with fleets must have a name of at most {FLEET_SHARD_NAME_MAX_LEN} characters").as_str()))]
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

    /// The backend's ID for the shard: the `believer.dev/shard-id` label on its game servers.
    /// Needed only once the shard has Fleets; until then a missing value is a `ConfigValid=False`
    /// condition rather than a schema error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(regex(pattern = UUID_PATTERN))]
    pub shard_id: Option<String>,

    /// The address game servers reach the backend's partner API at (`-PragmaPartnerBackendAddress`).
    /// Needed only once the shard has Fleets, like `shardId`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(length(max = 2048), regex(pattern = "^https?://"))]
    pub backend_address: Option<String>,

    /// Where and within what bounds the shard may run Fleets. Absent: the shard never has Fleets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fleets: Option<FleetsConfig>,
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

/// A shard with Fleets has a replica limit: every sizing is checked against it.
const FLEET_LIMITS_RULE: &str = "has(self.limits) && has(self.limits.maxReplicasPerFleet)";

/// The git-owned side of a shard's Fleets. `Shard.spec` (edited in Friendshipper) decides whether
/// they are on and how they are sized, within these bounds.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, KubeSchema)]
#[serde(rename_all = "camelCase")]
#[x_kube(validation = Rule::new(FLEET_LIMITS_RULE).message("fleets.limits.maxReplicasPerFleet must be set"))]
pub struct FleetsConfig {
    /// The clusters this shard may run Fleets in, unique by name.
    #[schemars(length(min = 1, max = MAX_CLUSTERS))]
    #[schemars(extend("x-kubernetes-list-type" = "map", "x-kubernetes-list-map-keys" = ["name"]))]
    pub clusters: Vec<FleetCluster>,

    /// The game server image repository; a release's image is `<imageRepository>:<version>`, e.g.
    /// `123456789012.dkr.ecr.us-west-2.amazonaws.com/game`.
    #[schemars(length(min = 1, max = 512))]
    pub image_repository: String,

    /// Bounds on `Shard.spec` sizing. `maxReplicasPerFleet` is required (CEL).
    #[serde(default)]
    pub limits: FleetLimits,

    /// The warm gate. Unset fields take the defaults in [`fleet_defaults`].
    #[serde(default)]
    pub gate: FleetGate,

    /// How long a Draining release may have no Allocated servers before it retires. Default 30m.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(regex(pattern = DURATION_PATTERN))]
    pub retire_after_idle: Option<kube::core::Duration>,

    /// How long a release no channel has served may stay Warming or Warm before it retires.
    /// Default 24h.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(regex(pattern = DURATION_PATTERN))]
    pub warm_ttl: Option<kube::core::Duration>,
}

impl FleetsConfig {
    /// The cluster with this name, if the shard may run Fleets there.
    pub fn cluster(&self, name: &str) -> Option<&FleetCluster> {
        self.clusters.iter().find(|c| c.name == name)
    }

    /// `retireAfterIdle`, or its default.
    pub fn retire_after_idle(&self) -> Duration {
        self.retire_after_idle
            .map_or(fleet_defaults::RETIRE_AFTER_IDLE, Duration::from)
    }

    /// `warmTtl`, or its default.
    pub fn warm_ttl(&self) -> Duration {
        self.warm_ttl
            .map_or(fleet_defaults::WARM_TTL, Duration::from)
    }
}

/// A cluster a shard may run Fleets in.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FleetCluster {
    /// The cluster's name as the coordinator knows it (its `--cluster` flag).
    #[schemars(length(min = 1, max = 63), regex(pattern = SHARD_NAME_PATTERN))]
    pub name: String,

    /// The zone the backend routes this cluster's allocations by: the `believer.dev/zone` label on
    /// its game servers, and the key of the backend's cluster mapping.
    #[schemars(length(min = 1, max = 63), regex(pattern = CHANNEL_NAME_PATTERN))]
    pub zone: String,
}

/// Bounds on `Shard.spec` sizing.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FleetLimits {
    /// The most `maxReplicas` any (map, cluster) Fleet may have, and the `maxReplicas` of a warm
    /// sizing that sets none. Required when the shard has `fleets` (CEL). CEL can't
    /// compare it with the Shard, so the coordinator refuses sizing above it (and reports it), and
    /// Friendshipper bounds its inputs by it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1))]
    pub max_replicas_per_fleet: Option<i32>,
}

/// A gate whose timeout is shorter than `stableFor` can never pass. Unset fields take their defaults.
fn gate_timeout_rule() -> String {
    format!(
        "duration(has(self.timeout) ? self.timeout : '{}s') >= duration(has(self.stableFor) ? self.stableFor : '{}s')",
        fleet_defaults::GATE_TIMEOUT.as_secs(),
        fleet_defaults::GATE_STABLE_FOR.as_secs()
    )
}

/// The warm gate: what a release's Fleets must show before a promotion publishes the client.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, KubeSchema)]
#[serde(rename_all = "camelCase")]
#[x_kube(validation = Rule::new(gate_timeout_rule()).message("fleets.gate.timeout must be at least fleets.gate.stableFor, counting unset fields as their defaults"))]
pub struct FleetGate {
    /// Ready servers each non-parked map needs in every cluster. Default 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 1))]
    pub min_ready: Option<i32>,

    /// How long `minReady` must hold with no server Unhealthy and no game-server restart. Default 2m.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(regex(pattern = DURATION_PATTERN))]
    pub stable_for: Option<kube::core::Duration>,

    /// How long a promotion waits for the gate before failing with `FleetGateTimeout`. Default 20m.
    /// Only the coordinator applies it: clusters never fail the gate for slowness.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(regex(pattern = DURATION_PATTERN))]
    pub timeout: Option<kube::core::Duration>,
}

impl FleetGate {
    /// `minReady`, or its default.
    pub fn min_ready(&self) -> i32 {
        self.min_ready.unwrap_or(fleet_defaults::GATE_MIN_READY)
    }

    /// `stableFor`, or its default.
    pub fn stable_for(&self) -> Duration {
        self.stable_for
            .map_or(fleet_defaults::GATE_STABLE_FOR, Duration::from)
    }

    /// `timeout`, or its default.
    pub fn timeout(&self) -> Duration {
        self.timeout
            .map_or(fleet_defaults::GATE_TIMEOUT, Duration::from)
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
    fn shards_with_fleets_have_short_names() {
        let named = |name: &str, fleets: bool| {
            let mut spec = sample_spec();
            spec.shard_name = name.into();
            spec.fleets = fleets.then(|| {
                serde_json::from_value(json!({
                    "clusters": [{"name": "games", "zone": "games"}],
                    "imageRepository": "registry.example.com/game",
                    "limits": {"maxReplicasPerFleet": 50},
                }))
                .unwrap()
            });
            shard_config(name, spec)
        };
        let longest = "s".repeat(FLEET_SHARD_NAME_MAX_LEN);
        named(&longest, true).validate_cel().unwrap();
        let too_long = "s".repeat(FLEET_SHARD_NAME_MAX_LEN + 1);
        assert!(named(&too_long, true).validate_cel().is_err());
        named(&too_long, false).validate_cel().unwrap();
    }

    #[test]
    fn fleets_need_a_replica_limit() {
        let with_fleets = |limits: serde_json::Value| {
            let mut spec = sample_spec();
            spec.fleets = Some(
                serde_json::from_value(json!({
                    "clusters": [{"name": "games", "zone": "games"}],
                    "imageRepository": "registry.example.com/game",
                    "limits": limits,
                }))
                .unwrap(),
            );
            shard_config("test", spec)
        };
        with_fleets(json!({"maxReplicasPerFleet": 50}))
            .validate_cel()
            .unwrap();
        let errors = with_fleets(json!({})).validate_cel().unwrap_err();
        assert!(
            errors
                .as_slice()
                .iter()
                .any(|e| e.rule == FLEET_LIMITS_RULE),
            "{errors}"
        );
        // A shard without Fleets needs no limit.
        shard_config("test", sample_spec()).validate_cel().unwrap();
    }

    #[test]
    fn the_live_default_is_a_valid_sizing() {
        use crate::sizing::{BufferSize, Sizing};

        let live = fleet_defaults::LIVE;
        assert_eq!(live.buffer_size, Some(BufferSize::Percent(20)));
        assert_eq!((live.min_replicas, live.max_replicas), (Some(1), Some(10)));
        live.validate(None).unwrap();
        live.validate(Some(10)).unwrap();
        Sizing::validate_cel(&serde_json::to_value(&live).unwrap(), None).unwrap();
        // A partial level keeps what it sets and takes the rest from the default.
        let partial = Sizing {
            max_replicas: Some(4),
            ..Default::default()
        };
        assert_eq!(
            partial.filled_from(&live),
            Sizing {
                max_replicas: Some(4),
                ..live
            }
        );
    }

    #[test]
    fn gate_timeouts_cover_stable_for() {
        let with_gate = |gate: serde_json::Value| {
            let mut spec = sample_spec();
            spec.fleets = Some(
                serde_json::from_value(json!({
                    "clusters": [{"name": "games", "zone": "games"}],
                    "imageRepository": "registry.example.com/game",
                    "limits": {"maxReplicasPerFleet": 50},
                    "gate": gate,
                }))
                .unwrap(),
            );
            shard_config("test", spec)
        };
        for ok in [
            json!({}),
            json!({"timeout": "2m", "stableFor": "2m"}),
            json!({"timeout": "1h30m", "stableFor": "90s"}),
            // The default stableFor (2m) fits a 2m timeout; the default timeout (20m) fits 20m.
            json!({"timeout": "2m"}),
            json!({"stableFor": "20m"}),
        ] {
            with_gate(ok.clone())
                .validate_cel()
                .unwrap_or_else(|e| panic!("{ok}: {e}"));
        }
        for bad in [
            json!({"timeout": "1m", "stableFor": "2m"}),
            json!({"timeout": "1m"}),
            json!({"stableFor": "21m"}),
        ] {
            let errors = with_gate(bad.clone()).validate_cel().unwrap_err();
            assert!(
                errors
                    .as_slice()
                    .iter()
                    .any(|e| e.rule == gate_timeout_rule()),
                "{bad}: {errors}"
            );
        }
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
