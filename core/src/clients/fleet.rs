//! Fleet objects (`ShardConfig`, `Shard`, `Promotion`) in the `fleet-system` namespace.
//!
//! Friendshipper reads all three, creates `Promotion`s and asks to cancel them; the coordinator does
//! the rest.
//! See the `fleet-api` crate for the contract.

use anyhow::anyhow;
use fleet_api::{
    is_full_sha, ChannelKey, GameConfig, Promotion, PromotionSpec, Shard, ShardConfig,
    DEFAULT_NAMESPACE, LABEL_CHANNEL, LABEL_SHARD,
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Time;
use k8s_openapi::jiff::Timestamp;
use kube::api::{ListParams, Patch, PatchParams, PostParams};
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
}
