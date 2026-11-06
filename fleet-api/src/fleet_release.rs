//! `FleetRelease`: one build's Fleets on one shard, across every cluster the shard runs Fleets in.

use std::collections::BTreeMap;

use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, Time};
use kube::core::ObjectMeta;
use kube::{CustomResource, KubeSchema};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::naming::{release_name, NameError, FLEET_SHARD_NAME_MAX_LEN};
use crate::shard::{SizingOverride, MAX_OVERRIDES};
use crate::shard_config::SHARD_NAME_PATTERN;
use crate::{ANNOTATION_CHANGED_BY, LABEL_SHARD, LABEL_VERSION};

/// `metadata.name` must be `<shard>-<first 8 characters of version>` ([`release_name`]).
pub(crate) const RELEASE_NAME_RULE: &str =
    "self.metadata.name == self.spec.shard + '-' + self.spec.version.substring(0, 8)";

/// The Fleets of one build on one shard. One per (shard, version), named [`release_name`]. The
/// coordinator drives it through its phases and copies it to each cluster as a `LocalFleetRelease`.
///
/// People (through Friendshipper) create one to warm a build (`spec.phase: Warm`), pin it, add
/// overrides, or retire it (`spec.phase: Retired`, plus `force` while servers are Allocated). They
/// never set `Live`: the coordinator does when a channel serves the version, and treats a hand-set
/// `Live` on a release no channel serves as `Warm`.
// Each printcolumn repeats `type_`, which clippy mistakes for a duplicated attribute.
#[allow(clippy::duplicated_attributes)]
#[derive(CustomResource, KubeSchema, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[kube(
    group = "fleet.believer.dev",
    version = "v1alpha1",
    kind = "FleetRelease",
    namespaced,
    shortname = "fr",
    status = "FleetReleaseStatus",
    doc = "One build's Fleets on one shard, across the shard's clusters. Named <shard>-<first 8 characters of the version>.",
    validation = Rule::new(RELEASE_NAME_RULE).message("metadata.name must be <spec.shard>-<the first 8 characters of spec.version>"),
    printcolumn(name = "Shard", type_ = "string", json_path = ".spec.shard"),
    printcolumn(name = "Version", type_ = "string", json_path = ".spec.version"),
    printcolumn(name = "Desired", type_ = "string", json_path = ".spec.phase"),
    printcolumn(name = "Phase", type_ = "string", json_path = ".status.phase"),
    printcolumn(name = "Pinned", type_ = "boolean", json_path = ".spec.pinned"),
    printcolumn(name = "Served by", type_ = "string", json_path = ".status.servedBy"),
    printcolumn(
        name = "Age",
        type_ = "date",
        json_path = ".metadata.creationTimestamp"
    )
)]
#[cfg_attr(test, kube(cel))]
#[serde(rename_all = "camelCase")]
pub struct FleetReleaseSpec {
    /// The shard. Immutable.
    #[schemars(length(min = 1, max = FLEET_SHARD_NAME_MAX_LEN), regex(pattern = SHARD_NAME_PATTERN))]
    #[x_kube(validation = Rule::new("self == oldSelf").message("spec.shard is immutable"))]
    pub shard: String,

    /// The build: a full 40-character lowercase commit SHA. Immutable.
    #[schemars(regex(pattern = "^[0-9a-f]{40}$"))]
    #[x_kube(validation = Rule::new("self == oldSelf").message("spec.version is immutable"))]
    pub version: String,

    /// The phase wanted: `Warm` to warm the build, `Retired` to retire it. The coordinator sets
    /// `Live`; people never do.
    #[serde(default)]
    pub phase: FleetReleaseDesiredPhase,

    /// Never retire automatically, and keep the live sizing while Draining.
    #[serde(default)]
    pub pinned: bool,

    /// Per (map, cluster) sizing for this release only, over the shard's. Copied forward to the next
    /// release on promotion.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(length(max = MAX_OVERRIDES))]
    #[schemars(extend("x-kubernetes-list-type" = "map", "x-kubernetes-list-map-keys" = ["cluster", "mapId"]))]
    pub overrides: Vec<SizingOverride>,

    /// Let retiring delete Fleets that still have Allocated servers, ending those matches.
    #[serde(default)]
    pub force: bool,
}

impl FleetRelease {
    /// A new release ready to create: name, labels and the `changed-by` annotation set, phase `Warm`.
    /// `changed_by` is a username, or the promotion's name when a promotion creates it. The
    /// namespace is left to the `Api` it is created through.
    pub fn request(shard: &str, version: &str, changed_by: &str) -> Result<Self, NameError> {
        let spec = FleetReleaseSpec {
            shard: shard.to_string(),
            version: version.to_string(),
            phase: FleetReleaseDesiredPhase::Warm,
            pinned: false,
            overrides: Vec::new(),
            force: false,
        };
        Ok(FleetRelease {
            metadata: ObjectMeta {
                name: Some(release_name(shard, version)?),
                labels: Some(release_labels(shard, version)),
                annotations: Some(BTreeMap::from([(
                    ANNOTATION_CHANGED_BY.to_string(),
                    changed_by.to_string(),
                )])),
                ..Default::default()
            },
            spec,
            status: None,
        })
    }

    /// The labels this release should carry, from its spec.
    pub fn expected_labels(&self) -> BTreeMap<String, String> {
        release_labels(&self.spec.shard, &self.spec.version)
    }

    /// The observed phase, or `None` if the coordinator hasn't picked the release up yet.
    pub fn phase(&self) -> Option<FleetReleasePhase> {
        self.status.as_ref().and_then(|s| s.phase)
    }

    /// The merge patch that pins or unpins the release, recording who did.
    pub fn pin_patch(pinned: bool, changed_by: &str) -> serde_json::Value {
        serde_json::json!({
            "metadata": {"annotations": {ANNOTATION_CHANGED_BY: changed_by}},
            "spec": {"pinned": pinned},
        })
    }

    /// The merge patch that retires the release, recording who did. `force` also deletes Fleets
    /// that still have Allocated servers.
    pub fn retire_patch(force: bool, changed_by: &str) -> serde_json::Value {
        serde_json::json!({
            "metadata": {"annotations": {ANNOTATION_CHANGED_BY: changed_by}},
            "spec": {"phase": FleetReleaseDesiredPhase::Retired, "force": force},
        })
    }
}

/// `fleet.believer.dev/shard` and `fleet.believer.dev/version`, as on a `FleetRelease` and its
/// `LocalFleetRelease`s.
pub(crate) fn release_labels(shard: &str, version: &str) -> BTreeMap<String, String> {
    BTreeMap::from([
        (LABEL_SHARD.to_string(), shard.to_string()),
        (LABEL_VERSION.to_string(), version.to_string()),
    ])
}

/// The phase a release's spec asks for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum FleetReleaseDesiredPhase {
    /// Warm the Fleets (the warm buffer) and keep them until a channel serves the build or the
    /// warm TTL passes.
    #[default]
    Warm,
    /// A channel serves the build. Set by the coordinator only.
    Live,
    /// Retire the Fleets once they have no Allocated servers (or at once, with `force`).
    Retired,
}

/// The phase a release is in, as observed by the coordinator.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum FleetReleasePhase {
    /// Fleets are being created or haven't passed the gate in every cluster yet.
    Warming,
    /// The gate passed in every cluster; no channel serves the build.
    Warm,
    /// A channel of the shard serves the build; Fleets run the live sizing.
    Live,
    /// No channel serves the build any more; Fleets keep one Ready server for late joiners until
    /// idle for `retireAfterIdle`.
    Draining,
    /// Retiring: the `LocalFleetRelease`s are deleted and the clusters are removing the Fleets.
    Retiring,
    /// Every cluster has removed its Fleets. Final: the release never leaves Retired. Kept as
    /// history for [`RETIRED_RELEASE_RETENTION`](crate::RETIRED_RELEASE_RETENTION), unless a promotion
    /// of the same build deletes it sooner to create a fresh release under the same name.
    Retired,
    /// No channel serves the release and it can't be rendered: `MapsChecked`, `ConfigValid` or
    /// `ImageExists` is False. Not sticky: recomputed on every pass, so fixing the cause recovers it.
    /// Its existing `LocalFleetRelease`s are left as they are, and `warmUntil` still retires it.
    Failed,
}

impl FleetReleasePhase {
    /// `Retiring` or `Retired`.
    pub fn is_retiring(self) -> bool {
        matches!(
            self,
            FleetReleasePhase::Retiring | FleetReleasePhase::Retired
        )
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FleetReleaseStatus {
    #[serde(default)]
    pub phase: Option<FleetReleasePhase>,

    /// When `phase` last changed. A Retired release is deleted
    /// [`RETIRED_RELEASE_RETENTION`](crate::RETIRED_RELEASE_RETENTION) after it.
    #[serde(default)]
    pub phase_since: Option<Time>,

    /// Channel keys of the shard serving the version now, from `Shard.status.channels`.
    #[serde(default)]
    pub served_by: Vec<String>,

    /// When a release no channel has served retires: creation + the shard's `warmTtl`. Set while
    /// Warming, Warm or Failed, cleared once a channel serves it.
    #[serde(default)]
    pub warm_until: Option<Time>,

    /// The newest `lastAllocationAt` across clusters.
    #[serde(default)]
    pub last_allocation_at: Option<Time>,

    /// Draining only: when Allocated last dropped to zero across clusters. The release retires
    /// `retireAfterIdle` after it, unless pinned.
    #[serde(default)]
    pub idle_since: Option<Time>,

    /// Every map ID the build has a level for: the sorted keys of its manifest's `maps.levelAssets`.
    /// Written as soon as the manifest is read, even when the release then fails. Empty until then.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(extend("x-kubernetes-list-type" = "set"))]
    pub build_map_ids: Vec<String>,

    /// The map check for this build, per fleet map. Empty when the manifest lacks a map field
    /// (`MapsChecked=False` with reason `NoLevelAssets` or `NoCookedMaps`).
    #[serde(default)]
    #[schemars(extend("x-kubernetes-list-type" = "map", "x-kubernetes-list-map-keys" = ["mapId"]))]
    pub maps: Vec<ReleaseMapCheck>,

    /// Per cluster, from its `LocalFleetRelease`.
    #[serde(default)]
    #[schemars(extend("x-kubernetes-list-type" = "map", "x-kubernetes-list-map-keys" = ["name"]))]
    pub clusters: Vec<ReleaseCluster>,

    #[serde(default)]
    #[schemars(extend("x-kubernetes-list-type" = "map", "x-kubernetes-list-map-keys" = ["type"]))]
    pub conditions: Vec<Condition>,
}

impl FleetReleaseStatus {
    /// The status of the cluster with this name, if reported.
    pub fn cluster(&self, name: &str) -> Option<&ReleaseCluster> {
        self.clusters.iter().find(|c| c.name == name)
    }

    /// Ready servers across every cluster and map.
    pub fn ready(&self) -> i32 {
        self.clusters
            .iter()
            .flat_map(|c| &c.maps)
            .map(|m| m.ready)
            .sum()
    }

    /// Allocated servers across every cluster and map.
    pub fn allocated(&self) -> i32 {
        self.clusters
            .iter()
            .flat_map(|c| &c.maps)
            .map(|m| m.allocated)
            .sum()
    }
}

/// The map check for one fleet map of a build.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseMapCheck {
    pub map_id: String,

    pub check: MapCheck,

    /// The level URL the Fleets start with: [`level_url`](crate::level_url) of the map's
    /// `maps.levelAssets` entry. Empty unless the check is `OK`.
    #[serde(default)]
    pub level_url: String,
}

/// The outcome of the map check, against the build manifest's `maps` fields. Anything but `OK`
/// fails the release: a fleet map the build can't serve is an error, not a skipped Fleet.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum MapCheck {
    /// The map is in `maps.levelAssets` and its level package is in `maps.cooked.server`.
    #[serde(rename = "OK")]
    Ok,
    /// The map ID isn't a key of the manifest's `maps.levelAssets`: the build doesn't have the map.
    NotInBuild,
    /// The map's level package isn't in `maps.cooked.server` (on-demand matches would fail too).
    NotCooked,
}

impl MapCheck {
    /// Whether the map gets a Fleet.
    pub fn renders(self) -> bool {
        self == MapCheck::Ok
    }
}

/// A release in one cluster.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseCluster {
    pub name: String,

    /// From the cluster's `LocalFleetRelease`.
    #[serde(default)]
    pub gate: Option<GatePhase>,

    #[serde(default)]
    #[schemars(extend("x-kubernetes-list-type" = "map", "x-kubernetes-list-map-keys" = ["mapId"]))]
    pub maps: Vec<ReleaseClusterMap>,

    /// When the cluster's `LocalFleetRelease` was last read successfully; a cluster that can't be
    /// reached keeps its last values and this goes stale.
    #[serde(default)]
    pub observed_at: Option<Time>,

    /// The last read's error; empty when it succeeded.
    #[serde(default)]
    pub error: String,

    /// Set while the cluster's `LocalFleetRelease` reports `RetireStuck=True`; absent otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retire_stuck: Option<RetireStuck>,
}

/// A cluster's retirement waiting on Allocated servers, from its `LocalFleetRelease`'s `RetireStuck`
/// condition.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RetireStuck {
    /// The condition's `lastTransitionTime`.
    pub since: Time,

    /// The condition's message, as is.
    pub message: String,
}

/// One map's Fleet in one cluster.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseClusterMap {
    pub map_id: String,

    /// The Fleet's name.
    #[serde(default)]
    pub fleet: String,

    /// The autoscaler policy in force, for people, e.g. `Buffer 2`.
    #[serde(default)]
    pub policy: String,

    /// The Fleet's desired replicas.
    #[serde(default)]
    pub desired: i32,

    #[serde(default)]
    pub ready: i32,

    #[serde(default)]
    pub allocated: i32,
}

/// The warm gate in one cluster. `Passed` and `Failed` stick until the `LocalFleetRelease`'s spec
/// changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum GatePhase {
    /// Not passed yet. Clusters never fail the gate for slowness; the coordinator times it out.
    Pending,
    /// Every non-parked map had `minReady` Ready servers, with none Unhealthy and no restarts, for
    /// `stableFor`.
    Passed,
    /// A server went Unhealthy or a game-server container restarted before the gate passed.
    Failed,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::sizing::Sizing;

    const SHA: &str = "abc12345e89abcdef0123456789abcdef0123456";

    fn release() -> FleetRelease {
        FleetRelease::request("test", SHA, "someone").unwrap()
    }

    #[test]
    fn request_sets_name_labels_and_annotation() {
        let release = release();
        assert_eq!(release.metadata.name.as_deref(), Some("test-abc12345"));
        let labels = release.metadata.labels.as_ref().unwrap();
        assert_eq!(labels[LABEL_SHARD], "test");
        assert_eq!(labels[LABEL_VERSION], SHA);
        assert_eq!(labels, &release.expected_labels());
        assert_eq!(
            release.metadata.annotations.as_ref().unwrap()[ANNOTATION_CHANGED_BY],
            "someone"
        );
        assert_eq!(release.spec.phase, FleetReleaseDesiredPhase::Warm);
        assert_eq!(release.phase(), None);
        assert!(FleetRelease::request("test", "abc12345", "someone").is_err());
    }

    #[test]
    fn request_serializes_like_the_contract() {
        let value = serde_json::to_value(release()).unwrap();
        assert_eq!(
            value["spec"],
            json!({"shard": "test", "version": SHA, "phase": "Warm", "pinned": false, "force": false})
        );
    }

    #[test]
    fn name_must_match_shard_and_version() {
        let ok = release();
        ok.validate_cel().unwrap();

        let mut renamed = ok.clone();
        renamed.metadata.name = Some("test-other".into());
        assert!(renamed.validate_cel().is_err());
    }

    #[test]
    fn shard_and_version_are_immutable() {
        let old = release();
        let mut changed = old.clone();
        changed.spec.version = "abc12345ffffffffffffffffffffffffffffffff".into();
        assert!(changed.validate_cel_update(&old).is_err());

        let mut pinned = old.clone();
        pinned.spec.pinned = true;
        pinned.spec.phase = FleetReleaseDesiredPhase::Retired;
        pinned.spec.overrides.push(SizingOverride {
            cluster: "cluster-a".into(),
            map_id: "Arena".into(),
            sizing: Sizing::parked(),
        });
        pinned.validate_cel_update(&old).unwrap();
    }

    #[test]
    fn override_sizing_is_checked() {
        let mut release = release();
        release.spec.overrides.push(SizingOverride {
            cluster: "cluster-a".into(),
            map_id: "Arena".into(),
            sizing: Sizing::absolute(5, 0, 4),
        });
        assert!(release.validate_cel().is_err());
    }

    #[test]
    fn patches_record_who() {
        assert_eq!(
            FleetRelease::pin_patch(true, "someone"),
            json!({"metadata": {"annotations": {ANNOTATION_CHANGED_BY: "someone"}},
                   "spec": {"pinned": true}})
        );
        assert_eq!(
            FleetRelease::retire_patch(false, "someone"),
            json!({"metadata": {"annotations": {ANNOTATION_CHANGED_BY: "someone"}},
                   "spec": {"phase": "Retired", "force": false}})
        );
    }

    #[test]
    fn retire_stuck_round_trips_and_is_omitted_when_absent() {
        let stuck = json!({
            "name": "cluster-a",
            "maps": [],
            "error": "",
            "retireStuck": {
                "since": "2026-11-05T10:00:00Z",
                "message": "2 Allocated servers after 6h",
            },
        });
        let cluster: ReleaseCluster = serde_json::from_value(stuck.clone()).unwrap();
        let retire_stuck = cluster.retire_stuck.as_ref().unwrap();
        assert_eq!(retire_stuck.message, "2 Allocated servers after 6h");
        assert_eq!(
            serde_json::to_value(&cluster).unwrap()["retireStuck"],
            stuck["retireStuck"]
        );
        let again: ReleaseCluster =
            serde_json::from_value(serde_json::to_value(&cluster).unwrap()).unwrap();
        assert_eq!(again, cluster);

        let clear: ReleaseCluster = serde_json::from_value(json!({"name": "cluster-a"})).unwrap();
        assert_eq!(clear.retire_stuck, None);
        let value = serde_json::to_value(&clear).unwrap();
        assert!(value.get("retireStuck").is_none(), "{value}");
    }

    #[test]
    fn deserializes_a_full_example() {
        let release: FleetRelease = serde_json::from_value(json!({
            "apiVersion": "fleet.believer.dev/v1alpha1",
            "kind": "FleetRelease",
            "metadata": {"name": "test-abc12345", "namespace": "fleet-system"},
            "spec": {
                "shard": "test",
                "version": SHA,
                "phase": "Live",
                "pinned": false,
                "overrides": [{"cluster": "cluster-a", "mapId": "Arena", "sizing": {"parked": true}}],
                "force": false,
            },
            "status": {
                "phase": "Live",
                "servedBy": ["launcher/stable-1.0"],
                "phaseSince": "2026-01-02T03:00:00Z",
                "lastAllocationAt": "2026-01-02T03:04:05Z",
                "buildMapIds": ["Arena", "Old"],
                "maps": [
                    {"mapId": "Arena", "check": "OK",
                     "levelUrl": "/Game/Maps/L_Arena.L_Arena?MapRowName=Arena"},
                    {"mapId": "Old", "check": "NotCooked"},
                    {"mapId": "Gone", "check": "NotInBuild"},
                ],
                "clusters": [{
                    "name": "cluster-a",
                    "gate": "Passed",
                    "maps": [{"mapId": "Arena", "fleet": "test-abc12345-arena", "policy": "Buffer 2",
                              "desired": 3, "ready": 2, "allocated": 1}],
                    "observedAt": "2026-01-02T03:05:00Z",
                }, {
                    "name": "cluster-b",
                    "gate": "Pending",
                    "maps": [{"mapId": "Arena", "desired": 2, "ready": 1, "allocated": 2}],
                }],
                "conditions": [],
            },
        }))
        .unwrap();

        assert_eq!(release.phase(), Some(FleetReleasePhase::Live));
        let status = release.status.as_ref().unwrap();
        assert_eq!(status.maps[0].check, MapCheck::Ok);
        assert!(status.maps[0].check.renders());
        assert!(!status.maps[1].check.renders());
        assert_eq!(status.maps[2].check, MapCheck::NotInBuild);
        assert_eq!(status.build_map_ids, ["Arena", "Old"]);
        assert_eq!(
            status.cluster("cluster-a").unwrap().gate,
            Some(GatePhase::Passed)
        );
        assert_eq!(status.ready(), 3);
        assert_eq!(status.allocated(), 3);
        assert!(release.spec.overrides[0].sizing.is_parked());
        assert!(FleetReleasePhase::Retired.is_retiring());
        assert!(!FleetReleasePhase::Draining.is_retiring());
    }
}
