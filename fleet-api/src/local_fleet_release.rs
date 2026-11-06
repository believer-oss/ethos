//! `LocalFleetRelease`: a `FleetRelease` resolved for one game cluster.

use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, Time};
use kube::core::ObjectMeta;
use kube::{CustomResource, KubeSchema};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::channel::CHANNEL_NAME_PATTERN;
use crate::fleet_release::{release_labels, GatePhase};
use crate::naming::{
    release_name, NameError, FLEET_SHARD_NAME_MAX_LEN, MAP_ID_MAX_LEN, MAP_ID_PATTERN,
};
use crate::shard::MAX_FLEET_MAPS;
use crate::shard_config::{DURATION_PATTERN, SHARD_NAME_PATTERN};
use crate::sizing::Sizing;
use crate::RETIRE_FINALIZER;

/// The coordinator's resolved copy of a `FleetRelease` for one game cluster: everything cluster mode
/// needs to render the Fleets, with no access to the `ShardConfig`, the `Shard` or the backend.
///
/// Named like its `FleetRelease`. The coordinator writes the spec; cluster mode writes the status and
/// renders one `Fleet` and `FleetAutoscaler` per map. Deleting it retires the Fleets: the
/// [`RETIRE_FINALIZER`] holds it until every Fleet is gone, which waits for Allocated servers to
/// finish unless `force` is set.
// Each printcolumn repeats `type_`, which clippy mistakes for a duplicated attribute.
#[allow(clippy::duplicated_attributes)]
#[derive(CustomResource, KubeSchema, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[kube(
    group = "fleet.believer.dev",
    version = "v1alpha1",
    kind = "LocalFleetRelease",
    namespaced,
    shortname = "lfr",
    status = "LocalFleetReleaseStatus",
    doc = "A FleetRelease resolved for one game cluster: cluster mode renders its Fleets. Deleting it retires them.",
    validation = Rule::new("self.metadata.name == self.spec.shardName + '-' + self.spec.version.substring(0, 8)").message("metadata.name must be <spec.shardName>-<the first 8 characters of spec.version>"),
    validation = Rule::new("self.spec.image.endsWith(':' + self.spec.version)").message("spec.image must be tagged with spec.version"),
    printcolumn(name = "Shard", type_ = "string", json_path = ".spec.shardName"),
    printcolumn(name = "Version", type_ = "string", json_path = ".spec.version"),
    printcolumn(name = "Gate", type_ = "string", json_path = ".status.gate"),
    printcolumn(
        name = "Age",
        type_ = "date",
        json_path = ".metadata.creationTimestamp"
    )
)]
#[cfg_attr(test, kube(cel))]
#[serde(rename_all = "camelCase")]
pub struct LocalFleetReleaseSpec {
    /// The backend shard name: the `believer.dev/shard-name` label.
    #[schemars(length(min = 1, max = FLEET_SHARD_NAME_MAX_LEN), regex(pattern = SHARD_NAME_PATTERN))]
    pub shard_name: String,

    /// The backend's shard ID: the `believer.dev/shard-id` label.
    #[schemars(length(min = 1, max = 63), regex(pattern = CHANNEL_NAME_PATTERN))]
    pub shard_id: String,

    /// The build: a full 40-character lowercase commit SHA, and the `believer.dev/version` label.
    #[schemars(regex(pattern = "^[0-9a-f]{40}$"))]
    pub version: String,

    /// The game server image, `<repository>:<version>`.
    #[schemars(length(min = 1, max = 512))]
    pub image: String,

    /// The backend's partner address (`-PragmaPartnerBackendAddress`).
    #[schemars(length(max = 2048), regex(pattern = "^https?://"))]
    pub backend_address: String,

    /// This cluster's zone: the `believer.dev/zone` label.
    #[schemars(length(min = 1, max = 63), regex(pattern = CHANNEL_NAME_PATTERN))]
    pub zone: String,

    /// The gate cluster mode evaluates. The timeout is the coordinator's.
    pub gate: LocalGate,

    /// Let retiring delete Fleets that still have Allocated servers.
    #[serde(default)]
    pub force: bool,

    /// One Fleet per map, unique by map ID.
    #[serde(default)]
    #[schemars(length(max = MAX_FLEET_MAPS))]
    #[schemars(extend("x-kubernetes-list-type" = "map", "x-kubernetes-list-map-keys" = ["mapId"]))]
    pub maps: Vec<LocalFleetMap>,
}

impl LocalFleetRelease {
    /// A new object ready to create: name, labels and [`RETIRE_FINALIZER`] set from the spec. The
    /// namespace is left to the `Api` it is created through.
    pub fn request(spec: LocalFleetReleaseSpec) -> Result<Self, NameError> {
        Ok(LocalFleetRelease {
            metadata: ObjectMeta {
                name: Some(release_name(&spec.shard_name, &spec.version)?),
                labels: Some(release_labels(&spec.shard_name, &spec.version)),
                finalizers: Some(vec![RETIRE_FINALIZER.to_string()]),
                ..Default::default()
            },
            spec,
            status: None,
        })
    }
}

/// The gate as cluster mode evaluates it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LocalGate {
    /// Ready servers each non-parked map needs.
    #[schemars(range(min = 1))]
    pub min_ready: i32,

    /// How long `minReady` must hold with no server Unhealthy and no game-server restart.
    #[schemars(regex(pattern = DURATION_PATTERN))]
    pub stable_for: kube::core::Duration,
}

/// One map's Fleet, resolved.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, KubeSchema)]
#[serde(rename_all = "camelCase")]
#[x_kube(validation = Rule::new("(has(self.sizing.parked) && self.sizing.parked) || (has(self.sizing.bufferSize) && has(self.sizing.maxReplicas))").message("a resolved sizing needs bufferSize and maxReplicas, or parked: true"))]
#[cfg_attr(test, x_kube(cel))]
pub struct LocalFleetMap {
    /// The `believer.dev/map` label, and the Fleet's name suffix ([`fleet_name`](crate::fleet_name)).
    #[schemars(length(min = 1, max = MAP_ID_MAX_LEN), regex(pattern = MAP_ID_PATTERN))]
    pub map_id: String,

    /// The servers' first argument: [`level_url`](crate::level_url) of the build's level asset.
    #[schemars(length(min = 1, max = 1024))]
    pub level_url: String,

    /// Resolved: every field the autoscaler needs is set (an unset `minReplicas` is 0), or `parked: true`.
    pub sizing: Sizing,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LocalFleetReleaseStatus {
    #[serde(default)]
    pub observed_generation: Option<i64>,

    #[serde(default)]
    pub gate: Option<GatePhase>,

    /// The newest allocation across the release's Fleets.
    #[serde(default)]
    pub last_allocation_at: Option<Time>,

    #[serde(default)]
    #[schemars(extend("x-kubernetes-list-type" = "map", "x-kubernetes-list-map-keys" = ["mapId"]))]
    pub maps: Vec<LocalMapStatus>,

    #[serde(default)]
    #[schemars(extend("x-kubernetes-list-type" = "map", "x-kubernetes-list-map-keys" = ["type"]))]
    pub conditions: Vec<Condition>,
}

/// One map's Fleet as cluster mode last saw it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LocalMapStatus {
    pub map_id: String,

    #[serde(default)]
    pub fleet: String,

    /// Empty while parked: a parked Fleet has no autoscaler.
    #[serde(default)]
    pub autoscaler: String,

    /// The autoscaler policy in force, for people, e.g. `Buffer 2`.
    #[serde(default)]
    pub policy: String,

    #[serde(default)]
    pub desired: i32,

    #[serde(default)]
    pub ready: i32,

    #[serde(default)]
    pub allocated: i32,

    /// Unhealthy GameServers in the Fleet now.
    #[serde(default)]
    pub unhealthy: i32,

    /// Game-server container restarts across the Fleet's current pods.
    #[serde(default)]
    pub restarts: i32,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::{LABEL_SHARD, LABEL_VERSION};

    const SHA: &str = "abc12345e89abcdef0123456789abcdef0123456";

    fn spec() -> LocalFleetReleaseSpec {
        serde_json::from_value(json!({
            "shardName": "test",
            "shardId": "00000000-0000-0000-0000-000000000001",
            "version": SHA,
            "image": format!("123456789012.dkr.ecr.us-west-2.amazonaws.com/game:{SHA}"),
            "backendAddress": "https://backend.example.com:10100",
            "zone": "cluster-a",
            "gate": {"minReady": 1, "stableFor": "2m"},
            "maps": [{
                "mapId": "Arena",
                "levelUrl": "/Game/Maps/L_Arena.L_Arena?MapRowName=Arena",
                "sizing": {"bufferSize": 2, "minReplicas": 0, "maxReplicas": 50},
            }],
        }))
        .unwrap()
    }

    #[test]
    fn request_sets_name_labels_and_finalizer() {
        let local = LocalFleetRelease::request(spec()).unwrap();
        assert_eq!(local.metadata.name.as_deref(), Some("test-abc12345"));
        let labels = local.metadata.labels.as_ref().unwrap();
        assert_eq!(labels[LABEL_SHARD], "test");
        assert_eq!(labels[LABEL_VERSION], SHA);
        assert_eq!(
            local.metadata.finalizers.as_deref(),
            Some(&[RETIRE_FINALIZER.to_string()][..])
        );
        assert_eq!(
            std::time::Duration::from(local.spec.gate.stable_for),
            std::time::Duration::from_secs(120)
        );
        local.validate_cel().unwrap();
    }

    #[test]
    fn image_must_carry_the_version() {
        let mut local = LocalFleetRelease::request(spec()).unwrap();
        local.spec.image = "123456789012.dkr.ecr.us-west-2.amazonaws.com/game:latest".into();
        assert!(local.validate_cel().is_err());
    }

    #[test]
    fn name_must_match_shard_and_version() {
        let mut local = LocalFleetRelease::request(spec()).unwrap();
        local.metadata.name = Some("by-hand".into());
        assert!(local.validate_cel().is_err());
    }

    #[test]
    fn map_sizing_must_be_resolved() {
        let map = |sizing| json!({"mapId": "Arena", "levelUrl": "/Game/Maps/L_Arena.L_Arena?MapRowName=Arena", "sizing": sizing});
        LocalFleetMap::validate_cel(&map(json!({"parked": true})), None).unwrap();
        LocalFleetMap::validate_cel(&map(json!({"bufferSize": 1, "maxReplicas": 5})), None)
            .unwrap();
        assert!(LocalFleetMap::validate_cel(&map(json!({"bufferSize": 1})), None).is_err());
        assert!(LocalFleetMap::validate_cel(&map(json!({"maxReplicas": 5})), None).is_err());
        // The Buffer rules still apply inside.
        assert!(LocalFleetMap::validate_cel(
            &map(json!({"bufferSize": 6, "maxReplicas": 5})),
            None
        )
        .is_err());
    }

    #[test]
    fn deserializes_a_status() {
        let status: LocalFleetReleaseStatus = serde_json::from_value(json!({
            "observedGeneration": 3,
            "gate": "Passed",
            "lastAllocationAt": "2026-01-02T03:04:05Z",
            "maps": [{"mapId": "Arena", "fleet": "test-abc12345-arena", "autoscaler": "test-abc12345-arena",
                      "policy": "Buffer 2", "desired": 2, "ready": 2, "allocated": 0,
                      "unhealthy": 0, "restarts": 0}],
            "conditions": [],
        }))
        .unwrap();
        assert_eq!(status.gate, Some(GatePhase::Passed));
        assert_eq!(status.maps[0].fleet, "test-abc12345-arena");
    }
}
