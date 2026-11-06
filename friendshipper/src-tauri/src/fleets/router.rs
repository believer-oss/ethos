use std::cmp::Reverse;
use std::collections::BTreeMap;

use anyhow::anyhow;
use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use ethos_core::clients::kube::ensure_kube_client;
use ethos_core::types::errors::CoreError;
use fleet_api::{
    ChannelKey, Promotion, PromotionSpec, Shard, ShardConfig, ShardConfigSpec, ShardStatus,
};
use serde::{Deserialize, Serialize};
use tracing::{info, instrument};

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
}

/// A shard as the UI shows it: its config joined with its observed status. Either half may be
/// missing, e.g. while the coordinator has yet to create the `Shard` for a new `ShardConfig`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShardSummary {
    pub name: String,
    pub display_name: String,
    pub config: Option<ShardConfigSpec>,
    pub status: Option<ShardStatus>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShardDetail {
    #[serde(flatten)]
    pub shard: ShardSummary,
    /// Running first, then the most recent.
    pub promotions: Vec<Promotion>,
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
    let mut by_name: BTreeMap<String, (Option<ShardConfigSpec>, Option<ShardStatus>)> =
        BTreeMap::new();
    for config in configs {
        let name = config
            .metadata
            .name
            .unwrap_or(config.spec.shard_name.clone());
        by_name.entry(name).or_default().0 = Some(config.spec);
    }
    for shard in shards {
        let Some(name) = shard.metadata.name else {
            continue;
        };
        by_name.entry(name).or_default().1 = Some(shard.status.unwrap_or_default());
    }
    by_name
        .into_iter()
        .map(|(name, (config, status))| ShardSummary {
            display_name: config
                .as_ref()
                .and_then(|c| c.display_name.clone())
                .filter(|d| !d.is_empty())
                .unwrap_or_else(|| name.clone()),
            name,
            config,
            status,
        })
        .collect()
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
    let kube_client = ensure_kube_client(state.kube_client.read().clone())?;
    let (config, shard, mut promotions) = futures::try_join!(
        kube_client.get_shard_config(&name),
        kube_client.get_shard(&name),
        kube_client.list_promotions(Some(&name), None),
    )?;
    if config.is_none() && shard.is_none() {
        return Err(CoreError::Input(anyhow!("no shard named {name:?}")));
    }
    sort_promotions(&mut promotions);
    let summary = summarize(config.into_iter().collect(), shard.into_iter().collect())
        .pop()
        .ok_or_else(|| anyhow!("no shard named {name:?}"))?;
    Ok(Json(ShardDetail {
        shard: summary,
        promotions,
    }))
}

#[instrument(skip(state))]
async fn get_promotions<T>(
    State(state): State<AppState<T>>,
    Query(params): Query<GetPromotionsParams>,
) -> Result<Json<Vec<Promotion>>, CoreError>
where
    T: EngineProvider,
{
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
    require_promotion_writes(&state)?;
    let requested_by = require_promoter_name(&state)?;

    let kube_client = ensure_kube_client(state.kube_client.read().clone())?;
    info!("Requesting cancel of promotion {name} for {requested_by}");
    kube_client.cancel_promotion(&name, &requested_by).await
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

    fn message(result: Result<(), CoreError>) -> String {
        result.unwrap_err().to_string()
    }

    #[tokio::test]
    async fn writes_need_the_switch() {
        assert!(message(create(state(false, true)).await).contains("promotionViaCoordinator"));
        assert!(message(cancel(state(false, true)).await).contains("promotionViaCoordinator"));
    }

    #[tokio::test]
    async fn writes_need_release_controls() {
        assert!(message(create(state(true, false)).await).contains("Release controls"));
        assert!(message(cancel(state(true, false)).await).contains("Release controls"));
    }

    #[tokio::test]
    async fn writes_pass_the_gate_with_both_on() {
        // Past the gate, they stop at the missing kube client rather than at the gate.
        for result in [
            create(state(true, true)).await,
            cancel(state(true, true)).await,
        ] {
            let message = message(result);
            assert!(!message.contains("promotionViaCoordinator"), "{message}");
            assert!(!message.contains("Release controls"), "{message}");
        }
    }
}
