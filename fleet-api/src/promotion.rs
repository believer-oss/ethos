//! `Promotion`: a request to put a build on one channel of a shard.

use std::collections::BTreeMap;

use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, Time};
use kube::core::ObjectMeta;
use kube::{CustomResource, KubeSchema};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::channel::{ChannelKey, ChannelRef, GameConfig, ParseChannelKeyError};
use crate::shard_config::{ShardChannel, SHARD_NAME_MAX_LEN, SHARD_NAME_PATTERN};
use crate::{
    ANNOTATION_CANCEL_REQUESTED_AT, ANNOTATION_CANCEL_REQUESTED_BY, ANNOTATION_CHANGED_BY,
    ANNOTATION_NOTE, LABEL_CHANNEL, LABEL_SHARD,
};

/// A promotion's only step today. A promotion may later run several, each with its own workflow.
pub const STEP_PROMOTE: &str = "promote";

/// Longest workflow name the coordinator may create: `promote-<promotion name>`, plus `-<step>` if a
/// promotion runs several steps. Kubernetes names are at most 63 characters.
const WORKFLOW_NAME_MAX_LEN: usize = 63;

/// Room kept for a step suffix on a workflow name, as long as `-deploy-backend`.
const LONGEST_STEP_SUFFIX_LEN: usize = "-deploy-backend".len();

/// Characters the apiserver appends to a `generateName` prefix.
const GENERATED_SUFFIX_LEN: usize = 5;

/// Longest `generateName` prefix [`Promotion::request`] produces, so that
/// `promote-` + prefix + 5 random characters + a 15-character step suffix still fits 63: 8 + 35 + 5 + 15.
const GENERATE_NAME_MAX_LEN: usize = WORKFLOW_NAME_MAX_LEN
    - WORKFLOW_NAME_PREFIX.len()
    - GENERATED_SUFFIX_LEN
    - LONGEST_STEP_SUFFIX_LEN;

/// What the coordinator puts before the Promotion's name to name its workflow.
const WORKFLOW_NAME_PREFIX: &str = "promote-";

/// A request to put a build on one channel of a shard.
///
/// Created by Friendshipper (or by hand); the spec is immutable, so to change a request, create
/// another. The coordinator validates it, runs the promote workflow, and is the only writer of its
/// status. To cancel, set the cancel annotations (see [`Promotion::cancel_request`]); the promotion
/// stays in history. Deleting one by hand also stops its workflow, but loses the record.
// Each printcolumn repeats `type_`, which clippy mistakes for a duplicated attribute.
#[allow(clippy::duplicated_attributes)]
#[derive(CustomResource, KubeSchema, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[kube(
    group = "fleet.believer.dev",
    version = "v1alpha1",
    kind = "Promotion",
    namespaced,
    shortname = "promo",
    status = "PromotionStatus",
    doc = "A request to put a build on one channel of a shard. The spec is immutable; to cancel, set the fleet.believer.dev/cancel-requested-at and cancel-requested-by annotations.",
    printcolumn(name = "Shard", type_ = "string", json_path = ".spec.shard"),
    printcolumn(
        name = "Channel",
        type_ = "string",
        json_path = ".metadata.labels.fleet\\.believer\\.dev/channel"
    ),
    printcolumn(name = "Version", type_ = "string", json_path = ".spec.version"),
    printcolumn(name = "Phase", type_ = "string", json_path = ".status.phase"),
    printcolumn(name = "Reason", type_ = "string", json_path = ".status.reason"),
    printcolumn(
        name = "Age",
        type_ = "date",
        json_path = ".metadata.creationTimestamp"
    )
)]
#[cfg_attr(test, kube(cel))]
#[x_kube(validation = Rule::new("self == oldSelf").message("a Promotion's spec is immutable; create another Promotion, or cancel this one with the cancel annotations"))]
#[serde(rename_all = "camelCase")]
pub struct PromotionSpec {
    /// The `ShardConfig` (and `Shard`) to promote on.
    #[schemars(length(min = 1, max = SHARD_NAME_MAX_LEN), regex(pattern = SHARD_NAME_PATTERN))]
    pub shard: String,

    /// One of the shard's channels.
    pub channel: ChannelRef,

    /// The build: a full 40-character lowercase commit SHA.
    #[schemars(regex(pattern = "^[0-9a-f]{40}$"))]
    pub version: String,

    /// Defaults to the channel's `gameConfig` in the `ShardConfig`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub game_config: Option<GameConfig>,

    /// Also deploy the backend to the shard. Rejected if the shard doesn't allow it.
    #[serde(default)]
    pub deploy_backend: bool,

    /// Steam only: sign the build. Unset leaves it to the promotion workflow's default (sign).
    /// Ignored for launcher channels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sign: Option<bool>,
}

impl PromotionSpec {
    pub fn channel_key(&self) -> Result<ChannelKey, ParseChannelKeyError> {
        self.channel.key()
    }

    /// The game config to promote with: the spec's, else the channel's default.
    pub fn effective_game_config(&self, channel: &ShardChannel) -> GameConfig {
        self.game_config.unwrap_or(channel.game_config)
    }
}

impl Promotion {
    /// A new promotion ready to create: `generateName`, labels and annotations set from the spec.
    /// The namespace is left to the `Api` it is created through.
    pub fn request(spec: PromotionSpec, changed_by: &str, note: Option<&str>) -> Self {
        let mut annotations =
            BTreeMap::from([(ANNOTATION_CHANGED_BY.to_string(), changed_by.to_string())]);
        if let Some(note) = note.filter(|n| !n.is_empty()) {
            annotations.insert(ANNOTATION_NOTE.to_string(), note.to_string());
        }
        Promotion {
            metadata: ObjectMeta {
                generate_name: Some(generate_name_prefix(&spec)),
                labels: Some(expected_labels(&spec)),
                annotations: Some(annotations),
                ..Default::default()
            },
            spec,
            status: None,
        }
    }

    /// The labels this promotion should carry, from its spec. The coordinator corrects any that differ.
    pub fn expected_labels(&self) -> BTreeMap<String, String> {
        expected_labels(&self.spec)
    }

    /// The phase, or `None` if the coordinator hasn't picked the promotion up yet.
    pub fn phase(&self) -> Option<PromotionPhase> {
        self.status.as_ref().and_then(|s| s.phase)
    }

    /// Whether this promotion holds its channel: `Pending` or `Running`, or not yet picked up.
    pub fn is_active(&self) -> bool {
        self.phase().is_none_or(PromotionPhase::is_active)
    }

    /// The request to cancel, if the `cancel-requested-at` annotation is present. A value that
    /// doesn't parse still counts.
    pub fn cancel_request(&self) -> Option<CancelRequest> {
        let annotations = self.metadata.annotations.as_ref()?;
        let requested_at = annotations.get(ANNOTATION_CANCEL_REQUESTED_AT)?.clone();
        let requested_by = annotations
            .get(ANNOTATION_CANCEL_REQUESTED_BY)
            .filter(|by| !by.trim().is_empty())
            .cloned();
        Some(CancelRequest {
            requested_by,
            requested_at,
        })
    }

    /// The merge patch that asks to cancel: both cancel annotations, nothing else.
    pub fn cancel_patch(requested_by: &str, requested_at: &Time) -> serde_json::Value {
        // Whole seconds, truncated; a whole-second Timestamp displays as `2026-10-29T17:45:00Z`.
        let requested_at = k8s_openapi::jiff::Timestamp::from_second(requested_at.0.as_second())
            .unwrap_or(requested_at.0);
        serde_json::json!({
            "metadata": {
                "annotations": {
                    ANNOTATION_CANCEL_REQUESTED_AT: requested_at.to_string(),
                    ANNOTATION_CANCEL_REQUESTED_BY: requested_by,
                },
            },
        })
    }
}

/// A request to cancel a [`Promotion`], from its annotations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CancelRequest {
    /// Who asked, if the annotation is set and not blank.
    pub requested_by: Option<String>,
    /// The annotation's value as written: RFC 3339 UTC when Friendshipper set it, but kept raw so a
    /// malformed value can still be shown.
    pub requested_at: String,
}

impl CancelRequest {
    /// `requested_at` parsed, if it is a valid RFC 3339 time.
    pub fn requested_at_time(&self) -> Option<Time> {
        self.requested_at.parse().ok().map(Time)
    }
}

fn expected_labels(spec: &PromotionSpec) -> BTreeMap<String, String> {
    let mut labels = BTreeMap::from([(LABEL_SHARD.to_string(), spec.shard.clone())]);
    if let Ok(key) = spec.channel_key() {
        labels.insert(LABEL_CHANNEL.to_string(), key.label_value());
    }
    labels
}

/// `<shard>-<channel kind>-`, e.g. `test-steam-`. The channel itself is in the spec and labels. A
/// shard name too long to fit is cut short (and any `-` it then ends with dropped); the schema
/// already limits it to `[a-z0-9-]`, and anything else is turned into `-`.
fn generate_name_prefix(spec: &PromotionSpec) -> String {
    let kind = spec.channel.kind.to_string();
    let shard_budget = GENERATE_NAME_MAX_LEN - kind.len() - 2;
    let shard: String = spec
        .shard
        .chars()
        .map(|c| match c.to_ascii_lowercase() {
            c @ ('a'..='z' | '0'..='9') => c,
            _ => '-',
        })
        .take(shard_budget)
        .collect();
    format!("{}-{kind}-", shard.trim_end_matches('-'))
}

/// The name of a promotion's Argo workflow: `promote-<promotion name>`. One step today; a promotion
/// may later run several, each named `promote-<promotion name>-<step>`.
pub fn workflow_name(promotion_name: &str) -> String {
    format!("{WORKFLOW_NAME_PREFIX}{promotion_name}")
}

/// Whether `version` is a full 40-character lowercase hex SHA, as `Promotion.spec.version` requires.
pub fn is_full_sha(version: &str) -> bool {
    version.len() == 40
        && version
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum PromotionPhase {
    /// Accepted and validated; the workflow isn't observed yet.
    Pending,
    /// The step's workflow exists and isn't finished.
    Running,
    Succeeded,
    /// The workflow failed or errored.
    Failed,
    /// Validation failed; nothing was submitted. Terminal.
    Rejected,
    /// Cancel requested (reason `CancelRequested`) before the workflow finished, or deleted by hand
    /// while active (reason `Deleted`).
    Cancelled,
}

impl PromotionPhase {
    /// `Pending` or `Running`: the promotion holds its channel.
    pub fn is_active(self) -> bool {
        matches!(self, PromotionPhase::Pending | PromotionPhase::Running)
    }

    /// The promotion will not change phase again.
    pub fn is_finished(self) -> bool {
        !self.is_active()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PromotionStatus {
    #[serde(default)]
    pub phase: Option<PromotionPhase>,

    /// Machine-readable, e.g. `ChannelBusy`; empty when there's nothing to explain.
    #[serde(default)]
    pub reason: String,

    #[serde(default)]
    pub message: String,

    /// The channel's version in the Shard's status when the promotion was accepted: the rollback
    /// target. Recorded then and never updated, so it is not what the channel serves now.
    #[serde(default)]
    pub previous_version: Option<String>,

    /// The game config the promotion was submitted with, resolved at acceptance: `spec.gameConfig`,
    /// else the channel's default.
    #[serde(default)]
    pub game_config: Option<GameConfig>,

    /// Filled on acceptance.
    #[serde(default)]
    pub plan: Option<PromotionPlan>,

    #[serde(default)]
    #[schemars(extend("x-kubernetes-list-type" = "map", "x-kubernetes-list-map-keys" = ["name"]))]
    pub steps: Vec<PromotionStep>,

    /// When the coordinator accepted the Promotion (phase `Pending`), not when its workflow started
    /// (that is `steps[].startedAt`). Unset until accepted; `metadata.creationTimestamp` is when it
    /// was submitted.
    #[serde(default)]
    pub started_at: Option<Time>,

    /// When the Promotion finished. For `Succeeded` and `Failed` it is the workflow's `finishedAt`
    /// (the coordinator's clock if the workflow has none); for `Rejected`, `Cancelled` and a missing
    /// workflow it is when the coordinator decided.
    #[serde(default)]
    pub finished_at: Option<Time>,

    #[serde(default)]
    #[schemars(extend("x-kubernetes-list-type" = "map", "x-kubernetes-list-map-keys" = ["type"]))]
    pub conditions: Vec<Condition>,
}

/// What the promotion will change.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PromotionPlan {
    #[serde(default)]
    pub backend: Option<BackendPlan>,
}

/// The backend side of a plan: the build's backend version against what the shard runs now.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BackendPlan {
    #[serde(default)]
    pub deploy: bool,

    #[serde(default)]
    pub parts: Vec<BackendPartChange>,

    /// When the `from` values were read: the healthcheck read at acceptance, or, if that read
    /// failed, the Shard's last successful backend observation (`backend.observedAt`).
    #[serde(default)]
    pub observed_at: Option<Time>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BackendPartChange {
    /// `platform`, `portal`, `content` or `config`.
    pub name: String,
    /// The part's version from a fresh healthcheck read at acceptance, or from the Shard's last
    /// successful backend observation if that read failed (see [`BackendPlan::observed_at`]); empty
    /// if unknown. Recorded then and never updated.
    #[serde(default)]
    pub from: String,
    /// The part's version in the build's manifest: what a backend deploy would install.
    #[serde(default)]
    pub to: String,
}

impl BackendPartChange {
    pub fn changes(&self) -> bool {
        self.from != self.to
    }
}

/// One step of a promotion and the workflow that runs it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PromotionStep {
    /// The step, e.g. `promote`.
    pub name: String,

    /// The Argo workflow's name: `promote-<promotion name>`, or `promote-<promotion name>-<step>` if a
    /// promotion runs several steps.
    #[serde(default)]
    pub workflow: String,

    /// Mirrors the workflow's phase.
    #[serde(default)]
    pub phase: String,

    /// The workflow's `startedAt`.
    #[serde(default)]
    pub started_at: Option<Time>,

    /// The workflow's `finishedAt`.
    #[serde(default)]
    pub finished_at: Option<Time>,

    /// The workflow's message on failure.
    #[serde(default)]
    pub message: String,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::channel::ChannelKind;

    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    fn steam_spec() -> PromotionSpec {
        PromotionSpec {
            shard: "test".into(),
            channel: ChannelRef::from(&ChannelKey::steam("test")),
            version: SHA.into(),
            game_config: None,
            deploy_backend: true,
            sign: Some(true),
        }
    }

    #[test]
    fn new_sets_name_labels_and_annotations() {
        let promo = Promotion::request(steam_spec(), "someone", Some("hotfix"));
        assert_eq!(promo.metadata.generate_name.as_deref(), Some("test-steam-"));
        let labels = promo.metadata.labels.as_ref().unwrap();
        assert_eq!(labels[LABEL_SHARD], "test");
        assert_eq!(labels[LABEL_CHANNEL], "steam.test");
        let annotations = promo.metadata.annotations.as_ref().unwrap();
        assert_eq!(annotations[ANNOTATION_CHANGED_BY], "someone");
        assert_eq!(annotations[ANNOTATION_NOTE], "hotfix");

        let promo = Promotion::request(steam_spec(), "someone", Some(""));
        assert!(!promo
            .metadata
            .annotations
            .unwrap()
            .contains_key(ANNOTATION_NOTE));
    }

    #[test]
    fn generate_name_is_dns_safe_and_bounded() {
        let mut spec = steam_spec();
        assert_eq!(generate_name_prefix(&spec), "test-steam-");
        spec.channel = ChannelRef::from(&ChannelKey::launcher("Stable_1.0"));
        assert_eq!(generate_name_prefix(&spec), "test-launcher-");

        // The bound: promote- (8) + prefix (35) + 5 random + -deploy-backend (15) = 63.
        assert_eq!(GENERATE_NAME_MAX_LEN, 35);

        // The longest shard name the schema allows, cut where a `-` would end it.
        spec.shard = format!("{}-{}", "a".repeat(24), "b".repeat(38));
        let prefix = generate_name_prefix(&spec);
        assert_eq!(prefix, format!("{}-launcher-", "a".repeat(24)));
        spec.shard = "a".repeat(SHARD_NAME_MAX_LEN);
        let prefix = generate_name_prefix(&spec);
        assert_eq!(prefix.len(), GENERATE_NAME_MAX_LEN, "{prefix}");
        assert!(prefix.ends_with("-launcher-"));
        let longest = format!(
            "{}-deploy-backend",
            workflow_name(&format!("{prefix}abcde"))
        );
        assert_eq!(longest.len(), 63, "{longest}");
    }

    #[test]
    fn unpicked_promotions_are_active() {
        let mut promo = Promotion::request(steam_spec(), "someone", None);
        assert_eq!(promo.phase(), None);
        assert!(promo.is_active());

        promo.status = Some(PromotionStatus {
            phase: Some(PromotionPhase::Rejected),
            ..Default::default()
        });
        assert!(!promo.is_active());
        assert!(PromotionPhase::Running.is_active());
        assert!(PromotionPhase::Cancelled.is_finished());
    }

    #[test]
    fn game_config_falls_back_to_the_channel() {
        let channel = ShardChannel {
            kind: ChannelKind::Steam,
            metadata_path: None,
            branch: Some("test".into()),
            game_config: GameConfig::Shipping,
        };
        let mut spec = steam_spec();
        assert_eq!(spec.effective_game_config(&channel), GameConfig::Shipping);
        spec.game_config = Some(GameConfig::Test);
        assert_eq!(spec.effective_game_config(&channel), GameConfig::Test);
    }

    #[test]
    fn full_sha() {
        assert!(is_full_sha(SHA));
        assert!(!is_full_sha(&SHA[..39]));
        assert!(!is_full_sha(&SHA.to_uppercase()));
        assert!(!is_full_sha(&format!("{}g", &SHA[..39])));
    }

    #[test]
    fn spec_is_immutable() {
        let promo = Promotion::request(steam_spec(), "someone", None);
        promo.validate_cel().unwrap();
        promo.validate_cel_update(&promo).unwrap();

        let mut changed = promo.clone();
        changed.spec.sign = Some(false);
        assert!(changed.validate_cel_update(&promo).is_err());

        // Status changes are fine.
        let mut with_status = promo.clone();
        with_status.status = Some(PromotionStatus {
            phase: Some(PromotionPhase::Running),
            ..Default::default()
        });
        with_status.validate_cel_update(&promo).unwrap();

        // So are label and annotation changes, e.g. the coordinator correcting labels.
        let mut relabelled = promo.clone();
        let metadata = &mut relabelled.metadata;
        metadata
            .labels
            .get_or_insert_default()
            .insert(LABEL_CHANNEL.into(), "steam.other".into());
        metadata
            .annotations
            .get_or_insert_default()
            .insert(ANNOTATION_NOTE.into(), "edited".into());
        relabelled.validate_cel_update(&promo).unwrap();
    }

    #[test]
    fn deserializes_a_full_example() {
        let promo: Promotion = serde_json::from_value(json!({
            "apiVersion": "fleet.believer.dev/v1alpha1",
            "kind": "Promotion",
            "metadata": {"name": "test-steam-abcde", "namespace": "fleet-system"},
            "spec": {
                "shard": "test",
                "channel": {"kind": "steam", "branch": "test"},
                "version": SHA,
                "gameConfig": "Test",
                "deployBackend": true,
                "sign": true,
            },
            "status": {
                "phase": "Running",
                "reason": "",
                "message": "",
                "previousVersion": "fedcba9876543210fedcba9876543210fedcba98",
                "gameConfig": "Test",
                "plan": {"backend": {"deploy": true, "parts": [
                    {"name": "content", "from": "a_b_c_1", "to": "a_b_c_2"},
                    {"name": "portal", "from": "a_b_c_3", "to": "a_b_c_3"},
                ], "observedAt": "2026-01-02T03:04:00Z"}},
                "steps": [{"name": "promote", "workflow": "promote-test-steam-abcde",
                           "phase": "Running", "startedAt": "2026-01-02T03:04:05Z",
                           "finishedAt": null, "message": ""}],
                "startedAt": "2026-01-02T03:04:05Z",
                "finishedAt": null,
                "conditions": [],
            },
        }))
        .unwrap();

        assert_eq!(promo.spec.channel_key().unwrap(), ChannelKey::steam("test"));
        let status = promo.status.as_ref().unwrap();
        assert_eq!(status.phase, Some(PromotionPhase::Running));
        assert_eq!(status.game_config, Some(GameConfig::Test));
        let backend = status.plan.as_ref().unwrap().backend.as_ref().unwrap();
        assert_eq!(
            backend.observed_at.as_ref().unwrap().0.to_string(),
            "2026-01-02T03:04:00Z"
        );
        let parts = &backend.parts;
        assert!(parts[0].changes());
        assert!(!parts[1].changes());
        assert_eq!(status.steps[0].workflow, workflow_name("test-steam-abcde"));
    }

    #[test]
    fn cancel_request_is_keyed_on_requested_at() {
        let mut promo = Promotion::request(steam_spec(), "someone", None);
        assert_eq!(promo.cancel_request(), None);

        // Who without when is not a request.
        let annotations = promo.metadata.annotations.get_or_insert_default();
        annotations.insert(ANNOTATION_CANCEL_REQUESTED_BY.into(), "someone".into());
        assert_eq!(promo.cancel_request(), None);

        let at = Time(k8s_openapi::jiff::Timestamp::from_millisecond(1_793_295_900_750).unwrap());
        let patch = Promotion::cancel_patch("someone", &at);
        assert_eq!(
            patch,
            json!({"metadata": {"annotations": {
                ANNOTATION_CANCEL_REQUESTED_AT: "2026-10-29T17:45:00Z",
                ANNOTATION_CANCEL_REQUESTED_BY: "someone",
            }}})
        );
        let set = &patch["metadata"]["annotations"];
        let annotations = promo.metadata.annotations.get_or_insert_default();
        annotations.insert(
            ANNOTATION_CANCEL_REQUESTED_AT.into(),
            set[ANNOTATION_CANCEL_REQUESTED_AT].as_str().unwrap().into(),
        );
        let request = promo.cancel_request().unwrap();
        assert_eq!(request.requested_by.as_deref(), Some("someone"));
        assert_eq!(
            request.requested_at_time().unwrap().0.to_string(),
            "2026-10-29T17:45:00Z"
        );

        // A malformed time or a blank name still cancels.
        let annotations = promo.metadata.annotations.get_or_insert_default();
        annotations.insert(ANNOTATION_CANCEL_REQUESTED_AT.into(), "yesterday".into());
        annotations.insert(ANNOTATION_CANCEL_REQUESTED_BY.into(), " ".into());
        let request = promo.cancel_request().unwrap();
        assert_eq!(request.requested_at, "yesterday");
        assert_eq!(request.requested_at_time(), None);
        assert_eq!(request.requested_by, None);
    }

    #[test]
    fn spec_defaults() {
        let spec: PromotionSpec = serde_json::from_value(json!({
            "shard": "test",
            "channel": {"kind": "launcher", "metadataPath": "latest"},
            "version": SHA,
        }))
        .unwrap();
        assert!(!spec.deploy_backend);
        assert_eq!(spec.sign, None);
        assert_eq!(spec.game_config, None);
    }
}
