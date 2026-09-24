use std::borrow::Cow;
use std::collections::HashSet;
use std::fs;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use std::time::Duration;

use anyhow::anyhow;
use anyhow::Context;
use axum::extract::{Query, State};
use axum::{async_trait, Json};
use serde::Deserialize;
use serde::Serialize;
use tokio::sync::oneshot::error::RecvError;
use tracing::info;
use tracing::warn;

use crate::engine::EngineProvider;
use chrono::Utc;
use ethos_core::artifact_sync;
use ethos_core::artifact_sync::SyncEvent;
use ethos_core::artifact_sync::{
    DownloadCancellation, SyncError, SyncKind, SyncRecord, SyncRequest, TARGET_INDEX_CACHE_NAME,
};
use ethos_core::clients::aws::ensure_aws_client;
use ethos_core::clients::git;
use ethos_core::storage::config::Project;
use ethos_core::storage::ArtifactStorage;
use ethos_core::storage::{ArtifactBuildConfig, ArtifactConfig, ArtifactKind, Platform};
use ethos_core::types::config::RepoConfig;
use ethos_core::types::errors::CoreError;
use ethos_core::utils::process::is_held_by_another_program;
use ethos_core::worker::{Task, TaskSequence};
use ethos_core::AWSClient;

use crate::AppState;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadResponse {
    pub download_attempted: bool,
}

#[derive(Clone)]
pub struct DownloadDllsOp<T> {
    pub git_client: git::Git,
    pub project_name: String,
    pub dll_commit: String,
    pub download_symbols: bool,
    pub storage: ArtifactStorage,
    pub artifact_sync: artifact_sync::ArtifactSync,
    pub downloads: DownloadCancellation,
    pub tx: Sender<SyncEvent>,
    pub aws_client: AWSClient,
    pub project: Project,
    pub engine: T,
    pub engine_path: PathBuf,
    pub max_cache_size_bytes: u64,
    pub transfer_acceleration: bool,
}

#[async_trait]
impl<T> Task for DownloadDllsOp<T>
where
    T: EngineProvider,
{
    #[tracing::instrument(
        name = "DownloadDllsOp::execute",
        ret,
        skip(self),
        fields(
            project_name = %self.project_name,
            dll_commit = %self.dll_commit,
            download_symbols = %self.download_symbols,
            project = %self.project
        )
    )]
    async fn execute(&self) -> Result<(), CoreError> {
        self.engine.check_ready_to_sync_repo().await?;

        if self.dll_commit.is_empty() {
            return Err(CoreError::Internal(anyhow!(
                "No DLL archive found for current branch."
            )));
        }

        let mut binaries_staging_path = Path::join(
            &self.artifact_sync.download_path.0,
            Path::new("editor_staging"),
        );

        let mut binaries_cache_path = Path::join(
            &self.artifact_sync.download_path.0,
            Path::new("editor_cache"),
        );

        let mut binaries_destination_path = PathBuf::from(&self.git_client.repo_path);

        // If the project has a "Source" directory in the root, we place the DLLs there, otherwise
        // we expect it to be underneath a subdirectory with the project name.
        let source_exists = Path::new(&self.git_client.repo_path)
            .join("Source")
            .is_dir();
        if !source_exists {
            binaries_cache_path = binaries_cache_path.join(&self.project_name);
            binaries_staging_path = binaries_staging_path.join(&self.project_name);
            binaries_destination_path = binaries_destination_path.join(&self.project_name);
        };

        let editor_config = ArtifactConfig::new(
            self.project.clone(),
            ArtifactKind::Editor,
            ArtifactBuildConfig::Development,
            Platform::Win64,
        );

        let archive_url = match self
            .storage
            .get_from_short_sha(editor_config, &self.dll_commit)
            .await
        {
            Err(e) => {
                return Err(CoreError::Internal(anyhow!(
                    "Failed to determine editor dll archive URL: {}",
                    e
                )));
            }
            Ok(archive) => archive,
        };

        info!(
            "downloading editor binaries from url {} to {:?}...",
            &archive_url, &binaries_staging_path
        );

        let mut archive_urls: Vec<String> = vec![archive_url];

        if self.download_symbols {
            let symbols_config = ArtifactConfig::new(
                self.project.clone(),
                ArtifactKind::EditorSymbols,
                ArtifactBuildConfig::Development,
                Platform::Win64,
            );

            match self
                .storage
                .get_from_short_sha(symbols_config, &self.dll_commit)
                .await
            {
                Err(e) => {
                    warn!("Failed to determine symbols archive URL. Symbols will be unavailable. Error: {}", e)
                }
                Ok(url) => {
                    info!(
                        "downloading editor symbols from url {} to {:?}...",
                        &url, &binaries_staging_path
                    );
                    archive_urls.push(url);
                }
            };
        }

        let Some(download) = self.downloads.begin(SyncKind::EditorDlls) else {
            return Err(CoreError::Internal(anyhow!(
                "An editor binaries download is already running."
            )));
        };
        // Everything in the staging directory is copied into the user's repo below, and
        // longtail writes its cached scan of the target into the target. Not writing one
        // keeps it out of the repo; the cost is a scan of a staging directory that was
        // scanned every time anyway.
        let request =
            SyncRequest::download(SyncKind::EditorDlls, &binaries_staging_path, &archive_urls)
                .with_cache(Some(artifact_sync::CacheControl {
                    path: binaries_cache_path.clone(),
                    max_size_bytes: self.max_cache_size_bytes,
                }))
                .with_transfer_acceleration(self.transfer_acceleration)
                .without_target_index();
        let result = self
            .artifact_sync
            .get_archive(request, self.tx.clone(), &self.aws_client, download.token())
            .await;
        result.map_err(SyncError::into_core_error)?;

        T::post_download(&self.engine_path).await;

        // The download above asks for no target index, but the old CLI wrote one into
        // staging on every sync, and it is still sitting there on every machine that
        // synced before this. The copy below takes the staging tree wholesale, so it would
        // carry that file into the user's repo - which is exactly what the old post-copy
        // cleanup existed to undo. Clear it from both ends instead, so it is
        // neither copied in nor left behind.
        discard_target_index(&binaries_staging_path);
        discard_target_index(&binaries_destination_path);

        info!(
            "download done. copying binaries from '{:?}' to: '{:?}'",
            binaries_staging_path, &self.git_client.repo_path
        );

        // What this build provides. longtail keeps the staging directory matching the
        // build exactly - it deletes what the version does not name - so this is the
        // authoritative list of what should end up in the repo.
        let provided = relative_paths(&binaries_staging_path)
            .context("Failed to read the downloaded binaries")?;

        // Read before anything overwrites it: this is what an earlier sync put in the
        // repo, and the only thing that makes a file safe to remove later.
        let manifest_path = copied_manifest_path(&self.artifact_sync.download_path.0);
        let previous = CopiedFiles::load(&manifest_path);

        let outcome = copy_recursively(&binaries_staging_path, &binaries_destination_path)
            .context("Failed to read the downloaded binaries")?;

        if !outcome.is_ok() {
            // Files that did copy stay copied, so the record has to say so or a later
            // build dropping them would never reconcile them away.
            let ours = paths_we_wrote(&provided, &outcome, &previous, &binaries_destination_path);
            record_copied(&manifest_path, &binaries_destination_path, &ours);

            return Err(CoreError::Internal(anyhow!(
                "{}",
                outcome.describe(&binaries_destination_path)
            )));
        }

        info!(
            "copied {} editor binaries into {:?}",
            outcome.copied, binaries_destination_path
        );

        // Copying only ever adds, so a binary that an earlier build had and this one does
        // not would otherwise sit in the repo forever. Nothing loads it, but it
        // accumulates and makes the checkout harder to reason about.
        let stale = stale_paths(&previous, &binaries_destination_path, &provided);
        let mut ours = provided.clone();
        if !stale.is_empty() {
            // Whatever the removal could not get rid of is still in the repo and still
            // ours, so it stays on the record and the next sync tries again. Dropping it
            // would strand it there with nothing saying we put it there.
            ours.extend(remove_stale(&self.git_client, &binaries_destination_path, &stale).await);
        }

        record_copied(&manifest_path, &binaries_destination_path, &ours);

        // The repo copy, not the staging directory: staging is an implementation detail
        // of merging downloaded binaries into a checkout, and the copy in the repo is
        // what the editor actually loads.
        let recorded = self.artifact_sync.ledger().record(
            SyncKind::EditorDlls,
            SyncRecord {
                version: self.dll_commit.clone(),
                target: binaries_destination_path.clone(),
                staging: Some(binaries_staging_path.clone()),
                archives: archive_urls.clone(),
                cache_path: Some(binaries_cache_path.clone()),
                cache_size_bytes: self.max_cache_size_bytes,
                recorded_at: Utc::now(),
            },
        );

        // The download finished long before this: the binaries had still to be copied out
        // of staging into the repo. Only now do they match what the ledger records - and
        // if the record did not reach disk, they do not.
        if recorded {
            let _ = self.tx.send(SyncEvent::Installed {
                kind: SyncKind::EditorDlls,
            });
        }

        info!("dll download and copy to local repo finished");

        Ok(())
    }

    fn get_name(&self) -> String {
        String::from("DownloadDlls")
    }
}

/// Which editor build to fetch.
///
/// Defaults to the one the remote branch needs, which is what a pull wants. The diagnostics
/// page and the warning banner ask for the one the *local* checkout needs instead - a
/// different build whenever you are behind origin, and the only one that makes their own
/// report of the mismatch go away.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadDllsParams {
    pub commit: Option<String>,
}

pub async fn download_dlls_handler<T>(
    State(state): State<AppState<T>>,
    Query(params): Query<DownloadDllsParams>,
) -> Result<Json<DownloadResponse>, CoreError>
where
    T: EngineProvider,
{
    let aws_client = ensure_aws_client(state.aws_client.read().await.clone())?;

    if !state.app_config.read().pull_dlls {
        return Err(CoreError::Internal(anyhow!(
            "You must enable 'Download DLLs' in Preferences to force a DLL download."
        )));
    }

    let download_op = {
        let tx_lock = state.sync_event_tx.clone();
        let project_name = RepoConfig::get_project_name(&state.repo_config.read().uproject_path)
            .unwrap_or("unknown_project".to_string());

        let storage = state
            .storage
            .read()
            .clone()
            .context("Storage not configured. AWS may still be initializing.")?;

        let project = state
            .app_config
            .read()
            .clone()
            .selected_artifact_project
            .context("Project not configured. Repo may still be initializing.")?
            .as_str()
            .into();

        let engine_path = state
            .app_config
            .read()
            .load_engine_path_from_repo(&state.repo_config.read())?;

        DownloadDllsOp {
            git_client: state.git(),
            project_name,
            dll_commit: params
                .commit
                .unwrap_or_else(|| state.repo_status.read().dll_commit_remote.clone()),
            download_symbols: state.app_config.read().editor_download_symbols,
            storage,
            artifact_sync: state.artifact_sync.clone(),
            downloads: state.downloads.clone(),
            tx: tx_lock.clone(),
            aws_client: aws_client.clone(),
            project,
            engine: state.engine.clone(),
            engine_path,
            max_cache_size_bytes: state.app_config.read().editor_cache_size_bytes(),
            transfer_acceleration: state.app_config.read().s3_transfer_acceleration,
        }
    };

    let (tx, rx) = tokio::sync::oneshot::channel::<Option<CoreError>>();
    let mut sequence = TaskSequence::new().with_completion_tx(tx);
    sequence.push(Box::new(download_op));
    let _ = state.operation_tx.send(sequence).await;

    let res: Result<Option<CoreError>, RecvError> = rx.await;
    if let Ok(Some(e)) = res {
        return Err(e);
    }

    Ok(Json(DownloadResponse {
        download_attempted: true,
    }))
}

/// Remove longtail's cached scan of a directory, if one is there.
///
/// It is longtail's own bookkeeping, never part of a build and never needed once the sync
/// that wrote it has ended. In the user's repo it is an untracked file of several
/// megabytes that nobody can account for.
fn discard_target_index(root: &Path) {
    let path = root.join(TARGET_INDEX_CACHE_NAME);
    match fs::remove_file(&path) {
        Ok(()) => info!("Removed a leftover {path:?}"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => warn!("Could not remove {path:?}: {e}"),
    }
}

/// Copy files from source to destination recursively.
/// From: https://nick.groenen.me/notes/recursively-copy-files-in-rust/
/// Everything this sync has put in the destination, half-written files included.
///
/// `fs::copy` truncates the destination before it writes, so a file the copy failed on is
/// still one we put there - it is just not the one we meant. Leaving it off the record
/// strands it: no later sync knows it was ours, and once a build stops shipping it,
/// nothing ever removes it. A locked file is the opposite case - the OS refused the open,
/// so those bytes are untouched and are not ours unless an earlier sync wrote them.
///
/// Paths from a record naming a different destination never come across. The layout
/// moved, so that record says nothing about what is here, and adopting its paths would
/// name files we never wrote as ours to delete.
fn paths_we_wrote(
    provided: &HashSet<String>,
    outcome: &CopyOutcome,
    previous: &CopiedFiles,
    destination: &Path,
) -> HashSet<String> {
    let mut ours: HashSet<String> = provided
        .iter()
        .filter(|path| !outcome.locked.contains(&PathBuf::from(path)))
        .cloned()
        .collect();

    if previous.destination == destination {
        ours.extend(previous.paths.iter().cloned());
    }

    ours
}

/// What the last sync copied into the repo.
///
/// The bound on what may be deleted. Only a path this recorded is ever a candidate, so a
/// file the user or git put there is not one - the reconcile cannot reach outside what it
/// previously wrote, whatever else the directory happens to contain.
#[derive(Debug, Default, Serialize, Deserialize)]
struct CopiedFiles {
    destination: PathBuf,
    paths: Vec<String>,
}

impl CopiedFiles {
    fn load(path: &Path) -> CopiedFiles {
        let Ok(bytes) = fs::read(path) else {
            return CopiedFiles::default();
        };
        serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            warn!("Ignoring unreadable copied-binaries record at {path:?}: {e}");
            CopiedFiles::default()
        })
    }

    fn save(&self, path: &Path) {
        let write = || -> std::io::Result<()> {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            let bytes = serde_json::to_vec_pretty(self)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            fs::write(path, bytes)
        };

        // Losing this costs one sync's worth of cleanup, not correctness.
        if let Err(e) = write() {
            warn!("Could not record which binaries were copied to {path:?}: {e}");
        }
    }
}

/// Write down what is now in the repo from this build, so the next sync can tell a file
/// it put there from one that was always the user's.
fn record_copied(manifest_path: &Path, destination: &Path, paths: &HashSet<String>) {
    CopiedFiles {
        destination: destination.to_path_buf(),
        paths: paths.iter().cloned().collect(),
    }
    .save(manifest_path);
}

fn copied_manifest_path(download_path: &Path) -> PathBuf {
    download_path.join("editor-binaries-copied.json")
}

/// Every file under `root`, named relative to it with forward slashes.
///
/// A failure to read any part of the tree is an error rather than a smaller answer. This
/// list decides what a later sync may delete from the user's repo: a subtree silently
/// missing from it would make everything under it look like a file this build dropped.
fn relative_paths(root: &Path) -> std::io::Result<HashSet<String>> {
    let mut out = HashSet::new();
    collect_relative_paths(root, root, &mut out)?;
    Ok(out)
}

fn collect_relative_paths(
    root: &Path,
    dir: &Path,
    out: &mut HashSet<String>,
) -> std::io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_relative_paths(root, &path, out)?;
        } else if let Ok(relative) = path.strip_prefix(root) {
            out.push_normalised(relative);
        }
    }
    Ok(())
}

trait PushNormalised {
    fn push_normalised(&mut self, path: &Path);
}

impl PushNormalised for HashSet<String> {
    fn push_normalised(&mut self, path: &Path) {
        self.insert(path.to_string_lossy().replace('\\', "/"));
    }
}

/// Whether two spellings of a path name the same file on this platform.
///
/// Windows and macOS hand back `Foo.dll` when asked for `foo.dll`, so a build that
/// changed only the case of a name would leave the old spelling looking stale while
/// pointing at the file just copied - and deleting it would remove that file. Linux keeps
/// the two apart, where folding case would be the wrong answer instead.
const IGNORE_CASE: bool = cfg!(any(windows, target_os = "macos"));

fn folded(path: &str, ignore_case: bool) -> Cow<'_, str> {
    if ignore_case {
        Cow::Owned(path.to_lowercase())
    } else {
        Cow::Borrowed(path)
    }
}

/// Paths a previous sync copied that this one does not provide.
///
/// Nothing is stale when the destination has changed: the record describes a different
/// directory, so it says nothing about this one.
fn stale_paths(
    previous: &CopiedFiles,
    destination: &Path,
    provided: &HashSet<String>,
) -> Vec<String> {
    stale_paths_matching(previous, destination, provided, IGNORE_CASE)
}

fn stale_paths_matching(
    previous: &CopiedFiles,
    destination: &Path,
    provided: &HashSet<String>,
    ignore_case: bool,
) -> Vec<String> {
    if previous.destination != destination {
        return Vec::new();
    }

    let provided: HashSet<Cow<str>> = provided.iter().map(|p| folded(p, ignore_case)).collect();

    previous
        .paths
        .iter()
        .filter(|path| !provided.contains(&folded(path, ignore_case)))
        .filter(|path| is_contained(Path::new(path)))
        .cloned()
        .collect()
}

/// Delete stale binaries, skipping anything git knows about.
///
/// The manifest already bounds this to files a previous sync wrote, but a file that has
/// since been committed is no longer ours to remove - deleting it would show up as an
/// unexplained deletion in the user's working tree.
///
/// Returns the stale paths still on disk afterwards.
async fn remove_stale(git_client: &git::Git, destination: &Path, stale: &[String]) -> Vec<String> {
    let tracked = tracked_among(git_client, destination, stale).await;
    delete_stale(destination, stale, &tracked)
}

/// Delete each stale binary git does not track, returning the ones still there after.
///
/// A file we could not delete - the editor still has it open - is still one we put in the
/// repo, and dropping it from the manifest would leave it there with nothing recording
/// where it came from, so no later sync would ever clean it up. The same goes for one git
/// reported as tracked, because "tracked" is also the answer when git could not be asked
/// at all. Handing them back keeps them on the list for next time.
fn delete_stale(destination: &Path, stale: &[String], tracked: &HashSet<String>) -> Vec<String> {
    let mut remaining = Vec::new();

    for relative in stale {
        let path = destination.join(relative);

        if tracked.contains(relative) {
            info!("Leaving {relative} alone: it is tracked by git");
            if path.exists() {
                remaining.push(relative.clone());
            }
            continue;
        }

        if !path.exists() {
            continue;
        }

        match fs::remove_file(&path) {
            Ok(()) => info!("Removed {path:?}, which this editor build no longer contains"),
            // Most likely the editor has it open. It will go on the next sync.
            Err(e) => {
                warn!("Could not remove stale binary {path:?}: {e}");
                remaining.push(relative.clone());
            }
        }
    }

    remaining
}

/// Whether a recorded path is one we could have written, and so one we may remove.
///
/// A manifest is data on disk. An absolute path would replace the destination when
/// joined, and `..` would climb out of it, so anything that is not a plain relative path
/// is refused rather than trusted.
fn is_contained(relative: &Path) -> bool {
    relative
        .components()
        .all(|c| matches!(c, Component::Normal(_)))
}

/// Which of `candidates` git tracks. On any failure, treats them all as tracked: the safe
/// answer when we cannot tell is to delete nothing.
async fn tracked_among(
    git_client: &git::Git,
    destination: &Path,
    candidates: &[String],
) -> HashSet<String> {
    let all_tracked = || -> HashSet<String> { candidates.iter().cloned().collect() };

    // git reports paths relative to the repo root, so everything below is in those terms.
    let Ok(within_repo) = destination.strip_prefix(&git_client.repo_path) else {
        // The binaries are not in this repo at all, so we cannot reason about them.
        warn!(
            "{destination:?} is not inside {:?}; removing none",
            git_client.repo_path
        );
        return all_tracked();
    };
    let dir = within_repo.to_string_lossy().replace('\\', "/");

    // One pathspec for the directory rather than one per candidate, so that matching a
    // recorded name against a tracked one happens here, where it can fold case the way the
    // filesystem does. Handing git the recorded spellings instead would miss a tracked file
    // whose name differs only in case - and a missed file is the one that gets deleted.
    //
    // --literal-pathspecs, because this is a directory name and not a pattern. A path
    // holding `[` would otherwise be read as a character class and match nothing.
    let mut args: Vec<&str> = vec!["--literal-pathspecs", "ls-files", "-z", "--"];
    if !dir.is_empty() {
        args.push(&dir);
    }

    // new_without_logs, because the default logs stdout as a single tracing event and -z
    // output has no newlines to break it up: for a destination at the repo root the
    // pathspec covers the whole repo, and that is one log line holding every tracked path
    // in it. The pathspec stays that wide on purpose - narrowing it to the candidates'
    // own paths is what --icase-pathspecs would be for, and git refuses to combine that
    // with --literal-pathspecs, which is the one guarding against a name like `Foo[1].dll`
    // being read as a pattern, matching nothing, and so being taken for untracked.
    //
    // -z, because by default git C-quotes any path with non-ASCII or special bytes -
    // `"Binaries/Win64/\303\244.dll"`, quotes and octal escapes included. Comparing that
    // against a real path never matches, which would read a tracked file as untracked and
    // delete it out of the working tree.
    match git_client
        .run_and_collect_output(&args, git::Opts::new_without_logs())
        .await
    {
        Ok(output) => tracked_among_output(&dir, candidates, &output, IGNORE_CASE),
        Err(e) => {
            warn!("Could not ask git which binaries are tracked, so removing none: {e}");
            all_tracked()
        }
    }
}

/// Which `candidates`, named relative to `dir`, appear in `git ls-files -z` output.
///
/// An exact comparison of whole paths rather than a suffix match: `Foo.dll` must not be
/// judged by whether some tracked `MyFoo.dll` ends with it.
fn tracked_among_output(
    dir: &str,
    candidates: &[String],
    output: &str,
    ignore_case: bool,
) -> HashSet<String> {
    // No trimming: -z output is already exact, and a filename may legitimately begin or
    // end with a space.
    let tracked: HashSet<Cow<str>> = output
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(|s| folded(s, ignore_case))
        .collect();

    candidates
        .iter()
        .filter(|relative| {
            let full = if dir.is_empty() {
                (*relative).clone()
            } else {
                format!("{dir}/{relative}")
            };
            tracked.contains(&folded(&full, ignore_case))
        })
        .cloned()
        .collect()
}

/// How many times a locked file is retried before giving up, and how long between.
///
/// Short on purpose. This is for the program that is already closing - the editor takes a
/// moment to release its DLLs - not for waiting out someone who has not closed it yet.
/// Anything longer is a dialog's job, not a sleep's.
const LOCK_RETRY_ATTEMPTS: usize = 3;
const LOCK_RETRY_DELAY: Duration = Duration::from_millis(400);

/// What a copy managed, and what it could not.
#[derive(Debug, Default)]
pub struct CopyOutcome {
    pub copied: usize,
    /// Files another program has open. Closing that program fixes these, which is why
    /// they are kept apart from everything else.
    pub locked: Vec<PathBuf>,
    /// Failures that closing a program will not fix - a full disk, a read-only volume.
    pub failed: Vec<(PathBuf, String)>,
}

impl CopyOutcome {
    fn is_ok(&self) -> bool {
        self.locked.is_empty() && self.failed.is_empty()
    }

    /// Something a user can act on: which files, and what to do about them.
    fn describe(&self, destination: &Path) -> String {
        // Ask the OS which programs actually hold these files, rather than listing the
        // usual suspects and leaving the user to work out which one it is. Empty when
        // nothing can be determined, including everywhere that is not Windows.
        let holders = ethos_core::utils::windows::programs_holding(
            &self
                .locked
                .iter()
                .map(|relative| destination.join(relative))
                .collect::<Vec<_>>(),
        );
        self.describe_with(destination, &holders)
    }

    fn describe_with(&self, destination: &Path, holders: &[String]) -> String {
        let mut parts = Vec::new();

        if !self.locked.is_empty() {
            let names: Vec<String> = self
                .locked
                .iter()
                .map(|p| p.display().to_string())
                .collect();

            let close = if holders.is_empty() {
                "Close Unreal Editor, the game, and your IDE, then sync again".to_string()
            } else {
                format!("Close {}, then sync again", holders.join(", "))
            };

            parts.push(format!(
                "These files are open in another program, so they could not be updated:\n  {}\n\
                 {close} - the download is kept, so it only re-copies these.",
                names.join("\n  ")
            ));
        }

        if !self.failed.is_empty() {
            let details: Vec<String> = self
                .failed
                .iter()
                .map(|(path, why)| format!("{}: {why}", path.display()))
                .collect();
            parts.push(format!(
                "These files could not be written to {}:\n  {}",
                destination.display(),
                details.join("\n  ")
            ));
        }

        parts.join("\n\n")
    }
}

/// Copy a tree, reporting every file it could not write rather than stopping at the first.
///
/// Stopping at the first is what made this brittle. One locked DLL failed the whole sync,
/// said nothing about which file, and left the repo half updated - with no way to tell
/// that from a corrupt download, which is how deleting the block cache on failure came to
/// look like a reasonable response to a file being open.
///
/// Errors reading the source tree are still fatal: that is our own staging directory, and
/// not being able to read it means something is wrong that carrying on would hide.
pub fn copy_recursively(
    source: impl AsRef<Path>,
    destination: impl AsRef<Path>,
) -> std::io::Result<CopyOutcome> {
    let mut outcome = CopyOutcome::default();
    copy_tree(source.as_ref(), destination.as_ref(), &mut outcome)?;

    // One short retry pass for anything locked, which catches the program that was already
    // on its way out while we were copying.
    for _ in 0..LOCK_RETRY_ATTEMPTS {
        if outcome.locked.is_empty() {
            break;
        }

        std::thread::sleep(LOCK_RETRY_DELAY);

        let still_locked = std::mem::take(&mut outcome.locked);
        for relative in still_locked {
            let from = source.as_ref().join(&relative);
            let to = destination.as_ref().join(&relative);
            match fs::copy(&from, &to) {
                Ok(_) => outcome.copied += 1,
                Err(e) if is_held_by_another_program(&e) => outcome.locked.push(relative),
                Err(e) => outcome.failed.push((relative, e.to_string())),
            }
        }
    }

    Ok(outcome)
}

fn copy_tree(source: &Path, destination: &Path, outcome: &mut CopyOutcome) -> std::io::Result<()> {
    copy_tree_from(source, source, destination, outcome)
}

fn copy_tree_from(
    root: &Path,
    source: &Path,
    destination: &Path,
    outcome: &mut CopyOutcome,
) -> std::io::Result<()> {
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let filetype = entry.file_type()?;
        if filetype.is_dir() {
            copy_tree_from(
                root,
                &entry.path(),
                &destination.join(entry.file_name()),
                outcome,
            )?;
        } else {
            let from = entry.path();
            let to = destination.join(entry.file_name());
            // Relative, so a retry can rebuild both sides and a message can name
            // something the user recognises rather than a full staging path.
            let relative = from.strip_prefix(root).unwrap_or(&from).to_path_buf();

            match fs::copy(&from, &to) {
                Ok(_) => outcome.copied += 1,
                Err(e) if is_held_by_another_program(&e) => {
                    warn!("{to:?} is open in another program");
                    outcome.locked.push(relative);
                }
                Err(e) => {
                    warn!("Could not write {to:?}: {e}");
                    outcome.failed.push((relative, e.to_string()));
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The old CLI wrote one of these into staging on every sync, so it is sitting in the
    /// staging directory of every machine that synced before this. The copy into the repo
    /// takes the staging tree wholesale, so it would go along with the binaries.
    #[test]
    fn a_leftover_target_index_is_discarded() {
        let dir = tempfile::tempdir().unwrap();
        let staging = dir.path();
        fs::create_dir_all(staging.join("Binaries/Win64")).unwrap();
        fs::write(staging.join("Binaries/Win64/Game.dll"), b"x").unwrap();
        fs::write(staging.join(TARGET_INDEX_CACHE_NAME), b"index").unwrap();

        discard_target_index(staging);

        assert!(!staging.join(TARGET_INDEX_CACHE_NAME).exists());
        assert!(
            staging.join("Binaries/Win64/Game.dll").exists(),
            "only longtail's own bookkeeping is removed"
        );
    }

    /// The common case by far: there is none, and that is not a failure.
    #[test]
    fn discarding_a_target_index_that_is_not_there_is_fine() {
        let dir = tempfile::tempdir().unwrap();
        discard_target_index(dir.path());
        assert!(!dir.path().join(TARGET_INDEX_CACHE_NAME).exists());
    }

    fn set(paths: &[&str]) -> HashSet<String> {
        paths.iter().map(|p| (*p).to_string()).collect()
    }

    fn copied(destination: &str, paths: &[&str]) -> CopiedFiles {
        CopiedFiles {
            destination: PathBuf::from(destination),
            paths: paths.iter().map(|p| (*p).to_string()).collect(),
        }
    }

    /// The whole point: a binary an earlier build shipped and this one does not.
    #[test]
    fn a_binary_the_new_build_dropped_is_stale() {
        let previous = copied(
            "/repo/Game",
            &["Binaries/Win64/Game.dll", "Binaries/Win64/Old.dll"],
        );
        let provided = set(&["Binaries/Win64/Game.dll"]);

        assert_eq!(
            stale_paths(&previous, Path::new("/repo/Game"), &provided),
            vec!["Binaries/Win64/Old.dll".to_string()]
        );
    }

    #[test]
    fn nothing_is_stale_when_the_build_still_provides_everything() {
        let previous = copied("/repo/Game", &["Binaries/Win64/Game.dll"]);
        let provided = set(&["Binaries/Win64/Game.dll", "Binaries/Win64/New.dll"]);

        assert!(stale_paths(&previous, Path::new("/repo/Game"), &provided).is_empty());
    }

    /// A record about a different directory says nothing about this one, and guessing
    /// would mean deleting from a path we never wrote to.
    #[test]
    fn a_record_for_another_destination_is_ignored() {
        let previous = copied("/repo/OtherGame", &["Binaries/Win64/Old.dll"]);
        let provided = set(&["Binaries/Win64/Game.dll"]);

        assert!(stale_paths(&previous, Path::new("/repo/Game"), &provided).is_empty());
    }

    /// A manifest is a file on disk. An absolute path would replace the destination when
    /// joined to it, and `..` would climb out of the repo entirely - so neither is ever a
    /// candidate for deletion, whatever the file says.
    #[test]
    fn a_path_that_escapes_the_destination_is_never_stale() {
        let previous = copied(
            "/repo/Game",
            &[
                "../../../etc/passwd",
                "/etc/passwd",
                "Binaries/Win64/Old.dll",
            ],
        );
        let provided = set(&[]);

        assert_eq!(
            stale_paths(&previous, Path::new("/repo/Game"), &provided),
            vec!["Binaries/Win64/Old.dll".to_string()],
            "only the plain relative path is a candidate"
        );
    }

    /// Nothing recorded means nothing was copied by us, so nothing is ours to remove.
    /// This is the first-run case, and the case where the record was lost.
    #[test]
    fn no_record_means_nothing_is_stale() {
        let provided = set(&["Binaries/Win64/Game.dll"]);

        assert!(
            stale_paths(&CopiedFiles::default(), Path::new("/repo/Game"), &provided).is_empty()
        );
    }

    #[test]
    fn the_record_survives_a_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("copied.json");

        let record = copied("/repo/Game", &["Binaries/Win64/Game.dll"]);
        record.save(&path);

        let loaded = CopiedFiles::load(&path);
        assert_eq!(loaded.destination, record.destination);
        assert_eq!(loaded.paths, record.paths);
    }

    /// A record we cannot parse must not be read as "nothing was ever copied" in a way
    /// that loses data - it simply means this sync cleans nothing.
    #[test]
    fn an_unreadable_record_deletes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("copied.json");
        fs::write(&path, b"{ not json").unwrap();

        let loaded = CopiedFiles::load(&path);
        assert!(loaded.paths.is_empty());
        assert!(stale_paths(&loaded, Path::new("/repo/Game"), &set(&[])).is_empty());
    }

    /// On Windows and macOS a build that re-cased a name still ships the same file, and
    /// the old spelling points straight at the new one - deleting it would delete what
    /// was just copied.
    #[test]
    fn a_re_cased_name_is_not_stale_where_case_does_not_separate_files() {
        let previous = copied("/repo/Game", &["Binaries/Win64/Game.dll"]);
        let provided = set(&["Binaries/Win64/game.dll"]);

        assert!(
            stale_paths_matching(&previous, Path::new("/repo/Game"), &provided, true).is_empty()
        );
        assert_eq!(
            stale_paths_matching(&previous, Path::new("/repo/Game"), &provided, false),
            vec!["Binaries/Win64/Game.dll".to_string()]
        );
    }

    #[test]
    fn a_tracked_binary_is_recognised_whatever_its_case() {
        let candidates = vec!["Binaries/Win64/Game.dll".to_string()];
        let output = "Game/Binaries/Win64/game.dll\0";

        assert_eq!(
            tracked_among_output("Game", &candidates, output, true),
            set(&["Binaries/Win64/Game.dll"])
        );
        assert!(tracked_among_output("Game", &candidates, output, false).is_empty());
    }

    /// The binaries directory is the repo root, so there is no prefix to add.
    #[test]
    fn candidates_match_when_the_destination_is_the_repo_root() {
        let candidates = vec!["Game.dll".to_string()];

        assert_eq!(
            tracked_among_output("", &candidates, "Game.dll\0", false),
            set(&["Game.dll"])
        );
    }

    /// A tracked `MyGame.dll` must not answer for `Game.dll`.
    #[test]
    fn a_longer_tracked_name_does_not_match_a_shorter_candidate() {
        let candidates = vec!["Binaries/Win64/Game.dll".to_string()];
        let output = "Game/Binaries/Win64/MyGame.dll\0";

        assert!(tracked_among_output("Game", &candidates, output, false).is_empty());
    }

    #[test]
    fn a_stale_binary_that_could_not_be_deleted_stays_on_the_record() {
        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path();
        fs::create_dir_all(destination.join("Binaries/Win64")).unwrap();
        fs::write(destination.join("Binaries/Win64/Gone.dll"), b"x").unwrap();
        fs::write(destination.join("Binaries/Win64/Kept.dll"), b"x").unwrap();

        let stale = vec![
            "Binaries/Win64/Gone.dll".to_string(),
            "Binaries/Win64/Kept.dll".to_string(),
            // Already gone: nothing left to record.
            "Binaries/Win64/Missing.dll".to_string(),
        ];
        let tracked = set(&["Binaries/Win64/Kept.dll"]);

        let remaining = delete_stale(destination, &stale, &tracked);

        assert!(!destination.join("Binaries/Win64/Gone.dll").exists());
        assert!(destination.join("Binaries/Win64/Kept.dll").exists());
        assert_eq!(remaining, vec!["Binaries/Win64/Kept.dll".to_string()]);
    }

    #[test]
    fn relative_paths_are_found_recursively_and_normalised() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("Binaries/Win64")).unwrap();
        fs::write(root.join("Binaries/Win64/Game.dll"), b"x").unwrap();
        fs::write(root.join("top.txt"), b"y").unwrap();

        let found = relative_paths(root).unwrap();

        assert_eq!(found.len(), 2);
        assert!(found.contains("Binaries/Win64/Game.dll"), "{found:?}");
        assert!(found.contains("top.txt"), "{found:?}");
    }
}

#[cfg(test)]
mod copy_tests {
    use super::*;

    fn write(path: &Path, contents: &[u8]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    #[test]
    fn a_whole_tree_is_copied() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("staging");
        let destination = dir.path().join("repo");
        write(&source.join("Binaries/Win64/Game.dll"), b"game");
        write(&source.join("Binaries/Win64/Editor.dll"), b"editor");

        let outcome = copy_recursively(&source, &destination).unwrap();

        assert!(outcome.is_ok(), "{outcome:?}");
        assert_eq!(outcome.copied, 2);
        assert_eq!(
            fs::read(destination.join("Binaries/Win64/Game.dll")).unwrap(),
            b"game"
        );
    }

    /// The point of the rewrite: one file nobody can write must not cost the other
    /// thirty. Before this, the first failure aborted the copy and left the repo in a
    /// state nothing could describe.
    #[cfg(unix)]
    #[test]
    fn a_file_that_cannot_be_written_does_not_stop_the_others() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("staging");
        let destination = dir.path().join("repo");

        write(&source.join("Locked.dll"), b"new");
        write(&source.join("Fine.dll"), b"new");
        write(&source.join("AlsoFine.dll"), b"new");

        // Stand-in for a file another program holds open: unwritable, for the same reason
        // as far as fs::copy is concerned.
        write(&destination.join("Locked.dll"), b"old");
        let locked = destination.join("Locked.dll");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o444)).unwrap();

        let outcome = copy_recursively(&source, &destination).unwrap();

        assert_eq!(outcome.locked, vec![PathBuf::from("Locked.dll")]);
        assert_eq!(outcome.copied, 2, "the other two still copied");
        assert_eq!(fs::read(destination.join("Fine.dll")).unwrap(), b"new");
        assert_eq!(
            fs::read(&locked).unwrap(),
            b"old",
            "the locked file is untouched, not half written"
        );

        fs::set_permissions(&locked, fs::Permissions::from_mode(0o644)).unwrap();
    }

    /// The message has to name the files, because "sync failed" is what sent people
    /// looking through logs in the first place.
    #[cfg(unix)]
    #[test]
    fn the_failure_names_the_files_and_says_what_to_do() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("staging");
        let destination = dir.path().join("repo");
        write(&source.join("Binaries/Win64/Game.dll"), b"new");
        write(&destination.join("Binaries/Win64/Game.dll"), b"old");
        let locked = destination.join("Binaries/Win64/Game.dll");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o444)).unwrap();

        let outcome = copy_recursively(&source, &destination).unwrap();
        let message = outcome.describe(&destination);

        assert!(message.contains("Game.dll"), "{message}");
        assert!(message.contains("Close Unreal Editor"), "{message}");

        // When the OS can name the programs, say those instead of listing suspects.
        let named = outcome.describe_with(&destination, &["UnrealEditor.exe".to_string()]);
        assert!(named.contains("Close UnrealEditor.exe"), "{named}");
        assert!(
            !named.contains("and your IDE"),
            "no need to guess once we know: {named}"
        );

        fs::set_permissions(&locked, fs::Permissions::from_mode(0o644)).unwrap();
    }

    /// Reading our own staging directory failing is a different kind of problem, and
    /// carrying on would hide it.
    #[test]
    fn an_unreadable_source_is_still_fatal() {
        let dir = tempfile::tempdir().unwrap();

        assert!(copy_recursively(dir.path().join("missing"), dir.path().join("repo")).is_err());
    }
}

#[cfg(test)]
mod manifest_tests {
    use super::*;

    /// A copy that could not finish still wrote most of its files. Forgetting them means
    /// a later build that drops one can never reconcile it away, because nothing records
    /// that we put it there.
    #[test]
    fn a_partial_copy_still_records_what_it_wrote() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = dir.path().join("copied.json");
        let destination = PathBuf::from("/repo/Game");

        let previous: HashSet<String> = ["FromLastTime.dll".to_string()].into_iter().collect();
        record_copied(&manifest, &destination, &previous);

        // This sync wrote one file and could not write the other.
        let mut ours: HashSet<String> = ["Written.dll".to_string()].into_iter().collect();
        ours.extend(CopiedFiles::load(&manifest).paths);
        record_copied(&manifest, &destination, &ours);

        let recorded = CopiedFiles::load(&manifest);
        let mut paths = recorded.paths.clone();
        paths.sort();
        assert_eq!(
            paths,
            vec!["FromLastTime.dll".to_string(), "Written.dll".to_string()],
            "both what we just wrote and what an earlier sync left are ours"
        );
        assert_eq!(recorded.destination, destination);
    }
}

#[cfg(test)]
mod partial_copy_bound_tests {
    use super::*;

    fn provided(paths: &[&str]) -> HashSet<String> {
        paths.iter().map(|p| (*p).to_string()).collect()
    }

    /// A manifest describing another destination - the layout changed, so the binaries
    /// moved between `repo/` and `repo/<Project>/` - says nothing about this one, and
    /// merging its paths in would name files we never wrote as ours to delete later.
    #[test]
    fn a_partial_copy_does_not_adopt_another_destinations_paths() {
        let here = PathBuf::from("/repo/Game");

        let previous = CopiedFiles {
            destination: PathBuf::from("/repo"),
            paths: vec!["Binaries/Win64/Old.dll".to_string()],
        };
        let ours = paths_we_wrote(
            &provided(&["Binaries/Win64/New.dll"]),
            &CopyOutcome::default(),
            &previous,
            &here,
        );

        assert_eq!(ours, provided(&["Binaries/Win64/New.dll"]));
    }

    #[test]
    fn the_same_destinations_paths_do_come_across() {
        let here = PathBuf::from("/repo/Game");

        let previous = CopiedFiles {
            destination: here.clone(),
            paths: vec!["Binaries/Win64/Old.dll".to_string()],
        };
        let ours = paths_we_wrote(
            &provided(&["Binaries/Win64/New.dll"]),
            &CopyOutcome::default(),
            &previous,
            &here,
        );

        assert_eq!(
            ours,
            provided(&["Binaries/Win64/New.dll", "Binaries/Win64/Old.dll"])
        );
    }

    /// The half-written file. The copy truncated it before it failed, so it is ours even
    /// though it never finished - and only the record makes it ours to clean up.
    #[test]
    fn a_file_the_copy_failed_on_is_still_recorded() {
        let here = PathBuf::from("/repo/Game");
        let outcome = CopyOutcome {
            failed: vec![(
                PathBuf::from("Binaries/Win64/Broken.dll"),
                "No space left on device".to_string(),
            )],
            ..CopyOutcome::default()
        };

        let ours = paths_we_wrote(
            &provided(&["Binaries/Win64/Broken.dll", "Binaries/Win64/Game.dll"]),
            &outcome,
            &CopiedFiles::default(),
            &here,
        );

        assert!(ours.contains("Binaries/Win64/Broken.dll"), "{ours:?}");
    }

    /// A locked file was never opened for writing, so whatever is there is not ours -
    /// unless an earlier sync put it there, which the record says separately.
    #[test]
    fn a_locked_file_is_not_recorded_as_ours() {
        let here = PathBuf::from("/repo/Game");
        let outcome = CopyOutcome {
            locked: vec![PathBuf::from("Binaries/Win64/Locked.dll")],
            ..CopyOutcome::default()
        };

        let ours = paths_we_wrote(
            &provided(&["Binaries/Win64/Locked.dll", "Binaries/Win64/Game.dll"]),
            &outcome,
            &CopiedFiles::default(),
            &here,
        );

        assert_eq!(ours, provided(&["Binaries/Win64/Game.dll"]));
    }
}
