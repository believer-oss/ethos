use std::fmt::{Display, Formatter};

use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct TauriError {
    pub message: String,
}

impl From<reqwest::Error> for TauriError {
    fn from(error: reqwest::Error) -> Self {
        TauriError {
            message: match error.status() {
                Some(status) => match status {
                    reqwest::StatusCode::BAD_REQUEST => "400: Bad Request".to_string(),
                    reqwest::StatusCode::NOT_FOUND => "404: Not Found".to_string(),
                    reqwest::StatusCode::UNAUTHORIZED => "401: Unauthorized".to_string(),
                    reqwest::StatusCode::FORBIDDEN => "403: Forbidden".to_string(),
                    reqwest::StatusCode::INTERNAL_SERVER_ERROR => {
                        "500: Internal Server Error".to_string()
                    }
                    _ => "Unknown Error".to_string(),
                },
                None => "Error contacting Friendshipper backend".to_string(),
            },
        }
    }
}

impl Display for TauriError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}
