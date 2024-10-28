use anyhow::anyhow;
use ethos_core::types::aws::AWSStaticCredentials;
use ethos_core::types::config::{FriendshipperConfig, OktaConfig};
use ethos_core::types::errors::CoreError;
use tracing::{error, instrument};

#[derive(Clone, Debug)]
pub struct FriendshipperClient {
    pub client: reqwest::Client,
    pub base_url: String,
}

impl FriendshipperClient {
    pub fn new(base_url: String) -> Result<Self, CoreError> {
        if base_url.is_empty() {
            return Err(CoreError::Internal(anyhow!("Base URL is empty")));
        }

        Ok(FriendshipperClient {
            client: reqwest::Client::new(),
            base_url,
        })
    }

    #[instrument]
    pub async fn check_health(&self) -> Result<(), CoreError> {
        let response = self
            .client
            .get(format!("{}/health", self.base_url))
            .send()
            .await
            .map_err(|e| {
                error!("Error contacting server: {}", e);
                CoreError::Internal(anyhow!(
                    "Error contacting server: Please check your Friendshipper server URL."
                ))
            })?;

        if response.status() != 200 {
            error!(
                "Server is not healthy: {}",
                response.text().await.unwrap_or_default()
            );
            return Err(CoreError::Internal(anyhow!(
                "Error contacting server: Please check your Friendshipper server URL."
            )));
        }

        Ok(())
    }

    #[instrument]
    pub async fn get_okta_config(&self) -> Result<OktaConfig, CoreError> {
        let response = self
            .client
            .get(format!("{}/discovery", self.base_url))
            .send()
            .await
            .map_err(|e| CoreError::Internal(anyhow!("Failed to fetch OktaConfig: {}", e)))?;

        if response.status() != 200 {
            return Err(CoreError::Internal(anyhow!(
                "Failed to fetch OktaConfig: {}",
                response.text().await.unwrap_or_default()
            )));
        }

        response
            .json()
            .await
            .map_err(|e| CoreError::Internal(anyhow!("Failed to parse OktaConfig: {}", e)))
    }

    #[instrument]
    pub async fn get_config(&self, token: &str) -> Result<FriendshipperConfig, CoreError> {
        let response = self
            .client
            .get(format!("{}/config", self.base_url))
            .header("Authorization", format!("Bearer {}", token))
            .send()
            .await
            .map_err(|e| {
                CoreError::Internal(anyhow!("Failed to fetch FriendshipperConfig: {}", e))
            })?;

        response
            .json()
            .await
            .map_err(|e| CoreError::Internal(anyhow!("Failed to parse FriendshipperConfig: {}", e)))
    }

    #[instrument(skip(token))]
    pub async fn get_aws_credentials(
        &self,
        token: &str,
    ) -> Result<AWSStaticCredentials, CoreError> {
        let response = self
            .client
            .get(format!("{}/aws/credentials", self.base_url))
            .header("Authorization", format!("Bearer {}", token))
            .send()
            .await
            .map_err(|e| CoreError::Internal(anyhow!("Failed to fetch AWS credentials: {}", e)))?;

        response
            .json()
            .await
            .map_err(|e| CoreError::Internal(anyhow!("Failed to parse AWS credentials: {}", e)))
    }
}
