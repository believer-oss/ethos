use anyhow::{anyhow, Result};
use retry::delay::Fixed;
use retry::{retry_with_index, OperationResult};
use std::net::TcpListener;
use std::process::{Output, Stdio};
use sysinfo::Pid;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tracing::{info, warn};

static STARTUP_RETRY_ATTEMPTS: usize = 30;

/// Run `cmd`, feeding it `stdin` while concurrently draining its stdout, and
/// return the captured `Output`.
///
/// The concurrency is the point. The naive approach — write all of stdin,
/// *then* read stdout — deadlocks whenever the child emits more than a pipe
/// buffer's worth (~64 KB) of output before it has consumed all of its input:
/// the child blocks writing stdout while we block writing stdin, each waiting
/// for the other to drain. Writing stdin from a dedicated task lets both
/// pipes move at once. Callers feeding large inputs (e.g. a NUL-separated
/// path list to `git check-attr --stdin`) depend on this.
///
/// The caller fully configures `cmd` (program, args, env, cwd, and on Windows
/// any creation flags); this function owns only the stdio wiring. A non-zero
/// exit is returned in `Output`, not turned into an error — callers that read
/// the exit code as data (e.g. `git diff --check`) need to see it.
pub async fn run_with_stdin(mut cmd: Command, stdin: Vec<u8>) -> Result<Output> {
    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    let mut child = cmd.spawn()?;
    let mut child_stdin = child
        .stdin
        .take()
        .ok_or_else(|| anyhow!("child stdin was not piped"))?;

    let writer = tokio::spawn(async move {
        let result = child_stdin.write_all(&stdin).await;
        // Dropping the handle closes the pipe, signalling EOF so the child
        // can finish reading and exit.
        drop(child_stdin);
        result
    });

    let output = child.wait_with_output().await?;
    writer
        .await
        .map_err(|e| anyhow!("stdin writer task panicked: {e}"))??;

    Ok(output)
}

/// A Windows Job Object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` wrapping one
/// spawned child: terminating the job — or merely dropping this guard — kills
/// the child's entire process tree, not just the child. Exists because
/// `Child::kill` is `TerminateProcess` on the direct child only: git's helpers
/// (`git-remote-https`, a GCM credential prompt) survive it, keep the
/// inherited stdout/stderr pipe write ends open, and starve the pipe readers
/// of EOF. Job membership is inherited by every descendant, so closing the job
/// takes the stragglers down and the pipes actually close.
///
/// Best-effort by design: `assign` returning `None` (already-exited child, job
/// API failure) must degrade at the call site to a plain kill, never to an
/// error.
#[cfg(windows)]
pub struct ProcessTreeJob {
    handle: windows::Win32::Foundation::HANDLE,
}

#[cfg(windows)]
impl ProcessTreeJob {
    /// Create the job and put `child` — and, transitively, its future
    /// descendants — in it. Anything the child spawned *before* this call
    /// lands outside the job; assign immediately after spawn to keep that
    /// window negligible.
    pub fn assign(child: &tokio::process::Child) -> Option<Self> {
        use windows::core::PCWSTR;
        use windows::Win32::Foundation::HANDLE;
        use windows::Win32::System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
            SetInformationJobObject, JOBOBJECT_BASIC_LIMIT_INFORMATION,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        };

        // None once the child has been reaped — nothing left to assign.
        let raw = child.raw_handle()?;

        // Wrapped in Self immediately so an early return below still closes
        // the job handle via Drop.
        let job = match unsafe { CreateJobObjectW(None, PCWSTR::null()) } {
            Ok(handle) => Self { handle },
            Err(e) => {
                warn!("CreateJobObjectW failed: {e}");
                return None;
            }
        };

        let info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION {
            BasicLimitInformation: JOBOBJECT_BASIC_LIMIT_INFORMATION {
                LimitFlags: JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                ..Default::default()
            },
            ..Default::default()
        };
        if let Err(e) = unsafe {
            SetInformationJobObject(
                job.handle,
                JobObjectExtendedLimitInformation,
                std::ptr::from_ref(&info).cast(),
                std::mem::size_of_val(&info) as u32,
            )
        } {
            warn!("SetInformationJobObject failed: {e}");
            return None;
        }

        if let Err(e) = unsafe { AssignProcessToJobObject(job.handle, HANDLE(raw as isize)) } {
            // E.g. a sandbox/launcher job that forbids nesting
            // (pre-Windows-8 semantics).
            warn!("AssignProcessToJobObject failed: {e}");
            return None;
        }

        Some(job)
    }

    /// Kill every process still in the job, immediately.
    pub fn terminate(&self) {
        if let Err(e) =
            unsafe { windows::Win32::System::JobObjects::TerminateJobObject(self.handle, 1) }
        {
            warn!("TerminateJobObject failed: {e}");
        }
    }
}

#[cfg(windows)]
impl Drop for ProcessTreeJob {
    fn drop(&mut self) {
        // KILL_ON_JOB_CLOSE fires when the last handle closes, so plain drop
        // also kills anything still alive in the job.
        let _ = unsafe { windows::Win32::Foundation::CloseHandle(self.handle) };
    }
}

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

pub fn check_for_process(name: &str, port: u16) -> Result<()> {
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

            match listeners::get_processes_by_port(port) {
                Ok(listeners) => {
                    for listener in listeners {
                        if listener.pid != my_pid {
                            if let Some(process) = system.process(Pid::from_u32(listener.pid)) {
                                warn!(
                                    "Found existing process {} on port {} but couldn't reach its API. Attempting to kill it.",
                                    process.name(),
                                    port
                                );

                                return match process.kill() {
                                    true => OperationResult::Ok(()),
                                    false => OperationResult::Retry(format!(
                                        "Failed to kill process {}, retrying, ({}/{})",
                                        listener.pid,
                                        attempt + 1,
                                        STARTUP_RETRY_ATTEMPTS
                                    )),
                                };
                            }
                        }
                    }
                }
                Err(e) => {
                    warn!("Failed to get processes by port: {:?}", e);
                    return OperationResult::Retry(format!(
                        "Failed to get processes by port: {:?}, retrying, ({}/{})",
                        e,
                        attempt + 1,
                        STARTUP_RETRY_ATTEMPTS
                    ));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn run_with_stdin_feeds_input_and_captures_output() {
        // `sort` ships on both Windows and Unix and reads stdin when given no
        // file argument — enough to prove stdin reached the child and its
        // stdout came back. We assert on ordering rather than exact bytes so
        // the test is immune to line-ending differences across platforms.
        let out = run_with_stdin(Command::new("sort"), b"banana\napple\ncherry\n".to_vec())
            .await
            .expect("sort ran");
        assert!(out.status.success());
        let stdout = String::from_utf8_lossy(&out.stdout);
        let apple = stdout.find("apple").expect("apple present");
        let banana = stdout.find("banana").expect("banana present");
        let cherry = stdout.find("cherry").expect("cherry present");
        assert!(apple < banana && banana < cherry, "not sorted: {stdout:?}");
    }

    #[tokio::test]
    async fn run_with_stdin_surfaces_nonzero_exit() {
        // A non-zero exit must come back as Ok(Output) with a failing status,
        // not an Err — callers like `git diff --check` read the code as data.
        #[cfg(windows)]
        let cmd = {
            let mut c = Command::new("cmd");
            c.args(["/c", "exit", "7"]);
            c
        };
        #[cfg(unix)]
        let cmd = Command::new("false");

        let out = run_with_stdin(cmd, Vec::new()).await.expect("spawned");
        assert!(!out.status.success());
    }

    // ProcessTreeJob must kill the WHOLE tree: cmd spawns ping as a grandchild
    // sharing our stdout pipe; after terminate(), read_to_end must hit EOF
    // promptly, which can only happen once every process holding the pipe's
    // write end (cmd AND ping) is dead. This mirrors the production hazard:
    // git.exe's orphaned children keeping the pipes open past kill() and
    // wedging the reader joins behind the git process lock.
    #[cfg(windows)]
    #[tokio::test]
    async fn process_tree_job_terminate_kills_grandchildren_and_closes_pipes() {
        use tokio::io::AsyncReadExt;

        let mut cmd = Command::new("cmd");
        // ~30s of pings; the test only finishes quickly if they're killed.
        cmd.args(["/c", "ping -n 30 127.0.0.1"]);
        cmd.stdout(Stdio::piped());
        let mut child = cmd.spawn().expect("spawn cmd");
        let mut stdout = child.stdout.take().expect("stdout piped");
        let job = ProcessTreeJob::assign(&child).expect("assign job");

        // Give cmd a beat to spawn ping, so the grandchild exists (and holds
        // the pipe) before the kill.
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        job.terminate();

        let mut buf = Vec::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            stdout.read_to_end(&mut buf),
        )
        .await
        .expect("pipe never reached EOF — a process in the job survived terminate()")
        .expect("read stdout");
        let _ = child.wait().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn run_with_stdin_large_payload_does_not_deadlock() {
        // `cat` streams stdin → stdout, emitting output before it has read all
        // its input — the exact shape that deadlocks the naive "write all
        // stdin, then read stdout" approach once the payload exceeds the pipe
        // buffer in both directions. This must complete and round-trip every
        // byte. Windows shares the production code path; `cat` isn't
        // guaranteed there, so the regression is guarded on Unix.
        let payload = vec![b'x'; 4 * 1024 * 1024];
        let out = run_with_stdin(Command::new("cat"), payload.clone())
            .await
            .expect("cat ran");
        assert!(out.status.success());
        assert_eq!(out.stdout, payload);
    }
}
