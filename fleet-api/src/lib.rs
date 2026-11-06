//! Kubernetes custom resources for fleet management, API group `fleet.believer.dev`, version `v1alpha1`.
//!
//! It defines five namespaced kinds:
//!
//! - [`ShardConfig`]: the guardrails for one shard, owned by git and applied by Flux;
//! - [`Shard`]: what a shard currently serves, observed by the coordinator, and its Fleet settings,
//!   edited in Friendshipper;
//! - [`Promotion`]: a request to put a build on one channel of a shard, created by Friendshipper
//!   (or by hand), validated and run step by step by the coordinator;
//! - [`FleetRelease`]: one build's Fleets on one shard, driven by the coordinator;
//! - [`LocalFleetRelease`]: a `FleetRelease` resolved for one game cluster, where cluster mode
//!   renders its Fleets.
//!
//! The crate holds no deployment-specific values: those live in the `ShardConfig` objects and the
//! coordinator's own configuration. See the README for the contract between the coordinator and its clients.

mod backend;
mod channel;
mod crds;
mod fleet_release;
mod local_fleet_release;
mod naming;
mod promotion;
mod shard;
mod shard_config;
mod sizing;

pub use backend::{BackendParts, BackendVersion, ParseBackendVersionError};
pub use channel::{ChannelKey, ChannelKind, ChannelRef, GameConfig, ParseChannelKeyError};
pub use crds::{render_crds, CrdFile};
pub use fleet_release::{
    FleetRelease, FleetReleaseDesiredPhase, FleetReleasePhase, FleetReleaseSpec,
    FleetReleaseStatus, GatePhase, MapCheck, ReleaseCluster, ReleaseClusterMap, ReleaseMapCheck,
    RetireStuck,
};
pub use local_fleet_release::{
    LocalFleetMap, LocalFleetRelease, LocalFleetReleaseSpec, LocalFleetReleaseStatus, LocalGate,
    LocalMapStatus,
};
pub use naming::{
    check_map_slugs, fleet_name, is_valid_map_id, level_package, level_url, map_slug, release_name,
    short_sha, step_workflow_name, NameError, FLEET_NAME_MAX_LEN, FLEET_SHARD_NAME_MAX_LEN,
    MAP_ID_MAX_LEN, MAP_SLUG_MAX_LEN, SHORT_SHA_LEN,
};
pub use promotion::{
    is_full_sha, step_phases, workflow_name, BackendPartChange, BackendPlan, CancelRequest,
    Promotion, PromotionChange, PromotionChangeKind, PromotionPhase, PromotionPlan, PromotionSpec,
    PromotionStatus, PromotionStep, SkipFleetGate, PROMOTION_STEPS, STEP_DEPLOY_BACKEND,
    STEP_FLIP_CHANNEL, STEP_MARK_LIVE, STEP_PROMOTE, STEP_PUBLISH_CLIENT, STEP_WARM_FLEETS,
};
pub use shard::{
    BackendStatus, ChannelSource, ChannelStatus, LiveSizing, PromotionSummary, ReleaseSummary,
    Shard, ShardSpec, ShardStatus, SizingOverride,
};
pub use shard_config::{
    fleet_defaults, FleetCluster, FleetGate, FleetLimits, FleetsConfig, PromotionPolicy,
    ShardChannel, ShardConfig, ShardConfigSpec, ShardConfigStatus,
};
pub use sizing::{BufferSize, ParseBufferSizeError, Sizing, SizingError};

/// The API group of every kind in this crate.
pub const GROUP: &str = "fleet.believer.dev";

/// The API version of every kind in this crate.
pub const VERSION: &str = "v1alpha1";

/// The namespace the objects live in.
pub const DEFAULT_NAMESPACE: &str = "fleet-system";

/// Label on a `Promotion`, `FleetRelease` or `LocalFleetRelease`: its shard. Mirrors `spec.shard`
/// (`spec.shardName` on a `LocalFleetRelease`).
pub const LABEL_SHARD: &str = "fleet.believer.dev/shard";

/// Label on a `FleetRelease` or `LocalFleetRelease`: the full version. Mirrors `spec.version`.
pub const LABEL_VERSION: &str = "fleet.believer.dev/version";

/// Label on a `Fleet` and its `FleetAutoscaler` (not the Fleet's templates): the
/// `LocalFleetRelease` they were rendered from. Cluster mode retires a labelled Fleet whose release
/// no longer exists.
pub const LABEL_RELEASE: &str = "fleet.believer.dev/release";

/// Annotation on a `Fleet`'s own metadata (not its templates), set by cluster mode when it creates
/// the Fleet: a hash of the spec fields the Fleet's template is made from (cluster mode's
/// `template_hash`), not of the rendered template, so changes to how cluster mode renders don't
/// change it. Agones fills in defaults on Fleets, so a live Fleet never equals a fresh render;
/// cluster mode compares the current spec's hash with this instead, to tell a spec change that would
/// change the template (refused) from one that wouldn't.
pub const ANNOTATION_TEMPLATE_HASH: &str = "fleet.believer.dev/template-hash";

/// Label on a promotion step's Argo `Workflow`: the step it runs, e.g. `deploy-backend`.
pub const LABEL_STEP: &str = "fleet.believer.dev/step";

/// Label on a `Promotion`: the channel it targets, in label form (see [`ChannelKey::label_value`]).
pub const LABEL_CHANNEL: &str = "fleet.believer.dev/channel";

/// Label on a promotion's Argo `Workflow`: the `Promotion` that submitted it.
pub const LABEL_PROMOTION: &str = "fleet.believer.dev/promotion";

/// Annotation on objects Friendshipper writes: the in-app username of the person who made the change.
pub const ANNOTATION_CHANGED_BY: &str = "fleet.believer.dev/changed-by";

/// Annotation on a `Promotion`: optional free text from the promote form.
pub const ANNOTATION_NOTE: &str = "fleet.believer.dev/note";

/// Annotation on a `Promotion`: when someone asked to cancel it, RFC 3339 UTC in whole seconds, by the
/// requester's clock. Its presence is the request, even if the value doesn't parse. See
/// [`Promotion::cancel_request`].
pub const ANNOTATION_CANCEL_REQUESTED_AT: &str = "fleet.believer.dev/cancel-requested-at";

/// Annotation on a `Promotion`: the in-app username of who asked to cancel it, like
/// [`ANNOTATION_CHANGED_BY`]. Set together with [`ANNOTATION_CANCEL_REQUESTED_AT`].
pub const ANNOTATION_CANCEL_REQUESTED_BY: &str = "fleet.believer.dev/cancel-requested-by";

/// Finalizer the coordinator puts on a `Promotion`, so it can stop the workflow before the object goes.
pub const PROMOTION_FINALIZER: &str = "fleet.believer.dev/stop-workflow";

/// Finalizer that retires a release before its object goes.
///
/// On a `LocalFleetRelease`: set by whoever creates it (cluster mode adds it if missing); cluster mode
/// removes it once the release's Fleets are gone.
///
/// On a `FleetRelease`: the coordinator adds it to every one. Deleting a `FleetRelease`, by hand or by
/// retention, retires it as if `spec.phase` were `Retired`, even a Live one: every cluster's
/// `LocalFleetRelease` is deleted. The coordinator removes the finalizer only once every cluster has
/// been read and none still holds the `LocalFleetRelease`; an unreachable cluster is waited for
/// (`ClustersSynced=False`, reason [`REASON_UNREACHABLE`](conditions::REASON_UNREACHABLE), and a
/// Warning event `RetireWaiting`). The `fleet release retired` record is logged as it is removed.
pub const RETIRE_FINALIZER: &str = "fleet.believer.dev/retire";

/// Labels the allocation contract requires on every Fleet GameServer, and on its pod template. The
/// backend's allocation selector matches on them, so their names and values must not change.
pub mod game_server_labels {
    /// The backend shard name.
    pub const SHARD_NAME: &str = "believer.dev/shard-name";
    /// The backend's shard ID.
    pub const SHARD_ID: &str = "believer.dev/shard-id";
    /// The build's full commit SHA.
    pub const VERSION: &str = "believer.dev/version";
    /// The map ID the server hosts.
    pub const MAP: &str = "believer.dev/map";
    /// The cluster's zone, as the backend routes by it.
    pub const ZONE: &str = "believer.dev/zone";
    /// How the server was made: [`ALLOCATION_MODE_FLEET`] for Fleet servers.
    pub const ALLOCATION_MODE: &str = "believer.dev/allocation-mode";
    /// The [`ALLOCATION_MODE`] value of a Fleet server.
    pub const ALLOCATION_MODE_FLEET: &str = "fleet";
}

/// How long the coordinator keeps a `Retired` `FleetRelease` before deleting it, counted from its
/// `status.phaseSince`.
pub const RETIRED_RELEASE_RETENTION: std::time::Duration =
    std::time::Duration::from_secs(7 * 24 * 60 * 60);

/// Default number of finished `Promotion`s the coordinator keeps per (shard, channel) (its `--promotion-keep`), on
/// top of the channel's latest `Succeeded` one.
pub const RETAINED_FINISHED_PROMOTIONS: usize = 5;

/// How many poll intervals may pass before readers show an observation as stale.
pub const STALE_AFTER_POLL_INTERVALS: u32 = 3;

/// Condition types and reasons used in the kinds' `status.conditions`.
pub mod conditions {
    /// `Shard`: the backend healthcheck was read on the last poll.
    pub const BACKEND_OBSERVED: &str = "BackendObserved";
    /// `Shard`: every channel was read on the last poll.
    pub const CHANNELS_OBSERVED: &str = "ChannelsObserved";
    /// `Shard`, `ShardConfig` and `FleetRelease`: the shard's configuration is usable. False on a
    /// `FleetRelease` with reason [`REASON_NO_SHARD_CONFIG`], [`REASON_NO_SHARD`], [`REASON_NO_FLEETS`],
    /// [`REASON_INCOMPLETE_SHARD_CONFIG`] or [`REASON_GATE_CANNOT_PASS`]: a release no channel serves is
    /// then Failed, and nothing new is rendered until it is True.
    pub const CONFIG_VALID: &str = "ConfigValid";
    /// `Shard`: the Steam channels were read through the Steam Web API on the last poll. When false,
    /// Steam channels fall back to the last successful promotion (`source: promotion`).
    pub const STEAM_OBSERVED: &str = "SteamObserved";
    /// `Shard`: every Live release has a Ready server for each of its `OK` maps in every cluster (or
    /// the map is parked there: desired 0). Set only when the `ShardConfig` has `fleets`. True with
    /// [`REASON_OK`], including when nothing is Live; False with [`REASON_MAPS_NOT_READY`], the
    /// message naming the first failure.
    pub const FLEETS_READY: &str = "FleetsReady";
    /// `FleetRelease`: every fleet map passed the map check against the build manifest. False with
    /// reason [`REASON_NO_LEVEL_ASSETS`], [`REASON_NO_COOKED_MAPS`], [`REASON_MAPS_UNAVAILABLE`] or
    /// [`REASON_NO_FLEET_MAPS`] or [`REASON_MANIFEST_UNREADABLE`]: a release no channel serves is
    /// then Failed. A served release stays Live: with [`REASON_MAPS_UNAVAILABLE`] it keeps its `OK`
    /// maps; with the others `status.maps` is empty and nothing new is rendered, its existing
    /// `LocalFleetRelease`s left as they are. Checked again every pass, so a fixed manifest or shard
    /// recovers the release. False with [`REASON_MANIFEST_PENDING`] while the manifest can't be read
    /// yet: the release keeps its phase and carries on from any earlier check, writing no
    /// `LocalFleetRelease` until one exists.
    pub const MAPS_CHECKED: &str = "MapsChecked";
    /// `FleetRelease`: every cluster's `LocalFleetRelease` is written and was read recently. False
    /// with reason [`REASON_UNREACHABLE`], [`REASON_SIZING_INVALID`] or [`REASON_WRITE_FAILED`]; the
    /// message names the cluster.
    pub const CLUSTERS_SYNCED: &str = "ClustersSynced";
    /// `FleetRelease`: the gate passed in every cluster for its current spec. False with reason
    /// [`REASON_PENDING`] while a cluster hasn't passed yet (the message names them), or
    /// [`REASON_GATE_FAILED`] when a cluster reports it failed.
    ///
    /// Current, not sticky: it goes False again while a cluster takes a new spec (a map added after
    /// Warm, say), though `status.phase` stays `Warm`. The `warm-fleets` step waits on this
    /// condition, so don't read phase `Warm` as "ready to publish".
    pub const GATE_PASSED: &str = "GatePassed";
    /// `LocalFleetRelease`: the image exists in the registry. False: nothing is rendered.
    /// `FleetRelease`: no cluster reports `ImageExists=False`. False with reason
    /// [`REASON_IMAGE_MISSING`], naming the clusters; a release no channel serves is then Failed.
    pub const IMAGE_EXISTS: &str = "ImageExists";
    /// `FleetRelease`: someone set `spec.phase` to a value the coordinator doesn't act on. True with
    /// reason [`REASON_LIVE_NOT_SERVED`] when `Live` was set by hand on a release no channel serves:
    /// it is treated as `Warm`.
    pub const DESIRED_PHASE_IGNORED: &str = "DesiredPhaseIgnored";
    /// `LocalFleetRelease`: the Fleets and autoscalers match the spec. False when maps are refused:
    /// the reason is the first refusal's, the messages are joined with "; ", and the other maps
    /// render. A Fleet a refused map already has keeps running, and follows sizing and parking unless
    /// the sizing or the name is what's refused.
    pub const RENDERED: &str = "Rendered";
    /// `LocalFleetRelease`: the gate failed; the message names the GameServer.
    pub const GATE_FAILED: &str = "GateFailed";
    /// `LocalFleetRelease`: retiring has waited longer than cluster mode's `--retire-stuck-after`
    /// for the Fleets' Allocated servers to reach zero. Informational: it never forces the retirement.
    pub const RETIRE_STUCK: &str = "RetireStuck";

    /// Reason: the observation or check succeeded.
    pub const REASON_OK: &str = "Ok";
    /// Reason: some, but not all, channels could not be read.
    pub const REASON_PARTIAL_FAILURE: &str = "PartialFailure";
    /// Reason: a Steam branch appears in more than one `ShardConfig`.
    pub const REASON_STEAM_BRANCH_CONFLICT: &str = "SteamBranchConflict";
    /// Reason (`SteamObserved=False`): no Steam Web API key is configured.
    pub const REASON_NO_KEY: &str = "NoKey";
    /// Reason (`SteamObserved=False`): the Steam Web API rejected the key.
    pub const REASON_KEY_REJECTED: &str = "KeyRejected";
    /// Reason (`GatePassed=False` on a `FleetRelease`): a cluster reported the gate failed.
    pub const REASON_GATE_FAILED: &str = "GateFailed";
    /// Reason (`Rendered=False` on a `LocalFleetRelease`): the spec changed in a way that would
    /// change an existing Fleet's template, which is refused; the coordinator makes a new release
    /// instead.
    pub const REASON_TEMPLATE_CHANGED: &str = "TemplateChanged";
    /// Reason (`Rendered=False` on a `LocalFleetRelease`): a map's Fleet name can't be made, or two
    /// map IDs' slugs collide and would name the same Fleet (every such map is refused).
    pub const REASON_INVALID_NAME: &str = "InvalidName";
    /// Reason (`Rendered=False` on a `LocalFleetRelease`): a map's sizing is a Buffer policy Agones
    /// would refuse. The coordinator checks this before writing, so it means a hand-written spec.
    pub const REASON_INVALID_SIZING: &str = "InvalidSizing";
    /// Reason (`Rendered=False` on a `LocalFleetRelease`): a value for the release label, or one of
    /// the `believer.dev/*` labels the templates carry, which the backend matches on, isn't a valid
    /// label value.
    pub const REASON_INVALID_LABEL_VALUE: &str = "InvalidLabelValue";
    /// Reason (`Rendered=False` on a `LocalFleetRelease`): a map's `levelUrl` doesn't end in
    /// `?MapRowName=<mapId>`, without which a warm server never goes Ready.
    pub const REASON_INVALID_LEVEL_URL: &str = "InvalidLevelUrl";
    /// Reason (`Rendered=False` on a `LocalFleetRelease`): creating a map's Fleet found its name
    /// taken by a Fleet without this release's label; that Fleet is left alone. (One with the label is
    /// adopted.)
    pub const REASON_NAME_COLLISION: &str = "NameCollision";
    /// Reason (`MapsChecked=False`): the build manifest has no `maps.levelAssets`.
    pub const REASON_NO_LEVEL_ASSETS: &str = "NoLevelAssets";
    /// Reason (`MapsChecked=False`): the build manifest has no `maps.cooked.server`.
    pub const REASON_NO_COOKED_MAPS: &str = "NoCookedMaps";
    /// Reason (`MapsChecked=False`): one or more fleet maps failed the check (`status.maps` says
    /// how); the message lists their IDs.
    pub const REASON_MAPS_UNAVAILABLE: &str = "MapsUnavailable";
    /// Reason (`MapsChecked=False`): the shard's `spec.maps` is empty. CEL refuses that on any write
    /// to a spec with `fleetsEnabled`; an older Shard keeps it through ratcheting until its spec is
    /// next written.
    pub const REASON_NO_FLEET_MAPS: &str = "NoFleetMaps";
    /// Reason (`MapsChecked=False`): the build manifest isn't readable yet; checked again later.
    pub const REASON_MANIFEST_PENDING: &str = "ManifestPending";
    /// Reason (`MapsChecked=False`): the build manifest was read but won't parse; the message is
    /// `the build manifest won't parse: <error>`. Read again every pass.
    pub const REASON_MANIFEST_UNREADABLE: &str = "ManifestUnreadable";
    /// Reason (`FleetsReady=False`): a Live release has no Ready server for an `OK` map in a cluster
    /// (a missing entry counts), or no cluster reports the release at all. The message names the
    /// first, checking releases by name, then maps and clusters in status order:
    /// `no Ready server: <release>: <cluster>/<map>` or `no Ready server: <release>: no cluster
    /// reports it`.
    pub const REASON_MAPS_NOT_READY: &str = "MapsNotReady";
    /// Reason (`ConfigValid=False` on a `FleetRelease`): no `ShardConfig` is named `spec.shard`.
    pub const REASON_NO_SHARD_CONFIG: &str = "NoShardConfig";
    /// Reason (`ConfigValid=False` on a `FleetRelease`): the shard's `Shard` doesn't exist.
    pub const REASON_NO_SHARD: &str = "NoShard";
    /// Reason (`ConfigValid=False` on a `FleetRelease`): the `ShardConfig` has no `fleets` section.
    pub const REASON_NO_FLEETS: &str = "NoFleets";
    /// Reason (`ConfigValid=False` on a `FleetRelease`): the `ShardConfig` lacks a field Fleets need
    /// (`shardId` or `backendAddress`); the message names it.
    pub const REASON_INCOMPLETE_SHARD_CONFIG: &str = "IncompleteShardConfig";
    /// Reason (`ConfigValid=False` on a `FleetRelease`): `fleets.gate.timeout` is shorter than
    /// `stableFor`, defaults included.
    pub const REASON_GATE_CANNOT_PASS: &str = "GateCannotPass";
    /// Reason (`GatePassed=False` on a `FleetRelease`): a cluster hasn't passed the gate yet.
    pub const REASON_PENDING: &str = "Pending";
    /// Reason (`GatePassed=False` on a `FleetRelease`): the release is Retiring or Retired and its
    /// gate never passed. Final: a gate that passed before retiring stays True.
    pub const REASON_RETIRED: &str = "Retired";
    /// Reason (`ClustersSynced=False`): a cluster's API couldn't be reached. Also used by the
    /// `Shard` observers for a transport error.
    pub const REASON_UNREACHABLE: &str = "Unreachable";
    /// Reason (`ClustersSynced=False`): the resolved sizing for a (map, cluster) breaks the Buffer
    /// rules or the shard's limit, so that cluster's `LocalFleetRelease` isn't written.
    pub const REASON_SIZING_INVALID: &str = "SizingInvalid";
    /// Reason (`ClustersSynced=False`): writing a cluster's `LocalFleetRelease` failed.
    pub const REASON_WRITE_FAILED: &str = "WriteFailed";
    /// Reason (`ImageExists=False` on a `FleetRelease`): a cluster reports the image missing.
    pub const REASON_IMAGE_MISSING: &str = "ImageMissing";
    /// Reason (`ImageExists=True` on a `LocalFleetRelease`): the image is in the registry, or every
    /// Fleet already exists, so the image isn't checked: a registry outage mustn't freeze them.
    pub const REASON_IMAGE_FOUND: &str = "Found";
    /// Reason (`ImageExists=False` on a `LocalFleetRelease`): the image isn't in ECR. The
    /// coordinator reports it on the `FleetRelease` as [`REASON_IMAGE_MISSING`].
    pub const REASON_IMAGE_NOT_FOUND: &str = "NotFound";
    /// Reason (`ImageExists=False` on a `LocalFleetRelease`): the image isn't an ECR image reference,
    /// so it can't be checked.
    pub const REASON_IMAGE_NOT_ECR: &str = "NotEcr";
    /// Reason (`ImageExists=False` on a `LocalFleetRelease`): the registry lookup failed; the
    /// message has the error. Checked again on the next pass.
    pub const REASON_IMAGE_CHECK_FAILED: &str = "CheckFailed";
    /// Reason (`DesiredPhaseIgnored=True`): `spec.phase` is `Live` but no channel serves the
    /// version, so the release is treated as `Warm`.
    pub const REASON_LIVE_NOT_SERVED: &str = "LiveNotServed";
    /// Reason (`RetireStuck=True` on a `LocalFleetRelease`): GameServers remain past
    /// `--retire-stuck-after`; the message gives the threshold, how many remain, and how many are
    /// Allocated.
    pub const REASON_GAME_SERVERS_REMAINING: &str = "GameServersRemaining";
}

/// Values of `Promotion.status.reason`.
pub mod reasons {
    /// No `ShardConfig` is named `spec.shard`.
    pub const UNKNOWN_SHARD: &str = "UnknownShard";
    /// The `ShardConfig` has no such channel.
    pub const UNKNOWN_CHANNEL: &str = "UnknownChannel";
    /// No build manifest exists for `spec.version`.
    pub const NO_BUILD_MANIFEST: &str = "NoBuildManifest";
    /// The build manifest exists, but not every artifact succeeded.
    pub const BUILD_NOT_GREEN: &str = "BuildNotGreen";
    /// The game config is not allowed on the channel (Steam takes only `Test` or `Shipping`).
    pub const INVALID_GAME_CONFIG: &str = "InvalidGameConfig";
    /// `spec.deployBackend` is true but the shard's `promotion.allowDeployBackend` is false.
    pub const BACKEND_DEPLOY_NOT_ALLOWED: &str = "BackendDeployNotAllowed";
    /// Another `Promotion` for the same (shard, channel) is `Pending` or `Running`.
    pub const CHANNEL_BUSY: &str = "ChannelBusy";
    /// Not a rejection: the build manifest can't be read yet, so the promotion stays phase-less and
    /// keeps retrying, holding its channel. `message` says why.
    pub const RETRYING: &str = "Retrying";
    /// The build manifest exists but can't be parsed. Rejected without retrying.
    pub const BAD_BUILD_MANIFEST: &str = "BadBuildManifest";
    /// Creating the workflow failed with a client error other than a conflict or throttling.
    pub const SUBMIT_FAILED: &str = "SubmitFailed";
    /// `Cancelled` because someone asked to (the cancel annotations).
    pub const CANCEL_REQUESTED: &str = "CancelRequested";
    /// `Cancelled` because someone deleted the Promotion by hand while it was active.
    pub const DELETED: &str = "Deleted";
    /// `Failed`: the build's `FleetRelease` didn't pass the warm gate (`GatePassed`) within the
    /// shard's `gate.timeout`. Nothing was published, so no channel changed.
    pub const FLEET_GATE_TIMEOUT: &str = "FleetGateTimeout";
    /// `Failed` at once, before `gate.timeout`: the build's `FleetRelease` can't warm. A cluster
    /// reports the gate failed (`GatePassed=False`, reason `GateFailed`), or the release is Failed
    /// (`MapsChecked`, `ConfigValid` or `ImageExists` False); the message names the cluster or the
    /// condition. Nothing was published, so no channel changed.
    pub const FLEET_GATE_FAILED: &str = "FleetGateFailed";
    /// The build's `FleetRelease` name is taken by a release of another version (the short SHAs
    /// collide).
    pub const NAME_COLLISION: &str = "NameCollision";
    /// `Failed`: a step's Argo workflow failed; the message is the step's message.
    pub const WORKFLOW_FAILED: &str = "WorkflowFailed";
    /// `Failed`: a step's Argo workflow disappeared before it finished.
    pub const WORKFLOW_MISSING: &str = "WorkflowMissing";
    /// `Failed`: `flip-channel` on a Steam channel has no BuildID. `publish-client` output no
    /// `build_id`, and the Steam lookup (the newest `GetAppBuilds` entry described
    /// `<Game> <sha> (<gameConfig>)`) found none, failed, or had no Web API key; the message says
    /// which. Nothing was flipped.
    pub const NO_BUILD_ID: &str = "NoBuildId";
}
