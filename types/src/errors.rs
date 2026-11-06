use aws_smithy_runtime_api::client::result::ConnectorError;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use crate::github::TokenNotFoundError;

#[derive(Debug)]
pub enum CoreError {
    Input(anyhow::Error),
    Internal(anyhow::Error),
    Unauthorized,
}

impl IntoResponse for CoreError {
    fn into_response(self) -> Response {
        match self {
            CoreError::Input(e) => (StatusCode::BAD_REQUEST, format!("{e}")).into_response(),
            CoreError::Unauthorized => (StatusCode::UNAUTHORIZED, "Unauthorized").into_response(),
            CoreError::Internal(e) => {
                if e.downcast_ref::<TokenNotFoundError>().is_some() {
                    return TokenNotFoundError.into_response();
                }
                if let Some(message) = unreachable_aws(&e) {
                    return (StatusCode::INTERNAL_SERVER_ERROR, message).into_response();
                }

                (StatusCode::INTERNAL_SERVER_ERROR, format!("{e}")).into_response()
            }
        }
    }
}

/// "Couldn't reach AWS: <cause>" when the error is an AWS SDK call that never got a response (a
/// connection or timeout failure), whichever service and call site it came from. The SDK's own text
/// for these is "dispatch failure", which says nothing to a person.
fn unreachable_aws(e: &anyhow::Error) -> Option<String> {
    let connector = e
        .chain()
        .find_map(|err| err.downcast_ref::<ConnectorError>())
        .filter(|c| c.is_io() || c.is_timeout())?;
    let mut causes: Vec<String> = Vec::new();
    let mut source = std::error::Error::source(connector);
    while let Some(s) = source {
        let text = s.to_string();
        if causes.last() != Some(&text) {
            causes.push(text);
        }
        source = s.source();
    }
    if causes.is_empty() {
        causes.push(connector.to_string());
    }
    Some(format!("Couldn't reach AWS: {}", causes.join(": ")))
}

impl std::fmt::Display for CoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CoreError::Input(e) => write!(f, "{e}"),
            CoreError::Unauthorized => write!(f, "Unauthorized"),
            CoreError::Internal(e) => write!(f, "{e}"),
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

#[cfg(test)]
mod tests {
    use aws_smithy_runtime_api::client::result::SdkError;

    use super::*;

    #[test]
    fn an_unreachable_aws_is_named_with_its_cause() {
        let refused =
            std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "connection refused");
        let dispatch: SdkError<std::io::Error, ()> =
            SdkError::dispatch_failure(ConnectorError::io(Box::new(refused)));
        let CoreError::Internal(e) = CoreError::from(dispatch) else {
            panic!("not internal");
        };
        assert_eq!(
            unreachable_aws(&e).as_deref(),
            Some("Couldn't reach AWS: connection refused")
        );

        // Any other error keeps its own message.
        assert_eq!(unreachable_aws(&anyhow::anyhow!("No object found")), None);
    }
}
