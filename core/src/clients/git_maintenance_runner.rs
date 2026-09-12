use crate::clients::git::{Git, ShouldPrune};
use crate::types::config::AppConfigRef;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::Sender as STDSender;
use std::sync::Arc;
use std::time::Duration;
use tracing::{error, info, warn};

use super::git::Opts;

/// Whether a periodic task should skip this tick.
///
/// `pause` is owned by `RepoWorker`, which sets it around every task sequence, so it means "an
/// operation is in flight". The config flag is the user preference and is read per tick, which is
/// what makes the setting take effect without a restart.
///
/// Reads the bool and drops the guard before returning: `AppConfigRef` is a `parking_lot` lock whose
/// guard is `!Send`, and both callers await immediately afterwards.
fn should_skip(pause: &AtomicBool, app_config: &AppConfigRef) -> bool {
    if pause.load(std::sync::atomic::Ordering::Relaxed) {
        return true;
    }

    app_config.read().disable_background_git_operations
}

pub struct GitMaintenanceRunner {
    git: Git,
    config: MaintenanceConfig,
    pause: Arc<AtomicBool>,
    app_config: AppConfigRef,
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
    pub fn new(
        path: String,
        pause: Arc<AtomicBool>,
        app_config: AppConfigRef,
        tx: STDSender<String>,
    ) -> Self {
        let git = Git::new(PathBuf::from(path.clone()), tx);

        let config = MaintenanceConfig::default();

        GitMaintenanceRunner {
            git,
            pause,
            config,
            app_config,
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

    pub async fn run(&self) -> anyhow::Result<()> {
        let git = self.git.clone();
        let fetch_interval = self.config.fetch_interval;
        let pause = self.pause.clone();
        let app_config = self.app_config.clone();
        let fetch_task = tokio::task::spawn(async move {
            loop {
                if !should_skip(&pause, &app_config) {
                    match git
                        .fetch(
                            ShouldPrune::Yes,
                            Opts::default().with_skip_notify_frontend(),
                        )
                        .await
                    {
                        Ok(_) => {}
                        Err(e) => {
                            warn!("Error fetching: {:?}", e);
                        }
                    }
                }

                tokio::time::sleep(fetch_interval).await;
            }
        });

        let git = self.git.clone();
        let maintenance_interval = self.config.maintenance_interval;
        let pause = self.pause.clone();
        let app_config = self.app_config.clone();
        let maintenance_task = tokio::task::spawn(async move {
            loop {
                if !should_skip(&pause, &app_config) {
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
    use super::*;
    use crate::types::config::AppConfig;
    use parking_lot::RwLock;

    fn config_with(disabled: bool) -> AppConfigRef {
        let mut config = AppConfig::new("test");
        config.disable_background_git_operations = disabled;
        Arc::new(RwLock::new(config))
    }

    /// `should_skip` is the entire logic of the feature, so it is tested as a pure function rather
    /// than by driving the infinite loops that call it.
    ///
    /// The two inputs mean different things and must not be conflated: `pause` is owned by
    /// `RepoWorker` and means "an operation is in flight", while the config flag is the user's
    /// preference. Either one alone is sufficient to skip.
    #[test]
    fn test_should_skip_neither_set_runs() {
        let pause = AtomicBool::new(false);
        assert!(!should_skip(&pause, &config_with(false)));
    }

    #[test]
    fn test_should_skip_pause_only_skips() {
        let pause = AtomicBool::new(true);
        assert!(should_skip(&pause, &config_with(false)));
    }

    #[test]
    fn test_should_skip_setting_only_skips() {
        let pause = AtomicBool::new(false);
        assert!(should_skip(&pause, &config_with(true)));
    }

    #[test]
    fn test_should_skip_both_set_skips() {
        let pause = AtomicBool::new(true);
        assert!(should_skip(&pause, &config_with(true)));
    }

    /// Drives the real `GitMaintenanceRunner` loop against a real clone and proves the setting is
    /// live: fetching stops when it is turned on and resumes when it is turned off, with no restart
    /// and no rebuild of the runner.
    ///
    /// This is the property the feature exists to provide, and it cannot be established by the
    /// pure-function tests above — a runner that read the config once at construction would pass
    /// all of them.
    #[tokio::test(flavor = "multi_thread")]
    async fn test_runner_stops_and_resumes_fetching_when_setting_toggles() {
        use std::process::Command as StdCommand;
        use std::sync::mpsc;

        fn git(dir: &std::path::Path, args: &[&str]) -> String {
            let out = StdCommand::new("git")
                .args(args)
                .current_dir(dir)
                .output()
                .expect("run git");
            assert!(
                out.status.success(),
                "git {:?} failed: {}",
                args,
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        }

        fn commit_to_origin(dir: &std::path::Path, n: u32) -> String {
            std::fs::write(dir.join("f.txt"), format!("v{n}")).unwrap();
            git(dir, &["add", "f.txt"]);
            git(dir, &["commit", "-m", &format!("c{n}")]);
            git(dir, &["rev-parse", "HEAD"])
        }

        /// Remote-tracking sha in the clone, or empty if the ref does not exist yet.
        fn tracked_sha(clone: &std::path::Path, branch: &str) -> String {
            StdCommand::new("git")
                .args(["rev-parse", &format!("refs/remotes/origin/{branch}")])
                .current_dir(clone)
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .unwrap_or_default()
        }

        /// Poll until the clone's remote-tracking ref reaches `want`. Returns false on timeout.
        async fn wait_for_sha(clone: &std::path::Path, branch: &str, want: &str, ms: u64) -> bool {
            let deadline = std::time::Instant::now() + Duration::from_millis(ms);
            while std::time::Instant::now() < deadline {
                if tracked_sha(clone, branch) == want {
                    return true;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            false
        }

        let origin_dir = tempfile::tempdir().expect("origin tempdir");
        let origin = origin_dir.path();
        for args in [
            vec!["init"],
            vec!["config", "user.email", "t@t.t"],
            vec!["config", "user.name", "t"],
            vec!["config", "commit.gpgsign", "false"],
            vec!["config", "core.autocrlf", "false"],
        ] {
            git(origin, &args);
        }
        commit_to_origin(origin, 0);
        let branch = git(origin, &["rev-parse", "--abbrev-ref", "HEAD"]);

        let clone_dir = tempfile::tempdir().expect("clone tempdir");
        let clone = clone_dir.path().join("work");
        git(
            clone_dir.path(),
            &["clone", origin.to_str().unwrap(), clone.to_str().unwrap()],
        );

        let app_config = config_with(false);
        let pause = Arc::new(AtomicBool::new(false));
        let (tx, _rx) = mpsc::channel();
        let runner = GitMaintenanceRunner::new(
            clone.to_string_lossy().to_string(),
            pause.clone(),
            app_config.clone(),
            tx,
        )
        .with_fetch_interval(Duration::from_millis(200));

        let handle = tokio::spawn(async move {
            let _ = runner.run().await;
        });

        // 1. enabled: a new origin commit must reach the clone's remote-tracking ref
        let first = commit_to_origin(origin, 1);
        assert!(
            wait_for_sha(&clone, &branch, &first, 15_000).await,
            "background fetch never picked up the first commit"
        );

        // 2. disabled: a further commit must NOT reach it
        app_config.write().disable_background_git_operations = true;
        // let any in-flight fetch settle before taking the baseline
        tokio::time::sleep(Duration::from_millis(600)).await;
        let baseline = tracked_sha(&clone, &branch);
        let second = commit_to_origin(origin, 2);
        assert!(
            !wait_for_sha(&clone, &branch, &second, 3_000).await,
            "fetching continued after the setting was turned on"
        );
        assert_eq!(
            tracked_sha(&clone, &branch),
            baseline,
            "remote-tracking ref moved while background operations were disabled"
        );

        // 3. re-enabled: the pending commit must arrive, with no restart
        app_config.write().disable_background_git_operations = false;
        assert!(
            wait_for_sha(&clone, &branch, &second, 15_000).await,
            "fetching did not resume after the setting was turned off"
        );

        handle.abort();
    }

    /// The setting is read on every call, not captured once, which is what makes the toggle take
    /// effect without an app restart. A version that cached the value would pass every test above.
    #[test]
    fn test_should_skip_rereads_config_between_calls() {
        let pause = AtomicBool::new(false);
        let config = config_with(false);

        assert!(!should_skip(&pause, &config));

        config.write().disable_background_git_operations = true;
        assert!(
            should_skip(&pause, &config),
            "should_skip must observe a config change without being rebuilt"
        );

        config.write().disable_background_git_operations = false;
        assert!(
            !should_skip(&pause, &config),
            "should_skip must observe the setting being turned back off"
        );
    }
}
