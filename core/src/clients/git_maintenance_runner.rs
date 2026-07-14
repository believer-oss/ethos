use crate::clients::git::{Git, ShouldPrune};
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::Sender as STDSender;
use std::sync::Arc;
use std::time::Duration;
use tracing::{error, info, warn};

use super::git::Opts;

pub struct GitMaintenanceRunner {
    git: Git,
    config: MaintenanceConfig,
    pause: Arc<AtomicBool>,
    reauth_tx: Option<tokio::sync::mpsc::Sender<()>>,
}

/// Does this fetch error look like a missing/rejected credential (as opposed to
/// a network blip)? These markers are git/HTTP-generic, not GitHub-specific.
/// Used to decide whether a background fetch failure is worth asking the app to
/// repopulate the credential.
fn is_auth_error(msg: &str) -> bool {
    const MARKERS: [&str; 5] = [
        "could not read Username",
        "Authentication failed",
        "terminal prompts disabled",
        "Cannot prompt",
        "HTTP 401",
    ];
    MARKERS.iter().any(|m| msg.contains(m))
}

struct MaintenanceConfig {
    fetch_interval: Duration,
    maintenance_interval: Duration,
}

impl Default for MaintenanceConfig {
    fn default() -> Self {
        MaintenanceConfig {
            fetch_interval: Duration::from_secs(30),
            maintenance_interval: Duration::from_secs(1800),
        }
    }
}

impl GitMaintenanceRunner {
    pub fn new(path: String, pause: Arc<AtomicBool>, tx: STDSender<String>) -> Self {
        let git = Git::new(PathBuf::from(path.clone()), tx);

        let config = MaintenanceConfig::default();

        GitMaintenanceRunner {
            git,
            pause,
            config,
            reauth_tx: None,
        }
    }

    pub fn with_fetch_interval(mut self, interval: Duration) -> Self {
        self.config.fetch_interval = interval;
        self
    }

    pub fn with_maintenance_interval(mut self, interval: Duration) -> Self {
        self.config.maintenance_interval = interval;
        self
    }

    /// Register a notifier that fires (best-effort) when a background fetch
    /// fails with an auth error, so the app can repopulate a stomped/erased
    /// credential without waiting for a restart. Use a small channel
    /// (capacity 1): a full channel just means a re-seed is already pending.
    pub fn with_reauth_notifier(mut self, tx: tokio::sync::mpsc::Sender<()>) -> Self {
        self.reauth_tx = Some(tx);
        self
    }

    pub async fn run(&self) -> anyhow::Result<()> {
        let git = self.git.clone();
        let fetch_interval = self.config.fetch_interval;
        let pause = self.pause.clone();
        let reauth_tx = self.reauth_tx.clone();
        let fetch_task = tokio::task::spawn(async move {
            loop {
                if !pause.clone().load(std::sync::atomic::Ordering::Relaxed) {
                    match git
                        .fetch(
                            ShouldPrune::Yes,
                            // with_complete_error so the failure carries git's
                            // stderr (e.g. "could not read Username", "HTTP 401")
                            // and we can tell an auth failure from a network blip.
                            Opts::default()
                                .with_skip_notify_frontend()
                                .with_skip_interactive_auth()
                                .with_complete_error(),
                        )
                        .await
                    {
                        Ok(_) => {}
                        Err(e) => {
                            let msg = e.to_string();
                            warn!("Error fetching: {}", msg);
                            // A stomped/erased credential surfaces here. Ask the
                            // app to repopulate it from the stored PAT so git
                            // recovers on the next tick instead of at restart.
                            if is_auth_error(&msg) {
                                if let Some(tx) = &reauth_tx {
                                    let _ = tx.try_send(());
                                }
                            }
                        }
                    }
                }

                tokio::time::sleep(fetch_interval).await;
            }
        });

        let git = self.git.clone();
        let maintenance_interval = self.config.maintenance_interval;
        let pause = self.pause.clone();
        let maintenance_task = tokio::task::spawn(async move {
            loop {
                if !pause.clone().load(std::sync::atomic::Ordering::Relaxed) {
                    match git.run_maintenance().await {
                        Ok(_) => {
                            info!("Maintenance complete");
                        }
                        Err(e) => {
                            error!("Error running maintenance: {:?}", e);
                        }
                    }
                }

                tokio::time::sleep(maintenance_interval).await;
            }
        });

        tokio::try_join!(fetch_task, maintenance_task)?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::is_auth_error;

    #[test]
    fn auth_errors_trigger_reseed_but_network_errors_do_not() {
        // Missing/erased or rejected credential → re-seed.
        assert!(is_auth_error(
            "fatal: could not read Username for 'https://github.com': terminal prompts disabled"
        ));
        assert!(is_auth_error("error: RPC failed; HTTP 401 curl 22"));
        assert!(is_auth_error(
            "fatal: Authentication failed for 'https://github.com'"
        ));
        assert!(is_auth_error(
            "fatal: Cannot prompt because user interactivity has been disabled"
        ));

        // Network / non-auth failures must NOT trigger a re-seed — re-seeding
        // can't fix them and would just churn the credential store.
        assert!(!is_auth_error(
            "fatal: unable to access 'https://github.com/': Could not resolve host: github.com"
        ));
        assert!(!is_auth_error(
            "Git command failed. Check the log for details."
        ));
        assert!(!is_auth_error("error: RPC failed; HTTP 500"));
    }
}
