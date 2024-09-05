use std::fs;
use std::io::Write;
use std::sync::Arc;

use anyhow::Result;
use axum::{middleware, Router};

use ethos_core::middleware::nonce;
use ethos_core::middleware::nonce::NONCE;

use crate::state::AppState;

pub mod config;
pub mod metadata;
pub mod repo;
pub mod server;
mod state;
mod system;
pub mod tools;
pub mod types;

pub static VERSION: &str = env!("CARGO_PKG_VERSION");
pub static APP_NAME: &str = env!("CARGO_PKG_NAME");
pub static KEYRING_USER: &str = "github_pat";

#[cfg(windows)]
pub static DEFAULT_DRIVE_MOUNT: &str = "Y:";

pub fn router(shared_state: Arc<AppState>) -> Result<Router> {
    // get data path as parent of the log path
    let data_path = shared_state.log_path.parent().unwrap().to_path_buf();

    // write nonce to file
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(data_path.join(nonce::NONCE_FILENAME))
        .unwrap();

    file.write_all(NONCE.as_bytes())?;

    Ok(Router::new()
        .nest("/repo", repo::router(shared_state.clone()))
        .nest("/metadata", metadata::router(shared_state.clone()))
        .nest("/tools", tools::router(shared_state.clone()))
        .nest("/system", system::router(shared_state.clone()))
        .nest("/config", config::router(shared_state))
        .route_layer(middleware::from_fn(move |headers, req, next| {
            nonce::nonce(headers, req, next, NONCE.as_str())
        })))
}
