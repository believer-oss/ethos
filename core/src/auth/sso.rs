use std::io::Write;
use std::sync::mpsc::Sender;

use anyhow::{anyhow, Result};
use aws_sdk_ssooidc::Client;
use aws_types::SdkConfig;
use chrono::{DateTime, Duration, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use tracing::{error, info, warn};

use crate::utils::serde::json_date_format;

// Credit for this SSO code goes to @sturmm / https://github.com/sturmm/aws-easy-sso/blob/main/src/aws/token.rs
// We've only slightly modified it to fit our needs.

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenExpiredError;

impl std::error::Error for TokenExpiredError {
    fn description(&self) -> &str {
        "Token expired"
    }
}

impl std::fmt::Display for TokenExpiredError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "Token expired")
    }
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Debug)]
#[serde(rename_all = "camelCase")]
pub struct AccessToken {
    pub start_url: String,
    pub region: String,
    pub access_token: String,
    #[serde(with = "json_date_format")]
    pub expires_at: DateTime<Utc>,
    #[serde(flatten)]
    pub device_client: DeviceClient,
    pub refresh_token: String,
}

impl AccessToken {
    pub fn is_expired(&self) -> bool {
        self.expires_at < Utc::now()
    }
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Debug)]
#[serde(rename_all = "camelCase")]
pub struct DeviceClient {
    pub client_id: String,
    pub client_secret: String,
    #[serde(with = "json_date_format")]
    pub registration_expires_at: DateTime<Utc>,
}

impl DeviceClient {
    pub fn is_expired(&self) -> bool {
        self.registration_expires_at < Utc::now()
    }
}

#[derive(Debug, Clone)]
pub struct SsoAccessTokenProvider {
    client: Client,
    client_name: String,
}

impl SsoAccessTokenProvider {
    const DEVICE_GRANT_TYPE: &'static str = "urn:ietf:params:oauth:grant-type:device_code";
    const REFRESH_GRANT_TYPE: &'static str = "refresh_token";

    pub fn new(config: &SdkConfig, client_name: String) -> Result<Self> {
        Ok(Self {
            client: Client::new(config),
            client_name,
        })
    }

    pub async fn get_new_token(
        &self,
        start_url: &str,
        verification_tx: Option<Sender<String>>,
    ) -> Result<AccessToken> {
        let device_client = self.register_device_client().await?;
        self.authenticate(start_url, device_client, verification_tx)
            .await
    }

    pub async fn register_device_client(&self) -> Result<DeviceClient, anyhow::Error> {
        let response = self
            .client
            .register_client()
            .client_name(self.client_name.clone())
            .client_type("public")
            .scopes("sso:account:access")
            .send()
            .await?;
        let client_id = response.client_id().unwrap();
        let client_secret = response.client_secret().unwrap();
        let registration_expires_at = Utc
            .timestamp_opt(response.client_secret_expires_at(), 0)
            .unwrap();
        let device_client = DeviceClient {
            client_id: String::from(client_id),
            client_secret: String::from(client_secret),
            registration_expires_at,
        };
        Ok(device_client)
    }

    async fn authenticate(
        &self,
        start_url: &str,
        device_client: DeviceClient,
        verification_tx: Option<Sender<String>>,
    ) -> Result<AccessToken> {
        let auth_response = self
            .client
            .start_device_authorization()
            .client_id(device_client.client_id.as_str())
            .client_secret(device_client.client_secret.as_str())
            .start_url(start_url)
            .send()
            .await?;

        open::that(auth_response.verification_uri_complete().unwrap())?;

        if let Some(tx) = verification_tx {
            tx.send(auth_response.user_code().unwrap().to_string())?;
        }

        let interval = auth_response.interval();
        loop {
            let token_response = self
                .client
                .create_token()
                .client_id(device_client.client_id.as_str())
                .client_secret(device_client.client_secret.as_str())
                .grant_type(Self::DEVICE_GRANT_TYPE)
                .device_code(auth_response.device_code().unwrap())
                .send()
                .await;

            match token_response {
                Ok(out) => {
                    let access_token = out.access_token().unwrap();
                    let refresh_token = out.refresh_token().unwrap();
                    let expires_in_seconds = match Duration::try_seconds(out.expires_in() as i64) {
                        Some(d) => d,
                        None => {
                            return Err(anyhow!("Invalid expires_in value"));
                        }
                    };
                    let expires_at = Utc::now() + expires_in_seconds;

                    let access_token = AccessToken {
                        region: self.client.config().region().unwrap().to_string(),
                        start_url: String::from(start_url),
                        access_token: String::from(access_token),
                        expires_at,
                        device_client,
                        refresh_token: String::from(refresh_token),
                    };

                    print!("\x1B[1A");
                    print!("\x1B[2K");
                    std::io::stdout().flush().unwrap();

                    break Ok(access_token);
                }
                Err(err) => {
                    let service_error = err.into_service_error();
                    if service_error.is_access_denied_exception() {
                        break Err(anyhow!("Access request rejected"));
                    }

                    let seconds = match Duration::try_seconds(interval as i64) {
                        Some(d) => d,
                        None => {
                            return Err(anyhow!("Invalid interval value"));
                        }
                    };
                    tokio::time::sleep(seconds.to_std()?).await;
                }
            }
        }
    }

    pub async fn refresh_token(&self, cached_token: AccessToken) -> Result<AccessToken> {
        let device_client = &cached_token.device_client;
        let response = match self
            .client
            .create_token()
            .client_id(device_client.client_id.as_str())
            .client_secret(device_client.client_secret.as_str())
            .grant_type(Self::REFRESH_GRANT_TYPE)
            .refresh_token(cached_token.refresh_token.as_str())
            .send()
            .await
        {
            Ok(response) => response,
            Err(err) => {
                let service_error = err.into_service_error();
                if service_error.is_expired_token_exception()
                    || service_error.is_invalid_grant_exception()
                {
                    warn!(
                        "Service error refreshing token: {:?}",
                        service_error.to_string()
                    );
                    return Err(anyhow!(TokenExpiredError));
                }

                error!("Error refreshing token: {:?}", service_error.to_string());
                return Err(anyhow!("Error refreshing token. Please check the logs."));
            }
        };

        let access_token = response.access_token().unwrap();
        let refresh_token = response.refresh_token().unwrap();
        let expires_in_seconds = match Duration::try_seconds(response.expires_in() as i64) {
            Some(d) => d,
            None => {
                return Err(anyhow!("Invalid expires_in value"));
            }
        };

        info!(
            "Token refreshed, expires in {} seconds",
            expires_in_seconds.num_seconds()
        );

        let expires_at = Utc::now() + expires_in_seconds;

        Ok(AccessToken {
            region: self.client.config().region().unwrap().to_string(),
            start_url: cached_token.start_url.clone(),
            access_token: String::from(access_token),
            expires_at,
            device_client: cached_token.device_client,
            refresh_token: String::from(refresh_token),
        })
    }
}
