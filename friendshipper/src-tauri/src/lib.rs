use std::fs;
use std::io::Write;
use std::path::Path;

use anyhow::Result;
use axum::{middleware, Router};

use crate::engine::EngineProvider;
use crate::state::AppState;
use ethos_core::middleware::nonce;
use ethos_core::middleware::nonce::NONCE;

pub mod auth;
pub mod builds;
pub mod client;
pub mod config;
pub mod engine;
pub mod obs;
pub mod playtests;
pub mod repo;
pub mod server;
mod servers;
pub mod state;
pub mod storage;
pub mod system;

pub static VERSION: &str = env!("CARGO_PKG_VERSION");

// Build artifacts written by the build system
pub const APP_NAME: &str = "Friendshipper";
pub static KEYRING_USER: &str = "github_pat";
pub static DEFAULT_ENGINE_OWNER: &str = "believerco";
pub static DEFAULT_ENGINE_REPO: &str = "unrealengine";
pub static PORT_FILENAME: &str = ".port";

pub fn router<T>(log_path: &Path, port: u16) -> Result<Router<AppState<T>>>
where
    T: EngineProvider,
{
    // get data path as parent of the log path
    let data_path = log_path.parent().unwrap().to_path_buf();

    // write nonce to file
    let mut nonce_file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(data_path.join(nonce::NONCE_FILENAME))
        .unwrap();

    nonce_file.write_all(NONCE.as_bytes())?;

    // write port to file
    let mut port_file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(data_path.join(PORT_FILENAME))
        .unwrap();

    port_file.write_all(port.to_string().as_bytes())?;

    Ok(Router::new()
        .nest("/auth", auth::router())
        .nest("/builds", builds::router())
        .nest("/config", config::router())
        .nest("/obs", obs::router())
        .nest("/playtests", playtests::router())
        .nest("/project", repo::project::router())
        .nest("/repo", repo::router())
        .nest("/servers", servers::router())
        .nest("/storage", storage::router())
        .nest("/system", system::router())
        .nest("/engine", engine::router())
        .route_layer(middleware::from_fn(move |headers, req, next| {
            nonce::nonce(headers, req, next, NONCE.as_str())
        })))
}
