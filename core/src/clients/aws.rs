use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::{anyhow, Context, Result};
use aws_credential_types::{provider::SharedCredentialsProvider, Credentials};
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
use http::Request;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::{debug, error, instrument};

use crate::types::config::DynamicConfig;
use crate::types::errors::CoreError;

#[derive(Debug, Clone)]
pub struct AWSAuthContext {
    pub credentials: Credentials,
    pub sdkconfig: SdkConfig,
    pub login_required: bool,
    pub expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct AWSClient {
    pub artifact_bucket_name: String,

    auth_context: Arc<RwLock<AWSAuthContext>>,
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
    #[instrument(skip_all)]
    pub async fn from_static_creds(
        access_key: &str,
        secret_key: &str,
        session_token: Option<&str>,
        expires_at: Option<DateTime<Utc>>,
        bucket_name: String,
    ) -> Self {
        let session_token = session_token.map(|t| t.to_string());
        let creds = Credentials::from_keys(access_key, secret_key, session_token);
        let shared_config = SdkConfig::builder()
            .http_client(create_hyper_client())
            .credentials_provider(SharedCredentialsProvider::new(creds.clone()))
            .region(Region::new(crate::AWS_REGION))
            .build();

        AWSClient {
            auth_context: Arc::new(RwLock::new(AWSAuthContext {
                credentials: creds,
                sdkconfig: shared_config.clone(),
                login_required: false,
                expires_at,
            })),
            artifact_bucket_name: bucket_name,
        }
    }

    pub async fn login_required(&self) -> bool {
        let auth_context = self.auth_context.read().await;
        auth_context.login_required
    }

    pub async fn logout(&self) -> Result<(), CoreError> {
        let mut auth_context = self.auth_context.write().await;
        auth_context.login_required = true;
        Ok(())
    }

    pub async fn check_expiration(&self) -> Result<(), CoreError> {
        let auth_context = self.auth_context.read().await;
        if let Some(expires_at) = auth_context.expires_at {
            if expires_at < Utc::now() {
                return Err(CoreError::Internal(anyhow!("Credentials have expired")));
            }
        }

        Ok(())
    }

    pub async fn get_credential_expiration(&self) -> Option<DateTime<Utc>> {
        let auth_context = self.auth_context.read().await;
        auth_context.expires_at
    }

    pub async fn get_sdk_config(&self) -> SdkConfig {
        self.auth_context.read().await.sdkconfig.clone()
    }

    pub async fn get_credentials(&self) -> Credentials {
        self.auth_context.read().await.credentials.clone()
    }

    pub fn get_artifact_bucket(&self) -> String {
        self.artifact_bucket_name.clone()
    }

    pub async fn get_dynamic_config(&self) -> Result<DynamicConfig, CoreError> {
        let client = S3Client::new(&self.get_sdk_config().await);
        let resp = match client
            .get_object()
            .bucket(self.artifact_bucket_name.clone())
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

    #[instrument(skip(self), err)]
    pub async fn list_all_objects(&self, prefix: &str) -> Result<Vec<String>, CoreError> {
        let mut output = vec![];
        let client = S3Client::new(&self.get_sdk_config().await);
        let mut paginator = client
            .list_objects_v2()
            .bucket(self.artifact_bucket_name.clone())
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
    pub async fn download_object_to_path(
        &self,
        path: &str,
        object_key: &str,
    ) -> Result<String, CoreError> {
        let client = S3Client::new(&self.get_sdk_config().await);

        let get_object_output = client
            .get_object()
            .bucket(self.artifact_bucket_name.clone())
            .key(object_key)
            .send()
            .await
            .map_err(|e| {
                CoreError::Internal(anyhow!(
                    "Failed to get object from S3: {}",
                    e.into_service_error().to_string()
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

        let bucket_name = self.artifact_bucket_name.clone();

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
                    e.into_service_error().to_string()
                ))
            })?;

        Ok(object_key)
    }

    // Ported from: https://github.com/awslabs/aws-sdk-rust/issues/980#issuecomment-1859340980
    #[instrument(skip_all)]
    pub async fn generate_k8s_token<'a>(&self, cluster_name: &str, region: &str) -> Result<String> {
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

        Ok(format!(
            "k8s-aws-v1.{}",
            &BASE64_URL_SAFE_NO_PAD.encode(uri)
        ))
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
