//! End-to-end tests for artifact downloads, against a real longtail store.
//!
//! No network: longtail treats a bare filesystem path as a store, and `longtail::put`
//! writes a complete one - get-config JSON, block store, version index - so each test
//! builds its own fixture in a temp directory and downloads it back. That exercises the
//! same `longtail::get` code path a real download takes, which is the point: these are
//! the tests that make it safe to delete the hand-written cache pruning and the retry
//! ladder the subprocess needed.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};

use ethos_core::artifact_sync::{
    ArtifactSync, CacheControl, CancellationToken, SyncErrorClass, SyncEvent, SyncKind, SyncMode,
    SyncRequest, SyncSummary,
};
use ethos_core::clients::aws::AWSClient;

/// Credentials are required by the signature but never used: every URI here is a local
/// path, so nothing reaches S3.
async fn offline_aws_client() -> AWSClient {
    AWSClient::from_static_creds(
        "AKIAUNUSED",
        "secret",
        Some("token"),
        None,
        "bucket".to_string(),
        "promoted-bucket".to_string(),
    )
    .await
}

struct Fixture {
    _tmp: tempfile::TempDir,
    get_config: String,
    source: PathBuf,
    root: PathBuf,
}

impl Fixture {
    /// Write a small tree and publish it as a longtail store.
    async fn publish() -> Fixture {
        let tmp = tempfile::tempdir().expect("temp dir");
        let root = tmp.path().to_path_buf();
        let source = root.join("source");

        fs::create_dir_all(source.join("Binaries")).unwrap();
        fs::create_dir_all(source.join("Content")).unwrap();
        // Big enough to span more than one block, so the cache actually holds several.
        fs::write(source.join("Binaries/game.dll"), vec![b'a'; 512 * 1024]).unwrap();
        fs::write(source.join("Binaries/game.pdb"), vec![b'b'; 256 * 1024]).unwrap();
        fs::write(source.join("Content/pak.bin"), vec![b'c'; 1024 * 1024]).unwrap();
        fs::write(source.join("version.txt"), b"1.2.3").unwrap();

        let get_config = root.join("build.json").to_string_lossy().into_owned();
        let options = longtail::PutOptions::new(get_config.clone(), source.to_string_lossy());
        longtail::put(options).await.expect("publish fixture store");

        Fixture {
            _tmp: tmp,
            get_config,
            source,
            root,
        }
    }

    fn target(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    fn cache(&self, name: &str, max_size_bytes: u64) -> CacheControl {
        CacheControl {
            path: self.root.join(name),
            max_size_bytes,
        }
    }
}

async fn sync(
    fixture: &Fixture,
    target: &Path,
    cache: Option<CacheControl>,
    cancel: CancellationToken,
    cache_target_index: bool,
) -> (
    Result<SyncSummary, ethos_core::artifact_sync::SyncError>,
    Receiver<SyncEvent>,
) {
    run(
        fixture,
        target,
        cache,
        cancel,
        cache_target_index,
        SyncMode::Download,
    )
    .await
}

async fn run(
    fixture: &Fixture,
    target: &Path,
    cache: Option<CacheControl>,
    cancel: CancellationToken,
    cache_target_index: bool,
    mode: SyncMode,
) -> (
    Result<SyncSummary, ethos_core::artifact_sync::SyncError>,
    Receiver<SyncEvent>,
) {
    let (tx, rx): (Sender<SyncEvent>, Receiver<SyncEvent>) = channel();
    let aws = offline_aws_client().await;
    let artifact_sync = ArtifactSync::new("friendshipper-tests");

    let mut request = SyncRequest::download(
        SyncKind::Client,
        target,
        std::slice::from_ref(&fixture.get_config),
    )
    .with_cache(cache)
    .with_transfer_acceleration(false);
    request.cache_target_index = cache_target_index;
    request.mode = mode;

    let result = artifact_sync.get_archive(request, tx, &aws, cancel).await;

    (result, rx)
}

fn tree(root: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out = vec![];
    collect(root, root, &mut out);
    out.sort();
    out
}

fn collect(root: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>)>) {
    for entry in fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(root, &path, out);
        } else {
            let rel = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            out.push((rel, fs::read(&path).unwrap()));
        }
    }
}

fn dir_size(path: &Path) -> u64 {
    if !path.exists() {
        return 0;
    }
    let mut total = 0;
    for entry in fs::read_dir(path).unwrap().flatten() {
        let p = entry.path();
        total += if p.is_dir() {
            dir_size(&p)
        } else {
            entry.metadata().unwrap().len()
        };
    }
    total
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn syncs_a_store_and_reports_progress() {
    let fixture = Fixture::publish().await;
    let target = fixture.target("client");

    let (result, rx) = sync(
        &fixture,
        &target,
        Some(fixture.cache("cache", 64 * 1024 * 1024)),
        CancellationToken::new(),
        true,
    )
    .await;

    let summary = result.expect("sync succeeds");
    assert_eq!(summary.assets_written, 4, "one per file in the fixture");
    assert!(summary.bytes_written > 0);
    assert!(summary.blocks_fetched > 0, "a cold cache must fetch blocks");

    assert_eq!(
        tree(&target)
            .into_iter()
            .filter(|(name, _)| !name.starts_with('.'))
            .collect::<Vec<_>>(),
        tree(&fixture.source),
        "downloaded tree matches what was published"
    );

    let events: Vec<SyncEvent> = rx.try_iter().collect();
    assert!(
        matches!(events.first(), Some(SyncEvent::Started { .. })),
        "first event is Started"
    );
    assert!(
        matches!(events.last(), Some(SyncEvent::Finished { .. })),
        "last event is Finished, got {:?}",
        events.last()
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, SyncEvent::Finished { .. }))
            .count(),
        1,
        "exactly one Finished"
    );

    // Progress is monotonic *within* a phase, not across the run: longtail resets the
    // counters on a phase change, so asserting globally would fail the moment indexing
    // hands over to applying.
    let mut phase = String::new();
    let mut last_done = 0;
    let mut saw_progress = false;
    for event in &events {
        if let SyncEvent::Progress { progress, .. } = event {
            saw_progress = true;
            if progress.phase != phase {
                phase = progress.phase.clone();
                last_done = 0;
            }
            assert!(
                progress.done_bytes >= last_done,
                "progress went backwards within phase {phase:?}"
            );
            assert!(
                progress.total_bytes == 0 || progress.done_bytes <= progress.total_bytes,
                "done exceeded total in phase {phase:?}"
            );
            last_done = progress.done_bytes;
        }
    }
    assert!(saw_progress, "the download reported progress");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_warm_cache_serves_the_second_download() {
    let fixture = Fixture::publish().await;
    let cache_dir = fixture.root.join("cache");

    let (first, _) = sync(
        &fixture,
        &fixture.target("first"),
        Some(fixture.cache("cache", 64 * 1024 * 1024)),
        CancellationToken::new(),
        true,
    )
    .await;
    assert!(first.expect("first sync").blocks_fetched > 0);
    assert!(
        dir_size(&cache_dir.join("chunks")) > 0,
        "cache was populated"
    );

    // A fresh target, so the second run really does rebuild the tree rather than
    // short-circuiting on a cached target index and reporting zero for that reason.
    let (second, _) = sync(
        &fixture,
        &fixture.target("second"),
        Some(fixture.cache("cache", 64 * 1024 * 1024)),
        CancellationToken::new(),
        true,
    )
    .await;
    let summary = second.expect("second sync");
    assert!(summary.assets_written > 0, "it did rebuild the tree");
    assert_eq!(
        summary.blocks_fetched, 0,
        "every block came from the cache, not the store"
    );
}

/// The whole of cache maintenance now: the budget that used to be enforced by walking
/// the cache directory by hand is enforced by longtail.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_cache_budget_is_enforced() {
    let fixture = Fixture::publish().await;
    let cache_dir = fixture.root.join("cache");
    let budget = 256 * 1024;

    let (result, _) = sync(
        &fixture,
        &fixture.target("client"),
        Some(fixture.cache("cache", budget)),
        CancellationToken::new(),
        true,
    )
    .await;
    result.expect("sync succeeds");

    let held = dir_size(&cache_dir.join("chunks"));
    assert!(
        held <= budget,
        "cache holds {held} bytes, over the {budget} byte budget"
    );

    // Eviction must leave a usable cache, not a broken one.
    let (again, _) = sync(
        &fixture,
        &fixture.target("client-again"),
        Some(fixture.cache("cache", budget)),
        CancellationToken::new(),
        true,
    )
    .await;
    again.expect("a download after eviction still succeeds");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cancelled_download_can_be_resumed() {
    let fixture = Fixture::publish().await;
    let target = fixture.target("client");

    let cancel = CancellationToken::new();
    cancel.cancel();

    let (result, rx) = sync(
        &fixture,
        &target,
        Some(fixture.cache("cache", 64 * 1024 * 1024)),
        cancel,
        true,
    )
    .await;

    let error = result.expect_err("a cancelled download does not succeed");
    assert_eq!(error.class, SyncErrorClass::Cancelled);
    assert!(
        rx.try_iter()
            .any(|e| matches!(e, SyncEvent::Cancelled { .. })),
        "cancelling reports Cancelled, not Failed"
    );

    // The point of cancelling rather than killing: run it again and it completes.
    let (resumed, _) = sync(
        &fixture,
        &target,
        Some(fixture.cache("cache", 64 * 1024 * 1024)),
        CancellationToken::new(),
        true,
    )
    .await;
    resumed.expect("a cancelled download resumes cleanly");

    assert_eq!(
        tree(&target)
            .into_iter()
            .filter(|(name, _)| !name.starts_with('.'))
            .collect::<Vec<_>>(),
        tree(&fixture.source),
        "the resumed tree is complete"
    );
}

/// The editor DLL download syncs into a staging directory that is then copied wholesale
/// into the user's git repo, so it must not leave longtail's cached target index behind.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_target_index_cache_can_be_suppressed() {
    let fixture = Fixture::publish().await;

    let with_cache = fixture.target("with-index");
    let (result, _) = sync(&fixture, &with_cache, None, CancellationToken::new(), true).await;
    result.expect("sync succeeds");
    assert!(
        with_cache.join(longtail::TARGET_INDEX_CACHE_NAME).exists(),
        "the default writes a target index"
    );

    let without = fixture.target("without-index");
    let (result, _) = sync(&fixture, &without, None, CancellationToken::new(), false).await;
    result.expect("sync succeeds");
    assert!(
        !without.join(longtail::TARGET_INDEX_CACHE_NAME).exists(),
        "nothing to copy into the repo"
    );
}

/// Pinned rather than assumed: a re-scanned target has anything the version does not
/// name removed. That is wanted for an engine directory, where stale files across a
/// version change cause real problems - but it is worth being visible in the tests.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_rescan_removes_files_the_version_does_not_name() {
    let fixture = Fixture::publish().await;
    let target = fixture.target("client");

    let (result, _) = sync(&fixture, &target, None, CancellationToken::new(), false).await;
    result.expect("sync succeeds");

    let stray = target.join("Binaries/left-behind.log");
    fs::write(&stray, b"from an older build").unwrap();

    let (result, _) = sync(&fixture, &target, None, CancellationToken::new(), false).await;
    let summary = result.expect("sync succeeds");

    assert!(!stray.exists(), "a rescan cleans the target");
    assert_eq!(summary.assets_removed, 1);
}

/// Verify re-hashes what it writes, so a block whose bytes no longer match the version
/// index is reported rather than installed. This is the principled replacement for the
/// old recovery ladder, which deleted the whole cache on a second failure because it had
/// no way to tell a corrupt block from anything else.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn verify_reports_a_corrupted_block() {
    let fixture = Fixture::publish().await;
    let target = fixture.target("client");

    let (result, _) = sync(&fixture, &target, None, CancellationToken::new(), false).await;
    result.expect("initial sync succeeds");

    // Corrupt a stored block in place, leaving its name - and so its hash - untouched.
    let block = find_block(&fixture.root.join("store")).expect("a stored block");
    let mut bytes = fs::read(&block).unwrap();
    let tail = bytes.len() - 1;
    bytes[tail] ^= 0xff;
    fs::write(&block, bytes).unwrap();

    // Force the blocks to be re-read rather than served from a warm target.
    fs::remove_dir_all(&target).unwrap();

    let (result, _) = run(
        &fixture,
        &target,
        None,
        CancellationToken::new(),
        false,
        SyncMode::Verify,
    )
    .await;

    let error = result.expect_err("a corrupted block must not be installed silently");
    assert!(
        matches!(
            error.class,
            SyncErrorClass::Corrupt | SyncErrorClass::Internal
        ),
        "expected a corruption class, got {:?}: {}",
        error.class,
        error.detail
    );
}

/// Verify repairs what the version names and leaves everything else where it is, which
/// is what makes it safe to run over an install that holds logs or user config.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn verify_repairs_the_version_and_keeps_everything_else() {
    let fixture = Fixture::publish().await;
    let target = fixture.target("client");

    let (result, _) = sync(&fixture, &target, None, CancellationToken::new(), false).await;
    result.expect("initial sync succeeds");

    // Something the build owns, damaged; and something it does not, which must survive.
    let owned = target.join("Binaries/game.dll");
    fs::write(&owned, b"clobbered").unwrap();
    let user_file = target.join("Saved/user.log");
    fs::create_dir_all(user_file.parent().unwrap()).unwrap();
    fs::write(&user_file, b"keep me").unwrap();

    let (result, _) = run(
        &fixture,
        &target,
        None,
        CancellationToken::new(),
        false,
        SyncMode::Verify,
    )
    .await;
    let summary = result.expect("verify succeeds");

    assert_eq!(
        fs::read(&owned).unwrap(),
        fs::read(fixture.source.join("Binaries/game.dll")).unwrap(),
        "the damaged file was repaired"
    );
    assert_eq!(
        fs::read(&user_file).unwrap(),
        b"keep me",
        "a file the version does not name was left alone"
    );
    assert_eq!(summary.assets_removed, 0, "verify removes nothing");
}

/// A verify over an install that is already correct should find nothing to do, rather
/// than rewriting it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn verify_of_a_healthy_install_changes_nothing() {
    let fixture = Fixture::publish().await;
    let target = fixture.target("client");

    let (result, _) = sync(&fixture, &target, None, CancellationToken::new(), false).await;
    result.expect("initial sync succeeds");
    let before = tree(&target);

    let (result, _) = run(
        &fixture,
        &target,
        None,
        CancellationToken::new(),
        false,
        SyncMode::Verify,
    )
    .await;
    let summary = result.expect("verify succeeds");

    assert_eq!(summary.assets_written, 0, "nothing needed rewriting");
    assert_eq!(summary.assets_removed, 0);
    assert_eq!(tree(&target), before, "the install is untouched");
}

/// The first block file found anywhere under a store, for corrupting.
fn find_block(store: &Path) -> Option<PathBuf> {
    for entry in fs::read_dir(store).ok()?.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = find_block(&path) {
                return Some(found);
            }
        } else if path.extension().is_some_and(|e| e == "lsb") {
            return Some(path);
        }
    }
    None
}
