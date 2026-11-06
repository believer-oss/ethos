use std::fmt::{Display, Formatter};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tracing::{error, warn};

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
                // failed to deserialize. A transport failure keeps its full
                // source chain, so the real cause reaches the UI and the logs.
                let mut detail = error.to_string();
                let mut source = std::error::Error::source(&error);
                while let Some(s) = source {
                    detail.push_str(": ");
                    detail.push_str(&s.to_string());
                    source = s.source();
                }

                let message = if error.is_decode() {
                    // The parser's complaint ("expected value at line 1 column 1") helps nobody.
                    warn!("Undecodable local backend response: {detail}");
                    "The local service sent a response Friendshipper couldn't read".to_string()
                } else {
                    format!("Couldn't reach the local service: {detail}")
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

/// How much of a response body an error message quotes.
const BODY_QUOTE_LEN: usize = 200;

/// The error for a response whose body isn't what was asked for: its status and the start of the
/// body, which for the local service is its own error text.
fn body_error(status: reqwest::StatusCode, body: &str) -> TauriError {
    let body = body.trim();
    let quoted: String = body.chars().take(BODY_QUOTE_LEN).collect();
    let message = match (body.is_empty(), quoted.len() < body.len()) {
        (true, _) => status.to_string(),
        (false, false) => format!("{}: {quoted}", status.as_u16()),
        (false, true) => format!("{}: {quoted}…", status.as_u16()),
    };
    TauriError {
        message,
        status_code: status.as_u16(),
    }
}

/// Reads a local backend response as JSON. An error status, or a body that isn't the JSON asked
/// for, becomes an error saying so, never the parser's complaint.
pub async fn json_response<T: DeserializeOwned>(res: reqwest::Response) -> Result<T, TauriError> {
    let status = res.status();
    let body = res.text().await?;
    if status.is_client_error() || status.is_server_error() {
        return Err(TauriError {
            message: body,
            status_code: status.as_u16(),
        });
    }
    serde_json::from_str(&body).map_err(|e| {
        warn!("Undecodable local backend response ({status}): {e}");
        body_error(status, &body)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(status: u16, body: &str) -> reqwest::Response {
        http::Response::builder()
            .status(status)
            .body(body.to_string())
            .unwrap()
            .into()
    }

    #[tokio::test]
    async fn json_responses_report_the_body_not_the_parser() {
        let ok: Vec<u32> = json_response(response(200, "[1, 2]")).await.unwrap();
        assert_eq!(ok, vec![1, 2]);

        // The local service's errors are plain text with an error status: passed on as they are.
        let refused = json_response::<Vec<u32>>(response(500, "client error (Connect)"))
            .await
            .unwrap_err();
        assert_eq!(refused.message, "client error (Connect)");
        assert_eq!(refused.status_code, 500);

        let html = json_response::<Vec<u32>>(response(200, "<html>proxy</html>"))
            .await
            .unwrap_err();
        assert_eq!(html.message, "200: <html>proxy</html>");

        let long = "x".repeat(300);
        let cut = json_response::<Vec<u32>>(response(200, &long))
            .await
            .unwrap_err();
        assert_eq!(cut.message, format!("200: {}…", "x".repeat(200)));
    }
}
