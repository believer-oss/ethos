use std::cmp::Reverse;
use std::collections::BTreeMap;

use anyhow::anyhow;
use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use ethos_core::clients::aws::ensure_aws_client;
use ethos_core::clients::kube::ensure_kube_client;
use ethos_core::clients::manifest::{self, BuildManifest};
use ethos_core::types::errors::CoreError;
use fleet_api::{
    fleet_defaults, BackendParts, ChannelKey, FleetRelease, Promotion, PromotionSpec, Shard,
    ShardConfig, ShardConfigSpec, ShardSpec, ShardStatus, Sizing, SizingOverride,
};
use serde::{Deserialize, Serialize};
use tracing::{info, instrument, warn};

use crate::engine::EngineProvider;
use crate::fleets::PromotePath;
use crate::state::AppState;

pub fn router<T>() -> Router<AppState<T>>
where
    T: EngineProvider,
{
    Router::new()
        .route("/shards", get(get_shards))
        .route("/shards/:name", get(get_shard))
        .route("/promotions", get(get_promotions).post(create_promotion))
        .route("/promotions/:name/cancel", post(cancel_promotion))
        .route("/promoter", get(get_promoter))
        .route("/shards/:name/spec", post(set_shard_spec))
        .route("/releases", get(get_releases).post(warm_release))
        .route("/releases/:name/pin", post(pin_release))
        .route("/releases/:name/retire", post(retire_release))
        .route("/releases/:name/overrides", post(set_release_overrides))
        .route("/promotion-plan", get(get_promotion_plan))
}

/// A shard as the UI shows it: its config joined with its observed status. Either half may be
/// missing, e.g. while the coordinator has yet to create the `Shard` for a new `ShardConfig`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShardSummary {
    pub name: String,
    pub display_name: String,
    pub config: Option<ShardConfigSpec>,
    /// The shard's Fleet settings; `None` while there is no `Shard`.
    pub spec: Option<ShardSpec>,
    pub status: Option<ShardStatus>,
    /// The `Shard`'s generation, sent back with a spec edit so it can't overwrite a newer one.
    /// Status writes don't change it.
    pub generation: Option<i64>,
    /// The live sizing under `spec.live.default`: `fleet_defaults::LIVE`, its max capped at the
    /// shard's `maxReplicasPerFleet` as the coordinator caps it. `None` without `fleets`.
    pub live_default: Option<Sizing>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShardDetail {
    #[serde(flatten)]
    pub shard: ShardSummary,
    /// Running first, then the most recent.
    pub promotions: Vec<Promotion>,
    /// Newest first.
    pub releases: Vec<FleetRelease>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GetReleasesParams {
    pub shard: Option<String>,
}

/// A new Fleet setting for a shard, replacing the whole spec, if the `Shard`'s spec is still at
/// `generation`.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetShardSpecRequest {
    pub generation: i64,
    pub spec: ShardSpec,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WarmReleaseRequest {
    pub shard: String,
    pub version: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PinReleaseRequest {
    pub pinned: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RetireReleaseRequest {
    /// Also end matches on Allocated servers. Without it, the Fleets go once those matches end.
    #[serde(default)]
    pub force: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetReleaseOverridesRequest {
    pub generation: i64,
    pub overrides: Vec<SizingOverride>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GetPromotionsParams {
    pub shard: Option<String>,
    /// Key form, e.g. `steam/test`. Only used together with `shard`.
    pub channel: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatePromotionRequest {
    pub spec: PromotionSpec,
    #[serde(default)]
    pub note: Option<String>,
}

/// Joins configs and shards by name, sorted by name. Names come from both sides.
pub fn summarize(configs: Vec<ShardConfig>, shards: Vec<Shard>) -> Vec<ShardSummary> {
    let mut by_name: BTreeMap<String, (Option<ShardConfigSpec>, Option<Shard>)> = BTreeMap::new();
    for config in configs {
        let name = config
            .metadata
            .name
            .unwrap_or(config.spec.shard_name.clone());
        by_name.entry(name).or_default().0 = Some(config.spec);
    }
    for shard in shards {
        let Some(name) = shard.metadata.name.clone() else {
            continue;
        };
        by_name.entry(name).or_default().1 = Some(shard);
    }
    by_name
        .into_iter()
        .map(|(name, (config, shard))| {
            let config = config.map(with_fleet_defaults);
            ShardSummary {
                display_name: config
                    .as_ref()
                    .and_then(|c| c.display_name.clone())
                    .filter(|d| !d.is_empty())
                    .unwrap_or_else(|| name.clone()),
                name,
                live_default: config
                    .as_ref()
                    .and_then(|c| c.fleets.as_ref())
                    .map(|f| live_default(f.limits.max_replicas_per_fleet)),
                config,
                generation: shard.as_ref().and_then(|s| s.metadata.generation),
                spec: shard.as_ref().map(|s| s.spec.clone()),
                status: shard.map(|s| s.status.unwrap_or_default()),
            }
        })
        .collect()
}

/// The config with every fleets field it leaves unset filled from `fleet_defaults`, as the
/// coordinator applies them, so Friendshipper shows the values in force and keeps no copy of them.
pub fn with_fleet_defaults(mut config: ShardConfigSpec) -> ShardConfigSpec {
    if let Some(fleets) = config.fleets.as_mut() {
        let gate = &mut fleets.gate;
        gate.min_ready.get_or_insert(fleet_defaults::GATE_MIN_READY);
        gate.stable_for
            .get_or_insert(fleet_defaults::GATE_STABLE_FOR.into());
        gate.timeout
            .get_or_insert(fleet_defaults::GATE_TIMEOUT.into());
        fleets
            .retire_after_idle
            .get_or_insert(fleet_defaults::RETIRE_AFTER_IDLE.into());
        fleets
            .warm_ttl
            .get_or_insert(fleet_defaults::WARM_TTL.into());
    }
    config
}

/// `fleet_defaults::LIVE`, its max capped at the shard's limit as the coordinator caps it.
pub fn live_default(limit: Option<i32>) -> Sizing {
    let mut sizing = fleet_defaults::LIVE;
    if let (Some(max), Some(limit)) = (sizing.max_replicas, limit) {
        sizing.max_replicas = Some(max.min(limit));
    }
    sizing
}

/// Newest first.
pub fn sort_releases(releases: &mut [FleetRelease]) {
    releases.sort_by_key(|r| Reverse(r.metadata.creation_timestamp.as_ref().map(|t| t.0)));
}

/// Active promotions first, then newest first.
pub fn sort_promotions(promotions: &mut [Promotion]) {
    promotions.sort_by_key(|p| {
        (
            !p.is_active(),
            Reverse(p.metadata.creation_timestamp.as_ref().map(|t| t.0)),
        )
    });
}

#[instrument(skip(state))]
async fn get_shards<T>(
    State(state): State<AppState<T>>,
) -> Result<Json<Vec<ShardSummary>>, CoreError>
where
    T: EngineProvider,
{
    let kube_client = ensure_kube_client(state.kube_client.read().clone())?;
    let (configs, shards) =
        futures::try_join!(kube_client.list_shard_configs(), kube_client.list_shards())?;
    Ok(Json(summarize(configs, shards)))
}

#[instrument(skip(state))]
async fn get_shard<T>(
    State(state): State<AppState<T>>,
    Path(name): Path<String>,
) -> Result<Json<ShardDetail>, CoreError>
where
    T: EngineProvider,
{
    require_object_name("shard", &name)?;
    let kube_client = ensure_kube_client(state.kube_client.read().clone())?;
    let (config, shard, mut promotions, mut releases) = futures::try_join!(
        kube_client.get_shard_config(&name),
        kube_client.get_shard(&name),
        kube_client.list_promotions(Some(&name), None),
        kube_client.list_fleet_releases(Some(&name)),
    )?;
    if config.is_none() && shard.is_none() {
        return Err(CoreError::Input(anyhow!("no shard named {name:?}")));
    }
    sort_promotions(&mut promotions);
    sort_releases(&mut releases);
    let summary = summarize(config.into_iter().collect(), shard.into_iter().collect())
        .pop()
        .ok_or_else(|| anyhow!("no shard named {name:?}"))?;
    Ok(Json(ShardDetail {
        shard: summary,
        promotions,
        releases,
    }))
}

#[instrument(skip(state))]
async fn get_releases<T>(
    State(state): State<AppState<T>>,
    Query(params): Query<GetReleasesParams>,
) -> Result<Json<Vec<FleetRelease>>, CoreError>
where
    T: EngineProvider,
{
    if let Some(shard) = &params.shard {
        require_object_name("shard", shard)?;
    }
    let kube_client = ensure_kube_client(state.kube_client.read().clone())?;
    let mut releases = kube_client
        .list_fleet_releases(params.shard.as_deref())
        .await?;
    sort_releases(&mut releases);
    Ok(Json(releases))
}

#[instrument(skip(state))]
async fn get_promotions<T>(
    State(state): State<AppState<T>>,
    Query(params): Query<GetPromotionsParams>,
) -> Result<Json<Vec<Promotion>>, CoreError>
where
    T: EngineProvider,
{
    if let Some(shard) = &params.shard {
        require_object_name("shard", shard)?;
    }
    let channel = params
        .channel
        .as_deref()
        .map(str::parse::<ChannelKey>)
        .transpose()
        .map_err(|e| CoreError::Input(anyhow!(e)))?;
    let kube_client = ensure_kube_client(state.kube_client.read().clone())?;
    let mut promotions = kube_client
        .list_promotions(params.shard.as_deref(), channel.as_ref())
        .await?;
    sort_promotions(&mut promotions);
    Ok(Json(promotions))
}

/// Shard, release and promotion names are Kubernetes object names (DNS-1123 labels). Checked before
/// they reach a kube path or a label selector, so a stray character can't change what is addressed.
fn require_object_name(kind: &str, name: &str) -> Result<(), CoreError> {
    let valid = (1..=63).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !name.starts_with('-')
        && !name.ends_with('-');
    if valid {
        Ok(())
    } else {
        Err(CoreError::Input(anyhow!(
            "{name:?} is not a valid {kind} name"
        )))
    }
}

/// Creating and cancelling promotions need the switch on and Release controls on, as in the UI.
/// Release controls aren't a security boundary (the role allows the same writes); checking here
/// keeps the gate the same everywhere.
fn require_promotion_writes<T>(state: &AppState<T>) -> Result<(), CoreError>
where
    T: EngineProvider,
{
    let via_coordinator = state.dynamic_config.read().promotion_via_coordinator;
    PromotePath::Coordinator.require(via_coordinator)?;
    if !state.app_config.read().release_controls_enabled {
        return Err(CoreError::Input(anyhow!(
            "Enable Release controls in Preferences before changing promotions"
        )));
    }
    Ok(())
}

/// Writes to shards and releases take the same gate as promotions: the switch, Release controls and
/// a name to record.
fn require_fleet_writes<T>(state: &AppState<T>) -> Result<String, CoreError>
where
    T: EngineProvider,
{
    require_promotion_writes(state)?;
    require_promoter_name(state)
}

fn require_promoter_name<T>(state: &AppState<T>) -> Result<String, CoreError>
where
    T: EngineProvider,
{
    state.promoter_name().ok_or_else(|| {
        CoreError::Input(anyhow!(
            "Set a display name in Preferences (or connect GitHub) before changing promotions"
        ))
    })
}

/// The name Friendshipper writes as `changed-by` and `cancel-requested-by`, if there is one, so the
/// UI can tell which promotions are this user's.
#[instrument(skip(state))]
async fn get_promoter<T>(State(state): State<AppState<T>>) -> Json<Option<String>>
where
    T: EngineProvider,
{
    Json(state.promoter_name())
}

#[instrument(skip(state))]
async fn create_promotion<T>(
    State(state): State<AppState<T>>,
    Json(request): Json<CreatePromotionRequest>,
) -> Result<Json<Promotion>, CoreError>
where
    T: EngineProvider,
{
    require_object_name("shard", &request.spec.shard)?;
    require_promotion_writes(&state)?;
    let changed_by = require_promoter_name(&state)?;

    let kube_client = ensure_kube_client(state.kube_client.read().clone())?;
    let promotion = kube_client
        .create_promotion(request.spec, &changed_by, request.note.as_deref())
        .await?;
    Ok(Json(promotion))
}

/// Asks the coordinator to cancel; the Promotion stays in history. Never deletes.
#[instrument(skip(state))]
async fn cancel_promotion<T>(
    State(state): State<AppState<T>>,
    Path(name): Path<String>,
) -> Result<(), CoreError>
where
    T: EngineProvider,
{
    require_object_name("promotion", &name)?;
    require_promotion_writes(&state)?;
    let requested_by = require_promoter_name(&state)?;

    let kube_client = ensure_kube_client(state.kube_client.read().clone())?;
    info!("Requesting cancel of promotion {name} for {requested_by}");
    kube_client.cancel_promotion(&name, &requested_by).await
}

#[instrument(skip(state, request))]
async fn set_shard_spec<T>(
    State(state): State<AppState<T>>,
    Path(name): Path<String>,
    Json(request): Json<SetShardSpecRequest>,
) -> Result<Json<Shard>, CoreError>
where
    T: EngineProvider,
{
    require_object_name("shard", &name)?;
    let changed_by = require_fleet_writes(&state)?;
    let kube_client = ensure_kube_client(state.kube_client.read().clone())?;
    let shard = kube_client
        .set_shard_spec(&name, request.generation, request.spec, &changed_by)
        .await?;
    Ok(Json(shard))
}

#[instrument(skip(state))]
async fn warm_release<T>(
    State(state): State<AppState<T>>,
    Json(request): Json<WarmReleaseRequest>,
) -> Result<Json<FleetRelease>, CoreError>
where
    T: EngineProvider,
{
    require_object_name("shard", &request.shard)?;
    let changed_by = require_fleet_writes(&state)?;
    let kube_client = ensure_kube_client(state.kube_client.read().clone())?;
    let release = kube_client
        .create_fleet_release(&request.shard, &request.version, &changed_by)
        .await?;
    Ok(Json(release))
}

#[instrument(skip(state))]
async fn pin_release<T>(
    State(state): State<AppState<T>>,
    Path(name): Path<String>,
    Json(request): Json<PinReleaseRequest>,
) -> Result<Json<FleetRelease>, CoreError>
where
    T: EngineProvider,
{
    require_object_name("release", &name)?;
    let changed_by = require_fleet_writes(&state)?;
    let kube_client = ensure_kube_client(state.kube_client.read().clone())?;
    let release = kube_client
        .pin_fleet_release(&name, request.pinned, &changed_by)
        .await?;
    Ok(Json(release))
}

#[instrument(skip(state))]
async fn retire_release<T>(
    State(state): State<AppState<T>>,
    Path(name): Path<String>,
    Json(request): Json<RetireReleaseRequest>,
) -> Result<Json<FleetRelease>, CoreError>
where
    T: EngineProvider,
{
    require_object_name("release", &name)?;
    let changed_by = require_fleet_writes(&state)?;
    let kube_client = ensure_kube_client(state.kube_client.read().clone())?;
    let release = kube_client
        .retire_fleet_release(&name, request.force, &changed_by)
        .await?;
    Ok(Json(release))
}

#[instrument(skip(state, request))]
async fn set_release_overrides<T>(
    State(state): State<AppState<T>>,
    Path(name): Path<String>,
    Json(request): Json<SetReleaseOverridesRequest>,
) -> Result<Json<FleetRelease>, CoreError>
where
    T: EngineProvider,
{
    require_object_name("release", &name)?;
    let changed_by = require_fleet_writes(&state)?;
    let kube_client = ensure_kube_client(state.kube_client.read().clone())?;
    let release = kube_client
        .set_fleet_release_overrides(&name, request.generation, request.overrides, &changed_by)
        .await?;
    Ok(Json(release))
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PromotionPlanParams {
    pub shard: String,
    pub version: String,
}

/// What the promote form shows before submitting, from the build's manifest: the backend parts it
/// carries next to the shard's, as the coordinator compares them at acceptance, and the maps it has
/// levels for.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromotionPlan {
    /// `None` when the build has no readable manifest (older builds): the form shows what it can
    /// without it.
    pub manifest: Option<BuildManifest>,
    /// The shard's backend as last observed; `None` before the first observation.
    pub shard_backend: Option<BackendParts>,
}

/// A manifest body as the plan uses it: one that won't parse counts as none, and is logged.
fn plan_manifest(version: &str, body: Option<Vec<u8>>) -> Option<BuildManifest> {
    let body = body?;
    match manifest::parse(&body) {
        Ok(m) => Some(m),
        Err(e) => {
            warn!("The build manifest for {version} won't parse: {e}");
            None
        }
    }
}

#[instrument(skip(state))]
async fn get_promotion_plan<T>(
    State(state): State<AppState<T>>,
    Query(params): Query<PromotionPlanParams>,
) -> Result<Json<PromotionPlan>, CoreError>
where
    T: EngineProvider,
{
    require_object_name("shard", &params.shard)?;
    if !fleet_api::is_full_sha(&params.version) {
        return Err(CoreError::Input(anyhow!(
            "{:?} is not a full 40-character lowercase commit SHA",
            params.version
        )));
    }
    let aws_client = ensure_aws_client(state.aws_client.read().await.clone())?;
    let kube_client = ensure_kube_client(state.kube_client.read().clone())?;
    let key = manifest::manifest_key(&params.version);
    let (body, shard) = futures::try_join!(
        aws_client.get_artifact_object(&key),
        kube_client.get_shard(&params.shard),
    )?;
    Ok(Json(PromotionPlan {
        manifest: plan_manifest(&params.version, body),
        shard_backend: shard
            .and_then(|s| s.status)
            .and_then(|s| s.backend)
            .map(|b| b.parts),
    }))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use ethos_core::artifact_sync::{ArtifactSync, DownloadCancellation};
    use ethos_core::types::config::{AppConfig, DynamicConfig, RepoConfig};
    use fleet_api::{ChannelRef, PromotionPhase, PromotionStatus};
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::Time;
    use parking_lot::RwLock;
    use serde_json::json;

    use super::*;
    use crate::engine::UnrealEngineProvider;

    fn config(name: &str, display_name: Option<&str>) -> ShardConfig {
        serde_json::from_value(json!({
            "apiVersion": "fleet.believer.dev/v1alpha1",
            "kind": "ShardConfig",
            "metadata": {"name": name},
            "spec": {
                "shardName": name,
                "displayName": display_name,
                "healthcheckUrl": "https://backend.example.com/v1/healthcheck",
                "channels": [{"kind": "steam", "branch": name, "gameConfig": "Test"}],
            },
        }))
        .unwrap()
    }

    fn shard(name: &str) -> Shard {
        Shard {
            metadata: ObjectMeta {
                name: Some(name.into()),
                resource_version: Some("5".into()),
                generation: Some(3),
                ..Default::default()
            },
            spec: Default::default(),
            status: None,
        }
    }

    #[test]
    fn summary_joins_by_name_from_both_sides() {
        let rows = summarize(
            vec![config("beta", Some("Beta")), config("alpha", None)],
            vec![shard("alpha"), shard("gamma")],
        );
        let names: Vec<_> = rows
            .iter()
            .map(|r| (r.name.as_str(), r.display_name.as_str()))
            .collect();
        assert_eq!(
            names,
            [("alpha", "alpha"), ("beta", "Beta"), ("gamma", "gamma")]
        );
        assert!(rows[0].config.is_some() && rows[0].status.is_some());
        assert!(rows[1].config.is_some() && rows[1].status.is_none());
        assert!(rows[2].config.is_none() && rows[2].status.is_some());
        // Spec and generation come with the Shard; the resource version, which status writes
        // change, isn't used.
        assert!(rows[0].spec.is_some() && rows[1].spec.is_none());
        assert_eq!(rows[0].generation, Some(3));
    }

    #[test]
    fn releases_sort_newest_first() {
        let at = |secs| {
            let mut r =
                FleetRelease::request("test", &format!("{secs:08}{}", "0".repeat(32)), "someone")
                    .unwrap();
            r.metadata.creation_timestamp = Some(Time(
                k8s_openapi::jiff::Timestamp::from_second(secs).unwrap(),
            ));
            r
        };
        let mut list = vec![at(100), at(300), at(200)];
        sort_releases(&mut list);
        let names: Vec<_> = list
            .iter()
            .map(|r| r.metadata.name.as_deref().unwrap())
            .collect();
        assert_eq!(names, ["test-00000300", "test-00000200", "test-00000100"]);
    }

    fn promotion(name: &str, created_secs: i64, phase: Option<PromotionPhase>) -> Promotion {
        let spec = PromotionSpec {
            shard: "test".into(),
            channel: ChannelRef::from(&ChannelKey::steam("test")),
            version: "0123456789abcdef0123456789abcdef01234567".into(),
            game_config: None,
            deploy_backend: false,
            sign: None,
            skip_fleet_gate: None,
        };
        let mut p = Promotion::request(spec, "someone", None);
        p.metadata.name = Some(name.into());
        p.metadata.creation_timestamp = Some(Time(
            k8s_openapi::jiff::Timestamp::from_second(created_secs).unwrap(),
        ));
        p.status = phase.map(|phase| PromotionStatus {
            phase: Some(phase),
            ..Default::default()
        });
        p
    }

    #[test]
    fn active_promotions_sort_first_then_newest() {
        let mut list = vec![
            promotion("old-done", 100, Some(PromotionPhase::Succeeded)),
            promotion("new-done", 300, Some(PromotionPhase::Failed)),
            promotion("old-running", 50, Some(PromotionPhase::Running)),
            promotion("submitted", 200, None),
        ];
        sort_promotions(&mut list);
        let names: Vec<_> = list
            .iter()
            .map(|p| p.metadata.name.as_deref().unwrap())
            .collect();
        assert_eq!(names, ["submitted", "old-running", "new-done", "old-done"]);
    }

    #[test]
    fn create_request_takes_a_promotion_spec() {
        let request: CreatePromotionRequest = serde_json::from_value(json!({
            "spec": {
                "shard": "test",
                "channel": {"kind": "launcher", "metadataPath": "latest"},
                "version": "0123456789abcdef0123456789abcdef01234567",
                "deployBackend": true,
            },
        }))
        .unwrap();
        assert!(request.spec.deploy_backend);
        assert_eq!(request.spec.sign, None);
        assert_eq!(request.note, None);
    }

    /// State with no AWS, kube or GitHub client: enough for the handlers' gates.
    fn state(via_coordinator: bool, release_controls: bool) -> AppState<UnrealEngineProvider> {
        let mut app_config = AppConfig::new(crate::APP_NAME);
        app_config.release_controls_enabled = release_controls;
        app_config.user_display_name = "someone".into();
        let dynamic_config = DynamicConfig {
            promotion_via_coordinator: via_coordinator,
            ..Default::default()
        };
        let (sync_event_tx, _) = std::sync::mpsc::channel();
        let (operation_tx, _) = tokio::sync::mpsc::channel(1);
        let (notification_tx, _) = std::sync::mpsc::channel();
        let (frontend_op_tx, _) = std::sync::mpsc::channel();
        let (log_tx, _) = std::sync::mpsc::channel();
        AppState {
            engine: UnrealEngineProvider::new_from_config(
                app_config.clone(),
                RepoConfig::default(),
            ),
            app_config: Arc::new(RwLock::new(app_config)),
            repo_config: Arc::new(RwLock::new(RepoConfig::default())),
            dynamic_config: Arc::new(RwLock::new(dynamic_config)),
            config_file: Default::default(),
            storage: Default::default(),
            repo_status: Default::default(),
            artifact_sync: ArtifactSync::new(crate::APP_NAME),
            sync_event_tx,
            operation_tx,
            notification_tx,
            frontend_op_tx,
            aws_client: Default::default(),
            kube_client: Default::default(),
            additional_kube_clients: Default::default(),
            github_client: Default::default(),
            version: String::new(),
            log_path: Default::default(),
            otel_reload_handle: None,
            gameserver_log_tx: log_tx.clone(),
            workflow_log_tx: log_tx.clone(),
            git_tx: log_tx.clone(),
            sync_phase_tx: log_tx.clone(),
            build_tools_tx: log_tx,
            downloads: DownloadCancellation::default(),
            workflow_log_cancel_tx: Default::default(),
        }
    }

    fn request() -> CreatePromotionRequest {
        CreatePromotionRequest {
            spec: promotion("p", 0, None).spec,
            note: None,
        }
    }

    async fn create(state: AppState<UnrealEngineProvider>) -> Result<(), CoreError> {
        create_promotion(State(state), Json(request()))
            .await
            .map(|_| ())
    }

    async fn cancel(state: AppState<UnrealEngineProvider>) -> Result<(), CoreError> {
        cancel_promotion(State(state), Path("p".to_string())).await
    }

    /// Every write besides creating and cancelling promotions, so the gate tests cover them all.
    async fn fleet_writes(state: AppState<UnrealEngineProvider>) -> Vec<Result<(), CoreError>> {
        let release = || Path("test-01234567".to_string());
        vec![
            set_shard_spec(
                State(state.clone()),
                Path("test".to_string()),
                Json(SetShardSpecRequest {
                    generation: 1,
                    spec: ShardSpec::default(),
                }),
            )
            .await
            .map(|_| ()),
            warm_release(
                State(state.clone()),
                Json(WarmReleaseRequest {
                    shard: "test".into(),
                    version: "0123456789abcdef0123456789abcdef01234567".into(),
                }),
            )
            .await
            .map(|_| ()),
            pin_release(
                State(state.clone()),
                release(),
                Json(PinReleaseRequest { pinned: true }),
            )
            .await
            .map(|_| ()),
            retire_release(
                State(state.clone()),
                release(),
                Json(RetireReleaseRequest::default()),
            )
            .await
            .map(|_| ()),
            set_release_overrides(
                State(state),
                release(),
                Json(SetReleaseOverridesRequest {
                    generation: 1,
                    overrides: vec![],
                }),
            )
            .await
            .map(|_| ()),
        ]
    }

    fn message(result: Result<(), CoreError>) -> String {
        result.unwrap_err().to_string()
    }

    #[tokio::test]
    async fn writes_need_the_switch() {
        assert!(message(create(state(false, true)).await).contains("promotionViaCoordinator"));
        assert!(message(cancel(state(false, true)).await).contains("promotionViaCoordinator"));
        for result in fleet_writes(state(false, true)).await {
            assert!(message(result).contains("promotionViaCoordinator"));
        }
    }

    #[tokio::test]
    async fn writes_need_release_controls() {
        assert!(message(create(state(true, false)).await).contains("Release controls"));
        assert!(message(cancel(state(true, false)).await).contains("Release controls"));
        for result in fleet_writes(state(true, false)).await {
            assert!(message(result).contains("Release controls"));
        }
    }

    #[tokio::test]
    async fn fleet_writes_need_a_name() {
        let nameless = state(true, true);
        nameless.app_config.write().user_display_name = String::new();
        for result in fleet_writes(nameless).await {
            assert!(message(result).contains("display name"));
        }
    }

    #[test]
    fn object_names_are_dns_labels() {
        for good in ["test", "test-01234567", "a", &"a".repeat(63)] {
            assert!(require_object_name("shard", good).is_ok(), "{good}");
        }
        for bad in [
            "",
            "Test",
            "-test",
            "test-",
            "te.st",
            "../test",
            "test,other",
            "a=b",
            &"a".repeat(64),
        ] {
            assert!(require_object_name("shard", bad).is_err(), "{bad:?}");
        }
    }

    #[tokio::test]
    async fn routes_refuse_a_bad_name_before_any_lookup() {
        // Refused at the name, not at the missing kube client behind it.
        let bad = || "a,b".to_string();
        let results = vec![
            get_shard(State(state(true, true)), Path(bad()))
                .await
                .map(|_| ()),
            get_releases(
                State(state(true, true)),
                Query(GetReleasesParams { shard: Some(bad()) }),
            )
            .await
            .map(|_| ()),
            get_promotion_plan(
                State(state(true, true)),
                Query(PromotionPlanParams {
                    shard: bad(),
                    version: "0123456789abcdef0123456789abcdef01234567".into(),
                }),
            )
            .await
            .map(|_| ()),
            cancel_promotion(State(state(true, true)), Path(bad())).await,
            pin_release(
                State(state(true, true)),
                Path(bad()),
                Json(PinReleaseRequest { pinned: true }),
            )
            .await
            .map(|_| ()),
        ];
        for result in results {
            assert!(message(result).contains("is not a valid"));
        }
    }

    #[tokio::test]
    async fn writes_pass_the_gate_with_both_on() {
        // Past the gate, they stop at the missing kube client rather than at the gate.
        let mut results = vec![
            create(state(true, true)).await,
            cancel(state(true, true)).await,
        ];
        results.extend(fleet_writes(state(true, true)).await);
        for result in results {
            let message = message(result);
            assert!(!message.contains("promotionViaCoordinator"), "{message}");
            assert!(!message.contains("Release controls"), "{message}");
        }
    }

    #[test]
    fn a_build_without_a_readable_manifest_has_no_plan_manifest() {
        assert_eq!(plan_manifest("abc", None), None);
        assert_eq!(plan_manifest("abc", Some(b"{not json".to_vec())), None);
        let read = plan_manifest(
            "abc",
            Some(br#"{"maps": {"levelAssets": {"Arena": "/Game/L_Arena"}}}"#.to_vec()),
        )
        .unwrap();
        assert_eq!(read.map_ids, vec!["Arena"]);
    }

    #[test]
    fn summaries_carry_the_fleet_defaults_in_force() {
        let mut with_fleets = config("test", None);
        with_fleets.spec.fleets = Some(
            serde_json::from_value(json!({
                "clusters": [{"name": "cluster-a", "zone": "zone-a"}],
                "imageRepository": "registry.example.com/game",
                "limits": {"maxReplicasPerFleet": 4},
                "gate": {"timeout": "30m"},
            }))
            .unwrap(),
        );
        let [summary]: [ShardSummary; 1] = summarize(vec![with_fleets], vec![]).try_into().unwrap();
        let fleets = summary.config.unwrap().fleets.unwrap();
        assert_eq!(fleets.gate.min_ready, Some(1));
        assert_eq!(
            serde_json::to_value(&fleets).unwrap()["gate"],
            // kube writes durations as seconds; the UI reads either.
            json!({"minReady": 1, "stableFor": "120s", "timeout": "1800s"})
        );
        assert_eq!(
            fleets.retire_after_idle(),
            fleet_defaults::RETIRE_AFTER_IDLE
        );
        assert_eq!(fleets.warm_ttl(), fleet_defaults::WARM_TTL);
        // The live default's max is capped at the shard's limit.
        assert_eq!(
            serde_json::to_value(summary.live_default).unwrap(),
            json!({"bufferSize": "20%", "minReplicas": 1, "maxReplicas": 4})
        );

        let [plain]: [ShardSummary; 1] = summarize(vec![config("test", None)], vec![])
            .try_into()
            .unwrap();
        assert_eq!(plain.live_default, None);
        assert_eq!(live_default(None).max_replicas, Some(10));
    }
}
