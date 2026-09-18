use anyhow::{anyhow, Result};
use retry::delay::Fixed;
use retry::{retry_with_index, OperationResult};
use std::collections::HashSet;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use sysinfo::{Pid, ProcessRefreshKind, System, UpdateKind};
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
        // We identify our own threads by TID rather than by name: the test
        // harness renames each test's own worker thread (visible in a panic as
        // `thread '<test name>' panicked`), and threads we spawn inherit that
        // renamed comm, not the process's — a harness quirk, not something
        // `check_for_process` relies on in production, where nothing renames
        // the main thread.
        //
        // Each thread reports its own TID via `/proc/thread-self`
        // (`<pid>/task/<tid>`) instead of the test scanning `/proc/<pid>/task`.
        // That directory holds *every* thread in the test binary, including
        // tokio workers belonging to other tests running in parallel; those
        // come and go independently of this test, and asserting on one that
        // exited between the scan and sysinfo's refresh is what made this test
        // flaky on loaded CI runners.
        //
        // Threads park on `stop` rather than sleeping a fixed duration, pinning
        // their lifetime to the test's own instead of racing sysinfo's refresh
        // against a guessed sleep.
        const THREAD_COUNT: usize = 8;
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (tid_tx, tid_rx) = std::sync::mpsc::channel();
        let handles: Vec<_> = (0..THREAD_COUNT)
            .map(|_| {
                let stop = std::sync::Arc::clone(&stop);
                let tid_tx = tid_tx.clone();
                std::thread::spawn(move || {
                    let tid: u32 = std::fs::read_link("/proc/thread-self")
                        .ok()
                        .and_then(|path| path.file_name()?.to_str()?.parse().ok())
                        .expect("/proc/thread-self should resolve to <pid>/task/<tid>");
                    tid_tx.send(tid).expect("receiver outlives the send");
                    while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                })
            })
            .collect();
        drop(tid_tx);
        // Receive exactly THREAD_COUNT rather than draining to channel close —
        // the senders live in threads that stay parked until `stop`.
        let task_ids: std::collections::HashSet<u32> = (0..THREAD_COUNT)
            .map(|_| tid_rx.recv().expect("each spawned thread reports its TID"))
            .collect();
        assert_eq!(
            task_ids.len(),
            THREAD_COUNT,
            "every spawned thread should have reported a distinct TID"
        );

        let mut system = sysinfo::System::new();
        let refresh_kind =
            sysinfo::ProcessRefreshKind::new().with_exe(sysinfo::UpdateKind::OnlyIfNotSet);
        system.refresh_processes_specifics(refresh_kind);

        // Those threads are all still parked, so sysinfo must have seen at
        // least one of them. Without this the loop below could pass vacuously —
        // and a failure here means sysinfo stopped listing tasks at all, which
        // is the premise `check_for_process`'s filter rests on.
        let listed = task_ids
            .iter()
            .filter(|tid| system.process(Pid::from_u32(**tid)).is_some())
            .count();
        assert!(
            listed > 0,
            "sysinfo listed none of our {THREAD_COUNT} live threads; the check below would be vacuous"
        );

        for tid in &task_ids {
            // Absence is acceptable: `check_for_process` only inspects entries
            // sysinfo actually lists, so a TID it never reports cannot be
            // mistaken for another instance. The invariant that matters is the
            // narrower one — anything it *does* list for one of our threads
            // must be marked as a thread, because `check_for_process` kills
            // entries whose `thread_kind()` is None.
            if let Some(process) = system.process(Pid::from_u32(*tid)) {
                assert!(
                    process.thread_kind().is_some(),
                    "TID {tid} is one of our own threads but sysinfo reported it as a \
                     process (thread_kind() == None) — exactly the condition that makes \
                     check_for_process kill its own instance"
                );
            }
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

/// A running program, for telling the user what is in the way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunningProcess {
    pub name: String,
    pub exe: PathBuf,
}

/// Programs currently running out of `root`.
///
/// A process holds its own image open for as long as it runs, so anything executing from
/// inside a directory we are about to overwrite will refuse to be overwritten - on Windows
/// as a sharing violation, which arrives as a permission error partway through a download
/// that has already done most of its work.
///
/// Checks by location rather than by name deliberately. A name list would have to be kept
/// in step with every executable a project ships, and would still miss the one someone
/// launched by hand; where a program is running from is the thing that actually decides
/// whether its files can be replaced.
pub fn processes_running_from(root: &Path) -> Vec<RunningProcess> {
    let Ok(root) = root.canonicalize() else {
        // A directory that does not exist yet cannot have anything running from it.
        return Vec::new();
    };

    let mut system = System::new();
    system.refresh_processes_specifics(ProcessRefreshKind::new().with_exe(UpdateKind::Always));

    let mut found: Vec<RunningProcess> = system
        .processes()
        .values()
        .filter_map(|process| {
            let exe = process.exe()?;
            // Canonicalised on both sides so a mix of short paths, symlinks and casing on
            // Windows does not read as a different directory.
            let exe = exe.canonicalize().ok()?;
            exe.starts_with(&root).then(|| RunningProcess {
                name: process.name().to_string(),
                exe,
            })
        })
        .collect();

    // One line per program, not per process: several instances of the editor is still one
    // thing for the user to close.
    found.sort_by(|a, b| a.name.cmp(&b.name));
    found.dedup_by(|a, b| a.name == b.name);
    found
}

/// Whether a filesystem error means another program has the file, rather than anything
/// else that could deny a write.
///
/// `ErrorKind::PermissionDenied` alone is not enough on Windows, which is the platform
/// where this actually happens. Rust maps only `ERROR_ACCESS_DENIED` to that kind; a file
/// held open by another process is `ERROR_SHARING_VIOLATION` (32), which has no kind of
/// its own and arrives as `Uncategorized` - so the case this exists for, an editor
/// holding a DLL, would be read as a disk problem and the user told to check free space.
///
/// `ERROR_LOCK_VIOLATION` (33) and `ERROR_USER_MAPPED_FILE` (1224) are the same situation
/// reported differently: a byte range locked, and a file someone has mapped into memory.
pub fn is_held_by_another_program(error: &std::io::Error) -> bool {
    if error.kind() == std::io::ErrorKind::PermissionDenied {
        return true;
    }

    #[cfg(target_os = "windows")]
    {
        const ERROR_SHARING_VIOLATION: i32 = 32;
        const ERROR_LOCK_VIOLATION: i32 = 33;
        const ERROR_USER_MAPPED_FILE: i32 = 1224;

        matches!(
            error.raw_os_error(),
            Some(ERROR_SHARING_VIOLATION | ERROR_LOCK_VIOLATION | ERROR_USER_MAPPED_FILE)
        )
    }

    #[cfg(not(target_os = "windows"))]
    {
        false
    }
}

/// A sentence naming what is in the way, or `None` when nothing is.
pub fn describe_blocking_processes(root: &Path) -> Option<String> {
    let running = processes_running_from(root);
    if running.is_empty() {
        return None;
    }

    let names: Vec<&str> = running.iter().map(|p| p.name.as_str()).collect();
    Some(format!(
        "{} running from {}",
        names.join(", "),
        root.display()
    ))
}

#[cfg(test)]
mod blocking_process_tests {
    use super::*;

    /// A directory that does not exist has nothing running from it, and asking must not
    /// be an error - this runs before a download that would have created it.
    #[test]
    fn a_missing_directory_blocks_nothing() {
        let missing = Path::new("/definitely/not/here/at/all");

        assert!(processes_running_from(missing).is_empty());
        assert!(describe_blocking_processes(missing).is_none());
    }

    /// An empty directory is the normal case: nothing is running from a freshly made
    /// download target, so a sync must not be refused.
    #[test]
    fn an_empty_directory_blocks_nothing() {
        let dir = tempfile::tempdir().unwrap();

        assert!(processes_running_from(dir.path()).is_empty());
        assert!(describe_blocking_processes(dir.path()).is_none());
    }

    /// The test binary itself is running, so the directory it lives in must be reported.
    /// This is what proves the check actually looks at running processes rather than
    /// quietly finding nothing.
    #[test]
    fn a_directory_we_are_running_from_is_reported() {
        let own_exe = std::env::current_exe().expect("test binary path");
        let own_dir = own_exe.parent().expect("test binary directory");

        let found = processes_running_from(own_dir);

        assert!(
            !found.is_empty(),
            "this test process runs from {own_dir:?} and should have been found"
        );
        assert!(
            describe_blocking_processes(own_dir)
                .expect("something is running from here")
                .contains(&own_dir.display().to_string()),
            "the message names the directory so the user knows which one"
        );
    }

    /// One line per program. Several editor windows is still one thing to close.
    #[test]
    fn each_program_is_reported_once() {
        let own_dir = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .to_owned();

        let found = processes_running_from(&own_dir);

        let mut names: Vec<&str> = found.iter().map(|p| p.name.as_str()).collect();
        let before = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(before, names.len(), "duplicate program names in {found:?}");
    }
}

#[cfg(test)]
mod held_file_tests {
    use super::*;

    #[test]
    fn a_permission_denied_write_counts_as_held() {
        let denied = std::io::Error::from(std::io::ErrorKind::PermissionDenied);

        assert!(is_held_by_another_program(&denied));
    }

    /// Everything else is a different problem with a different remedy, and telling
    /// someone to close a program they do not have open sends them hunting for nothing.
    #[test]
    fn other_failures_do_not_count_as_held() {
        for kind in [
            std::io::ErrorKind::NotFound,
            std::io::ErrorKind::StorageFull,
            std::io::ErrorKind::InvalidInput,
        ] {
            let error = std::io::Error::from(kind);
            assert!(!is_held_by_another_program(&error), "{kind:?}");
        }
    }

    /// The case this exists for. Rust maps only ERROR_ACCESS_DENIED to PermissionDenied,
    /// so a file an editor has open - ERROR_SHARING_VIOLATION - arrives with no kind of
    /// its own and would otherwise read as a disk problem.
    #[cfg(target_os = "windows")]
    #[test]
    fn a_windows_sharing_violation_counts_as_held() {
        for raw in [32, 33, 1224] {
            let error = std::io::Error::from_raw_os_error(raw);
            assert!(is_held_by_another_program(&error), "raw os error {raw}");
        }

        // A genuinely different Windows failure still is not this.
        let disk_full = std::io::Error::from_raw_os_error(112); // ERROR_DISK_FULL
        assert!(!is_held_by_another_program(&disk_full));
    }
}
