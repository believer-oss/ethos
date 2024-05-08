use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use crate::types::github::TokenNotFoundError;

#[derive(Debug)]
pub struct CoreError(pub anyhow::Error);

impl IntoResponse for CoreError {
    fn into_response(self) -> Response {
        if self.0.downcast_ref::<TokenNotFoundError>().is_some() {
            return TokenNotFoundError.into_response();
        }

        (StatusCode::BAD_REQUEST, format!("{}", self)).into_response()
    }
}

impl std::fmt::Display for CoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl<E> From<E> for CoreError
where
    E: Into<anyhow::Error>,
{
    fn from(e: E) -> Self {
        Self(e.into())
    }
}
