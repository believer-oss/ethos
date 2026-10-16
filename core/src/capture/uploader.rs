//! Capture uploader: hashing, scheduling, retry and pause handling.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use chrono::{DateTime, TimeDelta, Utc};
use rand::Rng;
use ring::digest::{Context, SHA256};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use super::config::ClientCaptureUploadConfig;
use super::key::{metadata_encode, metadata_sha, metadata_user, object_key};
use super::ledger::{mtime_ms, stat_matches};
use super::multipart::{
    part_size, upload_file, Meta, PartInfo, PartStore, PauseLevel, StoreError, UploadOutcome,
    UploadSpec,
};
use super::select::{candidates, Candidate, ResolvedWatch};
use super::service::{Phase, Shared};
use super::types::{CaptureLedger, CaptureSession, InflightUpload, SessionState, UploadedEntry};
use crate::utils::process::is_held_by_another_program;

pub const UPLOADER_TICK: Duration = Duration::from_secs(5);
const HASH_CHUNK: usize = 4 << 20;
const HASH_PARK: Duration = Duration::from_millis(500);
const BACKOFF_BASE_SECS: i64 = 30;
const BACKOFF_CAP_SECS: i64 = 15 * 60;
pub const JITTER: f64 = 0.2;
pub const DENIED_DELAY: TimeDelta = TimeDelta::minutes(15);
pub const LOCKED_DELAY: TimeDelta = TimeDelta::seconds(30);
const TOAST_INTERVAL: TimeDelta = TimeDelta::hours(1);

#[derive(Debug, PartialEq, Eq)]
pub enum HashOutcome {
    Done(String),
    /// `(size, mtime_ms)` moved while hashing; the file is still being written.
    Changed,
    Stopped,
}

/// SHA-256 of `path` in 4 MiB reads. While `paused` holds it calls `park` between chunks, keeping progress.
pub fn hash_file_blocking(
    path: &Path,
    size: u64,
    mtime: i64,
    stop: &dyn Fn() -> bool,
    paused: &dyn Fn() -> bool,
    park: &dyn Fn(),
) -> std::io::Result<HashOutcome> {
    let meta = std::fs::symlink_metadata(path)?;
    if meta.len() != size || mtime_ms(&meta) != Some(mtime) {
        return Ok(HashOutcome::Changed);
    }
    let mut file = std::fs::File::open(path)?;
    let mut context = Context::new(&SHA256);
    let mut buf = vec![0u8; HASH_CHUNK];
    loop {
        while paused() {
            if stop() {
                return Ok(HashOutcome::Stopped);
            }
            park();
        }
        if stop() {
            return Ok(HashOutcome::Stopped);
        }
        let n = match file.read(&mut buf) {
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if n == 0 {
            break;
        }
        context.update(&buf[..n]);
    }
    if !stat_matches(path, size, mtime) {
        return Ok(HashOutcome::Changed);
    }
    Ok(HashOutcome::Done(hex(context.finish().as_ref())))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub async fn hash_file(
    path: PathBuf,
    size: u64,
    mtime: i64,
    stop: Vec<CancellationToken>,
    pause_rx: watch::Receiver<PauseLevel>,
) -> std::io::Result<HashOutcome> {
    tokio::task::spawn_blocking(move || {
        hash_file_blocking(
            &path,
            size,
            mtime,
            &|| stop.iter().any(CancellationToken::is_cancelled),
            &|| *pause_rx.borrow() == PauseLevel::Hard,
            &|| std::thread::sleep(HASH_PARK),
        )
    })
    .await
    .unwrap_or_else(|e| Err(std::io::Error::other(e)))
}

/// `min(30 s x 2^(attempts-1), 15 min)`, scaled by `1 + jitter` (jitter clamped to +-20%).
pub fn transient_delay(attempts: u32, jitter: f64) -> TimeDelta {
    let doublings = attempts.saturating_sub(1).min(10);
    let base = (BACKOFF_BASE_SECS << doublings).min(BACKOFF_CAP_SECS);
    let factor = 1.0 + jitter.clamp(-JITTER, JITTER);
    TimeDelta::milliseconds((base as f64 * 1000.0 * factor).round() as i64)
}

pub fn random_jitter() -> f64 {
    rand::thread_rng().gen_range(-JITTER..=JITTER)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Failure {
    Transient(String),
    Denied(String),
    Auth(String),
    Fatal(String),
    Missing,
    Locked(String),
}

impl Failure {
    pub fn from_store(e: StoreError) -> Self {
        match e {
            StoreError::Auth(m) => Failure::Auth(m),
            StoreError::Denied(m) => Failure::Denied(m),
            StoreError::NoSuchUpload => Failure::Transient("multipart upload disappeared".into()),
            StoreError::Transient(m) => Failure::Transient(m),
            StoreError::Fatal(m) => Failure::Fatal(m),
            StoreError::Io(e) => Failure::from_io(&e),
        }
    }

    pub fn from_io(e: &std::io::Error) -> Self {
        if e.kind() == std::io::ErrorKind::NotFound {
            Failure::Missing
        } else if is_held_by_another_program(e) {
            Failure::Locked(e.to_string())
        } else {
            Failure::Transient(e.to_string())
        }
    }

    fn class(&self) -> &'static str {
        match self {
            Failure::Transient(_) => "transient",
            Failure::Denied(_) => "denied",
            Failure::Auth(_) => "auth",
            Failure::Fatal(_) => "fatal",
            Failure::Missing => "missing",
            Failure::Locked(_) => "locked",
        }
    }

    fn message(&self) -> String {
        match self {
            Failure::Transient(m) | Failure::Fatal(m) => m.clone(),
            Failure::Denied(_) => "Upload not permitted yet (access denied)".into(),
            Failure::Auth(_) => "Waiting for sign-in".into(),
            Failure::Missing => "File no longer exists".into(),
            Failure::Locked(_) => "In use by another program".into(),
        }
    }

    fn toast(&self, name: &str) -> Option<String> {
        match self {
            Failure::Transient(m) => Some(format!("Upload of {name} failed, will retry: {m}")),
            Failure::Denied(_) => Some(format!(
                "Upload of {name} not permitted yet (access denied)"
            )),
            Failure::Fatal(m) => Some(format!("Could not upload {name}: {m}")),
            Failure::Missing => Some(format!("File no longer exists: {name}")),
            Failure::Auth(_) | Failure::Locked(_) => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetryPlan {
    pub attempts: u32,
    pub next_attempt_at: Option<DateTime<Utc>>,
    pub failed: bool,
}

pub fn plan_retry(failure: &Failure, attempts: u32, now: DateTime<Utc>, jitter: f64) -> RetryPlan {
    let plan = |attempts, next_attempt_at, failed| RetryPlan {
        attempts,
        next_attempt_at,
        failed,
    };
    match failure {
        Failure::Transient(_) => {
            let attempts = attempts.saturating_add(1);
            plan(
                attempts,
                Some(now + transient_delay(attempts, jitter)),
                false,
            )
        }
        Failure::Denied(_) => plan(attempts.saturating_add(1), Some(now + DENIED_DELAY), false),
        Failure::Locked(_) => plan(attempts, Some(now + LOCKED_DELAY), false),
        Failure::Fatal(_) => plan(attempts, None, true),
        Failure::Auth(_) | Failure::Missing => plan(attempts, None, false),
    }
}

/// At most one toast per `(session, file, class)` per hour.
#[derive(Debug, Default)]
pub struct ToastLimiter {
    last: HashMap<(String, PathBuf, &'static str), DateTime<Utc>>,
}

impl ToastLimiter {
    pub fn allow(
        &mut self,
        session_id: &str,
        path: &Path,
        class: &'static str,
        now: DateTime<Utc>,
    ) -> bool {
        let key = (session_id.to_owned(), path.to_path_buf(), class);
        match self.last.get(&key) {
            Some(last) if now - *last < TOAST_INTERVAL => false,
            _ => {
                self.last.insert(key, now);
                true
            }
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct RetryState {
    pub attempts: u32,
    pub next_attempt_at: Option<DateTime<Utc>>,
    pub message: String,
}

#[derive(Debug, Default)]
pub(crate) struct Progress {
    done: AtomicU64,
    sent: AtomicU64,
}

impl Progress {
    pub(crate) fn done(&self) -> u64 {
        self.done.load(Ordering::Relaxed)
    }

    pub(crate) fn sent(&self) -> u64 {
        self.sent.load(Ordering::Relaxed)
    }

    fn add(&self, bytes: u64) {
        self.done.fetch_add(bytes, Ordering::Relaxed);
        self.sent.fetch_add(bytes, Ordering::Relaxed);
    }
}

/// Counts bytes for the status panel; the driver itself has no progress hook.
struct CountingStore {
    inner: Arc<dyn PartStore>,
    progress: Arc<Progress>,
}

#[async_trait]
impl PartStore for CountingStore {
    async fn put(
        &self,
        bucket: &str,
        key: &str,
        body: Bytes,
        meta: &Meta,
    ) -> Result<(), StoreError> {
        let len = body.len() as u64;
        self.inner.put(bucket, key, body, meta).await?;
        self.progress.add(len);
        Ok(())
    }

    async fn create_mpu(&self, bucket: &str, key: &str, meta: &Meta) -> Result<String, StoreError> {
        self.inner.create_mpu(bucket, key, meta).await
    }

    async fn list_parts(
        &self,
        bucket: &str,
        key: &str,
        upload_id: &str,
    ) -> Result<Vec<PartInfo>, StoreError> {
        let parts = self.inner.list_parts(bucket, key, upload_id).await?;
        let listed: u64 = parts.iter().map(|p| p.size).sum();
        self.progress.done.store(listed, Ordering::Relaxed);
        Ok(parts)
    }

    async fn upload_part(
        &self,
        bucket: &str,
        key: &str,
        upload_id: &str,
        number: i32,
        body: Bytes,
    ) -> Result<(), StoreError> {
        let len = body.len() as u64;
        self.inner
            .upload_part(bucket, key, upload_id, number, body)
            .await?;
        self.progress.add(len);
        Ok(())
    }

    async fn complete(
        &self,
        bucket: &str,
        key: &str,
        upload_id: &str,
        parts: Vec<PartInfo>,
    ) -> Result<(), StoreError> {
        self.inner.complete(bucket, key, upload_id, parts).await
    }

    async fn abort(&self, bucket: &str, key: &str, upload_id: &str) -> Result<(), StoreError> {
        self.inner.abort(bucket, key, upload_id).await
    }
}

fn capture_meta(session: &CaptureSession, sha256: &str) -> Meta {
    let mut meta = Meta::default();
    meta.metadata
        .insert("sha".into(), metadata_sha(session.sha.as_deref()));
    if let Some(user) = metadata_user(&session.user) {
        meta.metadata.insert("user".into(), user);
    }
    meta.metadata
        .insert("playtest".into(), metadata_encode(&session.playtest));
    meta.metadata.insert("sha256".into(), sha256.to_owned());
    meta
}

pub(crate) fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

enum Job {
    Resume {
        session: CaptureSession,
        row: InflightUpload,
    },
    New {
        session: CaptureSession,
        candidate: Candidate,
    },
}

impl Job {
    fn path(&self) -> &Path {
        match self {
            Job::Resume { row, .. } => &row.local_path,
            Job::New { candidate, .. } => &candidate.path,
        }
    }
}

struct Exclusions {
    blocked: HashSet<PathBuf>,
    hash_cache: HashMap<PathBuf, (u64, i64, String)>,
}

/// Next file in D2 7.1 order: sessions by `launched_at`, in-flight rows first, then candidates.
fn next_job(
    ledger: &CaptureLedger,
    sessions: &[(CaptureSession, Vec<ResolvedWatch>)],
    ex: &Exclusions,
    quiescence_seconds: u64,
    now: DateTime<Utc>,
) -> Option<Job> {
    for (session, watches) in sessions {
        let row = ledger.inflight.iter().find(|r| {
            r.session_id == session.id
                && !ex.blocked.contains(&r.local_path)
                && r.next_attempt_at.is_none_or(|t| t <= now)
        });
        if let Some(row) = row {
            return Some(Job::Resume {
                session: session.clone(),
                row: row.clone(),
            });
        }
        for candidate in candidates(session, ledger, watches, quiescence_seconds, now) {
            if ex.blocked.contains(&candidate.path) {
                continue;
            }
            let known_sha = ex
                .hash_cache
                .get(&candidate.path)
                .filter(|(size, mtime, _)| *size == candidate.size && *mtime == candidate.mtime_ms)
                .map(|(_, _, sha)| sha);
            if let Some(sha) = known_sha {
                if ledger.uploaded.contains_key(sha)
                    || ledger.inflight.iter().any(|r| &r.sha256 == sha)
                {
                    continue;
                }
            }
            return Some(Job::New {
                session: session.clone(),
                candidate,
            });
        }
    }
    None
}

/// The single upload worker: one file at a time.
pub(crate) struct CaptureUploader {
    shared: Arc<Shared>,
    pause_rx: watch::Receiver<PauseLevel>,
}

impl CaptureUploader {
    pub(crate) fn new(shared: Arc<Shared>) -> Self {
        let pause_rx = shared.pause_rx();
        Self { shared, pause_rx }
    }

    pub(crate) async fn run(self) {
        loop {
            tokio::select! {
                biased;
                _ = self.shared.root.cancelled() => return,
                _ = self.shared.wake_uploader.notified() => {}
                _ = tokio::time::sleep(UPLOADER_TICK) => {}
            }
            let mut tried = HashSet::new();
            while !self.shared.root.is_cancelled() && self.run_one(&mut tried).await {}
        }
    }

    async fn run_one(&self, tried: &mut HashSet<PathBuf>) -> bool {
        if *self.pause_rx.borrow() != PauseLevel::None {
            return false;
        }
        let Some(cfg) = self.shared.active_config() else {
            return false;
        };
        let now = self.shared.now();
        let ledger = self.shared.store.snapshot();
        let mut exited: Vec<CaptureSession> = ledger
            .sessions
            .iter()
            .filter(|s| s.state == SessionState::Exited)
            .cloned()
            .collect();
        if exited.is_empty() {
            return false;
        }
        exited.sort_by_key(|s| s.launched_at);
        let sessions: Vec<(CaptureSession, Vec<ResolvedWatch>)> = exited
            .into_iter()
            .map(|s| {
                let watches = self.shared.watches_for(&s.install_dir, &cfg);
                (s, watches)
            })
            .collect();

        let ex = {
            let work = self.shared.work.lock();
            let mut blocked: HashSet<PathBuf> = tried.clone();
            blocked.extend(work.owned.iter().cloned());
            blocked.extend(work.failed.keys().cloned());
            blocked.extend(
                work.retry
                    .iter()
                    .filter(|(_, r)| r.next_attempt_at.is_some_and(|t| t > now))
                    .map(|(p, _)| p.clone()),
            );
            Exclusions {
                blocked,
                hash_cache: work.hash_cache.clone(),
            }
        };

        let quiescence = cfg.quiescence_seconds;
        let job =
            tokio::task::spawn_blocking(move || next_job(&ledger, &sessions, &ex, quiescence, now))
                .await
                .ok()
                .flatten();
        let Some(job) = job else {
            return false;
        };
        let path = job.path().to_path_buf();
        tried.insert(path.clone());
        match job {
            Job::Resume { session, row } => {
                let token = self.shared.session_token(&session.id);
                self.shared.begin(&session.id, &path, Phase::Uploading);
                self.drive(session, row, &cfg, token).await;
            }
            Job::New { session, candidate } => {
                let token = self.shared.session_token(&session.id);
                self.shared.begin(&session.id, &path, Phase::Hashing);
                self.upload_new(session, &candidate, &cfg, token).await;
            }
        }
        self.shared.finish(&path);
        true
    }

    async fn upload_new(
        &self,
        session: CaptureSession,
        candidate: &Candidate,
        cfg: &ClientCaptureUploadConfig,
        token: CancellationToken,
    ) {
        let Some(sha256) = self.hash(&session.id, candidate, &token).await else {
            return;
        };
        let duplicate = self.shared.store.with(|l| {
            l.uploaded.contains_key(&sha256) || l.inflight.iter().any(|r| r.sha256 == sha256)
        });
        if duplicate {
            debug!(
                "Capture file {} matches an uploaded or in-flight file; skipping",
                candidate.path.display()
            );
            return;
        }

        let bucket = match cfg.bucket.clone() {
            Some(bucket) => Some(bucket),
            None => self.shared.backend.artifact_bucket().await,
        };
        let Some(bucket) = bucket else {
            return;
        };

        let now = self.shared.now();
        let attempts = self
            .shared
            .work
            .lock()
            .retry
            .get(&candidate.path)
            .map_or(0, |r| r.attempts);
        let row = InflightUpload {
            session_id: session.id.clone(),
            local_path: candidate.path.clone(),
            size: candidate.size,
            mtime_ms: candidate.mtime_ms,
            sha256,
            bucket,
            key: object_key(
                &candidate.key_prefix,
                now.date_naive(),
                &session.user,
                session.sha.as_deref(),
                candidate.mtime,
                &candidate.path,
            ),
            upload_id: None,
            part_size: part_size(
                candidate.size,
                cfg.part_size_mib,
                cfg.max_upload_mbps.is_some(),
            ),
            started_at: now,
            attempts,
            next_attempt_at: None,
        };

        let still_exited = self.shared.store.with(|l| {
            l.sessions
                .iter()
                .any(|s| s.id == session.id && s.state == SessionState::Exited)
        });
        if !still_exited || token.is_cancelled() {
            return;
        }
        self.shared.store.upsert_inflight(row.clone(), now);
        self.shared.store.flush(now);
        self.drive(session, row, cfg, token).await;
    }

    async fn hash(
        &self,
        session_id: &str,
        candidate: &Candidate,
        token: &CancellationToken,
    ) -> Option<String> {
        let cached = self
            .shared
            .work
            .lock()
            .hash_cache
            .get(&candidate.path)
            .filter(|(size, mtime, _)| *size == candidate.size && *mtime == candidate.mtime_ms)
            .map(|(_, _, sha)| sha.clone());
        if cached.is_some() {
            return cached;
        }
        self.shared.emit();
        let outcome = hash_file(
            candidate.path.clone(),
            candidate.size,
            candidate.mtime_ms,
            vec![self.shared.root.clone(), token.clone()],
            self.pause_rx.clone(),
        )
        .await;
        match outcome {
            Ok(HashOutcome::Done(sha)) => {
                self.shared.work.lock().hash_cache.insert(
                    candidate.path.clone(),
                    (candidate.size, candidate.mtime_ms, sha.clone()),
                );
                Some(sha)
            }
            Ok(HashOutcome::Changed) => {
                debug!(
                    "Capture file {} changed while hashing; retrying later",
                    candidate.path.display()
                );
                None
            }
            Ok(HashOutcome::Stopped) => None,
            Err(e) => {
                match Failure::from_io(&e) {
                    Failure::Missing => debug!(
                        "Capture file {} disappeared before hashing",
                        candidate.path.display()
                    ),
                    failure => self.retry_later(session_id, &candidate.path, None, failure),
                }
                None
            }
        }
    }

    async fn drive(
        &self,
        session: CaptureSession,
        mut row: InflightUpload,
        cfg: &ClientCaptureUploadConfig,
        token: CancellationToken,
    ) {
        let Some(inner) = self.shared.backend.part_store().await else {
            return;
        };
        let progress = Arc::new(Progress::default());
        self.shared.set_uploading(&row.local_path, progress.clone());
        self.shared.emit();

        let store: Arc<dyn PartStore> = Arc::new(CountingStore { inner, progress });
        let meta = capture_meta(&session, &row.sha256);
        self.shared.pacer.set_cap(cfg.max_upload_mbps);
        let path = row.local_path.clone();
        let spec = UploadSpec {
            path: &path,
            meta: &meta,
            part_concurrency: cfg.part_concurrency.max(1) as usize,
        };
        let persist = {
            let shared = self.shared.clone();
            move |r: &InflightUpload| shared.store.upsert_inflight(r.clone(), shared.now())
        };
        let result = tokio::select! {
            biased;
            _ = self.shared.root.cancelled() => None,
            result = upload_file(
                store.clone(),
                &mut row,
                &spec,
                self.pause_rx.clone(),
                self.shared.pacer.clone(),
                &token,
                persist,
            ) => Some(result),
        };
        // Shutdown drops the driver without aborting, so the row resumes on the next start.
        let Some(result) = result else {
            return;
        };
        self.settle(&session, row, store.as_ref(), result, cfg)
            .await;
    }

    async fn settle(
        &self,
        session: &CaptureSession,
        mut row: InflightUpload,
        store: &dyn PartStore,
        result: Result<UploadOutcome, StoreError>,
        cfg: &ClientCaptureUploadConfig,
    ) {
        let now = self.shared.now();
        match result {
            Ok(UploadOutcome::Uploaded) => {
                info!(
                    "Uploaded capture file {} to s3://{}/{}",
                    row.local_path.display(),
                    row.bucket,
                    row.key
                );
                self.shared.store.record_uploaded(
                    row.sha256.clone(),
                    UploadedEntry {
                        key: row.key.clone(),
                        bucket: row.bucket.clone(),
                        local_path: row.local_path.clone(),
                        size: row.size,
                        mtime_ms: row.mtime_ms,
                        uploaded_at: now,
                        session_id: row.session_id.clone(),
                    },
                    now,
                );
                self.shared.work.lock().retry.remove(&row.local_path);
                self.shared.maintain(cfg).await;
                self.shared.wake_watcher.notify_one();
            }
            Ok(UploadOutcome::FileChanged) => {
                self.shared.store.remove_inflight(&row.local_path, now);
                self.shared.work.lock().hash_cache.remove(&row.local_path);
                if !exists(&row.local_path).await {
                    self.toast(&session.id, &row.local_path, &Failure::Missing, now);
                }
            }
            Ok(UploadOutcome::Cancelled) => {
                self.shared.store.remove_inflight(&row.local_path, now);
            }
            Err(e) => match Failure::from_store(e) {
                Failure::Missing => {
                    if let Some(upload_id) = row.upload_id.take() {
                        if let Err(e) = store.abort(&row.bucket, &row.key, &upload_id).await {
                            warn!("Could not abort multipart upload for {}: {e:?}", row.key);
                        }
                    }
                    self.shared.store.remove_inflight(&row.local_path, now);
                    self.toast(&session.id, &row.local_path, &Failure::Missing, now);
                }
                Failure::Auth(message) => {
                    warn!(
                        "Capture upload of {} needs sign-in: {message}",
                        row.local_path.display()
                    );
                    self.shared.store.upsert_inflight(row, now);
                    self.shared.hold_for_sign_in().await;
                }
                failure => {
                    let path = row.local_path.clone();
                    let plan = plan_retry(&failure, row.attempts, now, random_jitter());
                    row.attempts = plan.attempts;
                    row.next_attempt_at = plan.next_attempt_at;
                    self.shared.store.upsert_inflight(row, now);
                    self.retry_later(&session.id, &path, Some(plan), failure);
                }
            },
        }
        self.shared.emit();
    }

    fn retry_later(
        &self,
        session_id: &str,
        path: &Path,
        plan: Option<RetryPlan>,
        failure: Failure,
    ) {
        let now = self.shared.now();
        let plan = plan.unwrap_or_else(|| {
            let attempts = self
                .shared
                .work
                .lock()
                .retry
                .get(path)
                .map_or(0, |r| r.attempts);
            plan_retry(&failure, attempts, now, random_jitter())
        });
        warn!(
            "Capture upload of {} failed ({}): {}",
            path.display(),
            failure.class(),
            failure.message()
        );
        {
            let mut work = self.shared.work.lock();
            if plan.failed {
                work.failed.insert(path.to_path_buf(), failure.message());
                work.retry.remove(path);
            } else {
                work.retry.insert(
                    path.to_path_buf(),
                    RetryState {
                        attempts: plan.attempts,
                        next_attempt_at: plan.next_attempt_at,
                        message: failure.message(),
                    },
                );
            }
        }
        self.toast(session_id, path, &failure, now);
    }

    fn toast(&self, session_id: &str, path: &Path, failure: &Failure, now: DateTime<Utc>) {
        let Some(text) = failure.toast(&file_name(path)) else {
            return;
        };
        let allowed = self
            .shared
            .work
            .lock()
            .toasts
            .allow(session_id, path, failure.class(), now);
        if allowed {
            self.shared.notify_error(text);
        }
    }
}

async fn exists(path: &Path) -> bool {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || path.try_exists().unwrap_or(true))
        .await
        .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::io::Write;

    use tempfile::TempDir;

    use super::*;

    const ABC_SHA256: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    fn write(dir: &TempDir, name: &str, data: &[u8]) -> (PathBuf, u64, i64) {
        let path = dir.path().join(name);
        std::fs::write(&path, data).unwrap();
        let meta = std::fs::metadata(&path).unwrap();
        (path, meta.len(), mtime_ms(&meta).unwrap())
    }

    fn never() -> bool {
        false
    }

    fn t(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000 + secs, 0).unwrap()
    }

    #[test]
    fn sha256_matches_known_vector() {
        let dir = TempDir::new().unwrap();
        let (path, size, mtime) = write(&dir, "a.log", b"abc");
        let outcome = hash_file_blocking(&path, size, mtime, &never, &never, &|| {}).unwrap();
        assert_eq!(outcome, HashOutcome::Done(ABC_SHA256.into()));
    }

    #[test]
    fn multi_chunk_file_hashes_like_one_read() {
        let dir = TempDir::new().unwrap();
        let data: Vec<u8> = (0..HASH_CHUNK * 2 + 17).map(|i| (i % 251) as u8).collect();
        let (path, size, mtime) = write(&dir, "big.utrace", &data);
        let mut context = Context::new(&SHA256);
        context.update(&data);
        let expected = hex(context.finish().as_ref());
        let outcome = hash_file_blocking(&path, size, mtime, &never, &never, &|| {}).unwrap();
        assert_eq!(outcome, HashOutcome::Done(expected));
    }

    #[test]
    fn stat_mismatch_before_hashing_is_changed() {
        let dir = TempDir::new().unwrap();
        let (path, size, mtime) = write(&dir, "a.log", b"abc");
        let outcome = hash_file_blocking(&path, size + 1, mtime, &never, &never, &|| {}).unwrap();
        assert_eq!(outcome, HashOutcome::Changed);
    }

    #[test]
    fn file_changed_during_hash_is_discarded() {
        let dir = TempDir::new().unwrap();
        let (path, size, mtime) = write(&dir, "a.log", b"abc");
        let paused = Cell::new(true);
        let park = || {
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            file.write_all(b"more").unwrap();
            paused.set(false);
        };
        let outcome =
            hash_file_blocking(&path, size, mtime, &never, &|| paused.get(), &park).unwrap();
        assert_eq!(outcome, HashOutcome::Changed);
    }

    #[test]
    fn hard_pause_parks_then_resumes() {
        let dir = TempDir::new().unwrap();
        let (path, size, mtime) = write(&dir, "a.log", b"abc");
        let parks = Cell::new(0);
        let paused = || parks.get() < 3;
        let park = || parks.set(parks.get() + 1);
        let outcome = hash_file_blocking(&path, size, mtime, &never, &paused, &park).unwrap();
        assert_eq!(outcome, HashOutcome::Done(ABC_SHA256.into()));
        assert_eq!(parks.get(), 3);
    }

    #[test]
    fn stop_while_parked_ends_hash() {
        let dir = TempDir::new().unwrap();
        let (path, size, mtime) = write(&dir, "a.log", b"abc");
        let stopped = Cell::new(false);
        let outcome = hash_file_blocking(&path, size, mtime, &|| stopped.get(), &|| true, &|| {
            stopped.set(true)
        })
        .unwrap();
        assert_eq!(outcome, HashOutcome::Stopped);
    }

    #[test]
    fn missing_file_maps_to_missing() {
        let dir = TempDir::new().unwrap();
        let err = hash_file_blocking(&dir.path().join("gone.log"), 1, 0, &never, &never, &|| {})
            .unwrap_err();
        assert_eq!(Failure::from_io(&err), Failure::Missing);
    }

    #[tokio::test]
    async fn async_hash_matches_blocking_hash() {
        let dir = TempDir::new().unwrap();
        let (path, size, mtime) = write(&dir, "a.log", b"abc");
        let (_tx, rx) = watch::channel(PauseLevel::Soft);
        let outcome = hash_file(path, size, mtime, vec![CancellationToken::new()], rx)
            .await
            .unwrap();
        assert_eq!(outcome, HashOutcome::Done(ABC_SHA256.into()));
    }

    #[test]
    fn transient_backoff_doubles_up_to_cap() {
        let secs: Vec<i64> = (1..=8)
            .map(|a| transient_delay(a, 0.0).num_seconds())
            .collect();
        assert_eq!(secs, vec![30, 60, 120, 240, 480, 900, 900, 900]);
        assert_eq!(transient_delay(1_000, 0.0).num_seconds(), 900);
        assert_eq!(transient_delay(0, 0.0).num_seconds(), 30);
    }

    #[test]
    fn jitter_is_bounded_to_twenty_percent() {
        assert_eq!(transient_delay(1, 0.2).num_seconds(), 36);
        assert_eq!(transient_delay(1, -0.2).num_seconds(), 24);
        assert_eq!(transient_delay(1, 5.0).num_seconds(), 36);
        assert_eq!(transient_delay(6, 0.2).num_seconds(), 1080);
        for _ in 0..1_000 {
            let j = random_jitter();
            assert!((-JITTER..=JITTER).contains(&j));
            let d = transient_delay(2, j).num_milliseconds();
            assert!((48_000..=72_000).contains(&d), "{d}");
        }
    }

    #[test]
    fn retry_plans_follow_error_class() {
        let now = t(0);
        let transient = plan_retry(&Failure::Transient("x".into()), 0, now, 0.0);
        assert_eq!(transient.attempts, 1);
        assert_eq!(transient.next_attempt_at, Some(t(30)));
        let transient = plan_retry(&Failure::Transient("x".into()), 1, now, 0.0);
        assert_eq!(transient.next_attempt_at, Some(t(60)));

        let auth = plan_retry(&Failure::Auth("x".into()), 3, now, 0.0);
        assert_eq!(auth.attempts, 3);
        assert_eq!(auth.next_attempt_at, None);
        assert!(!auth.failed);

        let denied = plan_retry(&Failure::Denied("x".into()), 3, now, 0.2);
        assert_eq!(denied.attempts, 4);
        assert_eq!(denied.next_attempt_at, Some(now + DENIED_DELAY));

        let locked = plan_retry(&Failure::Locked("x".into()), 5, now, 0.2);
        assert_eq!(locked.attempts, 5);
        assert_eq!(locked.next_attempt_at, Some(t(30)));

        let fatal = plan_retry(&Failure::Fatal("x".into()), 0, now, 0.0);
        assert!(fatal.failed);
        assert_eq!(fatal.next_attempt_at, None);
    }

    #[test]
    fn store_errors_classify() {
        assert_eq!(
            Failure::from_store(StoreError::Auth("e".into())),
            Failure::Auth("e".into())
        );
        assert!(matches!(
            Failure::from_store(StoreError::NoSuchUpload),
            Failure::Transient(_)
        ));
        let gone = std::io::Error::from(std::io::ErrorKind::NotFound);
        assert_eq!(Failure::from_store(StoreError::Io(gone)), Failure::Missing);
        let denied = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        assert!(matches!(
            Failure::from_store(StoreError::Io(denied)),
            Failure::Locked(_)
        ));
    }

    #[test]
    fn toasts_are_limited_per_session_file_and_class() {
        let mut limiter = ToastLimiter::default();
        let path = Path::new("a.log");
        assert!(limiter.allow("s", path, "transient", t(0)));
        assert!(!limiter.allow("s", path, "transient", t(3599)));
        assert!(limiter.allow("s", path, "fatal", t(10)));
        assert!(limiter.allow("other", path, "transient", t(10)));
        assert!(limiter.allow("s", Path::new("b.log"), "transient", t(10)));
        assert!(limiter.allow("s", path, "transient", t(3600)));
    }
}
