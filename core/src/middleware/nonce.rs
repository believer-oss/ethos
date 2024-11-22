use axum::http::StatusCode;
use axum::{extract::Request, http::HeaderMap, middleware::Next, response::Response};
use lazy_static::lazy_static;

pub static NONCE_HEADER: &str = "X-Ethos-Nonce";
pub static NONCE_FILENAME: &str = ".nonce";

lazy_static! {
    pub static ref NONCE: String = generate_nonce();
}

pub async fn nonce(
    headers: HeaderMap,
    request: Request,
    next: Next,
    value: &str,
) -> Result<Response, StatusCode> {
    // if the URI is /auth/callback, we don't need to check the nonce
    if request.uri().path() == "/auth/callback" {
        return Ok(next.run(request).await);
    }

    if let Some(nonce) = headers.get(NONCE_HEADER) {
        if nonce == value {
            return Ok(next.run(request).await);
        }
    }

    Err(StatusCode::UNAUTHORIZED)
}

pub fn generate_nonce() -> String {
    use rand::distributions::Alphanumeric;
    use rand::{thread_rng, Rng};

    thread_rng()
        .sample_iter(&Alphanumeric)
        .take(32)
        .map(char::from)
        .collect()
}
