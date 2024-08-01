use crate::clients::git::{Git, ShouldPrune};
use std::path::PathBuf;
use std::sync::mpsc::Sender as STDSender;
use std::time::Duration;
use tracing::{error, info};

pub struct GitMaintenanceRunner {
    git: Git,
    config: MaintenanceConfig,
}

struct MaintenanceConfig {
    fetch_interval: Duration,
    maintenance_interval: Duration,
}

impl Default for MaintenanceConfig {
    fn default() -> Self {
        MaintenanceConfig {
            fetch_interval: Duration::from_secs(30),
            maintenance_interval: Duration::from_secs(3600),
        }
    }
}

impl GitMaintenanceRunner {
    pub fn new(path: String, tx: STDSender<String>) -> Self {
        let git = Git::new(PathBuf::from(path.clone()), tx);

        let config = MaintenanceConfig::default();

        GitMaintenanceRunner { git, config }
    }

    pub fn with_fetch_interval(mut self, interval: Duration) -> Self {
        self.config.fetch_interval = interval;
        self
    }

    pub fn with_maintenance_interval(mut self, interval: Duration) -> Self {
        self.config.maintenance_interval = interval;
        self
    }

    pub async fn run(&self) -> anyhow::Result<()> {
        let git = self.git.clone();
        let fetch_interval = self.config.fetch_interval;
        let fetch_task = tokio::task::spawn(async move {
            loop {
                {
                    match git.fetch(ShouldPrune::Yes).await {
                        Ok(_) => {}
                        Err(e) => {
                            error!("Error fetching: {:?}", e);
                        }
                    }
                }

                tokio::time::sleep(fetch_interval).await;
            }
        });

        let git = self.git.clone();
        let maintenance_interval = self.config.maintenance_interval;
        let maintenance_task = tokio::task::spawn(async move {
            loop {
                {
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
