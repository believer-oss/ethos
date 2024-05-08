use std::fs;
use std::io::Write;
use std::sync::Arc;

use anyhow::Result;
use axum::{middleware, Router};

use ethos_core::middleware::nonce;
use ethos_core::middleware::nonce::NONCE;
use state::AppState;

pub mod auth;
pub mod builds;
pub mod config;
pub mod ludos;
pub mod obs;
pub mod playtests;
pub mod repo;
pub mod server;
mod servers;
pub mod state;
pub mod system;

pub static VERSION: &str = env!("CARGO_PKG_VERSION");

// Build artifacts written by the build system
pub const APP_NAME: &str = "Friendshipper";
pub static KEYRING_USER: &str = "github_pat";
pub static DEFAULT_ENGINE_OWNER: &str = "believerco";
pub static DEFAULT_ENGINE_REPO: &str = "unrealengine";

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
        .nest("/auth", auth::router(shared_state.clone()))
        .nest("/builds", builds::router(shared_state.clone()))
        .nest("/config", config::router(shared_state.clone()))
        .nest("/ludos", ludos::router(shared_state.clone()))
        .nest("/obs", obs::router(shared_state.clone()))
        .nest("/playtests", playtests::router(shared_state.clone()))
        .nest("/project", repo::project::router(shared_state.clone()))
        .nest("/repo", repo::router(shared_state.clone()))
        .nest("/servers", servers::router(shared_state.clone()))
        .nest("/system", system::router(shared_state))
        .route_layer(middleware::from_fn(move |headers, req, next| {
            nonce::nonce(headers, req, next, NONCE.as_str())
        })))
}
