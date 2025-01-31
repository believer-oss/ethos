use openidconnect::{CsrfToken, PkceCodeChallenge};
use std::{
    path::PathBuf,
    sync::{atomic::AtomicBool, Arc},
};
use tokio::sync::mpsc::Sender;

pub mod command;
pub mod error;

#[derive(Clone)]
pub struct AuthState {
    pub csrf_token: CsrfToken,
    pub pkce: Arc<(PkceCodeChallenge, String)>,
    pub issuer_url: Option<String>,
    pub client_id: Option<String>,
    pub in_flight: Arc<AtomicBool>,
}

#[derive(Clone)]
pub struct TauriState {
    pub server_url: String,
    pub log_path: PathBuf,
    pub client: reqwest::Client,
    pub auth_state: Option<AuthState>,

    pub shutdown_tx: Sender<()>,
}
