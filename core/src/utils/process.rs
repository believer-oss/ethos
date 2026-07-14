use anyhow::{anyhow, Result};
use retry::delay::Fixed;
use retry::{retry_with_index, OperationResult};
use std::collections::HashSet;
use std::net::TcpListener;
use std::path::Path;
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

/// PIDs from `pid` up to the root of the process tree.
///
/// Excludes our own ancestry from `check_for_process`'s kill scan — e.g. the
/// old instance that spawns a new one during `restart`/relaunch and is still
/// exiting when the new one starts up.
fn ancestor_pids(system: &sysinfo::System, pid: u32) -> HashSet<u32> {
    let mut ancestors = HashSet::new();
    let mut current = Pid::from_u32(pid);
    while let Some(parent) = system.process(current).and_then(sysinfo::Process::parent) {
        if !ancestors.insert(parent.as_u32()) {
            break; // cycle guard; shouldn't happen on a real process tree
        }
        current = parent;
    }
    ancestors
}

/// True if `candidate_exe` is the `.AppImage` file we ourselves were
/// launched from (per `$APPIMAGE`).
///
/// The AppImage runtime double-forks its FUSE-mount server, which gets
/// reparented away from us (not our ancestor) and re-execs the outer
/// `.AppImage` file — unprotected, `check_for_process`'s by-name scan kills
/// it and tears the mount down under us.
fn is_own_appimage_runtime(candidate_exe: Option<&Path>, appimage_env: Option<&str>) -> bool {
    match (candidate_exe, appimage_env) {
        (Some(exe), Some(appimage)) => exe == Path::new(appimage),
        _ => false,
    }
}

pub fn check_for_process(name: &str, port: u16) -> Result<()> {
    // add bin suffix

    info!("Checking for existing {} process...", name);
    let mut system = sysinfo::System::new();
    let refresh_kind =
        sysinfo::ProcessRefreshKind::new().with_exe(sysinfo::UpdateKind::OnlyIfNotSet);

    let my_pid: u32 = std::process::id();
    let appimage_env = std::env::var("APPIMAGE").ok();
    let result = retry_with_index(
        Fixed::from_millis(1000).take(STARTUP_RETRY_ATTEMPTS),
        |attempt| {
            system.refresh_processes_specifics(refresh_kind);
            let excluded_pids = ancestor_pids(&system, my_pid);

            for (pid, process) in system.processes() {
                let proc_name = process.name().to_lowercase();
                let proc_path_dev = process
                    .exe()
                    .map(|p| p.to_string_lossy().contains("target"));
                // `thread_kind()` is Some(_) for the thread entries sysinfo
                // lists alongside real processes on Linux (via
                // `/proc/<pid>/task`) — every thread of every process shows
                // up here with the owning process's own name. Without this,
                // the scan matches one of our own threads as "another
                // instance" and kills it, which is process-directed and
                // takes the whole thing down.
                if pid.as_u32() != my_pid
                    && process.thread_kind().is_none()
                    && !excluded_pids.contains(&pid.as_u32())
                    && !is_own_appimage_runtime(process.exe(), appimage_env.as_deref())
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
                        if listener.pid != my_pid && !excluded_pids.contains(&listener.pid) {
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

    #[test]
    fn ancestor_pids_includes_our_own_parent() {
        // We don't need an actual AppImage to exercise this: spawning any
        // child and walking up from its PID should land on ours, one hop in.
        #[cfg(windows)]
        let mut child = std::process::Command::new("cmd")
            .args(["/c", "timeout", "/t", "5"])
            .spawn()
            .expect("spawn child");
        #[cfg(unix)]
        let mut child = std::process::Command::new("sleep")
            .arg("5")
            .spawn()
            .expect("spawn child");

        let mut system = sysinfo::System::new();
        let refresh_kind =
            sysinfo::ProcessRefreshKind::new().with_exe(sysinfo::UpdateKind::OnlyIfNotSet);
        system.refresh_processes_specifics(refresh_kind);

        let my_pid = std::process::id();
        let ancestors = ancestor_pids(&system, child.id());
        assert!(
            ancestors.contains(&my_pid),
            "expected {ancestors:?} to contain our pid {my_pid}"
        );

        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn is_own_appimage_runtime_matches_appimage_env_exactly() {
        let appimage = "/home/user/Friendshipper.AppImage";

        // The AppImage runtime process itself: exe is the outer file.
        assert!(is_own_appimage_runtime(
            Some(Path::new(appimage)),
            Some(appimage)
        ));

        // Our own payload runs from inside the FUSE mount, not the outer
        // file — must not match, or we'd exclude every duplicate instance's
        // payload too.
        assert!(!is_own_appimage_runtime(
            Some(Path::new("/tmp/.mount_abc123/usr/bin/friendshipper")),
            Some(appimage)
        ));

        // Not running under AppImage at all.
        assert!(!is_own_appimage_runtime(Some(Path::new(appimage)), None));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn own_threads_are_never_mistaken_for_other_processes() {
        // The real-world failure this guards against: on Linux, sysinfo
        // lists every thread of every process as its own entry in
        // `system.processes()` (via `/proc/<pid>/task`), with `pid` = TID.
        // Without filtering on `thread_kind()`, `check_for_process`'s
        // by-name scan matched its own threads as "another instance" and
        // killed itself outright — confirmed against a packaged build, not
        // just in theory.
        //
        // We identify our own threads by TID (read straight from
        // `/proc/<pid>/task`) rather than by name: the test harness renames
        // each test's own worker thread (visible in a panic as `thread
        // '<test name>' panicked`), and threads we spawn inherit that
        // renamed comm, not the process's — a harness quirk, not something
        // `check_for_process` relies on in production, where nothing
        // renames the main thread.
        // Threads park on `stop` rather than sleeping a fixed duration: a
        // fixed sleep raced sysinfo's refresh on loaded CI runners (a slow
        // enough refresh could see the thread already exited), so pin their
        // lifetime to the test's own instead of guessing a duration.
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let handles: Vec<_> = (0..8)
            .map(|_| {
                let stop = std::sync::Arc::clone(&stop);
                std::thread::spawn(move || {
                    while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                })
            })
            .collect();
        // Give the kernel a moment to expose the new tasks under /proc.
        std::thread::sleep(std::time::Duration::from_millis(50));

        let my_pid = std::process::id();
        let task_ids: std::collections::HashSet<u32> =
            std::fs::read_dir(format!("/proc/{my_pid}/task"))
                .expect("our own /proc/<pid>/task should be readable")
                .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse().ok())
                .filter(|tid| *tid != my_pid)
                .collect();
        assert!(
            !task_ids.is_empty(),
            "expected our own spawned threads to show up under /proc/{my_pid}/task"
        );

        let mut system = sysinfo::System::new();
        let refresh_kind =
            sysinfo::ProcessRefreshKind::new().with_exe(sysinfo::UpdateKind::OnlyIfNotSet);
        system.refresh_processes_specifics(refresh_kind);

        for tid in &task_ids {
            let thread_kind = system
                .process(Pid::from_u32(*tid))
                .and_then(|p| p.thread_kind());
            assert!(
                thread_kind.is_some(),
                "TID {tid} is one of our own threads; sysinfo should report it as a thread, not a process"
            );
        }

        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        for handle in handles {
            let _ = handle.join();
        }
    }

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
