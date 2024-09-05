use anyhow::{anyhow, Result};
use async_trait::async_trait;
use aws_sdk_s3::Client;
use tracing::warn;

use crate::storage::{entry::ArtifactEntry, list::MethodPrefix, ArtifactProvider};
use crate::types::errors::CoreError;
use crate::AWSClient;

// AWS S3 storage provider
#[derive(Debug)]
pub struct S3ArtifactProvider {
    s3_bucket: String,
    aws_client: AWSClient,
}

impl S3ArtifactProvider {
    pub fn new(aws_client: &AWSClient, s3_bucket: &str) -> Self {
        Self {
            s3_bucket: s3_bucket.to_string(),
            aws_client: aws_client.clone(),
        }
    }
}

#[async_trait]
impl ArtifactProvider for S3ArtifactProvider {
    fn get_method_prefix(&self) -> MethodPrefix {
        format!("s3://{}/", self.s3_bucket.clone()).into()
    }

    async fn get_artifact_by_prefix(&self, prefix: &str) -> Result<String, CoreError> {
        self.aws_client.check_config().await?;
        let client = Client::new(&self.aws_client.get_sdk_config().await);

        let res = client
            .list_objects_v2()
            .bucket(self.s3_bucket.clone())
            .prefix(prefix)
            .delimiter("/")
            .send()
            .await?;

        match res.key_count() {
            None => {
                return Err(CoreError::Internal(anyhow!(
                    "No object found in bucket with prefix {}",
                    prefix
                )));
            }
            Some(1) => {
                let contents = res.contents().first().unwrap();
                let key = contents.key.as_ref().expect("no key found").to_string();
                return Ok(format!("s3://{}/{}", self.s3_bucket, key));
            }
            Some(c) => {
                return Err(CoreError::Internal(anyhow!(
                    "Multiple object found! Prefix {prefix} returned {c} objects, expected 1",
                )));
            }
        }
    }

    async fn get_artifact_list(
        &self,
        path: &str,
    ) -> Result<(MethodPrefix, Vec<ArtifactEntry>), CoreError> {
        self.aws_client.check_config().await?;
        let client = Client::new(&self.aws_client.get_sdk_config().await);

        let mut paginator = client
            .list_objects_v2()
            .bucket(self.s3_bucket.clone())
            .prefix(path)
            .delimiter("/")
            .into_paginator()
            .send();

        let mut entry_list = Vec::new();

        while let Some(resp) = paginator.next().await {
            if resp.is_err() {
                if let Err(err) = resp {
                    warn!("Error getting list of objects from S3: [{:?}", err);
                    if let Some(sdk_error) = err.as_service_error() {
                        if sdk_error.meta().code() == Some("ExpiredToken") {
                            return Err(CoreError::Unauthorized);
                        }
                    }

                    return Err(CoreError::Internal(anyhow!(
                        "Error getting list of objects from S3: {:?}",
                        err
                    )));
                }
            }
            for object in resp.unwrap().contents() {
                let entry = ArtifactEntry::from(object.clone());
                entry_list.push(entry);
            }
        }

        entry_list.sort_by(|a, b| b.last_modified.partial_cmp(&a.last_modified).unwrap());
        Ok((self.get_method_prefix(), entry_list))
    }
}

mod tests {
    #[allow(unused_imports)]
    use crate::storage::s3::S3ArtifactProvider;
    #[allow(unused_imports)]
    use crate::storage::*;
    #[allow(unused_imports)]
    use crate::AWSClient;

    #[allow(dead_code)]
    static ACCESS_KEY: &str = match option_env!("ACCESS_KEY") {
        Some(v) => v,
        None => "",
    };
    #[allow(dead_code)]
    static SECRET_KEY: &str = match option_env!("SECRET_KEY") {
        Some(v) => v,
        None => "",
    };
    #[allow(dead_code)]
    static TEST_BUCKET: &str = match option_env!("TEST_BUCKET") {
        Some(v) => v,
        None => "",
    };

    #[allow(dead_code)]
    async fn get_credentials() -> anyhow::Result<aws_config::SdkConfig> {
        let credentials = aws_config::load_from_env().await;
        Ok(credentials)
    }

    #[allow(dead_code)]
    fn get_test_bucket() -> String {
        TEST_BUCKET.to_string()
    }

    // Ignoring this test in ci for now. We need a stable location with a known len to test this
    // regularly.
    #[ignore]
    #[tokio::test]
    async fn test_s3_provider() {
        let aws_client = AWSClient::from_static_creds(ACCESS_KEY, SECRET_KEY, None).await;
        let bucket = get_test_bucket();
        let s3 = S3ArtifactProvider::new(&aws_client, &bucket);
        let schema = "v0".parse().unwrap();
        let storage = ArtifactStorage::new(Arc::new(s3), schema);
        let ac = ArtifactConfig::new(
            "believerco-gameprototypemp".into(),
            ArtifactKind::Editor,
            ArtifactBuildConfig::Development,
            Platform::Win64,
        );
        let al = storage.artifact_list(ac).await;
        al.entries.iter().for_each(|e| println!("Entry: {:?}", e));
        assert!(!al.entries.is_empty());
    }

    // Ignoring this test in ci for now. We need a stable location with a known contents to test this
    // regularly.
    #[ignore]
    #[tokio::test]
    async fn test_artifact_type_not_exists() {
        let aws_client = AWSClient::from_static_creds(ACCESS_KEY, SECRET_KEY, None).await;
        let bucket = get_test_bucket();
        let s3 = S3ArtifactProvider::new(&aws_client, &bucket);
        let schema = "v1".parse().unwrap();
        let storage = ArtifactStorage::new(Arc::new(s3), schema);
        let ac = ArtifactConfig::new(
            "believerco-gameprototypemp".into(),
            ArtifactKind::Engine, // Engine artifacts are not found in the game repos
            ArtifactBuildConfig::Development,
            Platform::Win64,
        );
        let al = storage.artifact_list(ac).await;
        al.entries.iter().for_each(|e| println!("Entry: {:?}", e));
        assert_eq!(al.entries.len(), 0);
    }

    // Ignoring this test in ci for now. We need a stable location with a known contents to test this
    // regularly.
    #[ignore]
    #[tokio::test]
    async fn test_get_artifact_by_prefix() {
        let aws_client = AWSClient::from_static_creds(ACCESS_KEY, SECRET_KEY, None).await;
        let bucket = get_test_bucket();
        let s3 = S3ArtifactProvider::new(&aws_client, &bucket);
        let prefix = "v1/cache/linux/linux-build.json"; // this is just a known prefix
        let artifact = s3.get_artifact_by_prefix(prefix).await;
        assert!(artifact.is_ok());
    }

    // Ignoring this test in ci for now. We need a stable location with a known contents to test this
    // regularly.
    #[ignore]
    #[tokio::test]
    async fn test_get_artifact_by_prefix_multiple() {
        let aws_client = AWSClient::from_static_creds(ACCESS_KEY, SECRET_KEY, None).await;
        let bucket = get_test_bucket();
        let s3 = S3ArtifactProvider::new(&aws_client, &bucket);
        let prefix = "v1/cache/linux/linux"; // this is just a known prefix
        let artifact = s3.get_artifact_by_prefix(prefix).await;
        assert!(artifact.is_err());
    }

    // Ignoring this test in ci for now. We need a stable location with a known contents to test this
    // regularly.
    #[ignore]
    #[tokio::test]
    async fn test_get_artifact_by_prefix_not_exists() {
        let aws_client = AWSClient::from_static_creds(ACCESS_KEY, SECRET_KEY, None).await;
        let bucket = get_test_bucket();
        let s3 = S3ArtifactProvider::new(&aws_client, &bucket);
        let prefix = "believerco-gameprototypemp/Engine/Development/Win64";
        let artifact = s3.get_artifact_by_prefix(prefix).await;
        assert!(artifact.is_err());
    }
}
