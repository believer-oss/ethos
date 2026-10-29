//! Kubernetes custom resources for fleet management, API group `fleet.believer.dev`, version `v1alpha1`.
//!
//! It defines three namespaced kinds:
//!
//! - [`ShardConfig`]: the guardrails for one shard, owned by git and applied by Flux;
//! - [`Shard`]: what a shard currently serves, written only by the coordinator;
//! - [`Promotion`]: a request to put a build on one channel of a shard, created by Friendshipper
//!   (or by hand), validated and run by the coordinator.
//!
//! The crate holds no deployment-specific values: those live in the `ShardConfig` objects and the
//! coordinator's own configuration. See the README for the contract between the coordinator and its clients.

mod backend;
mod channel;
mod crds;
mod promotion;
mod shard;
mod shard_config;

pub use backend::{BackendParts, BackendVersion, ParseBackendVersionError};
pub use channel::{ChannelKey, ChannelKind, ChannelRef, GameConfig, ParseChannelKeyError};
pub use crds::{render_crds, CrdFile};
pub use promotion::{
    is_full_sha, workflow_name, BackendPartChange, BackendPlan, CancelRequest, Promotion,
    PromotionPhase, PromotionPlan, PromotionSpec, PromotionStatus, PromotionStep, STEP_PROMOTE,
};
pub use shard::{
    BackendStatus, ChannelSource, ChannelStatus, PromotionSummary, Shard, ShardSpec, ShardStatus,
};
pub use shard_config::{
    PromotionPolicy, ShardChannel, ShardConfig, ShardConfigSpec, ShardConfigStatus,
};

/// The API group of every kind in this crate.
pub const GROUP: &str = "fleet.believer.dev";

/// The API version of every kind in this crate.
pub const VERSION: &str = "v1alpha1";

/// The namespace the objects live in.
pub const DEFAULT_NAMESPACE: &str = "fleet-system";

/// Label on a `Promotion`: the shard it targets. Mirrors `spec.shard`.
pub const LABEL_SHARD: &str = "fleet.believer.dev/shard";

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
    /// `Shard` and `ShardConfig`: the shard's configuration is usable.
    pub const CONFIG_VALID: &str = "ConfigValid";
    /// `Shard`: the Steam channels were read through the Steam Web API on the last poll. When false,
    /// Steam channels fall back to the last successful promotion (`source: promotion`).
    pub const STEAM_OBSERVED: &str = "SteamObserved";

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
