use anyhow::{anyhow, Result};
use retry::delay::Fixed;
use retry::{retry_with_index, OperationResult};
use std::net::TcpListener;
use tracing::{info, warn};

static STARTUP_RETRY_ATTEMPTS: usize = 30;

pub fn wait_for_port(port: u16) -> Result<()> {
    let result = retry_with_index(
        Fixed::from_millis(1000).take(STARTUP_RETRY_ATTEMPTS),
        |attempt| {
            if TcpListener::bind(("127.0.0.1", port)).is_ok() {
                return OperationResult::Ok(());
            }

            warn!(
                "Port {} is not available, retrying, ({}/{})",
                port,
                attempt + 1,
                STARTUP_RETRY_ATTEMPTS
            );

            OperationResult::Retry(())
        },
    );

    match result {
        Ok(_) => Ok(()),
        Err(e) => Err(anyhow!("Failed to wait for port: {:?}", e)),
    }
}

pub fn check_for_process(name: &str) -> Result<()> {
    // add bin suffix

    info!("Checking for existing {} process...", name);
    let mut system = sysinfo::System::new();
    let refresh_kind =
        sysinfo::ProcessRefreshKind::new().with_exe(sysinfo::UpdateKind::OnlyIfNotSet);

    let my_pid: u32 = std::process::id();
    let result = retry_with_index(
        Fixed::from_millis(1000).take(STARTUP_RETRY_ATTEMPTS),
        |attempt| {
            system.refresh_processes_specifics(refresh_kind);

            for (pid, process) in system.processes() {
                let proc_name = process.name().to_lowercase();
                let proc_path_dev = process
                    .exe()
                    .map(|p| p.to_string_lossy().contains("target"));
                if pid.as_u32() != my_pid
                    && proc_name.contains(&name.to_lowercase())
                    && !proc_path_dev.is_some_and(|p| p)
                {
                    warn!("Found existing process {} but couldn't reach its API. Attempting to kill it.", pid);
                    return match process.kill() {
                        true => OperationResult::Ok(()),
                        false => OperationResult::Retry(format!(
                            "Failed to kill process {}, retrying, ({}/{})",
                            pid,
                            attempt + 1,
                            STARTUP_RETRY_ATTEMPTS
                        )),
                    };
                }
            }

            OperationResult::Ok(())
        },
    );

    match result {
        Ok(_) => Ok(()),
        Err(e) => Err(anyhow!("Failed to check for existing process: {}", e)),
    }
}
