//! Is each downloaded artifact the one this checkout needs, and are its bytes intact?
//!
//! Those are two different questions and they have different answers. An engine that
//! perfectly matches the build it was downloaded from is still broken if the uproject has
//! since moved to a different engine - and "repairing" it against the new version would
//! be worse than leaving it, because a repair writes the new files over the old without
//! removing what the old version had, leaving a hybrid that matches no build at all.
//!
//! So: the checkout decides which version *should* be installed, the ledger records which
//! one *is*, and only when they agree is it worth checking the bytes.

use std::path::{Path as FsPath, PathBuf};

use axum::extract::{Path, State};
use axum::Json;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::warn;

use ethos_core::artifact_sync::{SyncError, SyncKind, SyncRequest, SyncSummary};
use ethos_core::clients::aws::ensure_aws_client;
use ethos_core::types::config::{EngineType, UProject};
use ethos_core::types::errors::CoreError;

use crate::engine::EngineProvider;
use crate::state::AppState;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ArtifactState {
    /// Nothing recorded, or the recorded location is empty. Sync it once.
    NotInstalled,
    /// Installed, but not the version this checkout asks for. A sync fixes it; a verify
    /// would not, and should not try.
    OutOfDate,
    /// The right version is installed. Whether its bytes are intact is what verify says.
    Installed,
    /// Installed, and there is nothing to compare it against - the game client, where you
    /// choose the build rather than the checkout choosing it for you.
    InstalledNoExpectation,
    /// Installed, but we could not work out which version it should be: the uproject
    /// would not parse, or the repo status has not been fetched yet. Distinct from
    /// having no expectation, because here there *is* a right answer and we do not know
    /// it - verifying against whatever happens to be recorded could be checking the
    /// wrong build entirely.
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactStatus {
    pub kind: SyncKind,
    pub state: ArtifactState,
    /// What the checkout asks for, where the checkout gets a say.
    pub expected: Option<String>,
    /// What the last successful download put there.
    pub installed: Option<String>,
    pub location: Option<String>,
    pub synced_at: Option<DateTime<Utc>>,
    /// Where `expected` comes from, so the row explains itself.
    pub expectation_source: Option<String>,
}

/// What a verify found, or why it did not run.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyResponse {
    pub status: ArtifactStatus,
    /// False when the version is wrong or missing - nothing was checked, and nothing was
    /// changed. Not a failure; a different problem, with a different fix.
    pub checked: bool,
    /// Files that did not match and were rewritten. Zero is healthy.
    pub repaired: u32,
    pub bytes_written: u64,
    pub message: String,
}

/// What the checkout asks for.
enum Expectation {
    /// The checkout has no opinion, and that is correct rather than a gap: you choose the
    /// game client build, and a source engine is the user's own business.
    None,
    /// There is a right answer and we could not determine it.
    Unknown,
    Version {
        version: String,
        source: String,
    },
}

/// The version the checkout asks for, and what to call the thing that asked.
async fn expectation<T>(state: &AppState<T>, kind: SyncKind) -> Expectation
where
    T: EngineProvider,
{
    match kind {
        // You pick the build; nothing in the checkout has an opinion about which.
        SyncKind::Client => Expectation::None,

        SyncKind::Engine => {
            // Someone running a source engine builds it themselves; we have no opinion on
            // what should be there and must not tell them they are out of date. This is
            // the same switch that decides whether an engine is downloaded at all.
            if state.app_config.read().engine_type != EngineType::Prebuilt {
                return Expectation::None;
            }

            let uproject_path = state
                .app_config
                .read()
                .get_uproject_path(&state.repo_config.read());

            // Named from the configured path rather than hardcoded: this is a general
            // tool, and every project that uses it has a differently named uproject.
            let uproject_name = uproject_path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "uproject".to_string());

            match UProject::load(&uproject_path) {
                Ok(uproject) if uproject.is_custom_engine() => {
                    match uproject.get_custom_engine_sha() {
                        Ok(version) => Expectation::Version {
                            version,
                            source: format!("{uproject_name} engine association"),
                        },
                        Err(e) => {
                            warn!("Could not read the engine association: {e}");
                            Expectation::Unknown
                        }
                    }
                }
                // A stock Epic engine is not ours to check.
                Ok(_) => Expectation::None,
                Err(e) => {
                    warn!("Could not read {uproject_path:?} to find the expected engine: {e}");
                    Expectation::Unknown
                }
            }
        }

        SyncKind::EditorDlls => {
            // Someone who builds the editor themselves is not out of date when the last
            // downloadable build moves on, and telling them so offers a fix that refuses
            // to run. This is the same switch that decides whether DLLs are pulled at all.
            if !state.app_config.read().pull_dlls {
                return Expectation::None;
            }

            // The *local* commit: the last editor build reachable from the branch as it is
            // checked out now. dll_commit_remote is the build you would need after pulling,
            // so comparing against it reports every user whose origin has moved on as out
            // of date - for binaries that are correct for the commit they are sitting on.
            let commit = state.repo_status.read().dll_commit_local.clone();
            if commit.is_empty() {
                // Not "no expectation": there is a correct build, we just have not
                // fetched the repo status that names it.
                Expectation::Unknown
            } else {
                Expectation::Version {
                    version: commit,
                    source: "last editor build before your current commit".to_string(),
                }
            }
        }
    }
}

async fn status_for<T>(state: &AppState<T>, kind: SyncKind) -> ArtifactStatus
where
    T: EngineProvider,
{
    let wanted = expectation(state, kind).await;
    let record = state.artifact_sync.ledger().get(kind);

    let installed = record.as_ref().and_then(|r| {
        // A record pointing at a directory that is gone describes nothing. Where the
        // download landed somewhere else first, that has to still be there too: wiping
        // the data directory removes the staging copy but leaves this record, and without
        // this check a verify would then re-download the whole build to "check" it.
        let present = r.target.exists() && r.staging.as_ref().is_none_or(|s| s.exists());
        present.then(|| r.version.clone())
    });

    let (expected, expectation_source) = match &wanted {
        Expectation::Version { version, source } => (Some(version.clone()), Some(source.clone())),
        _ => (None, None),
    };

    let state_of_it = match (&wanted, &installed) {
        (_, None) => ArtifactState::NotInstalled,
        (Expectation::None, Some(_)) => ArtifactState::InstalledNoExpectation,
        (Expectation::Unknown, Some(_)) => ArtifactState::Unknown,
        (Expectation::Version { version, .. }, Some(have)) if version == have => {
            ArtifactState::Installed
        }
        (Expectation::Version { .. }, Some(_)) => ArtifactState::OutOfDate,
    };

    ArtifactStatus {
        kind,
        state: state_of_it,
        expected,
        installed,
        location: record
            .as_ref()
            .map(|r| r.target.to_string_lossy().into_owned()),
        synced_at: record.as_ref().map(|r| r.recorded_at),
        expectation_source,
    }
}

/// Files `source` provides that `destination` does not have, or has differently.
///
/// A plain comparison rather than anything longtail does: these are the same files copied
/// from one place to another, so length and bytes settle it, and it costs a read of the
/// binaries rather than a scan of the whole project.
/// Compare two files a block at a time.
///
/// Reading both whole would be the obvious thing and is the wrong thing here: with
/// symbols enabled these are PDBs of a gigabyte or more, and holding two of them in
/// memory to answer a yes/no question is a good way to be killed by the allocator.
/// Read until `buf` is full or the reader ends, returning how much was filled.
///
/// `read` may stop short wherever it likes, so comparing two raw read counts compares
/// where each reader chose to stop rather than what the files hold - two identical files
/// would look different if one happened to come back in smaller pieces. Filling first
/// means a short count can only mean EOF.
fn fill(reader: &mut impl std::io::Read, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match reader.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

fn same_contents(ours: &FsPath, theirs: &FsPath) -> std::io::Result<bool> {
    const CHUNK: usize = 64 * 1024;

    let mut ours = std::io::BufReader::new(std::fs::File::open(ours)?);
    let mut theirs = std::io::BufReader::new(std::fs::File::open(theirs)?);
    let mut a = vec![0u8; CHUNK];
    let mut b = vec![0u8; CHUNK];

    loop {
        let read_a = fill(&mut ours, &mut a)?;
        let read_b = fill(&mut theirs, &mut b)?;

        if read_a != read_b {
            // Both buffers were filled to EOF, so differing counts mean one file is
            // shorter than the other - it changed underneath us since the lengths matched.
            return Ok(false);
        }
        if read_a == 0 {
            return Ok(true);
        }
        if a[..read_a] != b[..read_b] {
            return Ok(false);
        }
    }
}

fn differences_from(source: &FsPath, destination: &FsPath) -> std::io::Result<Vec<PathBuf>> {
    let mut differing = Vec::new();
    compare_tree(source, source, destination, &mut differing)?;
    differing.sort();
    Ok(differing)
}

fn compare_tree(
    root: &FsPath,
    source: &FsPath,
    destination: &FsPath,
    differing: &mut Vec<PathBuf>,
) -> std::io::Result<()> {
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            compare_tree(root, &path, destination, differing)?;
            continue;
        }

        let relative = path.strip_prefix(root).unwrap_or(&path).to_path_buf();
        let theirs = destination.join(&relative);

        let same = match (std::fs::metadata(&path), std::fs::metadata(&theirs)) {
            (Ok(ours), Ok(theirs_meta)) if ours.len() == theirs_meta.len() => {
                same_contents(&path, &theirs)?
            }
            _ => false,
        };

        if !same {
            differing.push(relative);
        }
    }
    Ok(())
}

/// Where each downloaded artifact stands: what the checkout wants, what is on disk.
pub async fn artifact_status_handler<T>(
    State(state): State<AppState<T>>,
) -> Result<Json<Vec<ArtifactStatus>>, CoreError>
where
    T: EngineProvider,
{
    let mut out = Vec::with_capacity(3);
    for kind in [SyncKind::Client, SyncKind::Engine, SyncKind::EditorDlls] {
        out.push(status_for(&state, kind).await);
    }
    Ok(Json(out))
}

/// Check an installed artifact's bytes against the build it came from, repairing what
/// does not match and leaving everything else alone.
pub async fn verify_handler<T>(
    State(state): State<AppState<T>>,
    Path(kind): Path<SyncKind>,
) -> Result<Json<VerifyResponse>, CoreError>
where
    T: EngineProvider,
{
    let status = status_for(&state, kind).await;

    let refuse = |message: String| {
        Ok(Json(VerifyResponse {
            status: status.clone(),
            checked: false,
            repaired: 0,
            bytes_written: 0,
            message,
        }))
    };

    match status.state {
        ArtifactState::NotInstalled => {
            return refuse(format!(
                "No {kind} is installed, so there is nothing to check. Sync it once first."
            ));
        }
        // Repairing across versions would merge them rather than replace, so this stops.
        ArtifactState::OutOfDate => {
            return refuse(format!(
                "Your {kind} is {} but this checkout needs {}. Syncing replaces it; verifying \
                 would only write the new files over the old and leave both.",
                status.installed.as_deref().unwrap_or("unknown"),
                status.expected.as_deref().unwrap_or("unknown"),
            ));
        }
        // There is a right answer and we do not know it. Verifying would check whatever
        // the ledger happens to record, which may be a build this checkout left behind.
        ArtifactState::Unknown => {
            return refuse(format!(
                "Could not work out which {kind} this checkout needs, so there is nothing to \
                 check it against. Sync the repo first, then try again."
            ));
        }
        ArtifactState::Installed | ArtifactState::InstalledNoExpectation => {}
    }

    let record =
        state.artifact_sync.ledger().get(kind).ok_or_else(|| {
            CoreError::Internal(anyhow::anyhow!("no record for the {kind} download"))
        })?;

    let aws_client = ensure_aws_client(state.aws_client.read().await.clone())?;
    let transfer_acceleration = state.app_config.read().s3_transfer_acceleration;

    // Refuses rather than joining in: a verify writes every asset that differs, and an
    // engine download in progress makes most of them differ, so the two would be writing
    // the same files at the same time.
    let Some(download) = state.downloads.begin(kind) else {
        return refuse(format!(
            "A {kind} download is already running. Wait for it to finish, then verify."
        ));
    };
    // Check where the download landed, which is what the version index describes. For the
    // editor binaries that is the staging directory, not the repo: the repo also holds
    // every git-tracked file, and verifying against it would chunk and hash the whole
    // project to check a few hundred megabytes of DLLs.
    let checked_path = record
        .staging
        .clone()
        .unwrap_or_else(|| record.target.clone());
    // The verify runs on its own task holding the guard, so a browser that navigates away
    // mid-repair does not drop the future and leave the artifact half-rewritten - the
    // worst possible state, since neither the old nor the new bytes are all there.
    let token = download.token();
    let artifact_sync = state.artifact_sync.clone();
    let tx = state.sync_event_tx.clone();
    let target = checked_path.clone();
    let archives = record.archives.clone();
    let cache = record.cache();
    let handle = tokio::spawn(async move {
        let _download = download;
        let request = SyncRequest::verify(kind, &target, &archives)
            .with_cache(cache)
            .with_transfer_acceleration(transfer_acceleration);
        artifact_sync
            .get_archive(request, tx, &aws_client, token)
            .await
    });

    let result = handle
        .await
        .map_err(|e| CoreError::Internal(anyhow::anyhow!("the {kind} verify failed: {e}")))?;

    let summary: SyncSummary = result.map_err(SyncError::into_core_error)?;

    let mut message = if summary.assets_written == 0 {
        format!("The {kind} matches {}.", record.version)
    } else {
        format!(
            "Repaired {} file(s) in the {kind} that did not match {}.",
            summary.assets_written, record.version
        )
    };

    // Second half of the question for anything copied out of staging: the download being
    // intact says nothing about whether the copy in the repo still matches it.
    if record.staging.is_some() {
        // Reading every staged binary is filesystem work; doing it on the runtime would
        // block a worker for the length of the walk.
        let source = checked_path.clone();
        let destination = record.target.clone();
        let compared = tokio::task::spawn_blocking(move || differences_from(&source, &destination))
            .await
            .map_err(|e| CoreError::Internal(anyhow::anyhow!("comparison failed: {e}")))?;

        match compared {
            Ok(differing) if differing.is_empty() => {
                message.push_str(" Your repo matches the downloaded binaries.");
            }
            Ok(differing) => {
                let shown: Vec<String> = differing
                    .iter()
                    .take(5)
                    .map(|p| p.display().to_string())
                    .collect();
                let more = differing.len().saturating_sub(shown.len());
                message.push_str(&format!(
                    " {} file(s) in your repo do not match the downloaded binaries: {}{}. \
                     Sync to copy them across.",
                    differing.len(),
                    shown.join(", "),
                    if more > 0 {
                        format!(" and {more} more")
                    } else {
                        String::new()
                    }
                ));
            }
            Err(e) => {
                warn!("Could not compare the staged binaries against the repo: {e}");
            }
        }
    }

    Ok(Json(VerifyResponse {
        status,
        checked: true,
        repaired: summary.assets_written,
        bytes_written: summary.bytes_written,
        message,
    }))
}

#[cfg(test)]
mod comparison_tests {
    use super::*;

    fn write(path: &FsPath, bytes: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    #[test]
    fn identical_trees_have_no_differences() {
        let dir = tempfile::tempdir().unwrap();
        let staging = dir.path().join("staging");
        let repo = dir.path().join("repo");
        write(&staging.join("Binaries/Win64/Game.dll"), b"same");
        write(&repo.join("Binaries/Win64/Game.dll"), b"same");

        assert!(differences_from(&staging, &repo).unwrap().is_empty());
    }

    /// Missing, differing and same-length-but-different all have to be caught - the last
    /// is the one a length check alone would miss.
    #[test]
    fn differing_missing_and_same_length_files_are_all_reported() {
        let dir = tempfile::tempdir().unwrap();
        let staging = dir.path().join("staging");
        let repo = dir.path().join("repo");

        write(&staging.join("Same.dll"), b"aaaa");
        write(&repo.join("Same.dll"), b"aaaa");
        write(&staging.join("Missing.dll"), b"new");
        write(&staging.join("SameLength.dll"), b"aaaa");
        write(&repo.join("SameLength.dll"), b"bbbb");

        let differing = differences_from(&staging, &repo).unwrap();

        assert_eq!(
            differing,
            vec![
                PathBuf::from("Missing.dll"),
                PathBuf::from("SameLength.dll")
            ]
        );
    }

    /// Bigger than the read buffer, so a difference past the first chunk still counts.
    #[test]
    fn a_difference_beyond_the_first_chunk_is_found() {
        let dir = tempfile::tempdir().unwrap();
        let ours = dir.path().join("ours.pdb");
        let theirs = dir.path().join("theirs.pdb");

        let mut a = vec![b'x'; 200 * 1024];
        let mut b = a.clone();
        a[180 * 1024] = b'a';
        b[180 * 1024] = b'b';
        std::fs::write(&ours, &a).unwrap();
        std::fs::write(&theirs, &b).unwrap();

        assert!(!same_contents(&ours, &theirs).unwrap());

        std::fs::write(&theirs, &a).unwrap();
        assert!(same_contents(&ours, &theirs).unwrap());
    }

    /// A reader that hands back one byte at a time, as a network-backed or compressed
    /// reader is free to do.
    struct Dribble<'a>(&'a [u8]);

    impl std::io::Read for Dribble<'_> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.0.is_empty() || buf.is_empty() {
                return Ok(0);
            }
            buf[0] = self.0[0];
            self.0 = &self.0[1..];
            Ok(1)
        }
    }

    #[test]
    fn fill_keeps_reading_past_a_short_read() {
        let data = vec![b'z'; 10];
        let mut buf = vec![0u8; 8];

        let mut reader = Dribble(&data);
        assert_eq!(fill(&mut reader, &mut buf).unwrap(), 8);
        assert_eq!(buf, vec![b'z'; 8]);

        // Only what is left, and only then is a short count EOF.
        assert_eq!(fill(&mut reader, &mut buf).unwrap(), 2);
        assert_eq!(fill(&mut reader, &mut buf).unwrap(), 0);
    }
}
