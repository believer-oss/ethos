use std::time::Duration;

use anyhow::anyhow;
use obws::Client as OBSClient;
use tokio::time::timeout;
use tracing::error;

use crate::types::errors::CoreError;

pub struct Client {
    port: u16,
    scene: String,
}

impl Default for Client {
    fn default() -> Self {
        Self {
            port: 4455,
            scene: "friendshipper".to_string(),
        }
    }
}

impl Client {
    pub async fn start_recording(&self) -> Result<(), CoreError> {
        let connection = OBSClient::connect("localhost", self.port, Some(""));

        const OBS_CONNECTION_ERROR: &str = "Could not connect to OBS. Is it running?";
        let client = match timeout(Duration::from_secs(5), connection).await {
            Ok(c) => c.map_err(|_| CoreError(anyhow!(OBS_CONNECTION_ERROR)))?,
            Err(_) => {
                return Err(CoreError(anyhow!(OBS_CONNECTION_ERROR)));
            }
        };

        match client.scenes().set_current_program_scene(&self.scene).await {
            Ok(_) => {}
            Err(e) => {
                return Err(CoreError(anyhow!("Error setting scene: {}", e)));
            }
        }

        match client.recording().start().await {
            Ok(_) => {}
            Err(e) => {
                return Err(CoreError(anyhow!("Error starting recording: {}", e)));
            }
        };
        Ok(())
    }

    pub async fn stop_recording(&self) -> Result<(), CoreError> {
        let client = obws::Client::connect("localhost", self.port, Some("")).await?;
        match client.recording().stop().await {
            Ok(_) => {}
            Err(e) => {
                error!("Error stopping recording: {}", e);
            }
        };
        Ok(())
    }
}
