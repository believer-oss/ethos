use aws_config::load_from_env;
use aws_sdk_sts::Client as StsClient;
use aws_smithy_types_convert::date_time::DateTimeExt;
use axum::{extract::State, routing::get, Json, Router};
use ethos_core::types::{aws::AWSStaticCredentials, errors::CoreError};
use tracing::error;

use crate::ServerConfig;

async fn get_aws_credentials(
    State(server_config): State<ServerConfig>,
) -> Result<Json<AWSStaticCredentials>, CoreError> {
    let config = load_from_env().await;
    let sts_client = StsClient::new(&config);

    // Assume the role
    let assume_role_result = sts_client
        .assume_role()
        .role_arn(&server_config.role_to_assume)
        .role_session_name("Friendshipper")
        .send()
        .await
        .map_err(|e| {
            if let aws_sdk_sts::error::SdkError::ServiceError(service_error) = &e {
                error!("{:?}", service_error);
            }
            error!("{:?}", e);
            CoreError::Internal(anyhow::anyhow!("Failed to assume role"))
        })?;

    let credentials = assume_role_result
        .credentials()
        .ok_or_else(|| CoreError::Internal(anyhow::anyhow!("No credentials found")))?;

    // Create JSON response
    let response = AWSStaticCredentials {
        access_key_id: credentials.access_key_id().to_string(),
        secret_access_key: credentials.secret_access_key().to_string(),
        session_token: Some(credentials.session_token().to_string()),
        expiration: Some(credentials.expiration().to_chrono_utc()?),
    };

    Ok(Json(response))
}

pub fn create_router() -> Router<ServerConfig> {
    Router::new().route("/credentials", get(get_aws_credentials))
}
