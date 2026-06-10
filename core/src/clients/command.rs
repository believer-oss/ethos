use crate::middleware::nonce::{NONCE, NONCE_HEADER};
use anyhow::Result;
use http::header;

pub fn new_reqwest_client() -> Result<reqwest::Client> {
    let mut headers = header::HeaderMap::new();
    headers.insert(NONCE_HEADER, header::HeaderValue::from_str(&NONCE).unwrap());
    // Don't pool idle connections to the in-process server: pooled keep-alive
    // sockets can die across sleep/resume and get reused anyway, surfacing as
    // spurious "Error contacting local backend" failures. Connection setup on
    // localhost is sub-millisecond, so pooling buys nothing here.
    match reqwest::Client::builder()
        .default_headers(headers)
        .pool_max_idle_per_host(0)
        .build()
    {
        Ok(client) => Ok(client),
        Err(e) => Err(e.into()),
    }
}
