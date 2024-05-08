use std::path::PathBuf;
use tokio::sync::mpsc::Sender;

pub mod command;
pub mod error;

pub struct State {
    pub server_url: String,
    pub log_path: PathBuf,
    pub client: reqwest::Client,
    pub shutdown_tx: Sender<()>,
}
