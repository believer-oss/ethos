use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

pub mod merge_queue;
pub mod pulls;
pub mod user;

#[derive(Debug)]
pub struct TokenNotFoundError;
impl IntoResponse for TokenNotFoundError {
    fn into_response(self) -> Response {
        (
            StatusCode::UNAUTHORIZED,
            "Error: No GitHub PAT has been configured. Please check your preferences.",
        )
            .into_response()
    }
}

impl std::fmt::Display for TokenNotFoundError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "No GitHubPAT has been configured.")
    }
}
