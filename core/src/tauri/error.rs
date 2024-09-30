use std::fmt::{Display, Formatter};

use serde::{Deserialize, Serialize};

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
            None => ("Error contacting local backend".to_string(), 500),
        };

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
