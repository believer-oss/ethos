use crate::middleware::nonce::{NONCE, NONCE_HEADER};
use anyhow::Result;
use reqwest::header;

pub fn new_reqwest_client() -> Result<reqwest::Client> {
    let mut headers = header::HeaderMap::new();
    headers.insert(NONCE_HEADER, header::HeaderValue::from_str(&NONCE).unwrap());
    match reqwest::Client::builder().default_headers(headers).build() {
        Ok(client) => Ok(client),
        Err(e) => Err(e.into()),
    }
}
