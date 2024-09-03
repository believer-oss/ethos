use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use crate::types::github::TokenNotFoundError;

#[derive(Debug)]
pub enum CoreError {
    Input(anyhow::Error),
    Internal(anyhow::Error),
}

impl IntoResponse for CoreError {
    fn into_response(self) -> Response {
        match self {
            CoreError::Input(e) => (StatusCode::BAD_REQUEST, format!("{}", e)).into_response(),
            CoreError::Internal(e) => {
                if e.downcast_ref::<TokenNotFoundError>().is_some() {
                    return TokenNotFoundError.into_response();
                }

                (StatusCode::INTERNAL_SERVER_ERROR, format!("{}", e)).into_response()
            }
        }
    }
}

impl std::fmt::Display for CoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CoreError::Input(e) => write!(f, "{}", e),
            CoreError::Internal(e) => write!(f, "{}", e),
        }
    }
}

impl<E> From<E> for CoreError
where
    E: Into<anyhow::Error>,
{
    fn from(e: E) -> Self {
        Self::Internal(e.into())
    }
}
