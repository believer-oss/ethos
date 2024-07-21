use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::{anyhow, Context, Result};
use aws_credential_types::{
    provider::{ProvideCredentials, SharedCredentialsProvider},
    Credentials,
};
use aws_sdk_ecr::{types::ImageIdentifier, Client as EcrClient};
use aws_sdk_eks::Client as EksClient;
use aws_sdk_s3::Client as S3Client;
use aws_sdk_sso::Client as SsoClient;
use aws_sigv4::http_request::{SignableBody, SignableRequest, SignatureLocation, SigningSettings};
use aws_smithy_runtime_api::client::identity::Identity;
use aws_types::region::Region;
use aws_types::SdkConfig;
use base64::prelude::{Engine as _, BASE64_STANDARD, BASE64_URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use http::Request;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::{debug, error, info, instrument, warn};

use crate::auth::sso::{AccessToken, DeviceClient, SsoAccessTokenProvider, TokenExpiredError};
use crate::storage::entry::ArtifactEntry;
use crate::types::config::{AWSConfig, DynamicConfig};
use crate::types::errors::CoreError;
use crate::{AWS_SSO_START_URL, ETHOS_APP_NAME};

static AWS_KEYRING_TOKEN: &str = "aws_sso_token";
static AWS_KEYRING_DEVICE_CLIENT: &str = "aws_device_client";
static AWS_KEYRING_DEVICE_SECRET_FIRST: &str = "aws_device_secret_first";
static AWS_KEYRING_DEVICE_SECRET_SECOND: &str = "aws_device_secret_second";

#[derive(Debug, Clone)]
pub struct SsoConfig {
    config: SdkConfig,
    token_provider: SsoAccessTokenProvider,
    access_token: AccessToken,
}

#[derive(Debug, Clone)]
pub struct AWSAuthContext {
    pub credentials: Credentials,
    pub sdkconfig: SdkConfig,
    pub login_required: bool,

    sso_config: Option<SsoConfig>,
}

#[derive(Debug, Clone)]
pub struct AWSClient {
    pub config: Option<AWSConfig>,

    auth_context: Arc<RwLock<AWSAuthContext>>,
    verification_tx: Option<Sender<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredAccessToken {
    pub access_token: String,
    pub expires_at: DateTime<Utc>,
    pub refresh_token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredDeviceClientInfo {
    pub client_id: String,
    pub registration_expires_at: DateTime<Utc>,
}

impl AWSClient {
    pub async fn new(
        verification_tx: Option<Sender<String>>,
        client_name: String,
        config: AWSConfig,
    ) -> Result<Self> {
        let sso_config = SdkConfig::builder()
            .region(Region::new(crate::AWS_REGION))
            .build();

        let token_provider = SsoAccessTokenProvider::new(&sso_config, client_name).unwrap();

        let access_token = match Self::restore_token(config.clone()) {
            Ok(token) => {
                info!("Restored token. Expires at {:?}", token.expires_at);

                if token.is_expired() {
                    // get new token
                    warn!("Cached token expired, refreshing");
                    let token = match token_provider.refresh_token(token.clone()).await {
                        Ok(token) => token,
                        Err(e) => match e.downcast_ref() {
                            Some(TokenExpiredError) => {
                                warn!("Token can not automatically be refreshed, prompting for login.");
                                token_provider
                                    .get_new_token(&config.sso_start_url, verification_tx.clone())
                                    .await?
                            }
                            _ => return Err(anyhow!("Unable to refresh token: {:?}", e)),
                        },
                    };

                    info!("Refreshed token. Expires at {:?}", token.expires_at);

                    // save token to keyring
                    Self::store_token(token.clone())?;

                    token
                } else {
                    token
                }
            }
            Err(_) => {
                // get new token
                let token = token_provider
                    .get_new_token(&config.sso_start_url, verification_tx.clone())
                    .await?;

                // save token to keyring
                Self::store_token(token.clone())?;

                token
            }
        };

        let shared_config =
            Self::get_config(access_token.clone(), sso_config.clone(), config.clone()).await?;

        Ok(AWSClient {
            auth_context: Arc::new(RwLock::new(AWSAuthContext {
                credentials: shared_config
                    .credentials_provider()
                    .unwrap()
                    .provide_credentials()
                    .await
                    .unwrap(),
                sdkconfig: shared_config,
                sso_config: Some(SsoConfig {
                    config: sso_config,
                    token_provider,
                    access_token,
                }),
                login_required: false,
            })),
            config: Some(config),
            verification_tx,
        })
    }

    pub async fn from_static_creds(
        access_key: &str,
        secret_key: &str,
        session_token: Option<&str>,
    ) -> Self {
        let session_token = session_token.map(|t| t.to_string());
        let creds = Credentials::from_keys(access_key, secret_key, session_token);
        let shared_config = SdkConfig::builder()
            .credentials_provider(SharedCredentialsProvider::new(creds.clone()))
            .region(Region::new(crate::AWS_REGION))
            .build();

        AWSClient {
            auth_context: Arc::new(RwLock::new(AWSAuthContext {
                credentials: creds,
                sdkconfig: shared_config.clone(),
                sso_config: None,
                login_required: false,
            })),
            config: None,
            verification_tx: None,
        }
    }

    pub async fn login_required(&self) -> bool {
        let auth_context = self.auth_context.read().await;
        auth_context.login_required
    }

    pub async fn get_sdk_config(&self) -> SdkConfig {
        self.auth_context.read().await.sdkconfig.clone()
    }

    pub async fn get_credentials(&self) -> Credentials {
        self.auth_context.read().await.credentials.clone()
    }

    pub async fn get_credential_expiration(&self) -> Option<DateTime<Utc>> {
        let sso_config = self.auth_context.read().await.sso_config.clone();
        if let Some(sso_config) = sso_config {
            return Some(sso_config.access_token.expires_at);
        }

        None
    }

    // Returns true when the underlying access token was refreshed
    pub async fn check_config(&self) -> Result<()> {
        debug!("Checking AWS config");
        let sso_config: Option<SsoConfig>;
        let login_required: bool;

        {
            let auth_context = self.auth_context.read().await;
            sso_config = auth_context.sso_config.clone();
            login_required = auth_context.login_required;
        }

        if let Some(sso_config) = sso_config {
            let access_token = sso_config.access_token;

            if access_token.is_expired() && !login_required {
                info!("Access token expired, refreshing");
                self.refresh_token(false).await?
            } else {
                debug!("Access token still valid, or we've already signaled a refresh is needed.");
            }
        }

        Ok(())
    }

    pub async fn refresh_token(&self, allow_prompt: bool) -> Result<()> {
        let mut auth_context = self.auth_context.write().await;

        let sso_config = auth_context.sso_config.clone();
        if let Some(sso_config) = sso_config {
            if !sso_config.access_token.is_expired() {
                return Ok(());
            }

            let new_token = match sso_config
                .token_provider
                .refresh_token(sso_config.access_token.clone())
                .await
            {
                Ok(token) => token,
                Err(e) => match e.downcast_ref() {
                    Some(TokenExpiredError) => {
                        if allow_prompt {
                            sso_config
                                .token_provider
                                .get_new_token(AWS_SSO_START_URL, self.verification_tx.clone())
                                .await?
                        } else {
                            warn!("Token can not automatically be refreshed, prompting for login.");
                            auth_context.login_required = true;

                            return Err(anyhow!("Access token expired"));
                        }
                    }
                    _ => return Err(anyhow!("Unable to refresh token: {:?}", e)),
                },
            };

            // save token to keyring
            Self::store_token(new_token.clone())?;

            auth_context.sso_config = Some(SsoConfig {
                config: sso_config.config.clone(),
                token_provider: sso_config.token_provider.clone(),
                access_token: new_token.clone(),
            });

            // This unwrap is safe because we know the config is Some
            match Self::get_config(
                new_token,
                sso_config.config.clone(),
                self.config.clone().unwrap(),
            )
                .await
            {
                Ok(shared_config) => {
                    match shared_config.credentials_provider() {
                        Some(provider) => match provider.provide_credentials().await {
                            Ok(updated_creds) => {
                                auth_context.credentials = updated_creds;
                            }
                            Err(e) => {
                                return Err(anyhow!(
                                    "Unable to get credentials from provider: {:?}",
                                    e
                                ));
                            }
                        },
                        None => {
                            return Err(anyhow!("Unable to get credentials provider"));
                        }
                    }

                    auth_context.sdkconfig = shared_config;
                    auth_context.login_required = false;
                }
                Err(e) => {
                    error!("Unable to get updated SDK config: {:?}", e);
                }
            }
        }

        Ok(())
    }

    pub async fn get_config(
        token: AccessToken,
        sso_config: SdkConfig,
        aws_config: AWSConfig,
    ) -> Result<SdkConfig> {
        let client = SsoClient::new(&sso_config);
        let role_credentials = client
            .get_role_credentials()
            .role_name(aws_config.role_name.clone())
            .account_id(aws_config.account_id.clone())
            .access_token(token.access_token)
            .send()
            .await?;

        match role_credentials.role_credentials() {
            Some(role_credentials) => {
                let creds = Credentials::from_keys(
                    role_credentials.access_key_id().unwrap(),
                    role_credentials.secret_access_key().unwrap(),
                    Some(role_credentials.session_token().unwrap().to_string()),
                );

                info!("Role credentials: {:?}", role_credentials.expiration);

                Ok(SdkConfig::builder()
                    .credentials_provider(SharedCredentialsProvider::new(creds.clone()))
                    .region(Region::new(crate::AWS_REGION))
                    .build())
            }
            None => Err(anyhow!("Unable to get role credentials")),
        }
    }

    pub async fn get_dynamic_config(&self) -> Result<DynamicConfig, CoreError> {
        self.check_config().await?;

        let client = S3Client::new(&self.get_sdk_config().await);
        let resp = match client
            .get_object()
            .bucket(self.config.clone().unwrap().artifact_bucket_name.clone())
            .key(crate::DYNAMIC_CONFIG_KEY)
            .send()
            .await
        {
            Ok(resp) => resp,
            Err(e) => {
                let e = e.into_service_error();
                error!("Error getting dynamic config: {:?}", e);
                return Err(CoreError(anyhow!("Error getting dynamic config: {:?}", e)));
            }
        };

        let bytes = resp.body.collect().await?.into_bytes();
        let body = std::str::from_utf8(&bytes)?;
        let config: DynamicConfig = serde_json::from_str(body).unwrap();

        Ok(config)
    }

    pub async fn get_dynamic_config_or_exit(&self) -> DynamicConfig {
        match self.get_dynamic_config().await {
            Ok(config) => config,
            Err(e) => {
                error!("Unable to get dynamic config: {:?}", e);
                std::process::exit(1);
            }
        }
    }

    // TODO: This is used by the updater to identify new releases based on the semver. Should we
    // move this to the storage subsystem to unify all of the blob storage?
    pub async fn get_latest_object_key(
        &self,
        app_name: &str,
    ) -> Result<Option<ArtifactEntry>, CoreError> {
        match self.check_config().await {
            Ok(_) => {}
            Err(e) => {
                error!("Error getting object key: {:?}", e);
                return Ok(None);
            }
        }

        let prefix = format!("tauri-{}", app_name.to_lowercase());

        let mut output = vec![];
        let client = S3Client::new(&self.get_sdk_config().await);
        let aws_config = self.config.clone().unwrap();
        let mut paginator = client
            .list_objects_v2()
            .bucket(aws_config.artifact_bucket_name.clone())
            .prefix(prefix)
            .into_paginator()
            .send();

        while let Some(resp) = paginator.next().await {
            if resp.is_err() {
                debug!("Resp: [{:?}", resp);
                return Err(CoreError(anyhow!("Error getting object key: {:?}", resp)));
            };
            for object in resp.unwrap().contents() {
                let entry = ArtifactEntry::from(object.clone());
                output.push(entry);
            }
        }

        // filter for semver-compliant files for the correct platform
        let mut versions: Vec<ArtifactEntry> = output
            .into_iter()
            .filter(|entry| entry.get_semver().is_some())
            .filter(|entry| entry.key.to_string().ends_with(crate::BIN_SUFFIX))
            .collect();

        if versions.is_empty() {
            return Ok(None);
        }

        versions.sort_by_key(|a| a.get_semver().unwrap());
        Ok(Some(versions.last().unwrap().clone()))
    }

    // Ported from: https://github.com/awslabs/aws-sdk-rust/issues/980#issuecomment-1859340980
    #[instrument(skip_all)]
    pub async fn generate_k8s_token<'a>(&self, cluster_name: &str) -> Result<String> {
        let credentials = self.get_credentials().await;
        let expiration = credentials.expiry();
        let region = crate::AWS_REGION;
        let identity = Identity::new(credentials.clone(), expiration);
        let mut signing_settings = SigningSettings::default();
        signing_settings.signature_location = SignatureLocation::QueryParams;
        signing_settings.expires_in = Some(Duration::from_secs(60)); // 1 minute

        let signing_params = match aws_sigv4::sign::v4::SigningParams::builder()
            .identity(&identity)
            .region(region)
            .name("sts")
            .time(SystemTime::now())
            .settings(signing_settings)
            .build()
        {
            Ok(params) => params,
            Err(e) => {
                return Err(anyhow!("Unable to create signing params: {:?}", e));
            }
        };

        // Convert the HTTP request into a signable request
        let url = format!(
            "https://sts.{region}.amazonaws.com/?Action=GetCallerIdentity&Version=2011-06-15"
        );
        let headers = vec![("x-k8s-aws-id", cluster_name)];
        let signable_request = SignableRequest::new(
            "GET",
            url.clone(),
            headers.into_iter(),
            SignableBody::Bytes(&[]),
        )?;

        let (signing_instructions, _signature) = aws_sigv4::http_request::sign(
            signable_request,
            &aws_sigv4::http_request::SigningParams::V4(signing_params),
        )?
            .into_parts();

        // We create a fake request here to create the signed URL
        let mut fake_req = Request::builder()
            .uri(url)
            .body(())
            .expect("empty body request should not fail");

        signing_instructions.apply_to_request_http0x(&mut fake_req);
        let uri = fake_req.uri().to_string();

        Ok(format!(
            "k8s-aws-v1.{}",
            &BASE64_URL_SAFE_NO_PAD.encode(uri)
        ))
    }

    #[instrument]
    pub async fn eks_k8s_cluster_info(
        &self,
        cluster_name: &str,
    ) -> Result<(http::Uri, Vec<Vec<u8>>)> {
        self.check_config().await?;

        debug!("Creating EKS client");
        let client = EksClient::new(&self.get_sdk_config().await);

        debug!("Describing EKS cluster {:#?}", cluster_name);
        let resp = client.describe_cluster().name(cluster_name).send().await?;
        // debug!("EKS describe {:#?}", resp);

        let cluster = resp.cluster().context("Unable to find cluster")?.to_owned();
        let b64_cert = cluster
            .certificate_authority()
            .context("Unable to find certificate authority")?
            .data()
            .context("Unable to find certificate data")?;
        let cert = pem::parse(BASE64_STANDARD.decode(b64_cert)?)?.into_contents();
        let endpoint = cluster
            .endpoint()
            .context("Unable to find endpoint")?
            .parse::<http::Uri>()?;

        debug!("Returning cluster info");
        Ok((endpoint, [cert].to_vec()))
    }

    pub async fn verify_ecr_image_for_commit(&self, commit: String) -> bool {
        let sdk_config = self.get_sdk_config().await;
        let client = EcrClient::new(&sdk_config);
        // v1 uses the full 40-char sha and no linux-server- prepend.
        let tag = match commit.len() {
            40 => commit,
            _ => format!("linux-server-{}", commit),
        };

        let img = client
            .describe_images()
            .repository_name("game")
            .image_ids(ImageIdentifier::builder().image_tag(tag).build())
            .send()
            .await;
        debug!("Image: {:?}", img);

        img.is_ok()
    }

    #[instrument]
    fn restore_token(config: AWSConfig) -> Result<AccessToken> {
        let token_entry = keyring::Entry::new(ETHOS_APP_NAME, AWS_KEYRING_TOKEN)?;
        let token = token_entry.get_password()?;

        let stored_token: StoredAccessToken = serde_json::from_str(&token)?;

        let device_client_entry = keyring::Entry::new(ETHOS_APP_NAME, AWS_KEYRING_DEVICE_CLIENT)?;
        let device_client = device_client_entry.get_password()?;

        let stored_device_client: StoredDeviceClientInfo = serde_json::from_str(&device_client)?;

        let device_secret_first_entry =
            keyring::Entry::new(ETHOS_APP_NAME, AWS_KEYRING_DEVICE_SECRET_FIRST)?;
        let device_secret_first = device_secret_first_entry.get_password()?;

        let device_secret_second_entry =
            keyring::Entry::new(ETHOS_APP_NAME, AWS_KEYRING_DEVICE_SECRET_SECOND)?;
        let device_secret_second = device_secret_second_entry.get_password()?;

        let stored_device_client = DeviceClient {
            client_id: stored_device_client.client_id,
            client_secret: format!("{}{}", device_secret_first, device_secret_second),
            registration_expires_at: stored_device_client.registration_expires_at,
        };

        Ok(AccessToken {
            start_url: config.sso_start_url.clone(),
            region: crate::AWS_REGION.to_string(),
            access_token: stored_token.access_token,
            expires_at: stored_token.expires_at,
            device_client: stored_device_client,
            refresh_token: stored_token.refresh_token,
        })
    }

    #[instrument]
    fn store_token(token: AccessToken) -> Result<()> {
        let stored_token = StoredAccessToken {
            access_token: token.access_token,
            expires_at: token.expires_at,
            refresh_token: token.refresh_token,
        };

        let token_entry = keyring::Entry::new(ETHOS_APP_NAME, AWS_KEYRING_TOKEN)?;
        token_entry.set_password(&serde_json::to_string(&stored_token)?)?;

        // split token device client token in half
        let device_client = token.device_client;
        let stored_device_client = StoredDeviceClientInfo {
            client_id: device_client.client_id,
            registration_expires_at: device_client.registration_expires_at,
        };

        let device_client_entry = keyring::Entry::new(ETHOS_APP_NAME, AWS_KEYRING_DEVICE_CLIENT)?;
        device_client_entry.set_password(&serde_json::to_string(&stored_device_client)?)?;

        // Windows has a character limit on what can be in the keyring, so we split the secret in half
        let device_client_first =
            &device_client.client_secret[..device_client.client_secret.len() / 2];
        let device_client_second =
            &device_client.client_secret[device_client.client_secret.len() / 2..];

        let device_secret_first_entry =
            keyring::Entry::new(ETHOS_APP_NAME, AWS_KEYRING_DEVICE_SECRET_FIRST)?;
        device_secret_first_entry.set_password(device_client_first)?;

        let device_secret_second_entry =
            keyring::Entry::new(ETHOS_APP_NAME, AWS_KEYRING_DEVICE_SECRET_SECOND)?;
        device_secret_second_entry.set_password(device_client_second)?;

        Ok(())
    }
}

pub fn ensure_aws_client(client: Option<AWSClient>) -> Result<AWSClient, CoreError> {
    match client {
        Some(client) => Ok(client),
        None => {
            error!("AWS client not initialized. Double check that AWS configuration is correct in the UI.");
            Err(CoreError(anyhow!("AWS client not initialized. See logs!")))
        }
    }
}
