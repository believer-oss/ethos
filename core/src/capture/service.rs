//! `CaptureService`: the entry point used by the rest of the app.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender as STDSender;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, TimeDelta, Utc};
use parking_lot::Mutex;
use tokio::runtime::Handle;
use tokio::sync::{watch, Notify, RwLock as TokioRwLock};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use super::cleanup::cleanup_local_traces;
use super::config::{CaptureWatchEntry, ClientCaptureUploadConfig};
use super::ledger::{stat_matches, CaptureStore, LEDGER_FILE};
use super::multipart::{Pacer, PartStore, PauseLevel, S3PartStore};
use super::select::{
    pending_candidates, resolve_watch_dirs, usable_watch_entries, Candidate, ResolvedWatch,
};
use super::types::{
    CaptureLedger, CaptureSession, CaptureStatus, CloseReason, FileKind, FileState, FileStatus,
    InflightUpload, PendingSummary, RegisterSession, SessionState, SessionStatus,
};
use super::uploader::{file_name, CaptureUploader, Progress, RetryState, ToastLimiter};
use super::watcher::{
    game_running, scan_roots, step, Clock, GameProcessWatcher, ProcessScanner, StepContext,
    SysinfoScanner, SystemClock, POLL,
};
use crate::clients::aws::AWSClient;
use crate::types::config::{AppConfigRef, DynamicConfigRef};

const PAUSE_TICK: Duration = Duration::from_secs(1);
const MAINTENANCE_INTERVAL: Duration = Duration::from_secs(60 * 60);
const UPLOADED_VISIBLE: TimeDelta = TimeDelta::seconds(60);
const AUTH_HOLD_MAX: TimeDelta = TimeDelta::minutes(15);
const SIGN_IN: &str = "Sign in to Friendshipper";
const DISABLED: &str = "Capture upload is disabled";

/// Toast raised by the service; the app maps it to its own notification type.
#[derive(Clone, Debug)]
pub enum CaptureNotification {
    Success(String),
    Error(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthState {
    NoClient,
    SignedOut,
    Expired,
    Ready { expires_at: Option<DateTime<Utc>> },
}

/// Everything the service needs from AWS, behind a trait so tests need no credentials.
#[async_trait]
pub trait CaptureBackend: Send + Sync + 'static {
    fn has_client(&self) -> bool;
    async fn auth_state(&self) -> AuthState;
    async fn artifact_bucket(&self) -> Option<String>;
    /// Built per file so refreshed credentials apply.
    async fn part_store(&self) -> Option<Arc<dyn PartStore>>;
}

pub struct AwsBackend {
    client: Arc<TokioRwLock<Option<AWSClient>>>,
}

impl AwsBackend {
    async fn client(&self) -> Option<AWSClient> {
        self.client.read().await.clone()
    }
}

#[async_trait]
impl CaptureBackend for AwsBackend {
    fn has_client(&self) -> bool {
        // A held write lock means a sign-in is installing a client.
        self.client.try_read().map_or(true, |c| c.is_some())
    }

    async fn auth_state(&self) -> AuthState {
        let Some(client) = self.client().await else {
            return AuthState::NoClient;
        };
        if client.login_required().await {
            AuthState::SignedOut
        } else if client.check_expiration().await.is_err() {
            AuthState::Expired
        } else {
            AuthState::Ready {
                expires_at: client.get_credential_expiration().await,
            }
        }
    }

    async fn artifact_bucket(&self) -> Option<String> {
        self.client().await.map(|c| c.get_artifact_bucket())
    }

    async fn part_store(&self) -> Option<Arc<dyn PartStore>> {
        let client = self.client().await?;
        let store: Arc<dyn PartStore> = Arc::new(S3PartStore::new(&client.get_sdk_config().await));
        Some(store)
    }
}

struct DynScanner(Arc<dyn ProcessScanner>);

impl ProcessScanner for DynScanner {
    fn running_from(&self, root: &Path) -> bool {
        self.0.running_from(root)
    }
}

struct DynClock(Arc<dyn Clock>);

impl Clock for DynClock {
    fn now(&self) -> DateTime<Utc> {
        self.0.now()
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    Hashing,
    Uploading,
}

pub(crate) struct Current {
    session_id: String,
    path: PathBuf,
    phase: Phase,
    progress: Option<Arc<Progress>>,
    started: Instant,
}

struct AuthHold {
    expires_at: Option<DateTime<Utc>>,
    since: DateTime<Utc>,
}

/// In-memory uploader state; never persisted.
#[derive(Default)]
pub(crate) struct WorkState {
    pub(crate) hash_cache: HashMap<PathBuf, (u64, i64, String)>,
    pub(crate) retry: HashMap<PathBuf, RetryState>,
    /// `Fatal` for this process lifetime, with the message.
    pub(crate) failed: HashMap<PathBuf, String>,
    /// Files owned by the worker (the in-flight path guard).
    pub(crate) owned: HashSet<PathBuf>,
    pub(crate) toasts: ToastLimiter,
    current: Option<Current>,
    tokens: HashMap<String, CancellationToken>,
    auth_hold: Option<AuthHold>,
    files: HashMap<String, Vec<Candidate>>,
    watch_cache: HashMap<PathBuf, (Vec<CaptureWatchEntry>, Vec<ResolvedWatch>)>,
    blocked_reason: Option<String>,
}

/// State shared by the service handle and its two tasks once started.
pub(crate) struct Shared {
    app_config: AppConfigRef,
    dynamic_config: DynamicConfigRef,
    data_dir: PathBuf,
    pub(crate) backend: Arc<dyn CaptureBackend>,
    clock: Arc<dyn Clock>,
    pub(crate) store: CaptureStore,
    pub(crate) root: CancellationToken,
    pause_tx: watch::Sender<PauseLevel>,
    pause_flag: Arc<AtomicBool>,
    game_running: AtomicBool,
    pub(crate) wake_watcher: Notify,
    pub(crate) wake_uploader: Notify,
    pub(crate) pacer: Arc<Pacer>,
    pub(crate) work: Mutex<WorkState>,
    notify_tx: STDSender<CaptureNotification>,
    events_tx: STDSender<CaptureStatus>,
    last_status: Mutex<Option<CaptureStatus>>,
    handle: Handle,
    tasks: Mutex<Vec<JoinHandle<()>>>,
}

impl Shared {
    pub(crate) fn now(&self) -> DateTime<Utc> {
        self.clock.now()
    }

    pub(crate) fn pause_rx(&self) -> watch::Receiver<PauseLevel> {
        self.pause_tx.subscribe()
    }

    /// The config to act on, or `None` when inert (serverless, no block, no usable watch entry).
    pub(crate) fn active_config(&self) -> Option<ClientCaptureUploadConfig> {
        active_config(&self.app_config, &self.dynamic_config)
    }

    pub(crate) fn watches_for(
        &self,
        install_dir: &Path,
        cfg: &ClientCaptureUploadConfig,
    ) -> Vec<ResolvedWatch> {
        if let Some((entries, resolved)) = self.work.lock().watch_cache.get(install_dir) {
            if *entries == cfg.watch {
                return resolved.clone();
            }
        }
        let resolved = resolve_watch_dirs(install_dir, &cfg.watch);
        self.work.lock().watch_cache.insert(
            install_dir.to_path_buf(),
            (cfg.watch.clone(), resolved.clone()),
        );
        resolved
    }

    pub(crate) fn session_token(&self, session_id: &str) -> CancellationToken {
        self.work
            .lock()
            .tokens
            .entry(session_id.to_owned())
            .or_default()
            .clone()
    }

    pub(crate) fn begin(&self, session_id: &str, path: &Path, phase: Phase) {
        let mut work = self.work.lock();
        work.owned.insert(path.to_path_buf());
        work.current = Some(Current {
            session_id: session_id.to_owned(),
            path: path.to_path_buf(),
            phase,
            progress: None,
            started: Instant::now(),
        });
    }

    pub(crate) fn set_uploading(&self, path: &Path, progress: Arc<Progress>) {
        let mut work = self.work.lock();
        if let Some(current) = work.current.as_mut().filter(|c| c.path == path) {
            current.phase = Phase::Uploading;
            current.progress = Some(progress);
            current.started = Instant::now();
        }
    }

    pub(crate) fn finish(&self, path: &Path) {
        {
            let mut work = self.work.lock();
            work.owned.remove(path);
            if work.current.as_ref().is_some_and(|c| c.path == path) {
                work.current = None;
            }
        }
        self.emit();
    }

    pub(crate) fn notify_error(&self, text: String) {
        let _ = self.notify_tx.send(CaptureNotification::Error(text));
    }

    /// Stops new uploads until the credentials change (a sign-in) or the hold times out.
    pub(crate) async fn hold_for_sign_in(&self) {
        let expires_at = match self.backend.auth_state().await {
            AuthState::Ready { expires_at } => expires_at,
            _ => None,
        };
        let since = self.now();
        {
            let mut work = self.work.lock();
            work.auth_hold = Some(AuthHold { expires_at, since });
            work.blocked_reason = Some(SIGN_IN.to_owned());
        }
        self.pause_tx.send_if_modified(|level| {
            let changed = *level == PauseLevel::None || *level == PauseLevel::Soft;
            if changed {
                *level = PauseLevel::Blocked;
            }
            changed
        });
    }

    /// Prune, then delete surplus uploaded traces (D2 section 10).
    pub(crate) async fn maintain(self: &Arc<Self>, cfg: &ClientCaptureUploadConfig) {
        let shared = self.clone();
        let (keep, days, now) = (
            cfg.keep_uploaded_traces,
            cfg.ledger_retention_days,
            self.now(),
        );
        let _ = tokio::task::spawn_blocking(move || {
            shared.store.prune(now, days);
            let ledger = shared.store.snapshot();
            let report = cleanup_local_traces(&ledger, keep, shared.store.persist_failed());
            if !report.deleted.is_empty() {
                info!(
                    "Deleted {} uploaded local trace(s): {:?}",
                    report.deleted.len(),
                    report.deleted
                );
            }
        })
        .await;
    }

    fn extra_root(&self) -> PathBuf {
        match self.app_config.read().selected_artifact_project.as_deref() {
            Some(project) if !project.is_empty() => self.data_dir.join(project),
            _ => self.data_dir.clone(),
        }
    }

    /// Removes rows (except ones the worker owns, which its driver aborts) and aborts their MPUs best effort.
    fn drop_rows(self: &Arc<Self>, rows: Vec<InflightUpload>) {
        if rows.is_empty() {
            return;
        }
        let now = self.now();
        for row in &rows {
            self.store.remove_inflight(&row.local_path, now);
        }
        self.store.flush(now);
        let aborts: Vec<InflightUpload> =
            rows.into_iter().filter(|r| r.upload_id.is_some()).collect();
        if aborts.is_empty() {
            return;
        }
        let shared = self.clone();
        self.handle.spawn(async move {
            let Some(store) = shared.backend.part_store().await else {
                warn!(
                    "No AWS client to abort {} capture upload(s); S3 lifecycle will clean them up",
                    aborts.len()
                );
                return;
            };
            for row in aborts {
                let upload_id = row.upload_id.as_deref().unwrap_or_default();
                if let Err(e) = store.abort(&row.bucket, &row.key, upload_id).await {
                    warn!("Could not abort multipart upload for {}: {e:?}", row.key);
                }
            }
        });
    }

    fn close_sessions(
        self: &Arc<Self>,
        matches: impl Fn(&CaptureSession) -> bool,
        reason: CloseReason,
        all_rows: bool,
    ) -> usize {
        let now = self.now();
        let ids: Vec<String> = self.store.with(|l| {
            l.sessions
                .iter()
                .filter(|s| s.state != SessionState::Closed && matches(s))
                .map(|s| s.id.clone())
                .collect()
        });
        let mut closed = 0;
        for id in &ids {
            let mut did = false;
            self.store.update_session(id, now, |s| {
                if s.state != SessionState::Closed {
                    s.state = SessionState::Closed;
                    s.close_reason = Some(reason);
                    s.closed_at = Some(now);
                    s.empty_polls = 0;
                    s.first_empty_at = None;
                    did = true;
                }
            });
            if did {
                closed += 1;
            }
            self.session_token(id).cancel();
        }
        let owned = self.work.lock().owned.clone();
        let rows: Vec<InflightUpload> = self.store.with(|l| {
            l.inflight
                .iter()
                .filter(|r| {
                    (all_rows || ids.contains(&r.session_id)) && !owned.contains(&r.local_path)
                })
                .cloned()
                .collect()
        });
        self.drop_rows(rows);
        if closed > 0 {
            info!("Closed {closed} capture session(s) as {reason:?}");
        }
        self.wake_watcher.notify_one();
        self.emit();
        closed
    }

    async fn initialize(self: &Arc<Self>) {
        if let Some(cfg) = self.active_config() {
            self.maintain(&cfg).await;
        }
        self.reconcile().await;
    }

    /// Drops rows whose session is closed or missing, or whose file is gone or changed.
    async fn reconcile(self: &Arc<Self>) {
        let ledger = self.store.snapshot();
        let stale = tokio::task::spawn_blocking(move || {
            ledger
                .inflight
                .iter()
                .filter(|r| {
                    let open = ledger
                        .sessions
                        .iter()
                        .any(|s| s.id == r.session_id && s.state != SessionState::Closed);
                    !open || !stat_matches(&r.local_path, r.size, r.mtime_ms)
                })
                .cloned()
                .collect::<Vec<_>>()
        })
        .await
        .unwrap_or_default();
        if !stale.is_empty() {
            info!("Dropping {} stale capture upload row(s)", stale.len());
        }
        self.drop_rows(stale);
    }

    async fn update_pause(&self, cfg: Option<&ClientCaptureUploadConfig>) {
        let auth = self.backend.auth_state().await;
        let now = self.now();
        let inert_reason = cfg.is_none().then(|| self.inert_reason(&auth));
        let level = {
            let mut work = self.work.lock();
            let released = work.auth_hold.as_ref().is_some_and(|hold| {
                now - hold.since >= AUTH_HOLD_MAX
                    || matches!(&auth, AuthState::Ready { expires_at } if *expires_at != hold.expires_at)
            });
            if released {
                work.auth_hold = None;
            }
            let signed_out = !matches!(auth, AuthState::Ready { .. }) || work.auth_hold.is_some();
            let hard = cfg.is_some_and(|c| c.pause_while_game_running)
                && self.game_running.load(Ordering::Relaxed);
            let (level, reason) = if inert_reason.is_some() {
                (PauseLevel::Blocked, inert_reason)
            } else if hard {
                (PauseLevel::Hard, None)
            } else if signed_out {
                (PauseLevel::Blocked, Some(SIGN_IN))
            } else if self.pause_flag.load(Ordering::Relaxed) {
                (PauseLevel::Soft, None)
            } else {
                (PauseLevel::None, None)
            };
            work.blocked_reason = reason.map(str::to_owned);
            level
        };
        let changed = self.pause_tx.send_if_modified(|current| {
            let changed = *current != level;
            *current = level;
            changed
        });
        if changed && level == PauseLevel::None {
            self.wake_uploader.notify_one();
        }
    }

    /// Dynamic config loads only with credentials, so a missing block while signed out usually
    /// means "not loaded yet"; every other inert case is a real disable.
    fn inert_reason(&self, auth: &AuthState) -> &'static str {
        let serverless = self.app_config.read().serverless;
        let block_missing = self.dynamic_config.read().client_capture_upload.is_none();
        if !serverless && block_missing && !matches!(auth, AuthState::Ready { .. }) {
            SIGN_IN
        } else {
            DISABLED
        }
    }

    async fn poll(
        self: &Arc<Self>,
        watcher: &GameProcessWatcher<DynScanner, DynClock>,
        cfg: &ClientCaptureUploadConfig,
        restored: &mut HashSet<String>,
    ) {
        let sessions: Vec<CaptureSession> = self.store.with(|l| {
            l.sessions
                .iter()
                .filter(|s| s.state != SessionState::Closed)
                .cloned()
                .collect()
        });
        let extra_root = self.extra_root();
        let snapshot = watcher.scan(scan_roots(&sessions, Some(&extra_root))).await;
        let now = self.now();
        let quiescence = TimeDelta::seconds(i64::try_from(cfg.quiescence_seconds).unwrap_or(3600));
        let busy = self
            .work
            .lock()
            .current
            .as_ref()
            .map(|c| c.session_id.clone());

        let mut changed = false;
        let mut expired = Vec::new();
        for session in &sessions {
            let ctx = StepContext {
                first_poll_after_start: restored.contains(&session.id),
                quiescence,
                has_uploading_file: busy.as_deref() == Some(session.id.as_str()),
            };
            let tr = step(session, snapshot.poll_for(session), now, &ctx);
            let noop = tr.state == session.state
                && tr.exited_at == session.exited_at
                && tr.close_reason == session.close_reason
                && tr.empty_polls == session.empty_polls
                && tr.first_empty_at == session.first_empty_at;
            if noop {
                continue;
            }
            self.store.update_session(&session.id, now, |cur| {
                if cur.state == session.state {
                    changed |= tr.apply(cur, now);
                }
            });
            if tr.state == SessionState::Closed && tr.close_reason == Some(CloseReason::Expired) {
                expired.push(session.id.clone());
            }
        }
        restored.clear();
        let running = self
            .store
            .with(|l| game_running(&snapshot, &l.sessions, now));
        self.game_running.store(running, Ordering::Relaxed);

        if !expired.is_empty() {
            info!("Expired {} capture session(s)", expired.len());
            let owned = self.work.lock().owned.clone();
            for id in &expired {
                self.session_token(id).cancel();
            }
            let rows: Vec<InflightUpload> = self.store.with(|l| {
                l.inflight
                    .iter()
                    .filter(|r| expired.contains(&r.session_id) && !owned.contains(&r.local_path))
                    .cloned()
                    .collect()
            });
            self.drop_rows(rows);
        }

        changed |= self.settle_exited(cfg, quiescence).await;
        if changed {
            self.wake_uploader.notify_one();
        }
    }

    /// Refreshes the per-session file lists and closes `Exited` sessions with nothing left to upload.
    async fn settle_exited(
        self: &Arc<Self>,
        cfg: &ClientCaptureUploadConfig,
        quiescence: TimeDelta,
    ) -> bool {
        let now = self.now();
        let ledger = self.store.snapshot();
        let jobs: Vec<(CaptureSession, Vec<ResolvedWatch>)> = ledger
            .sessions
            .iter()
            .filter(|s| s.state != SessionState::Closed)
            .map(|s| (s.clone(), self.watches_for(&s.install_dir, cfg)))
            .collect();
        let hash_cache = self.work.lock().hash_cache.clone();
        let q = cfg.quiescence_seconds;
        let files: HashMap<String, Vec<Candidate>> = tokio::task::spawn_blocking(move || {
            jobs.iter()
                .map(|(s, watches)| {
                    let files = unuploaded_files(s, &ledger, watches, q, now, &hash_cache);
                    (s.id.clone(), files)
                })
                .collect()
        })
        .await
        .unwrap_or_default();

        let ctx = StepContext {
            first_poll_after_start: false,
            quiescence,
            has_uploading_file: false,
        };
        let ready: Vec<CaptureSession> = self.store.with(|l| {
            l.sessions
                .iter()
                .filter(|s| {
                    ctx.quiescence_elapsed(s, now)
                        && files.get(&s.id).is_some_and(Vec::is_empty)
                        && !l.inflight.iter().any(|r| r.session_id == s.id)
                })
                .cloned()
                .collect()
        });
        let busy = {
            let mut work = self.work.lock();
            work.files = files;
            work.current.as_ref().map(|c| c.session_id.clone())
        };

        let mut changed = false;
        for session in ready {
            if busy.as_deref() == Some(session.id.as_str()) {
                continue;
            }
            let mut closed = false;
            self.store.update_session(&session.id, now, |s| {
                if s.state == SessionState::Exited {
                    s.state = SessionState::Closed;
                    s.close_reason = Some(CloseReason::Uploaded);
                    s.closed_at = Some(now);
                    closed = true;
                }
            });
            if !closed {
                continue;
            }
            changed = true;
            let (count, bytes) = self.store.with(|l| {
                l.uploaded
                    .values()
                    .filter(|e| e.session_id == session.id)
                    .fold((0u32, 0u64), |(n, b), e| (n + 1, b + e.size))
            });
            info!(
                "Capture session {} closed: {count} file(s) uploaded",
                session.id
            );
            if count > 0 {
                let _ = self.notify_tx.send(CaptureNotification::Success(format!(
                    "Uploaded {count} capture files ({}) for {}",
                    format_size(bytes),
                    session.playtest
                )));
            }
        }
        changed
    }

    pub(crate) fn emit(&self) {
        let status = self.build_status();
        let mut last = self.last_status.lock();
        if last.as_ref() != Some(&status) {
            let _ = self.events_tx.send(status.clone());
            *last = Some(status);
        }
    }

    fn build_status(&self) -> CaptureStatus {
        let cfg = self.active_config();
        // Inert hides the panel (D2 section 9); waiting for a sign-in is not inert.
        if cfg.is_none() && self.work.lock().blocked_reason.as_deref() != Some(SIGN_IN) {
            return CaptureStatus::default();
        }
        let now = self.now();
        let pause = *self.pause_tx.borrow();
        let q = cfg.map_or(30, |c| c.quiescence_seconds as i64);
        let (mut sessions, inflight, uploaded) = self.store.with(|l| {
            let sessions: Vec<CaptureSession> = l
                .sessions
                .iter()
                .filter(|s| {
                    s.state != SessionState::Closed
                        || (s.close_reason == Some(CloseReason::Uploaded)
                            && s.closed_at.is_some_and(|t| now - t <= UPLOADED_VISIBLE))
                })
                .cloned()
                .collect();
            let ids: HashSet<&str> = sessions.iter().map(|s| s.id.as_str()).collect();
            let inflight: Vec<InflightUpload> = l
                .inflight
                .iter()
                .filter(|r| ids.contains(r.session_id.as_str()))
                .cloned()
                .collect();
            let uploaded: Vec<(String, PathBuf, u64)> = l
                .uploaded
                .values()
                .filter(|e| ids.contains(e.session_id.as_str()))
                .map(|e| (e.session_id.clone(), e.local_path.clone(), e.size))
                .collect();
            (sessions, inflight, uploaded)
        });
        sessions.sort_by_key(|s| std::cmp::Reverse(s.launched_at));

        let work = self.work.lock();
        let describe =
            |path: &Path, size: u64, default: FileState, row_next: Option<DateTime<Utc>>| {
                let mut file = FileStatus {
                    name: file_name(path),
                    kind: kind_of(path),
                    size,
                    state: default,
                    uploaded_bytes: 0,
                    bytes_per_sec: None,
                    message: None,
                    next_attempt_at: None,
                };
                if let Some(message) = work.failed.get(path) {
                    file.state = FileState::Failed;
                    file.message = Some(message.clone());
                    return file;
                }
                if let Some(current) = work.current.as_ref().filter(|c| c.path == path) {
                    file.state = match (current.phase, pause) {
                        (Phase::Hashing, _) => FileState::Hashing,
                        (Phase::Uploading, PauseLevel::None) => FileState::Uploading,
                        (Phase::Uploading, _) => FileState::Paused,
                    };
                    if let Some(progress) = &current.progress {
                        file.uploaded_bytes = progress.done().min(size);
                        let secs = current.started.elapsed().as_secs_f64();
                        if secs >= 1.0 {
                            file.bytes_per_sec = Some((progress.sent() as f64 / secs) as u64);
                        }
                    }
                    return file;
                }
                let retry = work.retry.get(path);
                let next = row_next.or(retry.and_then(|r| r.next_attempt_at));
                if let Some(next) = next.filter(|t| *t > now) {
                    file.state = FileState::Retrying;
                    file.next_attempt_at = Some(next);
                    file.message = retry.map(|r| r.message.clone());
                }
                file
            };

        let sessions = sessions
            .into_iter()
            .map(|s| {
                let mut files: Vec<FileStatus> = uploaded
                    .iter()
                    .filter(|(id, _, _)| *id == s.id)
                    .map(|(_, path, size)| FileStatus {
                        name: file_name(path),
                        kind: kind_of(path),
                        size: *size,
                        state: FileState::Uploaded,
                        uploaded_bytes: *size,
                        bytes_per_sec: None,
                        message: None,
                        next_attempt_at: None,
                    })
                    .collect();
                files.extend(inflight.iter().filter(|r| r.session_id == s.id).map(|r| {
                    describe(&r.local_path, r.size, FileState::Queued, r.next_attempt_at)
                }));
                if s.state != SessionState::Closed {
                    let listed: HashSet<PathBuf> = uploaded
                        .iter()
                        .map(|(_, path, _)| path.clone())
                        .chain(inflight.iter().map(|r| r.local_path.clone()))
                        .collect();
                    let fresh = work.files.get(&s.id).into_iter().flatten();
                    for c in fresh.filter(|c| !listed.contains(&c.path)) {
                        let waiting = s.state != SessionState::Exited
                            || (now.timestamp_millis() - c.mtime_ms) < q * 1000;
                        let default = if waiting {
                            FileState::Waiting
                        } else {
                            FileState::Queued
                        };
                        files.push(describe(&c.path, c.size, default, None));
                    }
                }
                SessionStatus {
                    id: s.id,
                    playtest: s.playtest,
                    state: s.state,
                    launched_at: s.launched_at,
                    exited_at: s.exited_at,
                    files,
                }
            })
            .collect();

        CaptureStatus {
            sessions,
            pause: pause.into(),
            blocked_reason: (pause == PauseLevel::Blocked)
                .then(|| work.blocked_reason.clone())
                .flatten(),
            persist_failed: self.store.persist_failed(),
        }
    }
}

fn active_config(
    app_config: &AppConfigRef,
    dynamic_config: &DynamicConfigRef,
) -> Option<ClientCaptureUploadConfig> {
    if app_config.read().serverless {
        return None;
    }
    let cfg = dynamic_config.read().client_capture_upload.clone()?;
    usable_watch_entries(&cfg).then_some(cfg)
}

/// In-window files not yet uploaded, counting a file whose cached hash is already uploaded as done.
fn unuploaded_files(
    session: &CaptureSession,
    ledger: &CaptureLedger,
    watches: &[ResolvedWatch],
    quiescence_seconds: u64,
    now: DateTime<Utc>,
    hash_cache: &HashMap<PathBuf, (u64, i64, String)>,
) -> Vec<Candidate> {
    pending_candidates(session, ledger, watches, quiescence_seconds, now)
        .into_iter()
        .filter(|c| {
            !hash_cache.get(&c.path).is_some_and(|(size, mtime, sha)| {
                *size == c.size && *mtime == c.mtime_ms && ledger.uploaded.contains_key(sha)
            })
        })
        .collect()
}

fn kind_of(path: &Path) -> FileKind {
    let trace = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("utrace"));
    if trace {
        FileKind::Trace
    } else {
        FileKind::Log
    }
}

pub fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

async fn run_watcher(
    shared: Arc<Shared>,
    watcher: GameProcessWatcher<DynScanner, DynClock>,
    mut restored: HashSet<String>,
) {
    let mut last_poll: Option<Instant> = None;
    let mut last_maintenance = Instant::now();
    let mut seen: Option<(bool, Option<ClientCaptureUploadConfig>)> = None;
    loop {
        log_config_change(&shared, &mut seen);
        let cfg = shared.active_config();
        if last_poll.is_none_or(|t| t.elapsed() >= POLL) {
            if let Some(cfg) = &cfg {
                shared.poll(&watcher, cfg, &mut restored).await;
            }
            last_poll = Some(Instant::now());
        }
        if let Some(cfg) = &cfg {
            shared.pacer.set_cap(cfg.max_upload_mbps);
        }
        shared.update_pause(cfg.as_ref()).await;
        shared.store.flush_if_due(shared.now());
        if last_maintenance.elapsed() >= MAINTENANCE_INTERVAL {
            if let Some(cfg) = &cfg {
                shared.maintain(cfg).await;
            }
            last_maintenance = Instant::now();
        }
        shared.emit();

        tokio::select! {
            biased;
            _ = shared.root.cancelled() => return,
            _ = shared.wake_watcher.notified() => last_poll = None,
            _ = tokio::time::sleep(PAUSE_TICK) => {}
        }
    }
}

fn log_config_change(
    shared: &Shared,
    seen: &mut Option<(bool, Option<ClientCaptureUploadConfig>)>,
) {
    let current = (
        shared.app_config.read().serverless,
        shared.dynamic_config.read().client_capture_upload.clone(),
    );
    if seen.as_ref() == Some(&current) {
        return;
    }
    match &current {
        (true, _) => warn!("Capture upload is disabled in serverless mode"),
        (false, Some(cfg)) if !usable_watch_entries(cfg) => {
            warn!("clientCaptureUpload has no usable watch entries; capture upload is disabled")
        }
        _ => {}
    }
    *seen = Some(current);
}

struct Inner {
    app_config: AppConfigRef,
    dynamic_config: DynamicConfigRef,
    data_dir: PathBuf,
    backend: Arc<dyn CaptureBackend>,
    scanner: Arc<dyn ProcessScanner>,
    clock: Arc<dyn Clock>,
    running: Mutex<Option<Arc<Shared>>>,
}

#[derive(Clone)]
pub struct CaptureService {
    inner: Arc<Inner>,
}

impl CaptureService {
    pub fn new(
        app_config: AppConfigRef,
        dynamic_config: DynamicConfigRef,
        data_dir: PathBuf,
        aws_client: Arc<TokioRwLock<Option<AWSClient>>>,
    ) -> Self {
        Self::with_parts(
            app_config,
            dynamic_config,
            data_dir,
            Arc::new(AwsBackend { client: aws_client }),
            Arc::new(SysinfoScanner),
            Arc::new(SystemClock),
        )
    }

    pub fn with_parts(
        app_config: AppConfigRef,
        dynamic_config: DynamicConfigRef,
        data_dir: PathBuf,
        backend: Arc<dyn CaptureBackend>,
        scanner: Arc<dyn ProcessScanner>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                app_config,
                dynamic_config,
                data_dir,
                backend,
                scanner,
                clock,
                running: Mutex::new(None),
            }),
        }
    }

    fn running(&self) -> Option<Arc<Shared>> {
        self.inner.running.lock().clone()
    }

    /// Returns the session id, or `None` when inert; never an error.
    pub fn register_session(&self, req: RegisterSession) -> Option<String> {
        let inner = &self.inner;
        if inner.app_config.read().serverless {
            debug!("Capture session not registered: serverless mode");
            return None;
        }
        let Some(cfg) = inner.dynamic_config.read().client_capture_upload.clone() else {
            debug!("Capture session not registered: no clientCaptureUpload config");
            return None;
        };
        if !usable_watch_entries(&cfg) {
            debug!("Capture session not registered: no usable watch entries");
            return None;
        }
        if !inner.backend.has_client() {
            debug!("Capture session not registered: no AWS client");
            return None;
        }
        let Some(shared) = self.running() else {
            debug!("Capture session not registered: service not started");
            return None;
        };

        let id = format!(
            "{}-{:06x}",
            req.launched_at.timestamp_millis(),
            rand::random::<u32>() & 0xFF_FFFF
        );
        let session = CaptureSession {
            id: id.clone(),
            install_dir: req.install_dir,
            sha: req.sha,
            user: req.user,
            playtest: req.playtest,
            launched_at: req.launched_at,
            state: SessionState::Launched,
            exited_at: None,
            closed_at: None,
            close_reason: None,
            empty_polls: 0,
            first_empty_at: None,
        };
        shared.store.add_session(session, shared.now());
        shared.game_running.store(true, Ordering::Relaxed);
        if cfg.pause_while_game_running {
            shared.pause_tx.send_if_modified(|level| {
                let changed = *level != PauseLevel::Hard;
                *level = PauseLevel::Hard;
                changed
            });
        }
        shared.wake_watcher.notify_one();
        shared.emit();
        info!("Registered capture session {id}");
        Some(id)
    }

    /// `None` when inert or nothing is pending. Reads the watch directories; call it off the async runtime.
    pub fn pending_all(&self) -> Option<PendingSummary> {
        let shared = self.running()?;
        let cfg = shared.active_config()?;
        let now = shared.now();
        let ledger = shared.store.snapshot();
        let (hash_cache, uploading) = {
            let work = shared.work.lock();
            (work.hash_cache.clone(), work.current.is_some())
        };

        let mut summary = PendingSummary {
            session_ids: Vec::new(),
            playtests: Vec::new(),
            files: 0,
            bytes: 0,
            game_running: shared.game_running.load(Ordering::Relaxed),
            uploading,
        };
        for session in ledger
            .sessions
            .iter()
            .filter(|s| s.state != SessionState::Closed)
        {
            let watches = shared.watches_for(&session.install_dir, &cfg);
            let files = unuploaded_files(
                session,
                &ledger,
                &watches,
                cfg.quiescence_seconds,
                now,
                &hash_cache,
            );
            let rows: Vec<&InflightUpload> = ledger
                .inflight
                .iter()
                .filter(|r| r.session_id == session.id)
                .collect();
            let live = matches!(
                session.state,
                SessionState::Launched | SessionState::Running
            );
            if !live && files.is_empty() && rows.is_empty() {
                continue;
            }
            summary.session_ids.push(session.id.clone());
            if !summary.playtests.contains(&session.playtest) {
                summary.playtests.push(session.playtest.clone());
            }
            summary.files += (files.len() + rows.len()) as u32;
            summary.bytes += files.iter().map(|c| c.size).sum::<u64>();
            summary.bytes += rows.iter().map(|r| r.size).sum::<u64>();
        }
        (!summary.session_ids.is_empty()).then_some(summary)
    }

    /// Closes one open session as `Cancelled` and aborts its in-flight upload. Files stay on disk.
    pub fn cancel_session(&self, session_id: &str) -> bool {
        let Some(shared) = self.running() else {
            return false;
        };
        shared.close_sessions(|s| s.id == session_id, CloseReason::Cancelled, false) > 0
    }

    /// Closes every open session as `Wiped`, aborts in-flight MPUs and drops every in-flight row.
    pub fn cancel_all(&self) -> usize {
        let Some(shared) = self.running() else {
            return 0;
        };
        shared.close_sessions(|_| true, CloseReason::Wiped, true)
    }

    pub fn status(&self) -> CaptureStatus {
        self.running()
            .map(|shared| shared.build_status())
            .unwrap_or_default()
    }

    /// Loads the ledger, then (in the background) prunes, reconciles in-flight rows and spawns the
    /// watcher and uploader tasks. Must be called from inside a tokio runtime.
    pub fn start(
        &self,
        pause: Arc<AtomicBool>,
        notify: STDSender<CaptureNotification>,
        events: STDSender<CaptureStatus>,
    ) {
        let mut running = self.inner.running.lock();
        if running.is_some() {
            warn!("CaptureService::start called twice; ignoring");
            return;
        }
        let Ok(handle) = Handle::try_current() else {
            warn!("CaptureService::start called outside a tokio runtime; capture upload disabled");
            return;
        };
        let inner = &self.inner;
        let now = inner.clock.now();
        let store = CaptureStore::load(inner.data_dir.join(LEDGER_FILE), now);
        let restored: HashSet<String> = store.with(|l| {
            l.sessions
                .iter()
                .filter(|s| s.state != SessionState::Closed)
                .map(|s| s.id.clone())
                .collect()
        });
        let shared = Arc::new(Shared {
            app_config: inner.app_config.clone(),
            dynamic_config: inner.dynamic_config.clone(),
            data_dir: inner.data_dir.clone(),
            backend: inner.backend.clone(),
            clock: inner.clock.clone(),
            store,
            root: CancellationToken::new(),
            pause_tx: watch::Sender::new(PauseLevel::Blocked),
            pause_flag: pause,
            game_running: AtomicBool::new(false),
            wake_watcher: Notify::new(),
            wake_uploader: Notify::new(),
            pacer: Arc::new(Pacer::new(None)),
            work: Mutex::new(WorkState::default()),
            notify_tx: notify,
            events_tx: events,
            last_status: Mutex::new(None),
            handle: handle.clone(),
            tasks: Mutex::new(Vec::new()),
        });
        *running = Some(shared.clone());
        drop(running);

        let watcher = GameProcessWatcher::new(
            DynScanner(inner.scanner.clone()),
            DynClock(inner.clock.clone()),
        );
        let init = handle.spawn(async move {
            shared.initialize().await;
            let watcher_task = tokio::spawn(run_watcher(shared.clone(), watcher, restored));
            let uploader_task = tokio::spawn(CaptureUploader::new(shared.clone()).run());
            shared.tasks.lock().extend([watcher_task, uploader_task]);
        });
        if let Some(shared) = self.running() {
            shared.tasks.lock().push(init);
        }
    }

    /// Stops both tasks and writes the ledger. An upload in progress is dropped, not aborted, so it resumes.
    pub fn shutdown(&self) {
        let Some(shared) = self.running() else {
            return;
        };
        shared.root.cancel();
        shared.store.flush(shared.now());
    }

    #[cfg(test)]
    async fn join_tasks(&self) {
        let Some(shared) = self.running() else {
            return;
        };
        loop {
            let tasks: Vec<JoinHandle<()>> = std::mem::take(&mut *shared.tasks.lock());
            if tasks.is_empty() {
                return;
            }
            for task in tasks {
                let _ = task.await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::{channel, Receiver};

    use bytes::Bytes;
    use chrono::TimeZone;
    use parking_lot::RwLock;
    use tempfile::TempDir;

    use super::*;
    use crate::capture::multipart::{Call, FakePartStore};
    use crate::capture::types::{PauseLevelStatus, UploadedEntry};
    use crate::types::config::{AppConfig, DynamicConfig};

    const BUCKET: &str = "bucket";
    const WAIT_LIMIT: u32 = 900;

    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 8, 12, 0, 0).unwrap()
    }

    fn secs(n: i64) -> TimeDelta {
        TimeDelta::seconds(n)
    }

    struct TokioClock {
        base: DateTime<Utc>,
        start: Instant,
    }

    impl Clock for TokioClock {
        fn now(&self) -> DateTime<Utc> {
            self.base + TimeDelta::from_std(self.start.elapsed()).unwrap()
        }
    }

    #[derive(Default)]
    struct FakeScanner {
        running: Mutex<HashSet<PathBuf>>,
    }

    impl FakeScanner {
        fn set(&self, root: &Path, running: bool) {
            let mut set = self.running.lock();
            if running {
                set.insert(root.to_path_buf());
            } else {
                set.remove(root);
            }
        }
    }

    impl ProcessScanner for FakeScanner {
        fn running_from(&self, root: &Path) -> bool {
            self.running.lock().contains(root)
        }
    }

    struct FakeBackend {
        store: Arc<FakePartStore>,
        client: AtomicBool,
        signed_in: AtomicBool,
    }

    #[async_trait]
    impl CaptureBackend for FakeBackend {
        fn has_client(&self) -> bool {
            self.client.load(Ordering::Relaxed)
        }

        async fn auth_state(&self) -> AuthState {
            if !self.has_client() {
                AuthState::NoClient
            } else if !self.signed_in.load(Ordering::Relaxed) {
                AuthState::SignedOut
            } else {
                AuthState::Ready { expires_at: None }
            }
        }

        async fn artifact_bucket(&self) -> Option<String> {
            self.has_client().then(|| BUCKET.to_owned())
        }

        async fn part_store(&self) -> Option<Arc<dyn PartStore>> {
            let store: Arc<dyn PartStore> = self.store.clone();
            self.has_client().then_some(store)
        }
    }

    fn entry(dir: &str, pattern: &str, prefix: &str) -> CaptureWatchEntry {
        CaptureWatchEntry {
            dir: dir.to_owned(),
            patterns: vec![pattern.to_owned()],
            key_prefix: prefix.to_owned(),
        }
    }

    fn watch_cfg() -> ClientCaptureUploadConfig {
        ClientCaptureUploadConfig {
            watch: vec![
                entry("Game/Saved/Profiling", "Trace_*.utrace", "p/utrace"),
                entry("Game/Saved/Logs", "*.log", "p/logs"),
            ],
            quiescence_seconds: 5,
            part_size_mib: 8,
            ..Default::default()
        }
    }

    fn content(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i % 251) as u8).collect()
    }

    fn mtime_of(path: &Path) -> i64 {
        crate::capture::ledger::mtime_ms(&std::fs::metadata(path).unwrap()).unwrap()
    }

    struct Fixture {
        _tmp: TempDir,
        data_dir: PathBuf,
        install: PathBuf,
        app_config: AppConfigRef,
        dynamic_config: DynamicConfigRef,
        backend: Arc<FakeBackend>,
        scanner: Arc<FakeScanner>,
        clock: Arc<TokioClock>,
        service: CaptureService,
        notify_tx: STDSender<CaptureNotification>,
        notes: Receiver<CaptureNotification>,
        events_tx: STDSender<CaptureStatus>,
        events: Receiver<CaptureStatus>,
    }

    impl Fixture {
        fn new(cfg: Option<ClientCaptureUploadConfig>) -> Self {
            Self::at(cfg, t0())
        }

        fn at(cfg: Option<ClientCaptureUploadConfig>, base: DateTime<Utc>) -> Self {
            let tmp = TempDir::new().unwrap();
            let data_dir = tmp.path().join("data");
            let install = data_dir.join("project").join("client-win64");
            std::fs::create_dir_all(&install).unwrap();
            let app_config: AppConfigRef = Arc::new(RwLock::new(AppConfig::new("capture-test")));
            let dynamic_config: DynamicConfigRef = Arc::new(RwLock::new(DynamicConfig {
                client_capture_upload: cfg,
                ..Default::default()
            }));
            let backend = Arc::new(FakeBackend {
                store: FakePartStore::new(),
                client: AtomicBool::new(true),
                signed_in: AtomicBool::new(true),
            });
            let scanner = Arc::new(FakeScanner::default());
            let clock = Arc::new(TokioClock {
                base,
                start: Instant::now(),
            });
            let service = CaptureService::with_parts(
                app_config.clone(),
                dynamic_config.clone(),
                data_dir.clone(),
                backend.clone(),
                scanner.clone(),
                clock.clone(),
            );
            let (notify_tx, notes) = channel();
            let (events_tx, events) = channel();
            Self {
                _tmp: tmp,
                data_dir,
                install,
                app_config,
                dynamic_config,
                backend,
                scanner,
                clock,
                service,
                notify_tx,
                notes,
                events_tx,
                events,
            }
        }

        fn start(&self) {
            self.service.start(
                Arc::new(AtomicBool::new(false)),
                self.notify_tx.clone(),
                self.events_tx.clone(),
            );
        }

        fn register(&self) -> Option<String> {
            self.service.register_session(RegisterSession {
                install_dir: self.install.clone(),
                sha: Some("0123456789abcdef".into()),
                user: "Jane Doe".into(),
                playtest: "pt-1".into(),
                launched_at: self.clock.now(),
            })
        }

        fn write_at(&self, path: &Path, data: &[u8], mtime: DateTime<Utc>) -> PathBuf {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, data).unwrap();
            let file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
            file.set_modified(mtime.into()).unwrap();
            path.to_path_buf()
        }

        fn write(&self, rel: &str, data: &[u8], mtime: DateTime<Utc>) -> PathBuf {
            self.write_at(&self.install.join(rel), data, mtime)
        }

        fn ledger(&self) -> CaptureLedger {
            self.service.running().unwrap().store.snapshot()
        }

        fn ledger_path(&self) -> PathBuf {
            self.data_dir.join(LEDGER_FILE)
        }

        fn session(&self, id: &str) -> CaptureSession {
            self.ledger()
                .sessions
                .into_iter()
                .find(|s| s.id == id)
                .unwrap()
        }

        async fn until(&self, what: &str, done: impl Fn(&Fixture) -> bool) {
            for _ in 0..WAIT_LIMIT {
                if done(self) {
                    return;
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            panic!("timed out waiting for {what}");
        }

        async fn stop(&self) {
            self.service.shutdown();
            tokio::time::timeout(Duration::from_secs(60), self.service.join_tasks())
                .await
                .expect("capture tasks stop on shutdown");
        }
    }

    #[tokio::test]
    async fn register_session_is_inert_without_prerequisites() {
        let empty_watch = ClientCaptureUploadConfig::default();
        let invalid_watch = ClientCaptureUploadConfig {
            watch: vec![
                entry("Game/Saved/Logs", "*.txt", "p"),
                entry("../Game/Saved/Logs", "*.log", "p"),
                entry("Game/Saved/Logs", "*.log", ""),
            ],
            ..Default::default()
        };
        let cases: Vec<(&str, Fixture, bool)> = vec![
            ("serverless", Fixture::new(Some(watch_cfg())), true),
            ("no config", Fixture::new(None), true),
            ("empty watch", Fixture::new(Some(empty_watch)), true),
            (
                "all entries invalid",
                Fixture::new(Some(invalid_watch)),
                true,
            ),
            ("no aws client", Fixture::new(Some(watch_cfg())), true),
            ("not started", Fixture::new(Some(watch_cfg())), false),
        ];
        for (name, f, start) in cases {
            match name {
                "serverless" => f.app_config.write().serverless = true,
                "no aws client" => f.backend.client.store(false, Ordering::Relaxed),
                _ => {}
            }
            if start {
                f.start();
            }
            assert_eq!(f.register(), None, "{name}");
            assert!(f.service.pending_all().is_none(), "{name}");
            assert!(!f.ledger_path().exists(), "{name} persisted a ledger");
            if start {
                assert!(f.ledger().sessions.is_empty(), "{name}");
            }
            f.service.shutdown();
        }
    }

    #[tokio::test(start_paused = true)]
    async fn session_uploads_its_window_and_closes_as_uploaded() {
        let f = Fixture::new(Some(watch_cfg()));
        f.start();
        let id = f.register().expect("registered");
        assert!(
            f.ledger_path().exists(),
            "registration persists immediately"
        );
        assert_eq!(f.service.status().sessions[0].state, SessionState::Launched);

        f.scanner.set(&f.install, true);
        f.until("running", |f| f.session(&id).state == SessionState::Running)
            .await;

        let log = f.write("Game/Saved/Logs/Game.log", b"log body", t0() + secs(3));
        let trace_data = content(4096);
        let trace = f.write(
            "Game/Saved/Profiling/Trace_1.utrace",
            &trace_data,
            t0() + secs(4),
        );
        let before = f.write("Game/Saved/Logs/Old.log", b"old", t0() - secs(3600));
        let after = f.write(
            "Game/Saved/Profiling/Trace_2.utrace",
            b"later",
            t0() + secs(3600),
        );

        f.scanner.set(&f.install, false);
        f.until("closed", |f| f.session(&id).state == SessionState::Closed)
            .await;

        let ledger = f.ledger();
        let session = f.session(&id);
        assert_eq!(session.close_reason, Some(CloseReason::Uploaded));
        assert!(session.exited_at.is_some());
        assert!(ledger.inflight.is_empty());
        assert_eq!(ledger.uploaded.len(), 2);

        let uploaded: HashMap<PathBuf, (&String, &UploadedEntry)> = ledger
            .uploaded
            .iter()
            .map(|(sha, e)| (e.local_path.clone(), (sha, e)))
            .collect();
        assert!(!uploaded.contains_key(&before));
        assert!(!uploaded.contains_key(&after));
        let expected = [
            (
                &log,
                b"log body".to_vec(),
                "p/logs/2026-10-08/Jane-Doe/0123456789ab_Jane-Doe_",
            ),
            (
                &trace,
                trace_data.clone(),
                "p/utrace/2026-10-08/Jane-Doe/0123456789ab_Jane-Doe_",
            ),
        ];
        for (path, data, prefix) in expected {
            let (sha, entry) = uploaded[path];
            assert!(entry.key.starts_with(prefix), "{}", entry.key);
            assert_eq!(entry.session_id, id);
            let (body, meta) = f.backend.store.object(BUCKET, &entry.key).unwrap();
            assert_eq!(body, Bytes::from(data));
            assert_eq!(meta.metadata["sha256"], *sha);
            assert_eq!(meta.metadata["playtest"], "pt-1");
            assert_eq!(meta.metadata["sha"], "0123456789abcdef");
        }

        let notes: Vec<CaptureNotification> = f.notes.try_iter().collect();
        let success = notes.iter().any(|n| {
            matches!(n, CaptureNotification::Success(text)
                if text.starts_with("Uploaded 2 capture files (") && text.ends_with(") for pt-1"))
        });
        assert!(success, "{notes:?}");
        assert!(f.events.try_iter().count() > 0);
        let on_disk: CaptureLedger =
            serde_json::from_slice(&std::fs::read(f.ledger_path()).unwrap()).unwrap();
        assert_eq!(on_disk.uploaded.len(), 2);
        f.stop().await;
    }

    #[tokio::test(start_paused = true)]
    async fn start_resumes_inflight_upload_and_drops_changed_row() {
        let f = Fixture::at(Some(watch_cfg()), t0() + secs(60));
        let data = content(3 * (1 << 20) + 100);
        let trace = f.write("Game/Saved/Profiling/Trace_9.utrace", &data, t0() + secs(5));
        let other = f.write_at(
            &f.data_dir.join("elsewhere").join("Trace_x.utrace"),
            b"stale",
            t0() + secs(5),
        );
        let key = "p/utrace/2026-10-07/Jane-Doe/0123456789ab_Jane-Doe_x_Trace_9.utrace";
        let stale_key = "p/utrace/2026-10-07/Jane-Doe/stale.utrace";
        let part = 1usize << 20;
        let upload_id = f
            .backend
            .store
            .seed_upload(key, vec![(1, Bytes::copy_from_slice(&data[..part]))]);
        let stale_id = f.backend.store.seed_upload(stale_key, vec![]);

        let session = CaptureSession {
            id: "s1".into(),
            install_dir: f.install.clone(),
            sha: Some("0123456789abcdef".into()),
            user: "Jane Doe".into(),
            playtest: "pt-1".into(),
            launched_at: t0(),
            state: SessionState::Exited,
            exited_at: Some(t0() + secs(10)),
            closed_at: None,
            close_reason: None,
            empty_polls: 0,
            first_empty_at: None,
        };
        let row = |path: &Path, size: u64, key: &str, upload_id: &str| InflightUpload {
            session_id: "s1".into(),
            local_path: path.to_path_buf(),
            size,
            mtime_ms: mtime_of(path),
            sha256: format!("sha-{key}"),
            bucket: BUCKET.into(),
            key: key.into(),
            upload_id: Some(upload_id.into()),
            part_size: part as u64,
            started_at: t0() + secs(20),
            attempts: 0,
            next_attempt_at: None,
        };
        let ledger = CaptureLedger {
            version: 1,
            sessions: vec![session],
            uploaded: Default::default(),
            inflight: vec![
                row(&trace, data.len() as u64, key, &upload_id),
                row(&other, 999, stale_key, &stale_id),
            ],
        };
        std::fs::write(f.ledger_path(), serde_json::to_vec_pretty(&ledger).unwrap()).unwrap();

        f.start();
        f.until("closed", |f| f.session("s1").state == SessionState::Closed)
            .await;
        let stale_abort = Call::Abort {
            key: stale_key.into(),
            upload_id: stale_id.clone(),
        };
        f.until("stale abort", |f| {
            f.backend.store.calls().contains(&stale_abort)
        })
        .await;

        let calls = f.backend.store.calls();
        assert!(!calls.iter().any(|c| matches!(c, Call::Create { .. })));
        let mut parts: Vec<i32> = calls
            .iter()
            .filter_map(|c| match c {
                Call::Part {
                    number,
                    upload_id: id,
                    ..
                } if *id == upload_id => Some(*number),
                _ => None,
            })
            .collect();
        parts.sort_unstable();
        assert_eq!(parts, vec![2, 3, 4]);
        assert!(calls.contains(&Call::List {
            key: key.into(),
            upload_id: upload_id.clone(),
        }));
        assert_eq!(
            f.backend.store.object(BUCKET, key).unwrap().0,
            Bytes::from(data)
        );

        let ledger = f.ledger();
        assert_eq!(f.session("s1").close_reason, Some(CloseReason::Uploaded));
        assert!(ledger.inflight.is_empty());
        assert_eq!(ledger.uploaded.len(), 1);
        assert_eq!(ledger.uploaded[&format!("sha-{key}")].key, key);
        assert!(other.exists());
        f.stop().await;
    }

    #[tokio::test(start_paused = true)]
    async fn pending_all_counts_the_live_trace_of_a_running_session() {
        let f = Fixture::new(Some(watch_cfg()));
        f.start();
        let id = f.register().unwrap();
        f.scanner.set(&f.install, true);
        f.until("running", |f| f.session(&id).state == SessionState::Running)
            .await;
        f.write(
            "Game/Saved/Profiling/Trace_live.utrace",
            &content(1000),
            f.clock.now(),
        );

        let pending = f.service.pending_all().expect("pending");
        assert_eq!(pending.session_ids, vec![id.clone()]);
        assert_eq!(pending.playtests, vec!["pt-1".to_string()]);
        assert_eq!(pending.files, 1);
        assert_eq!(pending.bytes, 1000);
        assert!(pending.game_running);
        assert!(!pending.uploading);
        f.stop().await;
    }

    #[tokio::test(start_paused = true)]
    async fn pending_all_is_none_when_inert_and_some_when_signed_out() {
        let inert = Fixture::new(None);
        assert!(inert.service.pending_all().is_none(), "not started");
        inert.start();
        assert!(inert.service.pending_all().is_none(), "no config");
        inert.stop().await;

        let f = Fixture::new(Some(watch_cfg()));
        f.backend.signed_in.store(false, Ordering::Relaxed);
        f.start();
        assert!(f.service.pending_all().is_none(), "nothing registered");
        let id = f.register().expect("signed out is not inert");
        let pending = f.service.pending_all().expect("pending while signed out");
        assert_eq!(pending.session_ids, vec![id]);
        f.until("launch grace over and blocked", |f| {
            f.service.status().blocked_reason.as_deref() == Some(SIGN_IN)
        })
        .await;
        assert_eq!(f.service.status().pause, PauseLevelStatus::Blocked);
        f.stop().await;
    }

    #[tokio::test(start_paused = true)]
    async fn status_is_hidden_while_inert() {
        let f = Fixture::new(Some(watch_cfg()));
        f.start();
        f.register().expect("registered");
        assert_eq!(f.service.status().sessions.len(), 1);

        f.dynamic_config.write().client_capture_upload = Some(ClientCaptureUploadConfig::default());
        f.until("hidden with no usable watch entry", |f| {
            f.service.status() == CaptureStatus::default()
        })
        .await;

        f.dynamic_config.write().client_capture_upload = Some(watch_cfg());
        f.until("shown again", |f| f.service.status().sessions.len() == 1)
            .await;

        f.app_config.write().serverless = true;
        f.until("hidden in serverless", |f| {
            f.service.status() == CaptureStatus::default()
        })
        .await;
        f.stop().await;
    }

    #[tokio::test(start_paused = true)]
    async fn missing_config_while_signed_out_asks_for_sign_in() {
        let f = Fixture::new(Some(watch_cfg()));
        f.start();
        f.register().expect("registered");

        // Dynamic config is only loaded with credentials, so a signed-out start has no block.
        f.backend.signed_in.store(false, Ordering::Relaxed);
        f.dynamic_config.write().client_capture_upload = None;
        f.until("blocked on sign-in", |f| {
            f.service.status().blocked_reason.as_deref() == Some(SIGN_IN)
        })
        .await;
        let status = f.service.status();
        assert_eq!(status.pause, PauseLevelStatus::Blocked);
        assert_eq!(status.sessions.len(), 1);

        f.backend.signed_in.store(true, Ordering::Relaxed);
        f.until("hidden once signed in without the block", |f| {
            f.service.status() == CaptureStatus::default()
        })
        .await;
        f.stop().await;
    }

    #[tokio::test(start_paused = true)]
    async fn cancel_session_aborts_inflight_mpu_and_keeps_files() {
        let f = Fixture::new(Some(watch_cfg()));
        f.start();
        let id = f.register().unwrap();
        f.scanner.set(&f.install, true);
        f.until("running", |f| f.session(&id).state == SessionState::Running)
            .await;
        let trace = f.write(
            "Game/Saved/Profiling/Trace_big.utrace",
            &content(8 * (1 << 20) + 1024),
            t0() + secs(2),
        );
        f.backend.store.hold_parts(&[1, 2]);
        f.scanner.set(&f.install, false);
        tokio::time::timeout(Duration::from_secs(600), f.backend.store.wait_started(1))
            .await
            .expect("a part started");
        assert_eq!(
            f.service.status().sessions[0].files[0].state,
            FileState::Uploading
        );

        assert!(f.service.cancel_session(&id));
        assert!(!f.service.cancel_session(&id), "already closed");
        f.until("aborted", |f| {
            f.backend
                .store
                .calls()
                .iter()
                .any(|c| matches!(c, Call::Abort { .. }))
        })
        .await;
        f.until("row dropped", |f| f.ledger().inflight.is_empty())
            .await;

        let session = f.session(&id);
        assert_eq!(session.state, SessionState::Closed);
        assert_eq!(session.close_reason, Some(CloseReason::Cancelled));
        assert!(trace.exists());
        assert!(f.ledger().uploaded.is_empty());
        let completed = f
            .backend
            .store
            .calls()
            .iter()
            .any(|c| matches!(c, Call::Complete { .. }));
        assert!(!completed);
        assert!(f
            .notes
            .try_iter()
            .all(|n| !matches!(n, CaptureNotification::Success(_))));
        f.stop().await;
    }

    #[tokio::test(start_paused = true)]
    async fn cancel_all_closes_every_open_session_as_wiped() {
        let f = Fixture::new(Some(watch_cfg()));
        f.start();
        let a = f.register().unwrap();
        let b = f.register().unwrap();
        assert_ne!(a, b);
        assert_eq!(f.service.cancel_all(), 2);
        assert_eq!(f.service.cancel_all(), 0);
        for id in [&a, &b] {
            let s = f.session(id);
            assert_eq!(s.state, SessionState::Closed);
            assert_eq!(s.close_reason, Some(CloseReason::Wiped));
        }
        let on_disk: CaptureLedger =
            serde_json::from_slice(&std::fs::read(f.ledger_path()).unwrap()).unwrap();
        assert!(on_disk
            .sessions
            .iter()
            .all(|s| s.close_reason == Some(CloseReason::Wiped)));
        assert!(f.service.pending_all().is_none());
        f.stop().await;
    }

    #[test]
    fn sizes_format_with_binary_units() {
        assert_eq!(format_size(512), "512 B");
        assert_eq!(format_size(1536), "1.5 KB");
        assert_eq!(format_size(5 << 30), "5.0 GB");
    }
}
