//! Fleet objects (`ShardConfig`, `Shard`, `Promotion`, `FleetRelease`) in the `fleet-system`
//! namespace.
//!
//! Friendshipper reads them all, creates `Promotion`s and asks to cancel them, edits a shard's Fleet
//! settings (`Shard.spec`), and warms, pins, overrides and retires `FleetRelease`s; the coordinator
//! does the rest. See the `fleet-api` crate for the contract.

use anyhow::anyhow;
use fleet_api::{
    is_full_sha, is_valid_map_id, release_name, ChannelKey, FleetRelease, FleetReleaseDesiredPhase,
    FleetReleasePhase, GameConfig, Promotion, PromotionSpec, Shard, ShardConfig, ShardSpec, Sizing,
    SizingOverride, ANNOTATION_CHANGED_BY, DEFAULT_NAMESPACE, LABEL_CHANNEL, LABEL_SHARD,
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Time;
use k8s_openapi::jiff::Timestamp;
use kube::api::{DeleteParams, ListParams, Patch, PatchParams, PostParams, Preconditions};
use kube::{Api, Client};
use tracing::{info, instrument};

use crate::clients::kube::KubeClient;
use crate::types::errors::CoreError;

/// Why Friendshipper refused to create a `Promotion`. The coordinator runs the same checks; these
/// exist only so the user sees the problem before a pointless `Rejected` object is created.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PromotionCheckError {
    #[error("{0:?} is not a full 40-character lowercase commit SHA")]
    InvalidVersion(String),
    #[error("the promotion's channel is invalid: {0}")]
    InvalidChannel(String),
    #[error("shard {0:?} has no ShardConfig")]
    UnknownShard(String),
    #[error("shard {shard:?} has no channel {channel}")]
    UnknownChannel { shard: String, channel: String },
    #[error("a {channel} channel only takes Test or Shipping builds, not {game_config:?}")]
    InvalidGameConfig {
        channel: String,
        game_config: GameConfig,
    },
    #[error("shard {0:?} does not allow promotions to deploy the backend")]
    BackendDeployNotAllowed(String),
    #[error("{channel} on {shard} is busy with promotion {promotion}")]
    ChannelBusy {
        shard: String,
        channel: String,
        promotion: String,
    },
}

/// Checks a promotion request against its shard's config and the promotions that already exist.
///
/// `config` is the `ShardConfig` named `spec.shard`, if there is one. `promotions` may hold any
/// promotions; only active ones for the same shard and channel count.
pub fn check_promotion(
    spec: &PromotionSpec,
    config: Option<&ShardConfig>,
    promotions: &[Promotion],
) -> Result<(), PromotionCheckError> {
    if !is_full_sha(&spec.version) {
        return Err(PromotionCheckError::InvalidVersion(spec.version.clone()));
    }

    let key = spec
        .channel_key()
        .map_err(|e| PromotionCheckError::InvalidChannel(e.to_string()))?;

    let config = config.ok_or_else(|| PromotionCheckError::UnknownShard(spec.shard.clone()))?;
    let channel = config
        .spec
        .channel(&key)
        .ok_or_else(|| PromotionCheckError::UnknownChannel {
            shard: spec.shard.clone(),
            channel: key.to_string(),
        })?;

    let game_config = spec.effective_game_config(channel);
    if !game_config.allowed_on(key.kind()) {
        return Err(PromotionCheckError::InvalidGameConfig {
            channel: key.kind().to_string(),
            game_config,
        });
    }

    if spec.deploy_backend && !config.spec.promotion.allow_deploy_backend {
        return Err(PromotionCheckError::BackendDeployNotAllowed(
            spec.shard.clone(),
        ));
    }

    if let Some(busy) = promotions
        .iter()
        .find(|p| p.is_active() && targets(p, &spec.shard, &key))
    {
        return Err(PromotionCheckError::ChannelBusy {
            shard: spec.shard.clone(),
            channel: key.to_string(),
            promotion: busy.metadata.name.clone().unwrap_or_default(),
        });
    }

    Ok(())
}

fn targets(promotion: &Promotion, shard: &str, key: &ChannelKey) -> bool {
    promotion.spec.shard == shard && promotion.spec.channel_key().as_ref() == Ok(key)
}

/// Why Friendshipper refused to create or change a `FleetRelease`.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum FleetReleaseCheckError {
    #[error("{0:?} is not a full 40-character lowercase commit SHA")]
    InvalidVersion(String),
    #[error("shard {0:?} has no ShardConfig")]
    UnknownShard(String),
    #[error("shard {0:?} can't have Fleets (its ShardConfig has no fleets section)")]
    NoFleets(String),
    #[error("Fleets are turned off on shard {0:?}")]
    FleetsDisabled(String),
    #[error("a release can't be named for this build: {0}")]
    Name(String),
    #[error("release {name} already exists for this build ({phase})")]
    AlreadyExists { name: String, phase: String },
    #[error("release {name} belongs to another build ({version}); the short SHAs collide")]
    NameCollision { name: String, version: String },
    #[error("release {0} is already retiring")]
    AlreadyRetiring(String),
    #[error("release {0} is still retiring; try again in a few minutes")]
    StillRetiring(String),
    #[error("the retired release {0} is still being deleted; try again shortly")]
    StillDeleting(String),
}

/// What warming a build does, once [`check_warm`] allows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WarmPlan {
    /// Create the release with this name.
    Create(String),
    /// The build's release is Retired, which is final: delete it (only the object with `uid`),
    /// then create a fresh one with the same name.
    Recreate { name: String, uid: Option<String> },
}

impl WarmPlan {
    /// The name of the release that will be created.
    pub fn name(&self) -> &str {
        match self {
            WarmPlan::Create(name) | WarmPlan::Recreate { name, .. } => name,
        }
    }
}

/// How long warming waits for a deleted Retired release to go before giving up. The coordinator
/// removes its finalizer once it has read every cluster.
const RECREATE_WAIT: std::time::Duration = std::time::Duration::from_secs(60);
const RECREATE_POLL: std::time::Duration = std::time::Duration::from_secs(1);

/// Checks a request to warm `version` on `shard` (create its `FleetRelease`). `existing` is the
/// release already named [`release_name`] for it, if any. A Retired one is recreated; one still on
/// its way there (retiring, or being deleted) is refused until it gets there.
pub fn check_warm(
    shard: &str,
    version: &str,
    config: Option<&ShardConfig>,
    current: Option<&Shard>,
    existing: Option<&FleetRelease>,
) -> Result<WarmPlan, FleetReleaseCheckError> {
    if !is_full_sha(version) {
        return Err(FleetReleaseCheckError::InvalidVersion(version.to_string()));
    }
    let config = config.ok_or_else(|| FleetReleaseCheckError::UnknownShard(shard.to_string()))?;
    if config.spec.fleets.is_none() {
        return Err(FleetReleaseCheckError::NoFleets(shard.to_string()));
    }
    if !current.is_some_and(|s| s.spec.fleets_enabled) {
        return Err(FleetReleaseCheckError::FleetsDisabled(shard.to_string()));
    }
    let name =
        release_name(shard, version).map_err(|e| FleetReleaseCheckError::Name(e.to_string()))?;
    if let Some(existing) = existing {
        if existing.spec.version != version {
            return Err(FleetReleaseCheckError::NameCollision {
                name,
                version: existing.spec.version.clone(),
            });
        }
        if existing.metadata.deletion_timestamp.is_some() {
            return Err(FleetReleaseCheckError::StillDeleting(name));
        }
        if existing.phase() == Some(FleetReleasePhase::Retired) {
            let uid = existing.metadata.uid.clone();
            return Ok(WarmPlan::Recreate { name, uid });
        }
        if existing.phase() == Some(FleetReleasePhase::Retiring)
            || existing.spec.phase == FleetReleaseDesiredPhase::Retired
        {
            return Err(FleetReleaseCheckError::StillRetiring(name));
        }
        let phase = existing
            .phase()
            .map_or("not picked up yet".to_string(), |p| format!("{p:?}"));
        return Err(FleetReleaseCheckError::AlreadyExists { name, phase });
    }
    Ok(WarmPlan::Create(name))
}

/// Why creating `name` was refused with a conflict: the release that now holds the name, checked as
/// [`check_warm`] would. One already gone again, or one that's now Retired, is still on its way out.
fn create_conflict(
    name: &str,
    shard: &str,
    version: &str,
    config: Option<&ShardConfig>,
    current: Option<&Shard>,
    existing: Option<&FleetRelease>,
) -> FleetReleaseCheckError {
    match check_warm(shard, version, config, current, existing) {
        Err(e) => e,
        Ok(_) => FleetReleaseCheckError::StillDeleting(name.to_string()),
    }
}

/// Checks a request to retire a release. Without `force`, cluster mode waits for the Allocated
/// servers' matches to end before removing the Fleets; with it, it ends them.
pub fn check_retire(release: &FleetRelease, force: bool) -> Result<(), FleetReleaseCheckError> {
    let name = release.metadata.name.clone().unwrap_or_default();
    // Forcing a release that is already retiring is how a stuck retirement is pushed through.
    if release.spec.phase == FleetReleaseDesiredPhase::Retired && (!force || release.spec.force) {
        return Err(FleetReleaseCheckError::AlreadyRetiring(name));
    }
    Ok(())
}

/// Why Friendshipper refused a change to a shard's Fleet settings. The apiserver checks the sizing
/// rules within each object; these are the checks it can't make.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ShardSpecCheckError {
    #[error("shard {0:?} can't have Fleets (its ShardConfig has no fleets section)")]
    NoFleets(String),
    #[error("{0:?} is not a map ID")]
    InvalidMapId(String),
    #[error("{0:?} is not one of the shard's Fleet clusters")]
    UnknownCluster(String),
    #[error("{at}: maxReplicas {max} is above the shard's limit of {limit} per fleet")]
    AboveLimit { at: String, max: i32, limit: i32 },
}

/// Checks Fleet settings, or a release's overrides, against the shard's `ShardConfig`.
pub fn check_shard_spec(
    shard: &str,
    spec: &ShardSpec,
    config: Option<&ShardConfig>,
) -> Result<(), ShardSpecCheckError> {
    let Some(fleets) = config.and_then(|c| c.spec.fleets.as_ref()) else {
        // Without a fleets section only Fleets off is allowed, with the rest of the spec kept as it
        // is: there are no clusters or limit to check it against, and turning Fleets off must stay
        // possible after the ShardConfig loses its fleets section.
        if spec.fleets_enabled {
            return Err(ShardSpecCheckError::NoFleets(shard.to_string()));
        }
        return Ok(());
    };
    let mut map_ids: Vec<&str> = spec.maps.iter().map(String::as_str).collect();
    map_ids.extend(spec.live.maps.keys().map(String::as_str));
    if let Some(bad) = map_ids.into_iter().find(|m| !is_valid_map_id(m)) {
        return Err(ShardSpecCheckError::InvalidMapId(bad.to_string()));
    }
    let mut sizings: Vec<(String, &Sizing)> = Vec::new();
    if let Some(warm) = &spec.warm {
        sizings.push(("warm".into(), warm));
    }
    if let Some(default) = &spec.live.default {
        sizings.push(("live default".into(), default));
    }
    for (map_id, sizing) in &spec.live.maps {
        sizings.push((format!("live {map_id}"), sizing));
    }
    check_overrides(&spec.live.overrides, config)?;
    check_limit(sizings, fleets.limits.max_replicas_per_fleet)
}

/// Checks per (map, cluster) overrides: real clusters, map IDs, and the shard's replica limit.
pub fn check_overrides(
    overrides: &[SizingOverride],
    config: Option<&ShardConfig>,
) -> Result<(), ShardSpecCheckError> {
    let fleets = config.and_then(|c| c.spec.fleets.as_ref());
    for o in overrides {
        if !is_valid_map_id(&o.map_id) {
            return Err(ShardSpecCheckError::InvalidMapId(o.map_id.clone()));
        }
        if !fleets.is_some_and(|f| f.cluster(&o.cluster).is_some()) {
            return Err(ShardSpecCheckError::UnknownCluster(o.cluster.clone()));
        }
    }
    let limit = fleets.and_then(|f| f.limits.max_replicas_per_fleet);
    check_limit(
        overrides
            .iter()
            .map(|o| (format!("{} in {}", o.map_id, o.cluster), &o.sizing))
            .collect(),
        limit,
    )
}

fn check_limit(
    sizings: Vec<(String, &Sizing)>,
    limit: Option<i32>,
) -> Result<(), ShardSpecCheckError> {
    let Some(limit) = limit else {
        return Ok(());
    };
    for (at, sizing) in sizings {
        if let Some(max) = sizing
            .max_replicas
            .filter(|&max| max > limit && !sizing.is_parked())
        {
            return Err(ShardSpecCheckError::AboveLimit { at, max, limit });
        }
    }
    Ok(())
}

/// The JSON patch that replaces a shard's spec and records who did it. It fails, rather than
/// overwriting someone else's edit, unless the Shard's spec is still at `generation`. It tests
/// `generation`, not `resourceVersion`: the coordinator's status writes change the resource version
/// on every observe pass, but only spec changes bump the generation.
pub fn shard_spec_patch(
    current: &Shard,
    generation: i64,
    spec: &ShardSpec,
    changed_by: &str,
) -> Result<json_patch::Patch, CoreError> {
    let mut ops = vec![
        serde_json::json!({"op": "test", "path": "/metadata/generation", "value": generation}),
        serde_json::json!({"op": "add", "path": "/spec", "value": spec}),
    ];
    ops.push(changed_by_op(
        current.metadata.annotations.is_some(),
        changed_by,
    ));
    Ok(serde_json::from_value(serde_json::Value::Array(ops))?)
}

/// The JSON patch that replaces a release's overrides, as [`shard_spec_patch`] does for a shard.
pub fn release_overrides_patch(
    current: &FleetRelease,
    generation: i64,
    overrides: &[SizingOverride],
    changed_by: &str,
) -> Result<json_patch::Patch, CoreError> {
    let ops = vec![
        serde_json::json!({"op": "test", "path": "/metadata/generation", "value": generation}),
        serde_json::json!({"op": "add", "path": "/spec/overrides", "value": overrides}),
        changed_by_op(current.metadata.annotations.is_some(), changed_by),
    ];
    Ok(serde_json::from_value(serde_json::Value::Array(ops))?)
}

/// Sets the `changed-by` annotation in a JSON patch: one key if the object has annotations, else
/// the whole map (JSON patch can't add a key under a missing parent).
fn changed_by_op(has_annotations: bool, changed_by: &str) -> serde_json::Value {
    if has_annotations {
        // `/` in a key is written `~1` in a JSON pointer.
        let path = format!(
            "/metadata/annotations/{}",
            ANNOTATION_CHANGED_BY.replace('~', "~0").replace('/', "~1")
        );
        serde_json::json!({"op": "add", "path": path, "value": changed_by})
    } else {
        serde_json::json!({"op": "add", "path": "/metadata/annotations", "value": {ANNOTATION_CHANGED_BY: changed_by}})
    }
}

/// The apiserver's whole message when a JSON patch can't be applied, e.g. a `test` operation fails.
/// Validation failures are 422 too, but name the object and field instead.
const PATCH_NOT_APPLIED: &str = "the server rejected our request due to an error in our request";

/// Whether an API error is a JSON patch failing to apply. The patches here are built against the
/// object just fetched, so that means their `test` of the generation failed.
fn is_failed_test(code: u16, message: &str) -> bool {
    code == 422 && message == PATCH_NOT_APPLIED
}

/// Label selector for promotions, narrowed to a shard and, within it, a channel.
pub fn promotion_selector(shard: Option<&str>, channel: Option<&ChannelKey>) -> String {
    let mut terms = vec![];
    if let Some(shard) = shard {
        terms.push(format!("{LABEL_SHARD}={shard}"));
    }
    if let Some(channel) = channel {
        terms.push(format!("{LABEL_CHANNEL}={}", channel.label_value()));
    }
    terms.join(",")
}

impl KubeClient {
    async fn fleet_api<K>(&self) -> Result<Api<K>, CoreError>
    where
        K: kube::Resource<Scope = kube::core::NamespaceResourceScope>,
        <K as kube::Resource>::DynamicType: Default,
    {
        let client = Client::try_from(self.kubeconfig().await?)?;
        Ok(Api::namespaced(client, DEFAULT_NAMESPACE))
    }

    #[instrument(skip(self))]
    pub async fn list_shard_configs(&self) -> Result<Vec<ShardConfig>, CoreError> {
        let api: Api<ShardConfig> = self.fleet_api().await?;
        match api.list(&ListParams::default()).await {
            Ok(list) => Ok(list.items),
            Err(e) => Err(self.handle_kube_error(e).await),
        }
    }

    #[instrument(skip(self))]
    pub async fn get_shard_config(&self, name: &str) -> Result<Option<ShardConfig>, CoreError> {
        let api: Api<ShardConfig> = self.fleet_api().await?;
        match api.get_opt(name).await {
            Ok(config) => Ok(config),
            Err(e) => Err(self.handle_kube_error(e).await),
        }
    }

    #[instrument(skip(self))]
    pub async fn list_shards(&self) -> Result<Vec<Shard>, CoreError> {
        let api: Api<Shard> = self.fleet_api().await?;
        match api.list(&ListParams::default()).await {
            Ok(list) => Ok(list.items),
            Err(e) => Err(self.handle_kube_error(e).await),
        }
    }

    #[instrument(skip(self))]
    pub async fn get_shard(&self, name: &str) -> Result<Option<Shard>, CoreError> {
        let api: Api<Shard> = self.fleet_api().await?;
        match api.get_opt(name).await {
            Ok(shard) => Ok(shard),
            Err(e) => Err(self.handle_kube_error(e).await),
        }
    }

    /// Promotions, optionally for one shard and, within it, one channel.
    #[instrument(skip(self))]
    pub async fn list_promotions(
        &self,
        shard: Option<&str>,
        channel: Option<&ChannelKey>,
    ) -> Result<Vec<Promotion>, CoreError> {
        let api: Api<Promotion> = self.fleet_api().await?;
        let lp = ListParams::default().labels(&promotion_selector(shard, channel));
        match api.list(&lp).await {
            Ok(list) => Ok(list.items),
            Err(e) => Err(self.handle_kube_error(e).await),
        }
    }

    /// Creates a `Promotion` after the cheap pre-checks. The coordinator validates it again.
    #[instrument(skip(self))]
    pub async fn create_promotion(
        &self,
        spec: PromotionSpec,
        changed_by: &str,
        note: Option<&str>,
    ) -> Result<Promotion, CoreError> {
        let key = spec
            .channel_key()
            .map_err(|e| CoreError::Input(anyhow!("invalid channel: {e}")))?;
        let config = self.get_shard_config(&spec.shard).await?;
        let existing = self.list_promotions(Some(&spec.shard), Some(&key)).await?;
        check_promotion(&spec, config.as_ref(), &existing)
            .map_err(|e| CoreError::Input(anyhow!(e)))?;

        let api: Api<Promotion> = self.fleet_api().await?;
        let promotion = Promotion::request(spec, changed_by, note);
        match api.create(&PostParams::default(), &promotion).await {
            Ok(created) => {
                info!(
                    "Created promotion {:?} for {key}",
                    created.metadata.name.as_deref().unwrap_or_default()
                );
                Ok(created)
            }
            Err(e) => Err(self.handle_kube_error(e).await),
        }
    }

    /// Fleet releases, optionally for one shard.
    #[instrument(skip(self))]
    pub async fn list_fleet_releases(
        &self,
        shard: Option<&str>,
    ) -> Result<Vec<FleetRelease>, CoreError> {
        let api: Api<FleetRelease> = self.fleet_api().await?;
        let selector = shard.map_or(String::new(), |s| format!("{LABEL_SHARD}={s}"));
        match api.list(&ListParams::default().labels(&selector)).await {
            Ok(list) => Ok(list.items),
            Err(e) => Err(self.handle_kube_error(e).await),
        }
    }

    async fn get_fleet_release(&self, name: &str) -> Result<FleetRelease, CoreError> {
        let api: Api<FleetRelease> = self.fleet_api().await?;
        match api.get(name).await {
            Ok(release) => Ok(release),
            Err(e) => Err(self.handle_kube_error(e).await),
        }
    }

    /// Warms a build on a shard: creates its `FleetRelease` in `Warm`, after the checks in
    /// [`check_warm`]. A Retired release of the build is deleted first, and its deletion waited for
    /// (up to [`RECREATE_WAIT`]).
    #[instrument(skip(self))]
    pub async fn create_fleet_release(
        &self,
        shard: &str,
        version: &str,
        changed_by: &str,
    ) -> Result<FleetRelease, CoreError> {
        let api: Api<FleetRelease> = self.fleet_api().await?;
        let (config, current) =
            futures::try_join!(self.get_shard_config(shard), self.get_shard(shard))?;
        let existing = match release_name(shard, version) {
            Ok(name) => match api.get_opt(&name).await {
                Ok(existing) => existing,
                Err(e) => return Err(self.handle_kube_error(e).await),
            },
            Err(_) => None,
        };
        let plan = check_warm(
            shard,
            version,
            config.as_ref(),
            current.as_ref(),
            existing.as_ref(),
        )
        .map_err(|e| CoreError::Input(anyhow!(e)))?;
        if let WarmPlan::Recreate { name, uid } = &plan {
            self.delete_retired_release(&api, name, uid.as_deref(), changed_by)
                .await?;
        }
        let release = FleetRelease::request(shard, version, changed_by)
            .map_err(|e| CoreError::Input(anyhow!(e)))?;
        match api.create(&PostParams::default(), &release).await {
            Ok(created) => {
                info!(
                    "Created fleet release {:?} for {changed_by}",
                    created.metadata.name.as_deref().unwrap_or_default()
                );
                Ok(created)
            }
            // Someone made one with this name since the check: say what it is, not the raw 409.
            Err(kube::Error::Api(ae)) if ae.code == 409 => {
                let existing = api.get_opt(plan.name()).await.ok().flatten();
                Err(CoreError::Input(anyhow!(create_conflict(
                    plan.name(),
                    shard,
                    version,
                    config.as_ref(),
                    current.as_ref(),
                    existing.as_ref(),
                ))))
            }
            Err(e) => Err(self.handle_kube_error(e).await),
        }
    }

    /// Deletes a Retired release and waits until it's gone. With `uid`, only that object is deleted,
    /// never a newer one with the same name.
    async fn delete_retired_release(
        &self,
        api: &Api<FleetRelease>,
        name: &str,
        uid: Option<&str>,
        changed_by: &str,
    ) -> Result<(), CoreError> {
        let params = DeleteParams {
            preconditions: uid.map(|uid| Preconditions {
                uid: Some(uid.to_string()),
                resource_version: None,
            }),
            ..DeleteParams::default()
        };
        match api.delete(name, &params).await {
            Ok(_) => {
                info!("Deleted retired fleet release {name:?} to warm it afresh, for {changed_by}")
            }
            Err(kube::Error::Api(ae)) if ae.code == 404 => return Ok(()),
            Err(e) => return Err(self.handle_kube_error(e).await),
        }
        let deadline = tokio::time::Instant::now() + RECREATE_WAIT;
        loop {
            match api.get_opt(name).await {
                Ok(None) => return Ok(()),
                // Someone else already made a fresh one; creating ours reports the conflict.
                Ok(Some(r)) if uid.is_some() && r.metadata.uid.as_deref() != uid => return Ok(()),
                Ok(Some(_)) => {}
                Err(e) => return Err(self.handle_kube_error(e).await),
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(CoreError::Input(anyhow!(
                    FleetReleaseCheckError::StillDeleting(name.to_string())
                )));
            }
            tokio::time::sleep(RECREATE_POLL).await;
        }
    }

    #[instrument(skip(self))]
    pub async fn pin_fleet_release(
        &self,
        name: &str,
        pinned: bool,
        changed_by: &str,
    ) -> Result<FleetRelease, CoreError> {
        let api: Api<FleetRelease> = self.fleet_api().await?;
        let patch = FleetRelease::pin_patch(pinned, changed_by);
        match api
            .patch(name, &PatchParams::default(), &Patch::Merge(&patch))
            .await
        {
            Ok(release) => {
                info!("Set pinned={pinned} on fleet release {name}");
                Ok(release)
            }
            Err(e) => Err(self.handle_kube_error(e).await),
        }
    }

    /// Retires a release, after [`check_retire`] on its current state.
    #[instrument(skip(self))]
    pub async fn retire_fleet_release(
        &self,
        name: &str,
        force: bool,
        changed_by: &str,
    ) -> Result<FleetRelease, CoreError> {
        let release = self.get_fleet_release(name).await?;
        check_retire(&release, force).map_err(|e| CoreError::Input(anyhow!(e)))?;
        let api: Api<FleetRelease> = self.fleet_api().await?;
        let patch = FleetRelease::retire_patch(force, changed_by);
        match api
            .patch(name, &PatchParams::default(), &Patch::Merge(&patch))
            .await
        {
            Ok(release) => {
                info!("Retiring fleet release {name} (force={force})");
                Ok(release)
            }
            Err(e) => Err(self.handle_kube_error(e).await),
        }
    }

    /// Replaces a release's per (map, cluster) overrides, if its spec is still at `generation`.
    #[instrument(skip(self, overrides))]
    pub async fn set_fleet_release_overrides(
        &self,
        name: &str,
        generation: i64,
        overrides: Vec<SizingOverride>,
        changed_by: &str,
    ) -> Result<FleetRelease, CoreError> {
        let release = self.get_fleet_release(name).await?;
        let config = self.get_shard_config(&release.spec.shard).await?;
        check_overrides(&overrides, config.as_ref()).map_err(|e| CoreError::Input(anyhow!(e)))?;
        let patch = release_overrides_patch(&release, generation, &overrides, changed_by)?;
        let api: Api<FleetRelease> = self.fleet_api().await?;
        match api
            .patch(name, &PatchParams::default(), &Patch::Json::<()>(patch))
            .await
        {
            Ok(release) => {
                info!(
                    "Set {} override(s) on fleet release {name}",
                    overrides.len()
                );
                Ok(release)
            }
            Err(e) => Err(self.stale_edit_error(e, &format!("Release {name}")).await),
        }
    }

    /// Replaces a shard's Fleet settings, if the Shard's spec is still at `generation`.
    #[instrument(skip(self, spec))]
    pub async fn set_shard_spec(
        &self,
        name: &str,
        generation: i64,
        spec: ShardSpec,
        changed_by: &str,
    ) -> Result<Shard, CoreError> {
        let (config, current) =
            futures::try_join!(self.get_shard_config(name), self.get_shard(name))?;
        let current =
            current.ok_or_else(|| CoreError::Input(anyhow!("no Shard named {name:?}")))?;
        check_shard_spec(name, &spec, config.as_ref()).map_err(|e| CoreError::Input(anyhow!(e)))?;
        let patch = shard_spec_patch(&current, generation, &spec, changed_by)?;
        let api: Api<Shard> = self.fleet_api().await?;
        match api
            .patch(name, &PatchParams::default(), &Patch::Json::<()>(patch))
            .await
        {
            Ok(shard) => {
                info!(
                    "Updated the Fleet settings of shard {name} (fleets enabled: {})",
                    spec.fleets_enabled
                );
                Ok(shard)
            }
            Err(e) => Err(self.stale_edit_error(e, &format!("Shard {name}")).await),
        }
    }

    /// A failed `test` of the generation means someone changed the spec since the user
    /// loaded it: say so, rather than reporting an internal error.
    async fn stale_edit_error(&self, e: kube::Error, what: &str) -> CoreError {
        if let kube::Error::Api(ae) = &e {
            if is_failed_test(ae.code, &ae.message) {
                return CoreError::Input(anyhow!(
                    "{what} changed since you loaded it; reload and make the change again"
                ));
            }
        }
        self.handle_kube_error(e).await
    }

    /// Asks the coordinator to cancel a promotion by setting its cancel annotations. The promotion stays:
    /// it ends `Cancelled`, or with its real result if its workflow had already finished.
    #[instrument(skip(self))]
    pub async fn cancel_promotion(&self, name: &str, requested_by: &str) -> Result<(), CoreError> {
        let api: Api<Promotion> = self.fleet_api().await?;
        let promotion = match api.get(name).await {
            Ok(promotion) => promotion,
            Err(e) => return Err(self.handle_kube_error(e).await),
        };
        if let Some(phase) = promotion.phase().filter(|p| p.is_finished()) {
            return Err(CoreError::Input(anyhow!(
                "promotion {name} has already finished ({phase:?})"
            )));
        }
        let patch = Promotion::cancel_patch(requested_by, &Time(Timestamp::now()));
        match api
            .patch(name, &PatchParams::default(), &Patch::Merge(&patch))
            .await
        {
            Ok(_) => {
                info!("Requested cancel of promotion {name}");
                Ok(())
            }
            Err(e) => Err(self.handle_kube_error(e).await),
        }
    }
}

#[cfg(test)]
mod tests {
    use fleet_api::{ChannelRef, PromotionPhase, PromotionStatus};
    use kube::core::ObjectMeta;
    use serde_json::json;

    use super::*;

    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    fn config(allow_deploy_backend: bool) -> ShardConfig {
        serde_json::from_value(json!({
            "apiVersion": "fleet.believer.dev/v1alpha1",
            "kind": "ShardConfig",
            "metadata": {"name": "test", "namespace": "fleet-system"},
            "spec": {
                "shardName": "test",
                "healthcheckUrl": "https://backend.example.com/v1/healthcheck",
                "channels": [
                    {"kind": "launcher", "metadataPath": "latest", "gameConfig": "Development"},
                    {"kind": "steam", "branch": "test", "gameConfig": "Test"},
                ],
                "promotion": {"allowDeployBackend": allow_deploy_backend},
            },
        }))
        .unwrap()
    }

    fn spec(channel: ChannelKey) -> PromotionSpec {
        PromotionSpec {
            shard: "test".into(),
            channel: ChannelRef::from(&channel),
            version: SHA.into(),
            game_config: None,
            deploy_backend: false,
            sign: None,
            skip_fleet_gate: None,
        }
    }

    fn existing(name: &str, spec: PromotionSpec, phase: Option<PromotionPhase>) -> Promotion {
        let mut promotion = Promotion::request(spec, "someone", None);
        promotion.metadata = ObjectMeta {
            name: Some(name.into()),
            ..promotion.metadata
        };
        promotion.status = phase.map(|phase| PromotionStatus {
            phase: Some(phase),
            ..Default::default()
        });
        promotion
    }

    #[test]
    fn accepts_a_valid_request() {
        let spec = spec(ChannelKey::steam("test"));
        assert_eq!(check_promotion(&spec, Some(&config(false)), &[]), Ok(()));
    }

    #[test]
    fn version_must_be_a_full_sha() {
        let mut spec = spec(ChannelKey::steam("test"));
        for bad in [&SHA[..7], &SHA.to_uppercase(), ""] {
            spec.version = bad.to_string();
            assert!(matches!(
                check_promotion(&spec, Some(&config(false)), &[]),
                Err(PromotionCheckError::InvalidVersion(_))
            ));
        }
    }

    #[test]
    fn shard_and_channel_must_exist() {
        let spec_ok = spec(ChannelKey::steam("test"));
        assert_eq!(
            check_promotion(&spec_ok, None, &[]),
            Err(PromotionCheckError::UnknownShard("test".into()))
        );

        let other = spec(ChannelKey::steam("other"));
        assert_eq!(
            check_promotion(&other, Some(&config(false)), &[]),
            Err(PromotionCheckError::UnknownChannel {
                shard: "test".into(),
                channel: "steam/other".into(),
            })
        );

        let mut mismatched = spec(ChannelKey::steam("test"));
        mismatched.channel.metadata_path = Some("latest".into());
        assert!(matches!(
            check_promotion(&mismatched, Some(&config(false)), &[]),
            Err(PromotionCheckError::InvalidChannel(_))
        ));
    }

    #[test]
    fn steam_takes_only_test_or_shipping() {
        let mut steam = spec(ChannelKey::steam("test"));
        steam.game_config = Some(GameConfig::Development);
        assert!(matches!(
            check_promotion(&steam, Some(&config(false)), &[]),
            Err(PromotionCheckError::InvalidGameConfig { .. })
        ));

        let mut launcher = spec(ChannelKey::launcher("latest"));
        launcher.game_config = Some(GameConfig::Development);
        assert_eq!(
            check_promotion(&launcher, Some(&config(false)), &[]),
            Ok(())
        );
    }

    #[test]
    fn backend_deploy_needs_the_shard_to_allow_it() {
        let mut spec = spec(ChannelKey::launcher("latest"));
        spec.deploy_backend = true;
        assert_eq!(
            check_promotion(&spec, Some(&config(false)), &[]),
            Err(PromotionCheckError::BackendDeployNotAllowed("test".into()))
        );
        assert_eq!(check_promotion(&spec, Some(&config(true)), &[]), Ok(()));
    }

    #[test]
    fn busy_channel_is_refused() {
        let request = spec(ChannelKey::steam("test"));
        let config = config(false);

        // No status yet means the coordinator hasn't picked it up: still busy.
        for phase in [
            None,
            Some(PromotionPhase::Pending),
            Some(PromotionPhase::Running),
        ] {
            let busy = existing("test-steam-abcde", request.clone(), phase);
            assert_eq!(
                check_promotion(&request, Some(&config), &[busy]),
                Err(PromotionCheckError::ChannelBusy {
                    shard: "test".into(),
                    channel: "steam/test".into(),
                    promotion: "test-steam-abcde".into(),
                }),
                "{phase:?}"
            );
        }
    }

    #[test]
    fn finished_or_other_channel_promotions_do_not_block() {
        let request = spec(ChannelKey::steam("test"));
        let others = [
            existing("done", request.clone(), Some(PromotionPhase::Succeeded)),
            existing("rejected", request.clone(), Some(PromotionPhase::Rejected)),
            existing(
                "launcher",
                spec(ChannelKey::launcher("latest")),
                Some(PromotionPhase::Running),
            ),
        ];
        assert_eq!(
            check_promotion(&request, Some(&config(false)), &others),
            Ok(())
        );
    }

    #[test]
    fn selector_uses_the_label_forms() {
        assert_eq!(promotion_selector(None, None), "");
        assert_eq!(
            promotion_selector(Some("test"), None),
            "fleet.believer.dev/shard=test"
        );
        assert_eq!(
            promotion_selector(Some("test"), Some(&ChannelKey::launcher("stable-1.0"))),
            "fleet.believer.dev/shard=test,fleet.believer.dev/channel=launcher.stable-1.0"
        );
    }

    #[test]
    fn request_sets_labels_and_annotations() {
        let promotion = Promotion::request(
            spec(ChannelKey::launcher("stable-1.0")),
            "someone",
            Some("hotfix"),
        );
        let labels = promotion.metadata.labels.unwrap();
        assert_eq!(labels[LABEL_SHARD], "test");
        assert_eq!(labels[LABEL_CHANNEL], "launcher.stable-1.0");
        let annotations = promotion.metadata.annotations.unwrap();
        assert_eq!(annotations[fleet_api::ANNOTATION_CHANGED_BY], "someone");
        assert_eq!(annotations[fleet_api::ANNOTATION_NOTE], "hotfix");
        assert_eq!(
            promotion.metadata.generate_name.as_deref(),
            Some("test-launcher-")
        );
    }

    const OTHER_SHA: &str = "0123456789ffffffffffffffffffffffffffffff";

    fn fleet_config(with_fleets: bool) -> ShardConfig {
        let mut config = config(false);
        if with_fleets {
            config.spec.fleets = Some(
                serde_json::from_value(json!({
                    "clusters": [{"name": "cluster-a", "zone": "cluster-a"}],
                    "imageRepository": "123456789012.dkr.ecr.us-west-2.amazonaws.com/game",
                    "limits": {"maxReplicasPerFleet": 50},
                }))
                .unwrap(),
            );
        }
        config
    }

    fn fleet_shard(enabled: bool) -> Shard {
        let mut shard = Shard::new("test", ShardSpec::default());
        shard.spec.fleets_enabled = enabled;
        shard
    }

    fn release(version: &str, allocated: i32) -> FleetRelease {
        let mut release = FleetRelease::request("test", version, "someone").unwrap();
        release.status = Some(
            serde_json::from_value(json!({
                "phase": "Live",
                "clusters": [{"name": "cluster-a", "maps": [
                    {"mapId": "Arena", "ready": 1, "allocated": allocated},
                ]}],
            }))
            .unwrap(),
        );
        release
    }

    #[test]
    fn warming_needs_fleets_on_the_shard() {
        let config = fleet_config(true);
        let shard = fleet_shard(true);
        assert_eq!(
            check_warm("test", SHA, Some(&config), Some(&shard), None),
            Ok(WarmPlan::Create("test-01234567".into()))
        );
        assert_eq!(
            check_warm("test", &SHA[..8], Some(&config), Some(&shard), None),
            Err(FleetReleaseCheckError::InvalidVersion(SHA[..8].into()))
        );
        assert_eq!(
            check_warm("test", SHA, None, Some(&shard), None),
            Err(FleetReleaseCheckError::UnknownShard("test".into()))
        );
        assert_eq!(
            check_warm("test", SHA, Some(&fleet_config(false)), Some(&shard), None),
            Err(FleetReleaseCheckError::NoFleets("test".into()))
        );
        assert_eq!(
            check_warm("test", SHA, Some(&config), Some(&fleet_shard(false)), None),
            Err(FleetReleaseCheckError::FleetsDisabled("test".into()))
        );
        assert_eq!(
            check_warm("test", SHA, Some(&config), None, None),
            Err(FleetReleaseCheckError::FleetsDisabled("test".into()))
        );
    }

    #[test]
    fn warming_refuses_existing_and_colliding_names() {
        let config = fleet_config(true);
        let shard = fleet_shard(true);
        assert_eq!(
            check_warm(
                "test",
                SHA,
                Some(&config),
                Some(&shard),
                Some(&release(SHA, 0))
            ),
            Err(FleetReleaseCheckError::AlreadyExists {
                name: "test-01234567".into(),
                phase: "Live".into(),
            })
        );
        assert_eq!(
            check_warm(
                "test",
                SHA,
                Some(&config),
                Some(&shard),
                Some(&release(OTHER_SHA, 0))
            ),
            Err(FleetReleaseCheckError::NameCollision {
                name: "test-01234567".into(),
                version: OTHER_SHA.into(),
            })
        );
    }

    #[test]
    fn a_create_conflict_names_the_release_in_the_way() {
        let config = fleet_config(true);
        let shard = fleet_shard(true);
        let conflict = |existing: Option<&FleetRelease>| {
            create_conflict(
                "test-01234567",
                "test",
                SHA,
                Some(&config),
                Some(&shard),
                existing,
            )
        };
        let mut warm = release(SHA, 0);
        warm.status.as_mut().unwrap().phase = serde_json::from_value(json!("Warm")).unwrap();
        assert_eq!(
            conflict(Some(&warm)),
            FleetReleaseCheckError::AlreadyExists {
                name: "test-01234567".into(),
                phase: "Warm".into(),
            }
        );
        // Gone again, or Retired: still on its way out, not a raw 409.
        assert_eq!(
            conflict(None),
            FleetReleaseCheckError::StillDeleting("test-01234567".into())
        );
        let mut retired = warm.clone();
        retired.status.as_mut().unwrap().phase = serde_json::from_value(json!("Retired")).unwrap();
        assert_eq!(
            conflict(Some(&retired)),
            FleetReleaseCheckError::StillDeleting("test-01234567".into())
        );
    }

    #[test]
    fn warming_recreates_a_retired_release() {
        let config = fleet_config(true);
        let shard = fleet_shard(true);
        let warm = |existing: &FleetRelease| {
            check_warm("test", SHA, Some(&config), Some(&shard), Some(existing))
        };
        let in_phase = |phase: &str| {
            let mut r = release(SHA, 0);
            r.metadata.uid = Some("uid-1".into());
            r.status.as_mut().unwrap().phase = serde_json::from_value(json!(phase)).unwrap();
            r
        };

        let retired = in_phase("Retired");
        assert_eq!(
            warm(&retired),
            Ok(WarmPlan::Recreate {
                name: "test-01234567".into(),
                uid: Some("uid-1".into()),
            })
        );
        assert_eq!(warm(&retired).unwrap().name(), "test-01234567");

        // On its way to Retired: refused until it gets there.
        assert_eq!(
            warm(&in_phase("Retiring")),
            Err(FleetReleaseCheckError::StillRetiring(
                "test-01234567".into()
            ))
        );
        let mut requested = in_phase("Live");
        requested.spec.phase = FleetReleaseDesiredPhase::Retired;
        assert_eq!(
            warm(&requested),
            Err(FleetReleaseCheckError::StillRetiring(
                "test-01234567".into()
            ))
        );
        let mut deleting = in_phase("Retired");
        deleting.metadata.deletion_timestamp = Some(Time(Timestamp::UNIX_EPOCH));
        assert_eq!(
            warm(&deleting),
            Err(FleetReleaseCheckError::StillDeleting(
                "test-01234567".into()
            ))
        );

        // Another build's retired release is still a collision.
        let mut other = release(OTHER_SHA, 0);
        other.status.as_mut().unwrap().phase = Some(FleetReleasePhase::Retired);
        assert!(matches!(
            warm(&other),
            Err(FleetReleaseCheckError::NameCollision { .. })
        ));
    }

    #[test]
    fn retiring_waits_for_matches_unless_forced() {
        assert_eq!(check_retire(&release(SHA, 0), false), Ok(()));
        // Without force, cluster mode waits for the matches to end, so it isn't refused.
        assert_eq!(check_retire(&release(SHA, 3), false), Ok(()));
        assert_eq!(check_retire(&release(SHA, 3), true), Ok(()));

        // Already retiring: only escalating to force goes through.
        let mut retiring = release(SHA, 3);
        retiring.spec.phase = FleetReleaseDesiredPhase::Retired;
        assert_eq!(
            check_retire(&retiring, false),
            Err(FleetReleaseCheckError::AlreadyRetiring(
                "test-01234567".into()
            ))
        );
        assert_eq!(check_retire(&retiring, true), Ok(()));
        retiring.spec.force = true;
        assert!(check_retire(&retiring, true).is_err());
    }

    fn spec_with(value: serde_json::Value) -> ShardSpec {
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn shard_spec_is_checked_against_the_config() {
        let config = fleet_config(true);
        let good = spec_with(json!({
            "fleetsEnabled": true,
            "maps": ["Arena"],
            "warm": {"bufferSize": 2},
            "live": {
                "default": {"bufferSize": "20%", "minReplicas": 1, "maxReplicas": 10},
                "maps": {"Arena": {"maxReplicas": 50}},
                "overrides": [{"cluster": "cluster-a", "mapId": "Arena", "sizing": {"parked": true, "maxReplicas": 99}}],
            },
        }));
        assert_eq!(check_shard_spec("test", &good, Some(&config)), Ok(()));
        // Turning fleets off is always allowed, even without a fleets section.
        assert_eq!(
            check_shard_spec("test", &ShardSpec::default(), Some(&fleet_config(false))),
            Ok(())
        );

        assert_eq!(
            check_shard_spec("test", &good, Some(&fleet_config(false))),
            Err(ShardSpecCheckError::NoFleets("test".into()))
        );
        // Fleets off is allowed whatever the rest of the spec holds once the section is gone: its
        // maps, sizing and overrides are kept for when Fleets come back.
        let mut off = good.clone();
        off.fleets_enabled = false;
        assert_eq!(
            check_shard_spec("test", &off, Some(&fleet_config(false))),
            Ok(())
        );
        assert_eq!(check_shard_spec("test", &off, None), Ok(()));
        let above = spec_with(json!({"live": {"maps": {"Arena": {"maxReplicas": 51}}}}));
        assert_eq!(
            check_shard_spec("test", &above, Some(&config)),
            Err(ShardSpecCheckError::AboveLimit {
                at: "live Arena".into(),
                max: 51,
                limit: 50,
            })
        );
        let bad_map = spec_with(json!({"maps": ["has space"]}));
        assert_eq!(
            check_shard_spec("test", &bad_map, Some(&config)),
            Err(ShardSpecCheckError::InvalidMapId("has space".into()))
        );
        let bad_cluster = spec_with(json!({"live": {"overrides": [
            {"cluster": "elsewhere", "mapId": "Arena", "sizing": {"parked": true}},
        ]}}));
        assert_eq!(
            check_shard_spec("test", &bad_cluster, Some(&config)),
            Err(ShardSpecCheckError::UnknownCluster("elsewhere".into()))
        );
    }

    #[test]
    fn spec_patch_tests_the_generation_and_records_who() {
        let spec = spec_with(json!({"fleetsEnabled": true}));
        let shard = fleet_shard(false);
        let patch =
            serde_json::to_value(shard_spec_patch(&shard, 42, &spec, "someone").unwrap()).unwrap();
        assert_eq!(
            patch,
            json!([
                {"op": "test", "path": "/metadata/generation", "value": 42},
                {"op": "add", "path": "/spec", "value": {"fleetsEnabled": true}},
                {"op": "add", "path": "/metadata/annotations",
                 "value": {"fleet.believer.dev/changed-by": "someone"}},
            ])
        );

        // With annotations present, only the one key is set, its `/` escaped.
        let release = release(SHA, 0);
        let patch =
            serde_json::to_value(release_overrides_patch(&release, 7, &[], "someone").unwrap())
                .unwrap();
        assert_eq!(
            patch[2],
            json!({"op": "add", "path": "/metadata/annotations/fleet.believer.dev~1changed-by", "value": "someone"})
        );
        assert_eq!(
            patch[1],
            json!({"op": "add", "path": "/spec/overrides", "value": []})
        );
    }

    #[test]
    fn failed_tests_are_recognised() {
        // As a v1.33 apiserver answers a JSON patch whose `test` fails.
        assert!(is_failed_test(422, PATCH_NOT_APPLIED));
        assert!(!is_failed_test(
            422,
            "Shard.fleet.believer.dev \"test\" is invalid: spec.live: Invalid value"
        ));
        assert!(!is_failed_test(500, PATCH_NOT_APPLIED));
    }
}
