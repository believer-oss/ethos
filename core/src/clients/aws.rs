use std::fs;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::{anyhow, Context, Result};
use aws_credential_types::provider::{future, ProvideCredentials, SharedCredentialsProvider};
use aws_credential_types::Credentials;
use aws_sdk_ecr::{types::ImageIdentifier, Client as EcrClient};
use aws_sdk_eks::Client as EksClient;
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::Client as S3Client;
use aws_sigv4::http_request::{SignableBody, SignableRequest, SignatureLocation, SigningSettings};
use aws_smithy_runtime::client::http::hyper_014::HyperClientBuilder;
use aws_smithy_runtime_api::client::identity::Identity;
use aws_types::region::Region;
use aws_types::SdkConfig;
use base64::prelude::{Engine as _, BASE64_STANDARD, BASE64_URL_SAFE_NO_PAD};
use bytes::Buf;
use chrono::{DateTime, Utc};
use directories_next::BaseDirs;
use http::Request;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use tracing::{debug, error, instrument};

use crate::types::config::DynamicConfig;
use crate::types::errors::CoreError;

/// Everything a credential refresh replaces. Shared so that clones handed out earlier
/// see the new session - a download can outlast the token it started with.
#[derive(Debug, Clone)]
pub struct AWSClientContext {
    pub credentials: Credentials,
    pub login_required: bool,
    pub expires_at: Option<DateTime<Utc>>,
    pub artifact_bucket_name: String,
    pub promoted_artifact_bucket_name: String,
}

/// How long credentials whose expiry we do not know are served for.
///
/// The SDK caches what a provider hands it and only asks again once that has expired;
/// credentials carrying no expiry at all are held for its own default, which is fifteen
/// minutes. That is a long time to keep using a session that may already have been
/// replaced, so an unknown expiry is reported as a short one instead. The cost is one
/// lock read per minute.
const UNKNOWN_EXPIRY_TTL: Duration = Duration::from_secs(60);

/// Serves whatever session the client's context holds *now*, rather than a copy taken
/// when the client was built.
///
/// This is what lets a transfer outlive the session it started with: the SDK re-resolves
/// through here once its cached copy expires, so a re-login that swaps the context is
/// picked up by the next request without the in-flight operation being handed anything.
#[derive(Debug, Clone)]
struct RefreshableCredentials {
    context: Arc<RwLock<AWSClientContext>>,
}

impl RefreshableCredentials {
    fn resolve(&self) -> Credentials {
        let context = self.context.read();

        // Always report an expiry, even when we do not have one: see UNKNOWN_EXPIRY_TTL.
        let expiry = context
            .expires_at
            .map(SystemTime::from)
            .unwrap_or_else(|| SystemTime::now() + UNKNOWN_EXPIRY_TTL);

        Credentials::new(
            context.credentials.access_key_id(),
            context.credentials.secret_access_key(),
            context.credentials.session_token().map(str::to_string),
            Some(expiry),
            "friendshipper-refreshable",
        )
    }
}

impl ProvideCredentials for RefreshableCredentials {
    fn provide_credentials<'a>(&'a self) -> future::ProvideCredentials<'a>
    where
        Self: 'a,
    {
        future::ProvideCredentials::ready(Ok(self.resolve()))
    }
}

#[derive(Debug, Clone)]
pub struct AWSClient {
    context: Arc<RwLock<AWSClientContext>>,

    /// Holds the provider above, not a credential snapshot, so it stays valid across a
    /// refresh and never needs replacing.
    sdkconfig: SdkConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct S3ObjectEntry {
    pub key: String,
    pub size: i64,
    pub last_modified: Option<DateTime<Utc>>,
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

/// Parses a promoted-build metadata object: plain text whose entire content is a
/// commit SHA. An empty body is a broken deploy marker, not a valid SHA. The SHA's
/// format is the producer's contract, so it is not validated here.
fn parse_metadata_body(body: &str) -> Result<String, CoreError> {
    // First whitespace-delimited token, not the whole trimmed body: an object with a
    // trailing comment or second line would otherwise yield a "sha" that breaks the
    // GitHub link while still looking plausible in the UI.
    let trimmed = body.split_whitespace().next().unwrap_or_default();
    if trimmed.is_empty() {
        return Err(CoreError::Internal(anyhow!(
            "Promoted build metadata object was empty"
        )));
    }

    Ok(trimmed.to_string())
}

impl AWSClient {
    #[instrument(skip_all)]
    pub async fn from_static_creds(
        access_key: &str,
        secret_key: &str,
        session_token: Option<&str>,
        expires_at: Option<DateTime<Utc>>,
        bucket_name: String,
        input_promoted_artifact_bucket_name: String,
    ) -> Self {
        let session_token = session_token.map(|t| t.to_string());
        let creds = Credentials::from_keys(access_key, secret_key, session_token);

        // Order matters: the context owns the session, and the config borrows it through
        // the provider. Building the config from the credentials directly would pin this
        // session into every client made from it.
        let context = Arc::new(RwLock::new(AWSClientContext {
            credentials: creds,
            login_required: false,
            expires_at,
            artifact_bucket_name: bucket_name,
            promoted_artifact_bucket_name: input_promoted_artifact_bucket_name,
        }));

        let sdkconfig = SdkConfig::builder()
            .http_client(create_hyper_client())
            .credentials_provider(SharedCredentialsProvider::new(RefreshableCredentials {
                context: context.clone(),
            }))
            .region(Region::new(crate::AWS_REGION))
            .build();

        AWSClient { context, sdkconfig }
    }

    /// Adopt another client's session in place, so an operation already in flight picks
    /// up the refreshed credentials rather than the ones it started with.
    ///
    /// Only the session moves. Every `SdkConfig` already handed out resolves through the
    /// provider, which reads this same context, so none of them need replacing - which is
    /// what makes the refresh reach an operation that is already running.
    pub fn refresh_from(&self, other: &AWSClient) {
        if Arc::ptr_eq(&self.context, &other.context) {
            return;
        }

        let refreshed = other.context.read().clone();
        *self.context.write() = refreshed;
    }

    pub async fn login_required(&self) -> bool {
        self.context.read().login_required
    }

    pub async fn logout(&self) -> Result<(), CoreError> {
        self.context.write().login_required = true;
        Ok(())
    }

    pub async fn check_expiration(&self) -> Result<(), CoreError> {
        let expires_at = self.context.read().expires_at;
        if let Some(expires_at) = expires_at {
            if expires_at < Utc::now() {
                return Err(CoreError::Internal(anyhow!("Credentials have expired")));
            }
        }

        Ok(())
    }

    pub async fn get_credential_expiration(&self) -> Option<DateTime<Utc>> {
        self.context.read().expires_at
    }

    pub async fn get_sdk_config(&self) -> SdkConfig {
        self.sdkconfig.clone()
    }

    /// S3 access for the longtail block store, carrying the same refreshable provider as
    /// everything else here - so a transfer long enough to outlive its session keeps
    /// going rather than failing on an expired token.
    ///
    /// `transfer_acceleration` is a parameter because this type has no view of the app
    /// config and the caller does.
    pub fn longtail_s3_options(&self, transfer_acceleration: bool) -> longtail::S3Options {
        longtail::S3Options {
            sdk_config: Some(self.sdkconfig.clone()),
            region: Some(crate::AWS_REGION.to_string()),
            transfer_acceleration,
            ..Default::default()
        }
    }

    pub async fn get_credentials(&self) -> Credentials {
        self.context.read().credentials.clone()
    }

    pub fn get_artifact_bucket(&self) -> String {
        self.context.read().artifact_bucket_name.clone()
    }
    pub fn get_promoted_artifacts_bucket(&self) -> String {
        self.context.read().promoted_artifact_bucket_name.clone()
    }
    pub async fn get_dynamic_config(&self) -> Result<DynamicConfig, CoreError> {
        let client = S3Client::new(&self.get_sdk_config().await);
        let resp = match client
            .get_object()
            .bucket(self.get_artifact_bucket())
            .key(crate::DYNAMIC_CONFIG_KEY)
            .send()
            .await
        {
            Ok(resp) => resp,
            Err(e) => {
                let e = e.into_service_error();
                error!("Error getting dynamic config: {:?}", e);
                return Err(CoreError::Internal(anyhow!(
                    "Error getting dynamic config: {:?}",
                    e
                )));
            }
        };

        let bytes = resp.body.collect().await?.into_bytes();
        let body = std::str::from_utf8(&bytes)?;
        let config: serde_json::Value = serde_json::from_str(body)?;
        let app_name = "Friendshipper";

        let config_override: Option<Result<DynamicConfig, anyhow::Error>> = BaseDirs::new()
            .and_then(|b| {
                let override_file = b.config_dir().join(app_name).join("dynamic-config.json");
                debug!(
                    "Checking if we should load dynamic config from {:?}",
                    override_file
                );
                override_file.exists().then(|| {
                    debug!("Loading dynamic config from {:?}", override_file);
                    let override_str = fs::read_to_string(override_file)?;
                    let override_val: serde_json::Value = serde_json::from_str(&override_str)?;
                    let mut override_config = config.clone();
                    override_config
                        .as_object_mut()
                        .unwrap()
                        .extend(override_val.as_object().unwrap().to_owned());
                    Ok(serde_json::from_value(override_config)?)
                })
            });

        let config = match config_override {
            Some(Ok(config)) => config,
            Some(Err(e)) => {
                debug!("Failed to load dynamic config: {:?}", e);
                serde_json::from_value(config)?
            }
            None => serde_json::from_value(config)?,
        };

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

    /// Reads a plain-text object, returning its trimmed body and last-modified time.
    ///
    /// The bucket is a parameter rather than a field: it has three possible sources
    /// and resolving that precedence belongs to the caller.
    #[instrument(skip(self), err)]
    pub async fn read_object_to_string(
        &self,
        bucket: &str,
        key: &str,
    ) -> Result<(String, Option<DateTime<Utc>>), CoreError> {
        // A blank bucket is reachable: the server config field is an Option and the
        // build-time constant defaults to empty. The SDK's own error for this is an
        // opaque ConstructionFailure, so fail here instead.
        if bucket.is_empty() {
            return Err(CoreError::Internal(anyhow!(
                "No promoted artifact bucket configured. Set promotedArtifactBucketName in \
                 dynamic config, or promotedArtifactBucketName in the Friendshipper server \
                 config, or the PROMOTED_ARTIFACT_BUCKET_NAME build-time variable."
            )));
        }

        let client = S3Client::new(&self.get_sdk_config().await);
        let resp = match client
            .get_object()
            .bucket(bucket.to_string())
            .key(key)
            .send()
            .await
        {
            Ok(resp) => resp,
            Err(e) => {
                let e = e.into_service_error();
                error!("Error getting promoted build metadata object: {:?}", e);
                return Err(CoreError::Internal(anyhow!(
                    "Error getting promoted build metadata object: {:?}",
                    e
                )));
            }
        };

        // Capture last_modified before collect() consumes the body.
        let last_modified = resp
            .last_modified()
            .and_then(|t| DateTime::<Utc>::from_timestamp(t.secs(), t.subsec_nanos()));

        let bytes = resp.body.collect().await?.into_bytes();
        let body = std::str::from_utf8(&bytes)?;
        let sha = parse_metadata_body(body)?;

        Ok((sha, last_modified))
    }

    #[instrument(skip(self), err)]
    pub async fn list_all_objects(&self, prefix: &str) -> Result<Vec<String>, CoreError> {
        let mut output = vec![];
        let client = S3Client::new(&self.get_sdk_config().await);
        let mut paginator = client
            .list_objects_v2()
            .bucket(self.get_artifact_bucket())
            .prefix(prefix)
            .into_paginator()
            .send();

        while let Some(resp) = paginator.next().await {
            if resp.is_err() {
                debug!("Resp: [{:?}", resp);
                return Err(CoreError::Internal(anyhow!(
                    "Error listing objects: {:?}",
                    resp
                )));
            };
            for object in resp.unwrap().contents() {
                let entry = object.clone().key;
                match entry {
                    Some(entry) => output.push(entry.clone()),
                    None => {
                        return Err(CoreError::Internal(anyhow!(
                            "Error getting key from object: {:?}",
                            object
                        )))
                    }
                }
            }
        }

        Ok(output)
    }

    #[instrument(skip(self), err)]
    pub async fn list_common_prefixes(
        &self,
        prefix: &str,
        delimiter: &str,
    ) -> Result<Vec<String>, CoreError> {
        let mut output = vec![];
        let client = S3Client::new(&self.get_sdk_config().await);
        let mut paginator = client
            .list_objects_v2()
            .bucket(self.get_artifact_bucket())
            .prefix(prefix)
            .delimiter(delimiter)
            .into_paginator()
            .send();

        while let Some(resp) = paginator.next().await {
            let page = resp.map_err(|e| {
                CoreError::Internal(anyhow!("Error listing common prefixes: {:?}", e))
            })?;
            for cp in page.common_prefixes() {
                if let Some(p) = cp.prefix() {
                    output.push(p.to_string());
                }
            }
        }

        Ok(output)
    }

    #[instrument(skip(self), err)]
    pub async fn list_objects_with_metadata(
        &self,
        prefix: &str,
    ) -> Result<Vec<S3ObjectEntry>, CoreError> {
        let mut output = vec![];
        let client = S3Client::new(&self.get_sdk_config().await);
        let mut paginator = client
            .list_objects_v2()
            .bucket(self.get_artifact_bucket())
            .prefix(prefix)
            .into_paginator()
            .send();

        while let Some(resp) = paginator.next().await {
            let page =
                resp.map_err(|e| CoreError::Internal(anyhow!("Error listing objects: {:?}", e)))?;
            for object in page.contents() {
                let key = match object.key() {
                    Some(k) => k.to_string(),
                    None => continue,
                };
                let last_modified = object
                    .last_modified()
                    .and_then(|t| DateTime::<Utc>::from_timestamp(t.secs(), t.subsec_nanos()));
                output.push(S3ObjectEntry {
                    key,
                    size: object.size().unwrap_or(0),
                    last_modified,
                });
            }
        }

        Ok(output)
    }

    #[instrument(skip(self), err)]
    pub async fn download_object_to_path(
        &self,
        path: &str,
        object_key: &str,
    ) -> Result<String, CoreError> {
        let client = S3Client::new(&self.get_sdk_config().await);

        let get_object_output = client
            .get_object()
            .bucket(self.get_artifact_bucket())
            .key(object_key)
            .send()
            .await
            .map_err(|e| {
                CoreError::Internal(anyhow!(
                    "Failed to get object from S3: {}",
                    e.into_service_error()
                ))
            })?;

        let body =
            get_object_output.body.collect().await.map_err(|e| {
                CoreError::Internal(anyhow!("Failed to collect object body: {}", e))
            })?;

        let mut file = std::fs::File::create(path)
            .map_err(|e| CoreError::Internal(anyhow!("Failed to create file: {}", e)))?;

        let mut reader = body.into_bytes().reader();
        std::io::copy(&mut reader, &mut file)
            .map_err(|e| CoreError::Internal(anyhow!("Failed to write to file: {}", e)))?;

        Ok(path.to_string())
    }

    #[instrument(skip(self), err)]
    pub async fn upload_object(
        &self,
        file_path: &str,
        destination_prefix: &str,
    ) -> Result<String, CoreError> {
        let client = S3Client::new(&self.get_sdk_config().await);

        let file_name = std::path::Path::new(file_path)
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| CoreError::Internal(anyhow!("Invalid file path")))?;

        let object_key = format!("{}/{}", destination_prefix.trim_end_matches('/'), file_name);

        let bucket_name = self.get_artifact_bucket();

        client
            .put_object()
            .bucket(bucket_name)
            .key(&object_key)
            .body(ByteStream::from_path(file_path).await?)
            .send()
            .await
            .map_err(|e| {
                CoreError::Internal(anyhow!(
                    "Failed to upload object to S3: {}",
                    e.into_service_error()
                ))
            })?;

        Ok(object_key)
    }

    // Ported from: https://github.com/awslabs/aws-sdk-rust/issues/980#issuecomment-1859340980
    #[instrument(skip_all)]
    pub async fn generate_k8s_token(&self, cluster_name: &str, region: &str) -> Result<String> {
        let credentials = self.get_credentials().await;
        let expiration = credentials.expiry();
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

        Ok(format!("k8s-aws-v1.{}", BASE64_URL_SAFE_NO_PAD.encode(uri)))
    }

    #[instrument]
    pub async fn eks_k8s_cluster_info(
        &self,
        cluster_name: &str,
        region: &str,
    ) -> Result<(http::Uri, Vec<Vec<u8>>), CoreError> {
        debug!("Creating EKS client");

        let region = region.to_string();

        let current_sdk_config = self.get_sdk_config().await;
        let region = Region::new(region);
        let sdk_config = SdkConfig::builder()
            .http_client(create_hyper_client())
            .credentials_provider(current_sdk_config.credentials_provider().unwrap())
            .region(region)
            .build();
        let client = EksClient::new(&sdk_config);

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
            _ => format!("linux-server-{commit}"),
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
}

pub fn ensure_aws_client(client: Option<AWSClient>) -> Result<AWSClient, CoreError> {
    match client {
        Some(client) => Ok(client),
        None => {
            error!("AWS client not initialized. Double check that AWS configuration is correct in the UI.");
            Err(CoreError::Internal(anyhow!(
                "AWS client not initialized. See logs!"
            )))
        }
    }
}

pub fn create_hyper_client() -> aws_sdk_ssooidc::config::SharedHttpClient {
    let tls_connector = hyper_rustls::HttpsConnectorBuilder::new()
        .with_webpki_roots()
        .https_only()
        .enable_http1()
        .enable_http2()
        .build();

    HyperClientBuilder::new().build(tls_connector)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn client(
        access_key: &str,
        session_token: &str,
        expires_at: Option<DateTime<Utc>>,
    ) -> AWSClient {
        AWSClient::from_static_creds(
            access_key,
            "secret",
            Some(session_token),
            expires_at,
            "bucket".to_string(),
            "promoted-bucket".to_string(),
        )
        .await
    }

    async fn resolve(config: &SdkConfig) -> Credentials {
        config
            .credentials_provider()
            .expect("config carries a credentials provider")
            .provide_credentials()
            .await
            .expect("credentials resolve")
    }

    /// The property the whole download path depends on: a config captured before a
    /// re-login serves the session from after it.
    #[tokio::test]
    async fn provider_serves_the_current_session() {
        let aws = client("AKIAONE", "token-one", None).await;

        // Taken before the refresh, and deliberately never re-read from the client.
        let config = aws.get_sdk_config().await;
        assert_eq!(resolve(&config).await.access_key_id(), "AKIAONE");

        aws.refresh_from(&client("AKIATWO", "token-two", None).await);

        let after = resolve(&config).await;
        assert_eq!(after.access_key_id(), "AKIATWO");
        assert_eq!(after.session_token(), Some("token-two"));
    }

    /// Resolving straight off the config skips the SDK's credentials cache, so this pins
    /// "the provider reads live state", not "the SDK re-consults it". The latter is the
    /// library's own contract and is tested there.
    #[tokio::test]
    async fn credentials_always_carry_an_expiry() {
        let known = Utc::now() + chrono::Duration::hours(1);
        let with_expiry = client("AKIAONE", "token-one", Some(known)).await;
        assert_eq!(
            resolve(&with_expiry.get_sdk_config().await).await.expiry(),
            Some(SystemTime::from(known))
        );

        // Without this the SDK caches a no-expiry identity for its own default of fifteen
        // minutes, and a refresh inside that window goes unnoticed.
        let without = client("AKIAONE", "token-one", None).await;
        let expiry = resolve(&without.get_sdk_config().await)
            .await
            .expiry()
            .expect("an unknown expiry is still reported as one");
        assert!(expiry <= SystemTime::now() + UNKNOWN_EXPIRY_TTL);
    }

    #[tokio::test]
    async fn refresh_is_visible_to_clones() {
        let aws = client("AKIAONE", "token-one", None).await;
        let clone = aws.clone();

        aws.refresh_from(&client("AKIATWO", "token-two", None).await);

        let credentials = resolve(&clone.get_sdk_config().await).await;
        assert_eq!(credentials.access_key_id(), "AKIATWO");
        assert_eq!(clone.get_artifact_bucket(), "bucket");
    }

    #[tokio::test]
    async fn s3_options_carry_the_provider_and_leak_nothing() {
        let aws = client("AKIAONE", "token-one", None).await;

        let options = aws.longtail_s3_options(true);
        assert!(options.sdk_config.is_some());
        assert_eq!(options.region.as_deref(), Some(crate::AWS_REGION));
        assert!(options.transfer_acceleration);
        assert!(!aws.longtail_s3_options(false).transfer_acceleration);

        // These options cross a crate boundary and end up in longtail's own logs.
        let rendered = format!("{options:?}");
        assert!(!rendered.contains("secret"), "{rendered}");
        assert!(!rendered.contains("token-one"), "{rendered}");
    }

    // Synthetic, not a real SHA: this repo is public.
    fn fake_sha() -> String {
        "a".repeat(40)
    }

    /// A trailing comment or second line must not end up inside the sha.
    #[test]
    fn parse_metadata_body_takes_only_the_first_token() {
        let body = format!(
            "{}
# promoted by pipeline
",
            fake_sha()
        );
        assert_eq!(parse_metadata_body(&body).unwrap(), fake_sha());
    }

    #[test]
    fn parse_metadata_body_trims_trailing_newline() {
        let body = format!("{}\n", fake_sha());
        assert_eq!(parse_metadata_body(&body).unwrap(), fake_sha());
    }

    #[test]
    fn parse_metadata_body_trims_surrounding_whitespace() {
        let body = format!("  {}  \n\t", fake_sha());
        assert_eq!(parse_metadata_body(&body).unwrap(), fake_sha());
    }

    #[test]
    fn parse_metadata_body_accepts_body_with_no_whitespace() {
        let body = fake_sha();
        assert_eq!(parse_metadata_body(&body).unwrap(), fake_sha());
    }

    #[test]
    fn parse_metadata_body_rejects_empty_body() {
        assert!(parse_metadata_body("").is_err());
    }

    #[test]
    fn parse_metadata_body_rejects_whitespace_only_body() {
        assert!(parse_metadata_body("   \n\t ").is_err());
    }

    #[test]
    fn parse_metadata_body_does_not_validate_shape() {
        // SHA format is the producer's contract, not this layer's.
        assert_eq!(parse_metadata_body("not-a-sha").unwrap(), "not-a-sha");
    }
}
