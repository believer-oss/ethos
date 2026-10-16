//! Resumable multipart upload driver over a small part-store trait.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::future::Future;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use aws_sdk_s3::config::http::HttpResponse;
use aws_sdk_s3::config::retry::RetryConfig;
use aws_sdk_s3::config::RequestChecksumCalculation;
use aws_sdk_s3::error::{DisplayErrorContext, ProvideErrorMetadata, SdkError};
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::types::{CompletedMultipartUpload, CompletedPart};
use aws_types::SdkConfig;
use bytes::Bytes;
use parking_lot::Mutex;
use tokio::sync::{watch, Semaphore};
use tokio::task::JoinSet;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use super::ledger::stat_matches;
use super::types::{InflightUpload, PauseLevelStatus};

pub const MIB: u64 = 1 << 20;
pub const MAX_PART_SIZE: u64 = 5 << 30;
const MAX_PARTS: u64 = 10_000;
const TARGET_MAX_PARTS: u64 = 9_000;
const THROTTLED_PART_MIB: u64 = 8;
const PART_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const DEFAULT_CONTENT_TYPE: &str = "application/octet-stream";
/// Upload rounds that all succeed but still leave `ListParts` short before giving up as transient.
const MAX_FRUITLESS_ROUNDS: u32 = 3;

#[derive(Clone, Debug, Default)]
pub struct Meta {
    pub content_type: Option<String>,
    /// Values must already be percent-encoded (`key::metadata_*`).
    pub metadata: BTreeMap<String, String>,
}

impl Meta {
    fn content_type(&self) -> &str {
        self.content_type.as_deref().unwrap_or(DEFAULT_CONTENT_TYPE)
    }

    fn metadata_map(&self) -> Option<HashMap<String, String>> {
        (!self.metadata.is_empty()).then(|| self.metadata.clone().into_iter().collect())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PartInfo {
    pub number: i32,
    pub size: u64,
    pub e_tag: String,
    pub checksum_crc32: Option<String>,
}

#[derive(Debug)]
pub enum StoreError {
    Auth(String),
    Denied(String),
    NoSuchUpload,
    Transient(String),
    Fatal(String),
    /// Reading the local file failed; the caller decides between missing, locked and retry.
    Io(std::io::Error),
}

#[async_trait]
pub trait PartStore: Send + Sync {
    async fn put(
        &self,
        bucket: &str,
        key: &str,
        body: Bytes,
        meta: &Meta,
    ) -> Result<(), StoreError>;
    async fn create_mpu(&self, bucket: &str, key: &str, meta: &Meta) -> Result<String, StoreError>;
    async fn list_parts(
        &self,
        bucket: &str,
        key: &str,
        upload_id: &str,
    ) -> Result<Vec<PartInfo>, StoreError>;
    async fn upload_part(
        &self,
        bucket: &str,
        key: &str,
        upload_id: &str,
        number: i32,
        body: Bytes,
    ) -> Result<(), StoreError>;
    async fn complete(
        &self,
        bucket: &str,
        key: &str,
        upload_id: &str,
        parts: Vec<PartInfo>,
    ) -> Result<(), StoreError>;
    async fn abort(&self, bucket: &str, key: &str, upload_id: &str) -> Result<(), StoreError>;
}

/// S3-backed part store. Build one per file so refreshed credentials in the shared provider apply.
pub struct S3PartStore {
    client: aws_sdk_s3::Client,
}

impl S3PartStore {
    pub fn new(sdk_config: &SdkConfig) -> Self {
        // `create_mpu` names no checksum algorithm, so no request may carry a flexible checksum
        // (the SDK default would add CRC32 to every part). Payloads stay SigV4-signed (SHA-256).
        let config = aws_sdk_s3::config::Builder::from(sdk_config)
            .retry_config(RetryConfig::standard().with_max_attempts(5))
            .request_checksum_calculation(RequestChecksumCalculation::WhenRequired)
            .build();
        Self {
            client: aws_sdk_s3::Client::from_conf(config),
        }
    }
}

fn classify<E>(err: SdkError<E, HttpResponse>) -> StoreError
where
    E: ProvideErrorMetadata + std::error::Error + Send + Sync + 'static,
{
    let detail = DisplayErrorContext(&err).to_string();
    match &err {
        SdkError::ServiceError(e) => {
            classify_code(e.err().code(), e.raw().status().as_u16(), detail)
        }
        SdkError::ConstructionFailure(_) => StoreError::Fatal(detail),
        _ => StoreError::Transient(detail),
    }
}

fn classify_code(code: Option<&str>, status: u16, detail: String) -> StoreError {
    match code {
        Some("ExpiredToken" | "InvalidAccessKeyId" | "RequestExpired") => StoreError::Auth(detail),
        Some("AccessDenied") => StoreError::Denied(detail),
        Some("NoSuchUpload") => StoreError::NoSuchUpload,
        Some("SlowDown" | "Throttling" | "ThrottlingException" | "RequestTimeout") => {
            StoreError::Transient(detail)
        }
        _ if status == 429 || status >= 500 => StoreError::Transient(detail),
        _ if (400..500).contains(&status) => StoreError::Fatal(detail),
        _ => StoreError::Transient(detail),
    }
}

#[async_trait]
impl PartStore for S3PartStore {
    async fn put(
        &self,
        bucket: &str,
        key: &str,
        body: Bytes,
        meta: &Meta,
    ) -> Result<(), StoreError> {
        self.client
            .put_object()
            .bucket(bucket)
            .key(key)
            .content_type(meta.content_type())
            .set_metadata(meta.metadata_map())
            .content_length(body.len() as i64)
            .body(ByteStream::from(body))
            .send()
            .await
            .map(|_| ())
            .map_err(classify)
    }

    async fn create_mpu(&self, bucket: &str, key: &str, meta: &Meta) -> Result<String, StoreError> {
        let out = self
            .client
            .create_multipart_upload()
            .bucket(bucket)
            .key(key)
            .content_type(meta.content_type())
            .set_metadata(meta.metadata_map())
            .send()
            .await
            .map_err(classify)?;
        out.upload_id()
            .map(str::to_owned)
            .ok_or_else(|| StoreError::Fatal("CreateMultipartUpload returned no upload id".into()))
    }

    async fn list_parts(
        &self,
        bucket: &str,
        key: &str,
        upload_id: &str,
    ) -> Result<Vec<PartInfo>, StoreError> {
        let mut parts = Vec::new();
        let mut marker: Option<String> = None;
        loop {
            let out = self
                .client
                .list_parts()
                .bucket(bucket)
                .key(key)
                .upload_id(upload_id)
                .set_part_number_marker(marker.take())
                .send()
                .await
                .map_err(classify)?;
            parts.extend(out.parts().iter().map(|p| PartInfo {
                number: p.part_number().unwrap_or(0),
                size: p.size().and_then(|s| u64::try_from(s).ok()).unwrap_or(0),
                e_tag: p.e_tag().unwrap_or_default().to_owned(),
                checksum_crc32: p.checksum_crc32().map(str::to_owned),
            }));
            marker = out.next_part_number_marker().map(str::to_owned);
            if out.is_truncated() != Some(true) || marker.is_none() {
                return Ok(parts);
            }
        }
    }

    async fn upload_part(
        &self,
        bucket: &str,
        key: &str,
        upload_id: &str,
        number: i32,
        body: Bytes,
    ) -> Result<(), StoreError> {
        self.client
            .upload_part()
            .bucket(bucket)
            .key(key)
            .upload_id(upload_id)
            .part_number(number)
            .content_length(body.len() as i64)
            .body(ByteStream::from(body))
            .send()
            .await
            .map(|_| ())
            .map_err(classify)
    }

    async fn complete(
        &self,
        bucket: &str,
        key: &str,
        upload_id: &str,
        parts: Vec<PartInfo>,
    ) -> Result<(), StoreError> {
        let parts = parts
            .into_iter()
            .map(|p| {
                CompletedPart::builder()
                    .part_number(p.number)
                    .e_tag(p.e_tag)
                    .set_checksum_crc32(p.checksum_crc32)
                    .build()
            })
            .collect();
        self.client
            .complete_multipart_upload()
            .bucket(bucket)
            .key(key)
            .upload_id(upload_id)
            .multipart_upload(
                CompletedMultipartUpload::builder()
                    .set_parts(Some(parts))
                    .build(),
            )
            .send()
            .await
            .map(|_| ())
            .map_err(classify)
    }

    async fn abort(&self, bucket: &str, key: &str, upload_id: &str) -> Result<(), StoreError> {
        self.client
            .abort_multipart_upload()
            .bucket(bucket)
            .key(key)
            .upload_id(upload_id)
            .send()
            .await
            .map(|_| ())
            .map_err(classify)
    }
}

/// Part size for a new upload: at most 9,000 parts, whole MiB, capped at 8 MiB base when throttled.
pub fn part_size(size: u64, part_size_mib: u64, throttled: bool) -> u64 {
    let mib = if throttled {
        part_size_mib.min(THROTTLED_PART_MIB)
    } else {
        part_size_mib
    };
    let base = mib.max(1).saturating_mul(MIB);
    let wanted = base.max(size.div_ceil(TARGET_MAX_PARTS));
    wanted.div_ceil(MIB).saturating_mul(MIB).min(MAX_PART_SIZE)
}

#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum PauseLevel {
    #[default]
    None,
    Soft,
    Hard,
    Blocked,
}

impl From<PauseLevel> for PauseLevelStatus {
    fn from(level: PauseLevel) -> Self {
        match level {
            PauseLevel::None => Self::None,
            PauseLevel::Soft => Self::Soft,
            PauseLevel::Hard => Self::Hard,
            PauseLevel::Blocked => Self::Blocked,
        }
    }
}

/// Spaces part starts so the average rate stays under `maxUploadMbps`.
#[derive(Debug, Default)]
pub struct Pacer {
    state: Mutex<PacerState>,
}

#[derive(Debug, Default)]
struct PacerState {
    mbps: Option<f64>,
    next_free: Option<Instant>,
}

impl Pacer {
    pub fn new(mbps: Option<f64>) -> Self {
        Self {
            state: Mutex::new(PacerState {
                mbps,
                next_free: None,
            }),
        }
    }

    pub fn set_cap(&self, mbps: Option<f64>) {
        self.state.lock().mbps = mbps;
    }

    /// Reserves `bytes` of budget and returns when they may start; `None` when unlimited.
    pub fn reserve(&self, bytes: u64) -> Option<Instant> {
        let mut state = self.state.lock();
        let mbps = state.mbps.filter(|m| m.is_finite() && *m > 0.0)?;
        let now = Instant::now();
        let start = state.next_free.map_or(now, |next| next.max(now));
        state.next_free = Some(start + Duration::from_secs_f64(bytes as f64 / (mbps * 125_000.0)));
        Some(start)
    }

    pub async fn wait(&self, bytes: u64) {
        if let Some(start) = self.reserve(bytes) {
            tokio::time::sleep_until(start).await;
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum UploadOutcome {
    Uploaded,
    /// `(size, mtime_ms)` no longer match the row; any MPU was aborted.
    FileChanged,
    /// `cancel` fired; any MPU was aborted.
    Cancelled,
}

pub struct UploadSpec<'a> {
    pub path: &'a Path,
    pub meta: &'a Meta,
    pub part_concurrency: usize,
}

#[derive(Debug)]
enum PartOutcome {
    Done,
    Paused,
    Failed(StoreError),
}

enum Round {
    Done,
    Paused,
    Cancelled,
    Failed(StoreError),
}

struct Layout {
    size: u64,
    part_size: u64,
    count: i32,
}

impl Layout {
    fn new(size: u64, part_size: u64) -> Result<Self, StoreError> {
        if part_size == 0 || part_size > MAX_PART_SIZE {
            return Err(StoreError::Fatal(format!("invalid part size {part_size}")));
        }
        let count = size.div_ceil(part_size);
        if count > MAX_PARTS {
            return Err(StoreError::Fatal(format!(
                "{count} parts exceeds the S3 limit"
            )));
        }
        Ok(Self {
            size,
            part_size,
            count: count as i32,
        })
    }

    fn offset(&self, number: i32) -> u64 {
        (number as u64 - 1) * self.part_size
    }

    fn expected(&self, number: i32) -> u64 {
        if number < self.count {
            self.part_size
        } else {
            self.size - self.offset(number)
        }
    }

    fn is_done(&self, part: &PartInfo) -> bool {
        (1..=self.count).contains(&part.number) && part.size == self.expected(part.number)
    }

    fn missing(&self, listed: &[PartInfo]) -> Vec<i32> {
        let done: HashSet<i32> = listed
            .iter()
            .filter(|p| self.is_done(p))
            .map(|p| p.number)
            .collect();
        (1..=self.count).filter(|n| !done.contains(n)).collect()
    }

    fn completed(&self, listed: &[PartInfo]) -> Vec<PartInfo> {
        let mut parts: Vec<PartInfo> = listed.iter().filter(|p| self.is_done(p)).cloned().collect();
        parts.sort_by_key(|p| p.number);
        parts.dedup_by_key(|p| p.number);
        parts
    }
}

struct PartJob {
    store: Arc<dyn PartStore>,
    bucket: String,
    key: String,
    upload_id: String,
    path: PathBuf,
    pacer: Arc<Pacer>,
    cancel: CancellationToken,
    permits: Arc<Semaphore>,
}

/// Uploads one file described by `row`, resuming its MPU when `row.upload_id` is set.
///
/// `cancel` means the upload is abandoned (session cancelled or expired) and aborts the MPU.
/// To stop without aborting (shutdown), drop the future instead; the row then resumes later.
/// `Auth`, `Denied`, `Transient` and `Fatal` return with the row and MPU kept.
pub async fn upload_file<P>(
    store: Arc<dyn PartStore>,
    row: &mut InflightUpload,
    spec: &UploadSpec<'_>,
    mut pause_rx: watch::Receiver<PauseLevel>,
    pacer: Arc<Pacer>,
    cancel: &CancellationToken,
    persist: P,
) -> Result<UploadOutcome, StoreError>
where
    P: FnMut(&InflightUpload) + Send,
{
    if !wait_clear(&mut pause_rx, cancel).await {
        return Ok(abandon(store.as_ref(), row, UploadOutcome::Cancelled).await);
    }
    if !file_unchanged(spec.path, row).await {
        return Ok(abandon(store.as_ref(), row, UploadOutcome::FileChanged).await);
    }
    if row.size <= row.part_size {
        return put_whole(store.as_ref(), row, spec, pause_rx, &pacer, cancel).await;
    }
    upload_multipart(store, row, spec, pause_rx, pacer, cancel, persist).await
}

async fn put_whole(
    store: &dyn PartStore,
    row: &InflightUpload,
    spec: &UploadSpec<'_>,
    mut pause_rx: watch::Receiver<PauseLevel>,
    pacer: &Pacer,
    cancel: &CancellationToken,
) -> Result<UploadOutcome, StoreError> {
    let mut hard_rx = pause_rx.clone();
    loop {
        if !wait_clear(&mut pause_rx, cancel).await {
            return Ok(UploadOutcome::Cancelled);
        }
        let attempt = async {
            pacer.wait(row.size).await;
            let path = spec.path.to_path_buf();
            let size = row.size;
            let body = tokio::task::spawn_blocking(move || read_range(&path, 0, size))
                .await
                .map_err(|e| StoreError::Transient(format!("read task failed: {e}")))?
                .map_err(StoreError::Io)?;
            store
                .put(&row.bucket, &row.key, Bytes::from(body), spec.meta)
                .await
        };
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return Ok(UploadOutcome::Cancelled),
            _ = until_hard(&mut hard_rx) => continue,
            result = attempt => {
                result?;
                return Ok(UploadOutcome::Uploaded);
            }
        }
    }
}

async fn upload_multipart<P>(
    store: Arc<dyn PartStore>,
    row: &mut InflightUpload,
    spec: &UploadSpec<'_>,
    mut pause_rx: watch::Receiver<PauseLevel>,
    pacer: Arc<Pacer>,
    cancel: &CancellationToken,
    mut persist: P,
) -> Result<UploadOutcome, StoreError>
where
    P: FnMut(&InflightUpload) + Send,
{
    let layout = Layout::new(row.size, row.part_size)?;
    let mut recreated = false;
    let mut listed: Option<Vec<PartInfo>> = None;
    let mut fruitless = 0;
    loop {
        let upload_id = match row.upload_id.clone() {
            Some(id) => id,
            None => {
                let created =
                    guard(cancel, store.create_mpu(&row.bucket, &row.key, spec.meta)).await;
                let Some(created) = created else {
                    return Ok(UploadOutcome::Cancelled);
                };
                let id = created?;
                row.upload_id = Some(id.clone());
                persist(row);
                listed = Some(Vec::new());
                id
            }
        };

        let parts = match listed.take() {
            Some(parts) => parts,
            None => {
                match guard(cancel, store.list_parts(&row.bucket, &row.key, &upload_id)).await {
                    None => return Ok(abandon(store.as_ref(), row, UploadOutcome::Cancelled).await),
                    Some(Err(StoreError::NoSuchUpload)) if !recreated => {
                        recreated = true;
                        row.upload_id = None;
                        continue;
                    }
                    Some(result) => result?,
                }
            }
        };

        let missing = layout.missing(&parts);
        if missing.is_empty() {
            if !file_unchanged(spec.path, row).await {
                return Ok(abandon(store.as_ref(), row, UploadOutcome::FileChanged).await);
            }
            let done = layout.completed(&parts);
            match guard(
                cancel,
                store.complete(&row.bucket, &row.key, &upload_id, done),
            )
            .await
            {
                None => return Ok(abandon(store.as_ref(), row, UploadOutcome::Cancelled).await),
                Some(Ok(())) => return Ok(UploadOutcome::Uploaded),
                Some(Err(StoreError::NoSuchUpload)) if !recreated => {
                    recreated = true;
                    row.upload_id = None;
                    continue;
                }
                Some(Err(e)) => return Err(e),
            }
        }

        if fruitless >= MAX_FRUITLESS_ROUNDS {
            return Err(StoreError::Transient(format!(
                "{} parts still missing after upload",
                missing.len()
            )));
        }

        let job = Arc::new(PartJob {
            store: store.clone(),
            bucket: row.bucket.clone(),
            key: row.key.clone(),
            upload_id,
            path: spec.path.to_path_buf(),
            pacer: pacer.clone(),
            cancel: cancel.clone(),
            permits: Arc::new(Semaphore::new(spec.part_concurrency.max(1))),
        });
        match run_parts(job, &layout, missing, &pause_rx, cancel).await {
            Round::Done => fruitless += 1,
            Round::Paused => {
                if !wait_clear(&mut pause_rx, cancel).await {
                    return Ok(abandon(store.as_ref(), row, UploadOutcome::Cancelled).await);
                }
            }
            Round::Cancelled => {
                return Ok(abandon(store.as_ref(), row, UploadOutcome::Cancelled).await)
            }
            Round::Failed(StoreError::NoSuchUpload) if !recreated => {
                recreated = true;
                row.upload_id = None;
            }
            Round::Failed(e) => return Err(e),
        }
    }
}

async fn run_parts(
    job: Arc<PartJob>,
    layout: &Layout,
    missing: Vec<i32>,
    pause_rx: &watch::Receiver<PauseLevel>,
    cancel: &CancellationToken,
) -> Round {
    let mut set = JoinSet::new();
    for number in missing {
        set.spawn(part_task(
            job.clone(),
            number,
            layout.offset(number),
            layout.expected(number),
            pause_rx.clone(),
        ));
    }

    let mut paused = false;
    let round = loop {
        let next = tokio::select! {
            biased;
            _ = cancel.cancelled() => break Round::Cancelled,
            next = set.join_next() => next,
        };
        match next {
            None if paused => break Round::Paused,
            None => break Round::Done,
            Some(Ok(PartOutcome::Done)) => {}
            Some(Ok(PartOutcome::Paused)) => {
                paused = true;
                set.abort_all();
            }
            Some(Ok(PartOutcome::Failed(e))) => break Round::Failed(e),
            Some(Err(e)) if e.is_cancelled() => {}
            Some(Err(e)) => {
                break Round::Failed(StoreError::Transient(format!("part task failed: {e}")))
            }
        }
    };
    set.abort_all();
    while set.join_next().await.is_some() {}
    round
}

async fn part_task(
    job: Arc<PartJob>,
    number: i32,
    offset: u64,
    len: u64,
    pause_rx: watch::Receiver<PauseLevel>,
) -> PartOutcome {
    let mut hard_rx = pause_rx.clone();
    let mut clear_rx = pause_rx;
    let work = async {
        let Ok(permit) = job.permits.clone().acquire_owned().await else {
            return PartOutcome::Failed(StoreError::Transient("part semaphore closed".into()));
        };
        if !wait_clear(&mut clear_rx, &job.cancel).await {
            return PartOutcome::Paused;
        }
        job.pacer.wait(len).await;
        let path = job.path.clone();
        // The permit travels with the read so a dropped task still holds it until the buffer is freed.
        let read =
            tokio::task::spawn_blocking(move || (read_range(&path, offset, len), permit)).await;
        let (body, permit) = match read {
            Ok((Ok(body), permit)) => (body, permit),
            Ok((Err(e), _)) => return PartOutcome::Failed(StoreError::Io(e)),
            Err(e) => {
                return PartOutcome::Failed(StoreError::Transient(format!("read task failed: {e}")))
            }
        };
        let sent = tokio::time::timeout(
            PART_TIMEOUT,
            job.store.upload_part(
                &job.bucket,
                &job.key,
                &job.upload_id,
                number,
                Bytes::from(body),
            ),
        )
        .await;
        drop(permit);
        match sent {
            Ok(Ok(())) => PartOutcome::Done,
            Ok(Err(e)) => PartOutcome::Failed(e),
            Err(_) => {
                PartOutcome::Failed(StoreError::Transient(format!("part {number} timed out")))
            }
        }
    };
    tokio::select! {
        biased;
        _ = until_hard(&mut hard_rx) => PartOutcome::Paused,
        outcome = work => outcome,
    }
}

fn read_range(path: &Path, offset: u64, len: u64) -> std::io::Result<Vec<u8>> {
    let len = usize::try_from(len)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "range too large"))?;
    let mut file = std::fs::File::open(path)?;
    file.seek(SeekFrom::Start(offset))?;
    let mut buf = vec![0u8; len];
    file.read_exact(&mut buf)?;
    Ok(buf)
}

async fn file_unchanged(path: &Path, row: &InflightUpload) -> bool {
    let (path, size, mtime) = (path.to_path_buf(), row.size, row.mtime_ms);
    tokio::task::spawn_blocking(move || stat_matches(&path, size, mtime))
        .await
        .unwrap_or(false)
}

/// Aborts the row's MPU, if any, best effort; C's incomplete-upload lifecycle rule covers failures.
async fn abandon(
    store: &dyn PartStore,
    row: &mut InflightUpload,
    outcome: UploadOutcome,
) -> UploadOutcome {
    if let Some(upload_id) = row.upload_id.take() {
        if let Err(e) = store.abort(&row.bucket, &row.key, &upload_id).await {
            warn!("Could not abort multipart upload for {}: {e:?}", row.key);
        }
    }
    outcome
}

async fn guard<T>(cancel: &CancellationToken, fut: impl Future<Output = T>) -> Option<T> {
    tokio::select! {
        biased;
        _ = cancel.cancelled() => None,
        out = fut => Some(out),
    }
}

/// Waits for `PauseLevel::None`; false when cancelled.
async fn wait_clear(rx: &mut watch::Receiver<PauseLevel>, cancel: &CancellationToken) -> bool {
    loop {
        if cancel.is_cancelled() {
            return false;
        }
        if *rx.borrow_and_update() == PauseLevel::None {
            return true;
        }
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return false,
            changed = rx.changed() => {
                if changed.is_err() {
                    cancel.cancelled().await;
                    return false;
                }
            }
        }
    }
}

async fn until_hard(rx: &mut watch::Receiver<PauseLevel>) {
    loop {
        if *rx.borrow_and_update() == PauseLevel::Hard {
            return;
        }
        if rx.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

#[cfg(test)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Op {
    Put,
    Create,
    List,
    Part,
    Complete,
    Abort,
}

#[cfg(test)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum FaultKind {
    Auth,
    Denied,
    NoSuchUpload,
    Transient,
    Fatal,
}

#[cfg(test)]
impl FaultKind {
    fn error(self) -> StoreError {
        match self {
            FaultKind::Auth => StoreError::Auth("injected ExpiredToken".into()),
            FaultKind::Denied => StoreError::Denied("injected AccessDenied".into()),
            FaultKind::NoSuchUpload => StoreError::NoSuchUpload,
            FaultKind::Transient => StoreError::Transient("injected transient".into()),
            FaultKind::Fatal => StoreError::Fatal("injected fatal".into()),
        }
    }
}

#[cfg(test)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Call {
    Put {
        key: String,
        len: usize,
    },
    Create {
        key: String,
    },
    List {
        key: String,
        upload_id: String,
    },
    Part {
        key: String,
        upload_id: String,
        number: i32,
    },
    Complete {
        key: String,
        upload_id: String,
        parts: Vec<i32>,
    },
    Abort {
        key: String,
        upload_id: String,
    },
    Note(String),
}

#[cfg(test)]
struct FakeUpload {
    key: String,
    meta: Meta,
    parts: BTreeMap<i32, (Bytes, String)>,
}

#[cfg(test)]
struct Fault {
    op: Op,
    part: Option<i32>,
    kind: FaultKind,
    remaining: u32,
}

#[cfg(test)]
#[derive(Default)]
struct FakeState {
    objects: HashMap<String, (Bytes, Meta)>,
    uploads: HashMap<String, FakeUpload>,
    next_id: u32,
    next_etag: u32,
    calls: Vec<Call>,
    part_starts: Vec<(i32, Instant)>,
    faults: Vec<Fault>,
    held_parts: HashSet<i32>,
}

#[cfg(test)]
impl FakeState {
    fn take_fault(&mut self, op: Op, part: Option<i32>) -> Option<StoreError> {
        let fault = self
            .faults
            .iter_mut()
            .find(|f| f.op == op && f.remaining > 0 && (f.part.is_none() || f.part == part))?;
        fault.remaining -= 1;
        Some(fault.kind.error())
    }
}

/// In-memory part store: injectable faults, a call log, and held parts that block until released.
#[cfg(test)]
pub(crate) struct FakePartStore {
    state: Mutex<FakeState>,
    released: watch::Sender<bool>,
    in_flight: watch::Sender<usize>,
    started: watch::Sender<usize>,
}

#[cfg(test)]
struct InFlightGuard<'a>(&'a watch::Sender<usize>);

#[cfg(test)]
impl Drop for InFlightGuard<'_> {
    fn drop(&mut self) {
        self.0.send_modify(|n| *n -= 1);
    }
}

#[cfg(test)]
impl FakePartStore {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(FakeState::default()),
            released: watch::Sender::new(false),
            in_flight: watch::Sender::new(0),
            started: watch::Sender::new(0),
        })
    }

    pub(crate) fn fail(&self, op: Op, part: Option<i32>, kind: FaultKind, times: u32) {
        self.state.lock().faults.push(Fault {
            op,
            part,
            kind,
            remaining: times,
        });
    }

    pub(crate) fn hold_parts(&self, numbers: &[i32]) {
        self.released.send_replace(false);
        self.state.lock().held_parts.extend(numbers);
    }

    pub(crate) fn release(&self) {
        self.released.send_replace(true);
    }

    pub(crate) fn seed_upload(&self, key: &str, parts: Vec<(i32, Bytes)>) -> String {
        let mut state = self.state.lock();
        state.next_id += 1;
        let id = format!("upload-{}", state.next_id);
        let mut stored = BTreeMap::new();
        for (number, body) in parts {
            state.next_etag += 1;
            stored.insert(number, (body, format!("\"etag-{}\"", state.next_etag)));
        }
        state.uploads.insert(
            id.clone(),
            FakeUpload {
                key: key.to_owned(),
                meta: Meta::default(),
                parts: stored,
            },
        );
        id
    }

    pub(crate) fn note(&self, text: impl Into<String>) {
        self.state.lock().calls.push(Call::Note(text.into()));
    }

    pub(crate) fn calls(&self) -> Vec<Call> {
        self.state.lock().calls.clone()
    }

    pub(crate) fn part_starts(&self) -> Vec<(i32, Instant)> {
        self.state.lock().part_starts.clone()
    }

    pub(crate) fn object(&self, bucket: &str, key: &str) -> Option<(Bytes, Meta)> {
        self.state
            .lock()
            .objects
            .get(&format!("{bucket}/{key}"))
            .cloned()
    }

    pub(crate) fn upload_parts(&self, upload_id: &str) -> Option<Vec<i32>> {
        let state = self.state.lock();
        let upload = state.uploads.get(upload_id)?;
        Some(upload.parts.keys().copied().collect())
    }

    pub(crate) fn in_flight(&self) -> usize {
        *self.in_flight.borrow()
    }

    pub(crate) async fn wait_started(&self, count: usize) {
        let mut rx = self.started.subscribe();
        let _ = rx.wait_for(|n| *n >= count).await;
    }

    pub(crate) async fn wait_idle(&self) {
        let mut rx = self.in_flight.subscribe();
        let _ = rx.wait_for(|n| *n == 0).await;
    }
}

#[cfg(test)]
#[async_trait]
impl PartStore for FakePartStore {
    async fn put(
        &self,
        bucket: &str,
        key: &str,
        body: Bytes,
        meta: &Meta,
    ) -> Result<(), StoreError> {
        let mut state = self.state.lock();
        state.calls.push(Call::Put {
            key: key.to_owned(),
            len: body.len(),
        });
        if let Some(e) = state.take_fault(Op::Put, None) {
            return Err(e);
        }
        state
            .objects
            .insert(format!("{bucket}/{key}"), (body, meta.clone()));
        Ok(())
    }

    async fn create_mpu(&self, _: &str, key: &str, meta: &Meta) -> Result<String, StoreError> {
        let mut state = self.state.lock();
        state.calls.push(Call::Create {
            key: key.to_owned(),
        });
        if let Some(e) = state.take_fault(Op::Create, None) {
            return Err(e);
        }
        state.next_id += 1;
        let id = format!("upload-{}", state.next_id);
        state.uploads.insert(
            id.clone(),
            FakeUpload {
                key: key.to_owned(),
                meta: meta.clone(),
                parts: BTreeMap::new(),
            },
        );
        Ok(id)
    }

    async fn list_parts(
        &self,
        _: &str,
        key: &str,
        upload_id: &str,
    ) -> Result<Vec<PartInfo>, StoreError> {
        let mut state = self.state.lock();
        state.calls.push(Call::List {
            key: key.to_owned(),
            upload_id: upload_id.to_owned(),
        });
        if let Some(e) = state.take_fault(Op::List, None) {
            return Err(e);
        }
        let upload = state
            .uploads
            .get(upload_id)
            .ok_or(StoreError::NoSuchUpload)?;
        Ok(upload
            .parts
            .iter()
            .map(|(number, (body, e_tag))| PartInfo {
                number: *number,
                size: body.len() as u64,
                e_tag: e_tag.clone(),
                checksum_crc32: None,
            })
            .collect())
    }

    async fn upload_part(
        &self,
        _: &str,
        key: &str,
        upload_id: &str,
        number: i32,
        body: Bytes,
    ) -> Result<(), StoreError> {
        let held = {
            let mut state = self.state.lock();
            state.calls.push(Call::Part {
                key: key.to_owned(),
                upload_id: upload_id.to_owned(),
                number,
            });
            state.part_starts.push((number, Instant::now()));
            if let Some(e) = state.take_fault(Op::Part, Some(number)) {
                return Err(e);
            }
            state.held_parts.contains(&number)
        };
        self.in_flight.send_modify(|n| *n += 1);
        let _in_flight = InFlightGuard(&self.in_flight);
        self.started.send_modify(|n| *n += 1);
        if held {
            let mut rx = self.released.subscribe();
            let _ = rx.wait_for(|released| *released).await;
        }
        let mut state = self.state.lock();
        state.next_etag += 1;
        let e_tag = format!("\"etag-{}\"", state.next_etag);
        let upload = state
            .uploads
            .get_mut(upload_id)
            .ok_or(StoreError::NoSuchUpload)?;
        upload.parts.insert(number, (body, e_tag));
        Ok(())
    }

    async fn complete(
        &self,
        bucket: &str,
        key: &str,
        upload_id: &str,
        parts: Vec<PartInfo>,
    ) -> Result<(), StoreError> {
        let mut state = self.state.lock();
        state.calls.push(Call::Complete {
            key: key.to_owned(),
            upload_id: upload_id.to_owned(),
            parts: parts.iter().map(|p| p.number).collect(),
        });
        if let Some(e) = state.take_fault(Op::Complete, None) {
            return Err(e);
        }
        let upload = state
            .uploads
            .remove(upload_id)
            .ok_or(StoreError::NoSuchUpload)?;
        let mut body = Vec::new();
        for (i, part) in parts.iter().enumerate() {
            let stored = upload.parts.get(&part.number);
            let in_order = part.number == i as i32 + 1;
            match stored {
                Some((bytes, e_tag)) if in_order && *e_tag == part.e_tag => {
                    body.extend_from_slice(bytes)
                }
                _ => return Err(StoreError::Fatal(format!("InvalidPart {}", part.number))),
            }
        }
        assert_eq!(upload.key, key);
        state
            .objects
            .insert(format!("{bucket}/{key}"), (Bytes::from(body), upload.meta));
        Ok(())
    }

    async fn abort(&self, _: &str, key: &str, upload_id: &str) -> Result<(), StoreError> {
        let mut state = self.state.lock();
        state.calls.push(Call::Abort {
            key: key.to_owned(),
            upload_id: upload_id.to_owned(),
        });
        if let Some(e) = state.take_fault(Op::Abort, None) {
            return Err(e);
        }
        state
            .uploads
            .remove(upload_id)
            .map(|_| ())
            .ok_or(StoreError::NoSuchUpload)
    }
}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::time::SystemTime;

    use chrono::{DateTime, NaiveDate, TimeZone, Utc};
    use tempfile::TempDir;
    use tokio::task::JoinHandle;

    use super::*;
    use crate::capture::key::object_key;
    use crate::capture::ledger::mtime_ms;

    const GUARD: Duration = Duration::from_secs(10);
    const BUCKET: &str = "bucket";

    fn content(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i % 251) as u8).collect()
    }

    struct Fixture {
        _dir: TempDir,
        path: PathBuf,
        data: Vec<u8>,
        store: Arc<FakePartStore>,
        pause: watch::Sender<PauseLevel>,
        pacer: Arc<Pacer>,
        cancel: CancellationToken,
        meta: Meta,
    }

    impl Fixture {
        fn new(len: usize) -> Self {
            let dir = TempDir::new().unwrap();
            let path = dir.path().join("FileTrace_1.utrace");
            let data = content(len);
            std::fs::write(&path, &data).unwrap();
            let mut meta = Meta::default();
            meta.metadata.insert("sha256".into(), "ff".into());
            Self {
                _dir: dir,
                path,
                data,
                store: FakePartStore::new(),
                pause: watch::Sender::new(PauseLevel::None),
                pacer: Arc::new(Pacer::new(None)),
                cancel: CancellationToken::new(),
                meta,
            }
        }

        fn row(&self, part_size: u64) -> InflightUpload {
            self.row_with_key(
                part_size,
                "p/2026-10-08/user/abc_user_20261008T000000Z_FileTrace_1.utrace",
            )
        }

        fn row_with_key(&self, part_size: u64, key: &str) -> InflightUpload {
            let meta = std::fs::metadata(&self.path).unwrap();
            InflightUpload {
                session_id: "s".into(),
                local_path: self.path.clone(),
                size: meta.len(),
                mtime_ms: mtime_ms(&meta).unwrap(),
                sha256: "ff".into(),
                bucket: BUCKET.into(),
                key: key.into(),
                upload_id: None,
                part_size,
                started_at: Utc::now(),
                attempts: 0,
                next_attempt_at: None,
            }
        }

        fn chunk(&self, start: usize, end: usize) -> Bytes {
            Bytes::copy_from_slice(&self.data[start..end])
        }

        async fn run(
            &self,
            row: &mut InflightUpload,
            concurrency: usize,
        ) -> Result<UploadOutcome, StoreError> {
            let store = self.store.clone();
            let spec = UploadSpec {
                path: &self.path,
                meta: &self.meta,
                part_concurrency: concurrency,
            };
            upload_file(
                self.store.clone(),
                row,
                &spec,
                self.pause.subscribe(),
                self.pacer.clone(),
                &self.cancel,
                move |r| store.note(format!("persist {:?}", r.upload_id)),
            )
            .await
        }

        fn spawn(
            &self,
            mut row: InflightUpload,
            concurrency: usize,
        ) -> JoinHandle<(InflightUpload, Result<UploadOutcome, StoreError>)> {
            let store = self.store.clone();
            let path = self.path.clone();
            let meta = self.meta.clone();
            let pause_rx = self.pause.subscribe();
            let pacer = self.pacer.clone();
            let cancel = self.cancel.clone();
            tokio::spawn(async move {
                let spec = UploadSpec {
                    path: &path,
                    meta: &meta,
                    part_concurrency: concurrency,
                };
                let log = store.clone();
                let result =
                    upload_file(store, &mut row, &spec, pause_rx, pacer, &cancel, move |r| {
                        log.note(format!("persist {:?}", r.upload_id))
                    })
                    .await;
                (row, result)
            })
        }

        fn uploaded(&self, key: &str) -> Bytes {
            self.store.object(BUCKET, key).expect("object stored").0
        }

        fn part_calls(&self) -> Vec<i32> {
            self.store
                .calls()
                .into_iter()
                .filter_map(|c| match c {
                    Call::Part { number, .. } => Some(number),
                    _ => None,
                })
                .collect()
        }

        fn count(&self, pred: impl Fn(&Call) -> bool) -> usize {
            self.store.calls().iter().filter(|c| pred(c)).count()
        }
    }

    fn sorted(mut v: Vec<i32>) -> Vec<i32> {
        v.sort_unstable();
        v
    }

    async fn settle() {
        for _ in 0..50 {
            tokio::task::yield_now().await;
        }
    }

    // --- part sizing ---

    #[test]
    fn part_size_uses_configured_size_for_small_files() {
        assert_eq!(part_size(100 * MIB, 32, false), 32 * MIB);
        assert_eq!(part_size(1, 32, false), 32 * MIB);
    }

    #[test]
    fn part_size_caps_base_at_8_mib_when_throttled() {
        assert_eq!(part_size(100 * MIB, 32, true), 8 * MIB);
        assert_eq!(part_size(100 * MIB, 4, true), 4 * MIB);
    }

    #[test]
    fn part_size_applies_9000_part_floor_rounded_to_mib() {
        let size = 9_000 * 32 * MIB + 1;
        let ps = part_size(size, 32, false);
        assert_eq!(ps, 33 * MIB);
        assert_eq!(ps % MIB, 0);
        assert!(size.div_ceil(ps) <= 9_000);

        let size = 100_000_000_000;
        let ps = part_size(size, 8, true);
        assert_eq!(ps % MIB, 0);
        assert!(ps >= size.div_ceil(9_000));
        assert!(ps - size.div_ceil(9_000) < MIB);
    }

    #[test]
    fn part_size_never_exceeds_5_gib() {
        assert_eq!(part_size(u64::MAX / 2, 32, false), MAX_PART_SIZE);
        assert_eq!(part_size(1, u64::MAX, false), MAX_PART_SIZE);
    }

    #[test]
    fn part_size_110_gb_stays_under_10000_parts() {
        let size = 110_000_000_000u64;
        for (mib, throttled) in [(32, false), (8, false), (256, false), (32, true)] {
            let ps = part_size(size, mib, throttled);
            assert!(size.div_ceil(ps) < 10_000, "{mib} {throttled}");
            assert!(Layout::new(size, ps).is_ok());
        }
    }

    #[test]
    fn classify_maps_error_codes() {
        let c = |code: Option<&str>, status| classify_code(code, status, String::new());
        assert!(matches!(c(Some("ExpiredToken"), 400), StoreError::Auth(_)));
        assert!(matches!(
            c(Some("InvalidAccessKeyId"), 403),
            StoreError::Auth(_)
        ));
        assert!(matches!(
            c(Some("RequestExpired"), 400),
            StoreError::Auth(_)
        ));
        assert!(matches!(
            c(Some("AccessDenied"), 403),
            StoreError::Denied(_)
        ));
        assert!(matches!(
            c(Some("NoSuchUpload"), 404),
            StoreError::NoSuchUpload
        ));
        assert!(matches!(c(Some("SlowDown"), 503), StoreError::Transient(_)));
        assert!(matches!(
            c(Some("RequestTimeout"), 400),
            StoreError::Transient(_)
        ));
        assert!(matches!(c(None, 500), StoreError::Transient(_)));
        assert!(matches!(c(None, 429), StoreError::Transient(_)));
        assert!(matches!(
            c(Some("EntityTooLarge"), 400),
            StoreError::Fatal(_)
        ));
        assert!(matches!(c(Some("InvalidPart"), 400), StoreError::Fatal(_)));
    }

    // --- throttle ---

    #[tokio::test(start_paused = true)]
    async fn pacer_spaces_starts_by_bytes_over_rate() {
        let mbps = 8.0;
        let pacer = Pacer::new(Some(mbps));
        let t0 = Instant::now();
        let n = 500_000u64;
        let gap = Duration::from_secs_f64(n as f64 / (mbps * 125_000.0));
        assert_eq!(gap, Duration::from_millis(500));
        assert_eq!(pacer.reserve(n), Some(t0));
        assert_eq!(pacer.reserve(n), Some(t0 + gap));
        assert_eq!(pacer.reserve(2 * n), Some(t0 + 2 * gap));
        assert_eq!(pacer.reserve(n), Some(t0 + 4 * gap));

        pacer.wait(0).await;
        assert_eq!(Instant::now(), t0 + 5 * gap);

        pacer.set_cap(None);
        assert_eq!(pacer.reserve(n), None);
        pacer.set_cap(Some(mbps));
        tokio::time::advance(Duration::from_secs(10)).await;
        let now = Instant::now();
        assert_eq!(pacer.reserve(n), Some(now));
    }

    #[tokio::test(start_paused = true)]
    async fn throttled_driver_spaces_part_starts() {
        let f = Fixture::new(500_000);
        f.pacer.set_cap(Some(1.0));
        let mut row = f.row(125_000);
        let outcome = f.run(&mut row, 1).await.unwrap();
        assert_eq!(outcome, UploadOutcome::Uploaded);
        let starts = f.store.part_starts();
        assert_eq!(starts.len(), 4);
        for pair in starts.windows(2) {
            assert_eq!(pair[1].1 - pair[0].1, Duration::from_secs(1));
        }
    }

    // --- driver ---

    #[tokio::test]
    async fn small_file_uses_single_put() {
        let f = Fixture::new(1_000);
        let mut row = f.row(4_096);
        let outcome = f.run(&mut row, 4).await.unwrap();
        assert_eq!(outcome, UploadOutcome::Uploaded);
        assert_eq!(
            f.store.calls(),
            vec![Call::Put {
                key: row.key.clone(),
                len: 1_000
            }]
        );
        let (body, meta) = f.store.object(BUCKET, &row.key).unwrap();
        assert_eq!(&body[..], &f.data[..]);
        assert_eq!(meta.content_type(), "application/octet-stream");
        assert_eq!(meta.metadata.get("sha256").map(String::as_str), Some("ff"));
        assert!(row.upload_id.is_none());
    }

    #[tokio::test]
    async fn fresh_multipart_persists_after_create_and_completes() {
        let f = Fixture::new(3_500);
        let mut row = f.row(1_000);
        let outcome = f.run(&mut row, 2).await.unwrap();
        assert_eq!(outcome, UploadOutcome::Uploaded);

        let calls = f.store.calls();
        assert_eq!(
            calls[0],
            Call::Create {
                key: row.key.clone()
            }
        );
        assert_eq!(calls[1], Call::Note("persist Some(\"upload-1\")".into()));
        assert!(matches!(calls[2], Call::Part { .. }));
        assert_eq!(sorted(f.part_calls()), vec![1, 2, 3, 4]);
        assert_eq!(
            calls.last().unwrap(),
            &Call::Complete {
                key: row.key.clone(),
                upload_id: "upload-1".into(),
                parts: vec![1, 2, 3, 4]
            }
        );
        assert_eq!(&f.uploaded(&row.key)[..], &f.data[..]);
        assert_eq!(row.upload_id.as_deref(), Some("upload-1"));
    }

    #[tokio::test]
    async fn resume_uploads_only_parts_missing_from_list() {
        let f = Fixture::new(3_500);
        let mut row = f.row(1_000);
        let id = f.store.seed_upload(
            &row.key,
            vec![(1, f.chunk(0, 1_000)), (2, f.chunk(1_000, 2_000))],
        );
        row.upload_id = Some(id.clone());
        let outcome = f.run(&mut row, 4).await.unwrap();
        assert_eq!(outcome, UploadOutcome::Uploaded);
        assert_eq!(f.count(|c| matches!(c, Call::Create { .. })), 0);
        assert_eq!(
            f.store.calls()[0],
            Call::List {
                key: row.key.clone(),
                upload_id: id
            }
        );
        assert_eq!(sorted(f.part_calls()), vec![3, 4]);
        assert_eq!(&f.uploaded(&row.key)[..], &f.data[..]);
    }

    #[tokio::test]
    async fn wrong_size_listed_part_is_reuploaded_and_short_last_part_counts() {
        let f = Fixture::new(3_500);
        let mut row = f.row(1_000);
        let id = f.store.seed_upload(
            &row.key,
            vec![
                (1, f.chunk(0, 1_000)),
                (2, f.chunk(1_000, 1_400)),
                (4, f.chunk(3_000, 3_500)),
            ],
        );
        row.upload_id = Some(id);
        let outcome = f.run(&mut row, 4).await.unwrap();
        assert_eq!(outcome, UploadOutcome::Uploaded);
        assert_eq!(sorted(f.part_calls()), vec![2, 3]);
        assert_eq!(&f.uploaded(&row.key)[..], &f.data[..]);
    }

    #[tokio::test]
    async fn no_such_upload_recreates_mpu_for_same_key() {
        let f = Fixture::new(2_500);
        let mut row = f.row(1_000);
        row.upload_id = Some("expired-by-lifecycle".into());
        let outcome = f.run(&mut row, 2).await.unwrap();
        assert_eq!(outcome, UploadOutcome::Uploaded);
        let calls = f.store.calls();
        assert!(
            matches!(&calls[0], Call::List { upload_id, .. } if upload_id == "expired-by-lifecycle")
        );
        assert_eq!(
            calls[1],
            Call::Create {
                key: row.key.clone()
            }
        );
        assert_eq!(calls[2], Call::Note("persist Some(\"upload-1\")".into()));
        assert_eq!(sorted(f.part_calls()), vec![1, 2, 3]);
        assert_eq!(&f.uploaded(&row.key)[..], &f.data[..]);
        assert_eq!(f.count(|c| matches!(c, Call::Abort { .. })), 0);
    }

    #[tokio::test]
    async fn hard_pause_cancels_in_flight_parts_and_resumes_missing_only() {
        let f = Fixture::new(4_000);
        f.store.hold_parts(&[2, 3, 4]);
        let handle = f.spawn(f.row(1_000), 2);

        tokio::time::timeout(GUARD, f.store.wait_started(3))
            .await
            .unwrap();
        assert_eq!(f.store.in_flight(), 2);
        f.pause.send_replace(PauseLevel::Hard);
        tokio::time::timeout(GUARD, f.store.wait_idle())
            .await
            .unwrap();
        settle().await;
        assert!(!handle.is_finished());
        let upload_id = "upload-1";
        assert_eq!(f.store.upload_parts(upload_id), Some(vec![1]));
        let parts_before_resume = f.part_calls().len();
        assert_eq!(parts_before_resume, 3);

        f.store.release();
        f.pause.send_replace(PauseLevel::None);
        let (row, result) = tokio::time::timeout(GUARD, handle).await.unwrap().unwrap();
        assert_eq!(result.unwrap(), UploadOutcome::Uploaded);

        let parts = f.part_calls();
        assert_eq!(parts.iter().filter(|n| **n == 1).count(), 1);
        assert_eq!(sorted(parts[parts_before_resume..].to_vec()), vec![2, 3, 4]);
        assert_eq!(f.count(|c| matches!(c, Call::Abort { .. })), 0);
        assert_eq!(f.count(|c| matches!(c, Call::Create { .. })), 1);
        assert_eq!(&f.uploaded(&row.key)[..], &f.data[..]);
    }

    #[tokio::test]
    async fn soft_pause_lets_in_flight_part_finish_but_starts_no_new_part() {
        let f = Fixture::new(3_000);
        f.store.hold_parts(&[1]);
        let handle = f.spawn(f.row(1_000), 1);

        tokio::time::timeout(GUARD, f.store.wait_started(1))
            .await
            .unwrap();
        f.pause.send_replace(PauseLevel::Soft);
        f.store.release();
        tokio::time::timeout(GUARD, f.store.wait_idle())
            .await
            .unwrap();
        settle().await;
        assert_eq!(f.store.upload_parts("upload-1"), Some(vec![1]));
        assert_eq!(f.part_calls(), vec![1]);
        assert!(!handle.is_finished());

        f.pause.send_replace(PauseLevel::None);
        let (_, result) = tokio::time::timeout(GUARD, handle).await.unwrap().unwrap();
        assert_eq!(result.unwrap(), UploadOutcome::Uploaded);
        assert_eq!(f.part_calls(), vec![1, 2, 3]);
    }

    #[tokio::test]
    async fn auth_mid_upload_keeps_row_and_resumes_via_list_parts() {
        let f = Fixture::new(4_000);
        f.store.fail(Op::Part, Some(3), FaultKind::Auth, 1);
        let mut row = f.row(1_000);
        let err = f.run(&mut row, 1).await.unwrap_err();
        assert!(matches!(err, StoreError::Auth(_)));
        assert_eq!(row.upload_id.as_deref(), Some("upload-1"));
        assert_eq!(row.attempts, 0);
        assert_eq!(f.store.upload_parts("upload-1"), Some(vec![1, 2]));
        assert_eq!(f.count(|c| matches!(c, Call::Abort { .. })), 0);

        f.pause.send_replace(PauseLevel::Blocked);
        let calls_before = f.store.calls().len();
        let handle = f.spawn(row, 1);
        settle().await;
        assert_eq!(f.store.calls().len(), calls_before);
        assert!(!handle.is_finished());

        f.pause.send_replace(PauseLevel::None);
        let (row, result) = tokio::time::timeout(GUARD, handle).await.unwrap().unwrap();
        assert_eq!(result.unwrap(), UploadOutcome::Uploaded);

        let resumed = f.store.calls()[calls_before..].to_vec();
        assert_eq!(
            resumed[0],
            Call::List {
                key: row.key.clone(),
                upload_id: "upload-1".into()
            }
        );
        let parts: Vec<i32> = resumed
            .iter()
            .filter_map(|c| match c {
                Call::Part { number, .. } => Some(*number),
                _ => None,
            })
            .collect();
        assert_eq!(parts, vec![3, 4]);
        assert_eq!(f.count(|c| matches!(c, Call::Create { .. })), 1);
        assert_eq!(f.count(|c| matches!(c, Call::Abort { .. })), 0);
        assert_eq!(row.upload_id.as_deref(), Some("upload-1"));
        assert_eq!(&f.uploaded(&row.key)[..], &f.data[..]);
    }

    #[tokio::test]
    async fn resume_reuses_persisted_key_across_utc_midnight() {
        let f = Fixture::new(3_000);
        let before_midnight: SystemTime = Utc
            .with_ymd_and_hms(2026, 10, 8, 23, 59, 30)
            .unwrap()
            .into();
        let key = object_key(
            "p",
            NaiveDate::from_ymd_opt(2026, 10, 8).unwrap(),
            "User",
            Some("abcdef"),
            before_midnight,
            &f.path,
        );
        let mut row = f.row_with_key(1_000, &key);
        row.started_at = DateTime::<Utc>::from(before_midnight);
        let id = f.store.seed_upload(&key, vec![(1, f.chunk(0, 1_000))]);
        row.upload_id = Some(id.clone());

        // Persist and reload, as across a restart the next UTC day.
        let mut row: InflightUpload =
            serde_json::from_str(&serde_json::to_string(&row).unwrap()).unwrap();
        let outcome = f.run(&mut row, 2).await.unwrap();
        assert_eq!(outcome, UploadOutcome::Uploaded);

        assert!(key.contains("/2026-10-08/"));
        for call in f.store.calls() {
            match call {
                Call::List { key: k, .. }
                | Call::Part { key: k, .. }
                | Call::Complete { key: k, .. } => assert_eq!(k, key),
                Call::Note(_) => {}
                other => panic!("unexpected call {other:?}"),
            }
        }
        assert_eq!(&f.uploaded(&key)[..], &f.data[..]);
    }

    #[tokio::test]
    async fn file_changed_before_complete_aborts() {
        let f = Fixture::new(3_000);
        f.store.hold_parts(&[3]);
        let handle = f.spawn(f.row(1_000), 1);

        tokio::time::timeout(GUARD, f.store.wait_started(3))
            .await
            .unwrap();
        let file = File::options().write(true).open(&f.path).unwrap();
        file.set_modified(SystemTime::now() + Duration::from_secs(60))
            .unwrap();
        drop(file);
        f.store.release();

        let (row, result) = tokio::time::timeout(GUARD, handle).await.unwrap().unwrap();
        assert_eq!(result.unwrap(), UploadOutcome::FileChanged);
        assert!(row.upload_id.is_none());
        assert_eq!(
            f.store.calls().last().unwrap(),
            &Call::Abort {
                key: row.key.clone(),
                upload_id: "upload-1".into()
            }
        );
        assert_eq!(f.count(|c| matches!(c, Call::Complete { .. })), 0);
        assert!(f.store.upload_parts("upload-1").is_none());
    }

    #[tokio::test]
    async fn cancel_drops_parts_and_aborts() {
        let f = Fixture::new(3_000);
        f.store.hold_parts(&[2]);
        let handle = f.spawn(f.row(1_000), 2);

        tokio::time::timeout(GUARD, f.store.wait_started(2))
            .await
            .unwrap();
        f.cancel.cancel();
        let (row, result) = tokio::time::timeout(GUARD, handle).await.unwrap().unwrap();
        assert_eq!(result.unwrap(), UploadOutcome::Cancelled);
        assert!(row.upload_id.is_none());
        assert_eq!(f.store.in_flight(), 0);
        assert_eq!(f.count(|c| matches!(c, Call::Abort { .. })), 1);
        assert_eq!(f.count(|c| matches!(c, Call::Complete { .. })), 0);
    }

    #[tokio::test]
    async fn denied_on_create_leaves_row_without_upload() {
        let f = Fixture::new(2_000);
        f.store.fail(Op::Create, None, FaultKind::Denied, 1);
        let mut row = f.row(1_000);
        let err = f.run(&mut row, 2).await.unwrap_err();
        assert!(matches!(err, StoreError::Denied(_)));
        assert!(row.upload_id.is_none());
        assert_eq!(
            f.store.calls(),
            vec![Call::Create {
                key: row.key.clone()
            }]
        );
    }

    #[tokio::test]
    async fn no_such_upload_mid_upload_recreates_for_same_key() {
        let f = Fixture::new(3_000);
        f.store.fail(Op::Part, Some(2), FaultKind::NoSuchUpload, 1);
        let mut row = f.row(1_000);
        let outcome = f.run(&mut row, 1).await.unwrap();
        assert_eq!(outcome, UploadOutcome::Uploaded);
        let creates: Vec<Call> = f
            .store
            .calls()
            .into_iter()
            .filter(|c| matches!(c, Call::Create { .. }))
            .collect();
        assert_eq!(
            creates,
            vec![
                Call::Create {
                    key: row.key.clone()
                },
                Call::Create {
                    key: row.key.clone()
                }
            ]
        );
        assert_eq!(row.upload_id.as_deref(), Some("upload-2"));
        assert_eq!(&f.uploaded(&row.key)[..], &f.data[..]);
    }

    #[tokio::test]
    async fn fatal_on_complete_keeps_mpu() {
        let f = Fixture::new(2_000);
        f.store.fail(Op::Complete, None, FaultKind::Fatal, 1);
        let mut row = f.row(1_000);
        let err = f.run(&mut row, 2).await.unwrap_err();
        assert!(matches!(err, StoreError::Fatal(_)));
        assert_eq!(row.upload_id.as_deref(), Some("upload-1"));
        assert_eq!(f.count(|c| matches!(c, Call::Abort { .. })), 0);
        assert_eq!(f.store.upload_parts("upload-1"), Some(vec![1, 2]));
    }

    #[tokio::test]
    async fn transient_part_error_keeps_mpu() {
        let f = Fixture::new(2_000);
        f.store.fail(Op::Part, Some(2), FaultKind::Transient, 1);
        let mut row = f.row(1_000);
        let err = f.run(&mut row, 1).await.unwrap_err();
        assert!(matches!(err, StoreError::Transient(_)));
        assert_eq!(row.upload_id.as_deref(), Some("upload-1"));
        assert_eq!(f.count(|c| matches!(c, Call::Abort { .. })), 0);

        let outcome = f.run(&mut row, 1).await.unwrap();
        assert_eq!(outcome, UploadOutcome::Uploaded);
        assert_eq!(f.part_calls(), vec![1, 2, 2]);
    }
}
