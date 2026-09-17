//! Downloading build artifacts from the longtail store.
//!
//! This drives the `longtail` library directly. It used to drive a `longtail` executable
//! that was downloaded at runtime, and most of what is gone with that was there to cope
//! with the subprocess rather than with longtail: parsing a progress bar out of stdout to
//! find errors in it, guessing from those strings whether a failure was an expired
//! credential, and retrying blind - first deleting the target directory, then the cache -
//! because there was no way to tell what had actually gone wrong.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;

use longtail::{ErrorClass, GetOptions, LongtailError, Progress, ProgressSink};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tracing::{info, instrument, warn};

use super::fs::LocalDownloadPath;
use crate::clients::aws::AWSClient;

/// Re-exported so callers can hold a token without depending on `longtail` or
/// `tokio-util` directly, and without coupling to their versions.
pub use longtail::CancellationToken;
pub use longtail::TARGET_INDEX_CACHE_NAME;

/// How many times a download may be restarted after its credentials were rejected.
///
/// The provider refreshes credentials underneath a running transfer, so this only covers
/// the window between a session lapsing and the app renewing it. A restart resumes from
/// the block cache, so the cost is a target rescan rather than a re-download.
const MAX_UNAUTHORIZED_RETRIES: usize = 3;

/// How long to wait before retrying a download the store would not authorise.
///
/// Long enough for a renewal that is already in flight to land, short enough that a user
/// watching a progress bar does not think it has hung.
const UNAUTHORIZED_RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(5);

/// Leave a couple of cores for the UI and for whatever the user is running - most
/// likely the Unreal editor, which is why they are downloading anything.
fn chunk_worker_count() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get().saturating_sub(2).max(1))
        .unwrap_or(1)
}

/// Matches longtail's own default for an S3 store. Set explicitly so a local-path store
/// in a test behaves the way the real one does.
const BLOCK_WORKER_COUNT: usize = 8;

/// Which artifact a download, and so a cancellation, belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SyncKind {
    Client,
    Engine,
    EditorDlls,
}

impl SyncKind {
    /// The form used on the wire - matches the serde representation, so it is safe in a
    /// URL path. [`Display`](std::fmt::Display) is the human-readable form, for logs.
    pub fn as_str(&self) -> &'static str {
        match self {
            SyncKind::Client => "client",
            SyncKind::Engine => "engine",
            SyncKind::EditorDlls => "editorDlls",
        }
    }
}

impl std::fmt::Display for SyncKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            SyncKind::Client => "client",
            SyncKind::Engine => "engine",
            SyncKind::EditorDlls => "editor DLLs",
        };
        f.write_str(name)
    }
}

/// The cancellation tokens for whatever downloads are currently in flight.
///
/// One token per download, not one shared token. A [`CancellationToken`] never
/// un-cancels, so a single shared one would mean the first cancel left every later
/// download cancelled until the app restarted - and it could not express "stop the
/// engine update but leave the client sync running", which is the point.
///
/// The root exists so shutdown can stop everything at once: cancelling it cancels every
/// child, including downloads started after it.
#[derive(Debug, Clone)]
pub struct DownloadCancellation {
    root: CancellationToken,
    in_flight: Arc<Mutex<HashMap<SyncKind, Registered>>>,
    /// Distinguishes one download of a kind from the next, so a finishing download can
    /// only ever clear its own registration.
    next_id: Arc<AtomicU64>,
}

#[derive(Debug, Clone)]
struct Registered {
    id: u64,
    token: CancellationToken,
}

/// A download's registration, for as long as it runs.
///
/// Clearing the registration on drop rather than at a call site: an axum handler whose
/// future is dropped - the client disconnected, the runtime shut down - would otherwise
/// leave its kind registered forever, and since starting is now refused while one is
/// registered, that would block every later download of that kind until a restart.
#[derive(Debug)]
pub struct DownloadGuard {
    cancellation: DownloadCancellation,
    kind: SyncKind,
    id: u64,
    token: CancellationToken,
}

impl DownloadGuard {
    pub fn token(&self) -> CancellationToken {
        self.token.clone()
    }
}

impl Drop for DownloadGuard {
    fn drop(&mut self) {
        let mut in_flight = self.cancellation.in_flight.lock();
        // Only if it is still ours. A cancel removes the entry, and the next download of
        // the same kind may have registered before this guard was dropped.
        if in_flight
            .get(&self.kind)
            .is_some_and(|held| held.id == self.id)
        {
            in_flight.remove(&self.kind);
        }
    }
}

impl Default for DownloadCancellation {
    fn default() -> Self {
        Self::new()
    }
}

impl DownloadCancellation {
    pub fn new() -> Self {
        DownloadCancellation {
            root: CancellationToken::new(),
            in_flight: Arc::new(Mutex::new(HashMap::new())),
            next_id: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Register a download of `kind`, or refuse if one is already running.
    ///
    /// Refusing rather than replacing. Two downloads of a kind write to the same
    /// directory, and replacing dropped the first one's token on the floor - so it could
    /// no longer be cancelled or shut down, while both carried on writing the same files.
    /// That is reachable because the client sync runs in its HTTP handler rather than on
    /// the serialized worker queue, so it can overlap a download already in progress.
    pub fn begin(&self, kind: SyncKind) -> Option<DownloadGuard> {
        let mut in_flight = self.in_flight.lock();
        if in_flight.contains_key(&kind) {
            return None;
        }

        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let token = self.root.child_token();
        in_flight.insert(
            kind,
            Registered {
                id,
                token: token.clone(),
            },
        );

        Some(DownloadGuard {
            cancellation: self.clone(),
            kind,
            id,
            token,
        })
    }

    /// Cancel one download. Returns whether there was one to cancel.
    ///
    /// The registration stays until the download actually stops. Cancelling is not
    /// immediate - longtail finishes the blocks already in flight, then flushes and
    /// closes the store, which includes sweeping the cache down to its budget. Freeing
    /// the slot at the moment of cancelling would let the next download start into the
    /// same target and the same cache while the previous one is still writing, which is
    /// what refusing a second download exists to prevent. The guard clears it when the
    /// download has genuinely finished.
    ///
    /// Leaves every other in-flight download running, and leaves the root untouched so
    /// later downloads still start uncancelled.
    pub fn cancel(&self, kind: SyncKind) -> bool {
        match self.in_flight.lock().get(&kind) {
            Some(held) => {
                held.token.cancel();
                true
            }
            None => false,
        }
    }

    /// Cancel everything in flight, and anything started afterwards. Shutdown only.
    pub fn cancel_all(&self) {
        self.root.cancel();
        self.in_flight.lock().clear();
    }

    /// Whether a download of this kind is registered. For a caller that wants to report
    /// "already running" rather than simply failing to start.
    pub fn is_running(&self, kind: SyncKind) -> bool {
        self.in_flight.lock().contains_key(&kind)
    }
}

/// What a caller should *do* about a failure, mirroring [`longtail::ErrorClass`].
///
/// Mirrored rather than re-exported because this crosses to the frontend as a wire type:
/// `ErrorClass` is `#[non_exhaustive]`, and a new variant appearing there should not
/// silently widen what the UI has to handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SyncErrorClass {
    Cancelled,
    NotFound,
    Unauthorized,
    Transient,
    InvalidInput,
    Corrupt,
    Io,
    Internal,
}

impl From<ErrorClass> for SyncErrorClass {
    fn from(class: ErrorClass) -> Self {
        match class {
            ErrorClass::Cancelled => SyncErrorClass::Cancelled,
            ErrorClass::NotFound => SyncErrorClass::NotFound,
            ErrorClass::Unauthorized => SyncErrorClass::Unauthorized,
            ErrorClass::Transient => SyncErrorClass::Transient,
            ErrorClass::InvalidInput => SyncErrorClass::InvalidInput,
            ErrorClass::Corrupt => SyncErrorClass::Corrupt,
            ErrorClass::Io => SyncErrorClass::Io,
            _ => SyncErrorClass::Internal,
        }
    }
}

/// A failed download, in the two parts the UI wants: something to show, and the cause
/// chain to put behind a disclosure.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncError {
    pub class: SyncErrorClass,
    pub summary: String,
    pub detail: String,
}

impl SyncError {
    fn from_longtail(kind: SyncKind, error: &LongtailError) -> Self {
        let class = SyncErrorClass::from(error.class());
        let summary = match class {
            SyncErrorClass::Cancelled => format!("The {kind} download was cancelled."),
            SyncErrorClass::Unauthorized => format!(
                "Your session expired while downloading the {kind}. Sign in again and retry - \
                 cached data is kept, so it resumes rather than starting over."
            ),
            SyncErrorClass::NotFound => format!(
                "The {kind} build could not be found in storage. It may have been cleaned up."
            ),
            SyncErrorClass::Transient => {
                format!("The {kind} download hit a network problem. Retrying usually works.")
            }
            SyncErrorClass::Corrupt => format!(
                "Downloaded {kind} data did not match what the build says it should be. \
                 Retrying will re-fetch it."
            ),
            SyncErrorClass::Io => format!(
                "Writing the {kind} to disk failed. Check for free space and that no other \
                 program has the files open."
            ),
            SyncErrorClass::InvalidInput => {
                format!("The {kind} download was asked for something it cannot do.")
            }
            SyncErrorClass::Internal => format!("The {kind} download failed unexpectedly."),
        };

        SyncError {
            class,
            summary,
            // Not `to_string()`: a LongtailError's own Display is only a category, and the
            // part that says what happened hangs off its source chain.
            detail: error.full_chain(),
        }
    }

    /// Convert for an HTTP handler.
    ///
    /// Deliberately a method rather than `From`: `CoreError` has a blanket conversion
    /// from anything that is an error, which would swallow this one - and with it the
    /// distinction between "sign in again" and "something broke".
    pub fn into_core_error(self) -> crate::types::errors::CoreError {
        match self.class {
            SyncErrorClass::Unauthorized => crate::types::errors::CoreError::Unauthorized,
            _ => crate::types::errors::CoreError::Internal(anyhow::anyhow!("{self}")),
        }
    }
}

impl std::fmt::Display for SyncError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.summary, self.detail)
    }
}

impl std::error::Error for SyncError {}

/// How far along a download is, in two independent dimensions.
///
/// A `total` of zero means that dimension is not known yet - longtail reports some phases
/// without a total - and the UI shows an indeterminate bar rather than nought percent.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncProgress {
    pub done_bytes: u64,
    pub total_bytes: u64,
    pub done_items: u64,
    pub total_items: u64,
    /// The phase longtail is in, e.g. "Updating version". Distinct from the coarse
    /// `sync-phase` messages ethos emits for its own steps.
    pub phase: String,
}

/// What a finished download did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncSummary {
    pub bytes_written: u64,
    pub assets_written: u32,
    pub assets_removed: u32,
    pub blocks_fetched: u64,
}

/// Everything a download reports, tagged with which download it came from so a caller
/// watching several at once can tell them apart.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum SyncEvent {
    Started {
        kind: SyncKind,
    },
    Progress {
        kind: SyncKind,
        progress: SyncProgress,
    },
    Finished {
        kind: SyncKind,
        summary: SyncSummary,
    },
    Failed {
        kind: SyncKind,
        error: SyncError,
    },
    Cancelled {
        kind: SyncKind,
    },
}

/// Forwards longtail's progress onto the event channel.
///
/// Must stay cheap and non-blocking: longtail calls this from its async task and from
/// rayon workers. The channel is unbounded for the same reason - a bounded one would
/// park a worker - and a send failure is ignored, since a closed receiver only means
/// nobody is watching any more, which is not a reason to fail a download.
struct ChannelProgress {
    kind: SyncKind,
    tx: Sender<SyncEvent>,
    phase: Mutex<String>,
}

impl ChannelProgress {
    fn new(kind: SyncKind, tx: Sender<SyncEvent>) -> Self {
        ChannelProgress {
            kind,
            tx,
            phase: Mutex::new(String::new()),
        }
    }
}

impl ProgressSink for ChannelProgress {
    fn on_progress(&self, p: Progress) {
        let _ = self.tx.send(SyncEvent::Progress {
            kind: self.kind,
            progress: SyncProgress {
                done_bytes: p.done_bytes,
                total_bytes: p.total_bytes,
                done_items: p.done_items,
                total_items: p.total_items,
                phase: self.phase.lock().clone(),
            },
        });
    }

    fn on_phase(&self, phase: &str) {
        *self.phase.lock() = phase.to_string();

        // Emit immediately so the UI shows the new phase before the first sample of it
        // arrives; the zero totals leave the bar indeterminate in the meantime.
        let _ = self.tx.send(SyncEvent::Progress {
            kind: self.kind,
            progress: SyncProgress {
                phase: phase.to_string(),
                ..Default::default()
            },
        });
    }
}

/// A local block cache and the budget it is held to.
pub struct CacheControl {
    pub path: PathBuf,
    pub max_size_bytes: u64,
}

/// Where downloaded artifacts and their caches live.
#[derive(Debug, Clone)]
pub struct ArtifactSync {
    pub download_path: LocalDownloadPath,
}

/// One sync, described.
pub struct SyncRequest<'a> {
    pub kind: SyncKind,
    /// Where the artifact goes.
    pub target: &'a Path,
    /// Get-config URIs. Several are merged, which is how a build and its symbols arrive
    /// together; they must share a storage URI.
    pub archives: &'a [String],
    pub cache: Option<CacheControl>,
    /// Whether longtail may leave its scan of the target cached in the target.
    ///
    /// Off where the target is copied somewhere else afterwards, so the index does not
    /// travel with it.
    pub cache_target_index: bool,
    pub transfer_acceleration: bool,
}

impl<'a> SyncRequest<'a> {
    pub fn download(kind: SyncKind, target: &'a Path, archives: &'a [String]) -> Self {
        SyncRequest {
            kind,
            target,
            archives,
            cache: None,
            cache_target_index: true,
            transfer_acceleration: true,
        }
    }

    pub fn with_cache(mut self, cache: Option<CacheControl>) -> Self {
        self.cache = cache;
        self
    }

    pub fn with_transfer_acceleration(mut self, enabled: bool) -> Self {
        self.transfer_acceleration = enabled;
        self
    }

    /// Keep longtail's target index out of the target, for a target that is copied
    /// elsewhere afterwards.
    pub fn without_target_index(mut self) -> Self {
        self.cache_target_index = false;
        self
    }
}

impl ArtifactSync {
    pub fn new(app_name: &str) -> Self {
        ArtifactSync {
            download_path: LocalDownloadPath::new(app_name),
        }
    }

    /// Run one sync.
    ///
    /// The archives are longtail get-config JSONs; passing several merges them, which is
    /// how a build and its symbols arrive together. They must share a storage URI, which
    /// the build pipeline guarantees by uploading both to the same store.
    #[instrument(skip(self, request, tx, aws_client, cancel), fields(kind = %request.kind))]
    pub async fn get_archive(
        &self,
        request: SyncRequest<'_>,
        tx: Sender<SyncEvent>,
        aws_client: &AWSClient,
        cancel: CancellationToken,
    ) -> Result<SyncSummary, SyncError> {
        let kind = request.kind;
        info!(
            "Downloading {kind} archives {:?} to {:?}",
            request.archives, request.target
        );

        let Some(target) = request.target.to_str() else {
            return Err(SyncError {
                class: SyncErrorClass::InvalidInput,
                summary: format!("The {kind} download path cannot be used."),
                detail: format!("target path is not valid UTF-8: {:?}", request.target),
            });
        };

        let _ = tx.send(SyncEvent::Started { kind });

        let mut attempt = 0;
        let result = loop {
            let error = match self
                .run_once(&request, target, &tx, aws_client, cancel.clone())
                .await
            {
                Ok(summary) => break Ok(summary),
                Err(error) => error,
            };

            // The SDK refreshes credentials underneath a running transfer, so reaching
            // here means the session lapsed and had not been renewed yet. Retrying picks
            // up whatever the app has now and resumes from the cache. It is deliberately
            // the only retry: every other class either cannot be fixed by repeating the
            // request, or is already retried inside longtail.
            if error.class() != ErrorClass::Unauthorized || attempt >= MAX_UNAUTHORIZED_RETRIES {
                break Err(SyncError::from_longtail(kind, &error));
            }

            attempt += 1;

            // Wait before trying again. The session lapsed and the app has not renewed it
            // yet; going straight back would ask the same dead session three times inside
            // a millisecond and cost a target rescan for each. Cancelling during the wait
            // stops here rather than at the next checkpoint, so cancelling is as prompt
            // as it says it is.
            tokio::select! {
                _ = tokio::time::sleep(UNAUTHORIZED_RETRY_DELAY) => {}
                _ = cancel.cancelled() => {
                    break Err(SyncError {
                        class: SyncErrorClass::Cancelled,
                        summary: format!("The {kind} download was cancelled."),
                        detail: "cancelled while waiting to retry an expired session"
                            .to_string(),
                    });
                }
            }

            warn!(
                "{kind} download was not authorized (attempt {attempt} of \
                 {MAX_UNAUTHORIZED_RETRIES}); retrying with whatever session is current. \
                 Cached blocks are kept, so it resumes. Cause: {}",
                error.full_chain()
            );
        };

        match &result {
            Ok(summary) => {
                let _ = tx.send(SyncEvent::Finished {
                    kind,
                    summary: summary.clone(),
                });
            }
            Err(error) if error.class == SyncErrorClass::Cancelled => {
                let _ = tx.send(SyncEvent::Cancelled { kind });
            }
            Err(error) => {
                let _ = tx.send(SyncEvent::Failed {
                    kind,
                    error: error.clone(),
                });
            }
        }

        result
    }

    async fn run_once(
        &self,
        request: &SyncRequest<'_>,
        target: &str,
        tx: &Sender<SyncEvent>,
        aws_client: &AWSClient,
        cancel: CancellationToken,
    ) -> Result<SyncSummary, LongtailError> {
        // GetOptions is #[non_exhaustive]: it has to be built through its constructor and
        // then adjusted, so that new options can land upstream without breaking this.
        let mut options = GetOptions::new(request.archives.to_vec(), target);
        options.progress = Some(Arc::new(ChannelProgress::new(request.kind, tx.clone())));
        options.cancel = Some(cancel);
        options.worker_count = chunk_worker_count();
        options.remote_worker_count = BLOCK_WORKER_COUNT;
        options.cache_target_index = request.cache_target_index;
        options.s3_options = aws_client.longtail_s3_options(request.transfer_acceleration);

        if let Some(cache) = request.cache.as_ref() {
            options.cache_path = Some(cache.path.clone());
            // The whole of cache maintenance: longtail stamps each block on access and
            // evicts least-recently-used down to this when the store closes.
            options.cache_size_limit = Some(cache.max_size_bytes);
        }

        let report = longtail::get(options).await?;

        Ok(SyncSummary {
            bytes_written: report.bytes_written,
            assets_written: report.assets_written,
            assets_removed: report.assets_removed,
            blocks_fetched: report.blocks_fetched,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The trap a single shared token would fall into: `CancellationToken` never
    /// un-cancels, so cancelling one download must not poison the next.
    #[test]
    fn cancelling_a_download_leaves_the_next_one_runnable() {
        let downloads = DownloadCancellation::new();

        let first = downloads.begin(SyncKind::Client).expect("nothing running");
        assert!(downloads.cancel(SyncKind::Client));
        assert!(first.token().is_cancelled());
        drop(first);

        let second = downloads
            .begin(SyncKind::Client)
            .expect("the last one ended");
        assert!(
            !second.token().is_cancelled(),
            "a new download starts uncancelled"
        );
    }

    /// The reason there is a registry at all rather than one token.
    #[test]
    fn cancelling_one_kind_leaves_the_others_running() {
        let downloads = DownloadCancellation::new();

        let client = downloads.begin(SyncKind::Client).unwrap();
        let engine = downloads.begin(SyncKind::Engine).unwrap();
        let dlls = downloads.begin(SyncKind::EditorDlls).unwrap();

        downloads.cancel(SyncKind::Engine);

        assert!(engine.token().is_cancelled());
        assert!(!client.token().is_cancelled());
        assert!(!dlls.token().is_cancelled());
    }

    /// Shutdown has to stop everything, including anything that starts while it happens.
    #[test]
    fn cancel_all_stops_every_kind_and_everything_after() {
        let downloads = DownloadCancellation::new();

        let client = downloads.begin(SyncKind::Client).unwrap();
        let engine = downloads.begin(SyncKind::Engine).unwrap();

        downloads.cancel_all();

        assert!(client.token().is_cancelled());
        assert!(engine.token().is_cancelled());
        assert!(
            downloads
                .begin(SyncKind::EditorDlls)
                .unwrap()
                .token()
                .is_cancelled(),
            "a download racing shutdown must not start uncancelled"
        );
    }

    #[test]
    fn cancelling_a_download_that_is_not_running_reports_nothing_to_do() {
        let downloads = DownloadCancellation::new();

        assert!(!downloads.cancel(SyncKind::Client));

        drop(downloads.begin(SyncKind::Client));
        assert!(!downloads.cancel(SyncKind::Client));
    }

    /// Clones share the registry - ops are handed a clone, and cancelling through one
    /// has to reach a download begun through another.
    #[test]
    fn clones_share_the_registry() {
        let downloads = DownloadCancellation::new();
        let handed_to_an_op = downloads.clone();

        let guard = handed_to_an_op.begin(SyncKind::Engine).unwrap();
        assert!(downloads.cancel(SyncKind::Engine));
        assert!(guard.token().is_cancelled());
    }

    /// Two downloads of one kind write to the same directory. The old behaviour replaced
    /// the first one's registration, so it could no longer be cancelled while both kept
    /// writing - reachable because the client sync runs in its handler rather than on the
    /// serialized worker queue.
    #[test]
    fn a_second_download_of_the_same_kind_is_refused() {
        let downloads = DownloadCancellation::new();

        let first = downloads.begin(SyncKind::Engine).expect("nothing running");
        assert!(downloads.begin(SyncKind::Engine).is_none(), "refused");
        assert!(downloads.is_running(SyncKind::Engine));

        // A different kind is unaffected - that is the whole point of the registry.
        assert!(downloads.begin(SyncKind::Client).is_some());

        drop(first);
        assert!(!downloads.is_running(SyncKind::Engine));
        assert!(downloads.begin(SyncKind::Engine).is_some(), "freed on drop");
    }

    /// A handler whose future is dropped - client disconnected, runtime shutting down -
    /// must not leave its kind registered forever, because that would now block every
    /// later download of that kind.
    #[test]
    fn a_dropped_download_frees_its_kind() {
        let downloads = DownloadCancellation::new();

        {
            let _guard = downloads.begin(SyncKind::EditorDlls).unwrap();
            assert!(downloads.is_running(SyncKind::EditorDlls));
        }

        assert!(!downloads.is_running(SyncKind::EditorDlls));
    }

    /// Cancelling is not stopping. longtail finishes the blocks already in flight and
    /// then flushes and closes the store, sweeping the cache, so a download that has been
    /// cancelled is still writing for a while. Starting the next one into the same target
    /// during that window is the overlap refusing exists to prevent.
    #[test]
    fn a_cancelled_download_still_holds_its_slot_until_it_stops() {
        let downloads = DownloadCancellation::new();

        let cancelled = downloads.begin(SyncKind::Client).unwrap();
        assert!(downloads.cancel(SyncKind::Client));
        assert!(cancelled.token().is_cancelled());

        assert!(
            downloads.begin(SyncKind::Client).is_none(),
            "still unwinding, so a second download must not start"
        );

        // Only once the download has actually finished does the slot free up.
        drop(cancelled);
        assert!(!downloads.is_running(SyncKind::Client));
        assert!(downloads.begin(SyncKind::Client).is_some());
    }

    /// Shutdown clears everything, so a download may register afterwards. A guard from
    /// before must not then clear that newcomer's registration when it finally drops.
    #[test]
    fn a_stale_guard_does_not_clear_its_successor() {
        let downloads = DownloadCancellation::new();

        let before = downloads.begin(SyncKind::Client).unwrap();
        downloads.cancel_all();

        let successor = downloads
            .begin(SyncKind::Client)
            .expect("shutdown cleared it");
        drop(before);

        assert!(
            downloads.is_running(SyncKind::Client),
            "the successor is still registered"
        );
        assert!(
            successor.token().is_cancelled(),
            "though shutdown cancelled it"
        );
    }

    /// Errors must be classified by what the user should do, and must carry the cause.
    ///
    /// Both halves have been got wrong before in a port of this library: matching on
    /// concrete variants rather than `class()` misses failures that arrive flattened from
    /// a block fetch, and rendering `Display` rather than the source chain throws away
    /// everything except a category name.
    #[tokio::test]
    async fn errors_are_classified_and_keep_their_cause() {
        // No get-config at all: the request itself is wrong.
        let empty = longtail::get(longtail::GetOptions::new(vec![], "/tmp/unused"))
            .await
            .expect_err("no source paths is an error");
        let mapped = SyncError::from_longtail(SyncKind::Client, &empty);
        assert_eq!(mapped.class, SyncErrorClass::InvalidInput);
        assert_eq!(mapped.detail, empty.full_chain());

        // A get-config that is not there. Note this is Io, not NotFound: NotFound is for
        // a blob missing inside a store, not a path that does not exist.
        let missing = longtail::get(longtail::GetOptions::new(
            vec!["/nonexistent/build.json".to_string()],
            "/tmp/unused",
        ))
        .await
        .expect_err("a missing get-config is an error");
        let mapped = SyncError::from_longtail(SyncKind::Engine, &missing);
        assert_eq!(mapped.class, SyncErrorClass::Io);
        assert_eq!(mapped.detail, missing.full_chain());
        assert!(
            mapped.detail.len() > missing.to_string().len(),
            "an error with a source chain must render more than its category: {:?} vs {:?}",
            mapped.detail,
            missing.to_string()
        );
        assert!(mapped.summary.contains("engine"), "{}", mapped.summary);
    }

    /// The sink runs on longtail's async task and on rayon workers, so it must never
    /// block and must survive nobody listening.
    #[test]
    fn the_progress_sink_never_blocks_or_fails() {
        let (tx, rx) = std::sync::mpsc::channel();
        let sink = ChannelProgress::new(SyncKind::Client, tx);

        sink.on_phase("Updating version");
        for done in 1..=32u64 {
            sink.on_progress(Progress {
                done_bytes: done,
                total_bytes: 32,
                done_items: done,
                total_items: 32,
            });
        }

        let events: Vec<SyncEvent> = rx.try_iter().collect();
        assert_eq!(events.len(), 33, "one phase event plus every sample");
        assert!(events.iter().all(|e| matches!(
            e,
            SyncEvent::Progress { progress, .. } if progress.phase == "Updating version"
        )));

        // A closed receiver means nobody is watching, which must not fail a download.
        drop(rx);
        sink.on_phase("Still going");
        sink.on_progress(Progress::default());
    }

    /// The wire contract with the frontend. A Rust test cannot typecheck TypeScript, so
    /// this pins the Rust half and checks the names it emits all appear in the shared
    /// type; `svelte-check` pins the other half.
    #[test]
    fn sync_events_match_the_shared_typescript_type() {
        let ts = include_str!("../ui/src/lib/types/sync.ts");

        let summary = SyncSummary {
            bytes_written: 1,
            assets_written: 2,
            assets_removed: 3,
            blocks_fetched: 4,
        };
        let progress = SyncProgress {
            done_bytes: 1,
            total_bytes: 2,
            done_items: 3,
            total_items: 4,
            phase: "Updating version".to_string(),
        };
        let error = SyncError {
            class: SyncErrorClass::Unauthorized,
            summary: "expired".to_string(),
            detail: "store error: not authorized".to_string(),
        };

        let events = vec![
            SyncEvent::Started {
                kind: SyncKind::Client,
            },
            SyncEvent::Progress {
                kind: SyncKind::Engine,
                progress,
            },
            SyncEvent::Finished {
                kind: SyncKind::EditorDlls,
                summary,
            },
            SyncEvent::Failed {
                kind: SyncKind::Client,
                error,
            },
            SyncEvent::Cancelled {
                kind: SyncKind::Client,
            },
        ];

        for event in events {
            let value = serde_json::to_value(&event).expect("serializes");
            let object = value.as_object().expect("a tagged object");

            let tag = object["type"].as_str().expect("a type tag");
            assert!(
                ts.contains(&format!("type: '{tag}'")),
                "the shared type has no variant {tag:?}"
            );

            for key in object.keys() {
                assert!(
                    ts.contains(key),
                    "the shared type does not mention field {key:?} of {tag:?}"
                );
            }
        }

        // The discriminators the UI switches on.
        for kind in [SyncKind::Client, SyncKind::Engine, SyncKind::EditorDlls] {
            assert!(ts.contains(&format!("'{}'", kind.as_str())), "{kind}");
        }
        assert!(ts.contains("'unauthorized'"));
    }

    #[test]
    fn an_unauthorized_failure_becomes_an_unauthorized_response() {
        let error = SyncError {
            class: SyncErrorClass::Unauthorized,
            summary: "expired".to_string(),
            detail: "store error: not authorized".to_string(),
        };
        assert!(matches!(
            error.into_core_error(),
            crate::types::errors::CoreError::Unauthorized
        ));

        let other = SyncError {
            class: SyncErrorClass::Corrupt,
            summary: "bad block".to_string(),
            detail: "format error: bad magic".to_string(),
        };
        // Everything else keeps its detail rather than collapsing to a bare category.
        match other.into_core_error() {
            crate::types::errors::CoreError::Internal(e) => {
                assert!(e.to_string().contains("bad magic"), "{e}")
            }
            other => panic!("unexpected: {other:?}"),
        }
    }
}
