use std::fmt::{Display, Formatter};

use serde::{Deserialize, Serialize};
use tracing::error;

#[derive(Debug, Serialize, Deserialize)]
pub struct TauriError {
    pub message: String,
    pub status_code: u16,
}

impl From<reqwest::Error> for TauriError {
    fn from(error: reqwest::Error) -> Self {
        let (message, status_code) = match error.status() {
            Some(status) => {
                let code = status.as_u16();
                let msg = match status {
                    reqwest::StatusCode::BAD_REQUEST => "400: Bad Request",
                    reqwest::StatusCode::NOT_FOUND => "404: Not Found",
                    reqwest::StatusCode::UNAUTHORIZED => "401: Unauthorized",
                    reqwest::StatusCode::FORBIDDEN => "403: Forbidden",
                    reqwest::StatusCode::INTERNAL_SERVER_ERROR => "500: Internal Server Error",
                    _ => "Unknown Error",
                };
                (msg.to_string(), code)
            }
            None => {
                // No status means the failure happened outside a normal HTTP
                // response: connection refused/reset, timeout, or a body that
                // failed to deserialize. Keep the full source chain so the
                // real cause survives to the UI and the logs.
                let mut detail = error.to_string();
                let mut source = std::error::Error::source(&error);
                while let Some(s) = source {
                    detail.push_str(": ");
                    detail.push_str(&s.to_string());
                    source = s.source();
                }

                let message = if error.is_decode() {
                    format!("Failed to parse local backend response: {detail}")
                } else {
                    format!("Error contacting local backend: {detail}")
                };
                (message, 500)
            }
        };

        error!("Request to local backend failed: {message}");

        TauriError {
            message,
            status_code,
        }
    }
}

impl Display for TauriError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}
