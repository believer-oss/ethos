use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::anyhow;
use axum::extract::State;
use axum::Json;
use serde::{Deserialize, Serialize};
use tracing::{info, instrument, warn};
use zip::write::FileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::engine::EngineProvider;
use crate::state::AppState;
use ethos_core::types::errors::CoreError;
use ethos_core::types::repo::FileState;

use super::sanitize_repo_path;

const MANIFEST_NAME: &str = ".friendshipper-zip-manifest.json";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ZipManifestEntry {
    pub path: String,
    #[serde(default)]
    pub display_name: String,
    pub state: FileState,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ZipManifest {
    pub version: u32,
    pub created_by: String,
    pub created_at: String,
    pub entries: Vec<ZipManifestEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ZipLocalChangesRequest {
    pub files: Vec<String>,
    pub destination: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ZipLocalChangesResponse {
    pub destination: String,
    pub file_count: usize,
}

/// Resolves an absolute path provided by the frontend (file dialog output) to a canonical
/// `PathBuf`. We don't restrict location since this is a destination path the user chose,
/// but we do reject empty input.
fn resolve_absolute_destination(input: &str) -> Result<PathBuf, CoreError> {
    if input.trim().is_empty() {
        return Err(CoreError::Input(anyhow!("Destination path is empty")));
    }
    Ok(PathBuf::from(input))
}

/// Builds an in-tree absolute path from the repo root and a sanitized repo-relative path.
fn join_repo_path(repo_root: &Path, rel: &str) -> PathBuf {
    let mut p = repo_root.to_path_buf();
    for seg in rel.split('/') {
        p.push(seg);
    }
    p
}

#[instrument(skip(state, req))]
pub async fn zip_local_changes_handler<T>(
    State(state): State<AppState<T>>,
    Json(req): Json<ZipLocalChangesRequest>,
) -> Result<Json<ZipLocalChangesResponse>, CoreError>
where
    T: EngineProvider,
{
    if req.files.is_empty() {
        return Err(CoreError::Input(anyhow!("No files selected")));
    }

    let repo_path = state.app_config.read().repo_path.clone();
    let repo_root = PathBuf::from(&repo_path);
    if !repo_root.is_dir() {
        return Err(CoreError::Internal(anyhow!(
            "Repo path does not exist: {}",
            repo_path
        )));
    }

    let dest_path = resolve_absolute_destination(&req.destination)?;
    if let Some(parent) = dest_path.parent() {
        if !parent.exists() {
            fs::create_dir_all(parent).map_err(|e| {
                CoreError::Internal(anyhow!("Failed to create destination directory: {}", e))
            })?;
        }
    }

    // Pull current repo state so we can record file states for each entry.
    let (modified_files, untracked_files) = {
        let status = state.repo_status.read();
        (
            status.modified_files.clone(),
            status.untracked_files.clone(),
        )
    };

    let user_display_name = state.app_config.read().user_display_name.clone();

    let dest_file = File::create(&dest_path)
        .map_err(|e| CoreError::Internal(anyhow!("Failed to create zip: {}", e)))?;
    let mut writer = ZipWriter::new(BufWriter::new(dest_file));
    let options: FileOptions = FileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .unix_permissions(0o644);

    let mut manifest_entries: Vec<ZipManifestEntry> = Vec::with_capacity(req.files.len());

    for raw_path in &req.files {
        let rel = sanitize_repo_path(raw_path)?;
        if rel.is_empty() {
            return Err(CoreError::Input(anyhow!("Empty file path provided")));
        }

        let (state_value, display_name) = match modified_files.get(&rel) {
            Some(f) => (f.state.clone(), f.display_name.clone()),
            None => match untracked_files.get(&rel) {
                Some(f) => (f.state.clone(), f.display_name.clone()),
                None => (FileState::Unknown, String::new()),
            },
        };

        manifest_entries.push(ZipManifestEntry {
            path: rel.clone(),
            display_name,
            state: state_value.clone(),
        });

        // Deleted files have no working-tree contents; the manifest records
        // the deletion and the importer removes the file on extract.
        if matches!(state_value, FileState::Deleted) {
            continue;
        }

        let abs_path = join_repo_path(&repo_root, &rel);
        if !abs_path.exists() {
            warn!(
                "Skipping file content for {}: not present in working tree",
                rel
            );
            continue;
        }
        if !abs_path.is_file() {
            warn!("Skipping {}: not a regular file", rel);
            continue;
        }

        writer
            .start_file(&rel, options)
            .map_err(|e| CoreError::Internal(anyhow!("Failed to start zip entry: {}", e)))?;
        let src = File::open(&abs_path)
            .map_err(|e| CoreError::Internal(anyhow!("Failed to open {}: {}", rel, e)))?;
        let mut src = BufReader::with_capacity(64 * 1024, src);
        std::io::copy(&mut src, &mut writer)
            .map_err(|e| CoreError::Internal(anyhow!("Failed to write {} to zip: {}", rel, e)))?;
    }

    let manifest = ZipManifest {
        version: 1,
        created_by: user_display_name,
        created_at: chrono::Utc::now().to_rfc3339(),
        entries: manifest_entries,
    };
    let manifest_json = serde_json::to_vec_pretty(&manifest)
        .map_err(|e| CoreError::Internal(anyhow!("Failed to serialize manifest: {}", e)))?;
    writer
        .start_file(MANIFEST_NAME, options)
        .map_err(|e| CoreError::Internal(anyhow!("Failed to start manifest entry: {}", e)))?;
    writer
        .write_all(&manifest_json)
        .map_err(|e| CoreError::Internal(anyhow!("Failed to write manifest: {}", e)))?;

    writer
        .finish()
        .map_err(|e| CoreError::Internal(anyhow!("Failed to finish zip: {}", e)))?;

    info!("Wrote {} files to {}", req.files.len(), dest_path.display());

    Ok(Json(ZipLocalChangesResponse {
        destination: dest_path.to_string_lossy().to_string(),
        file_count: req.files.len(),
    }))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportZipQuery {
    pub source: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ZipPreviewEntry {
    pub path: String,
    pub display_name: String,
    pub state: FileState,
    pub size: u64,
    /// True if this path is currently in the user's modified or untracked file
    /// list — extracting will overwrite uncommitted local changes.
    pub conflicts_with_local: bool,
    /// True if a file at this path already exists on disk. Combined with
    /// `conflicts_with_local`, the UI calls out the most severe case.
    pub exists_on_disk: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ZipPreviewResponse {
    pub source: String,
    pub created_by: Option<String>,
    pub created_at: Option<String>,
    pub entries: Vec<ZipPreviewEntry>,
}

fn read_zip_manifest<R: Read + std::io::Seek>(archive: &mut ZipArchive<R>) -> Option<ZipManifest> {
    match archive.by_name(MANIFEST_NAME) {
        Ok(mut entry) => {
            let mut buf = String::new();
            if entry.read_to_string(&mut buf).is_err() {
                return None;
            }
            serde_json::from_str(&buf).ok()
        }
        Err(_) => None,
    }
}

#[instrument(skip(state, req))]
pub async fn preview_import_zip_handler<T>(
    State(state): State<AppState<T>>,
    axum::extract::Query(req): axum::extract::Query<ImportZipQuery>,
) -> Result<Json<ZipPreviewResponse>, CoreError>
where
    T: EngineProvider,
{
    let source = PathBuf::from(&req.source);
    if !source.is_file() {
        return Err(CoreError::Input(anyhow!(
            "Zip file does not exist: {}",
            req.source
        )));
    }

    let file = File::open(&source)
        .map_err(|e| CoreError::Internal(anyhow!("Failed to open zip: {}", e)))?;
    let mut archive = ZipArchive::new(BufReader::new(file))
        .map_err(|e| CoreError::Input(anyhow!("Not a valid zip file: {}", e)))?;

    let manifest = read_zip_manifest(&mut archive);

    let repo_path = state.app_config.read().repo_path.clone();
    let repo_root = PathBuf::from(&repo_path);

    let (modified_files, untracked_files) = {
        let status = state.repo_status.read();
        (
            status.modified_files.clone(),
            status.untracked_files.clone(),
        )
    };
    let in_local =
        |rel: &str| -> bool { modified_files.contains(rel) || untracked_files.contains(rel) };

    // If a manifest is present, treat it as authoritative for the entry list
    // (so deletions show up in the preview even though they are not zip entries).
    let mut entries: Vec<ZipPreviewEntry> = Vec::new();
    if let Some(m) = &manifest {
        for entry in &m.entries {
            let rel = sanitize_repo_path(&entry.path)?;
            if rel.is_empty() {
                continue;
            }
            let abs = join_repo_path(&repo_root, &rel);
            let size = if matches!(entry.state, FileState::Deleted) {
                0
            } else {
                archive.by_name(&rel).map(|e| e.size()).unwrap_or(0)
            };
            entries.push(ZipPreviewEntry {
                path: rel.clone(),
                display_name: entry.display_name.clone(),
                state: entry.state.clone(),
                size,
                conflicts_with_local: in_local(&rel),
                exists_on_disk: abs.exists(),
            });
        }
    } else {
        // Fall back to the raw zip listing.
        for i in 0..archive.len() {
            let entry = archive
                .by_index(i)
                .map_err(|e| CoreError::Internal(anyhow!("Failed to read zip entry: {}", e)))?;
            if entry.is_dir() {
                continue;
            }
            let name = entry.name().to_string();
            if name == MANIFEST_NAME {
                continue;
            }
            let rel = sanitize_repo_path(&name)?;
            if rel.is_empty() {
                continue;
            }
            let abs = join_repo_path(&repo_root, &rel);
            entries.push(ZipPreviewEntry {
                path: rel.clone(),
                display_name: String::new(),
                state: FileState::Unknown,
                size: entry.size(),
                conflicts_with_local: in_local(&rel),
                exists_on_disk: abs.exists(),
            });
        }
    }

    Ok(Json(ZipPreviewResponse {
        source: req.source.clone(),
        created_by: manifest.as_ref().map(|m| m.created_by.clone()),
        created_at: manifest.as_ref().map(|m| m.created_at.clone()),
        entries,
    }))
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportZippedChangesRequest {
    pub source: String,
    /// Optional repo-relative paths to extract. When `None` or empty, every entry in the
    /// zip is extracted. When set, only entries whose path is in the list are written
    /// (and only deletions in the list are applied).
    #[serde(default)]
    pub files: Option<Vec<String>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportZippedChangesResponse {
    pub extracted: usize,
    pub deleted: usize,
}

#[instrument(skip(state, req))]
pub async fn import_zipped_changes_handler<T>(
    State(state): State<AppState<T>>,
    Json(req): Json<ImportZippedChangesRequest>,
) -> Result<Json<ImportZippedChangesResponse>, CoreError>
where
    T: EngineProvider,
{
    let source = PathBuf::from(&req.source);
    if !source.is_file() {
        return Err(CoreError::Input(anyhow!(
            "Zip file does not exist: {}",
            req.source
        )));
    }

    let repo_path = state.app_config.read().repo_path.clone();
    let repo_root = PathBuf::from(&repo_path);
    if !repo_root.is_dir() {
        return Err(CoreError::Internal(anyhow!(
            "Repo path does not exist: {}",
            repo_path
        )));
    }

    let file = File::open(&source)
        .map_err(|e| CoreError::Internal(anyhow!("Failed to open zip: {}", e)))?;
    let mut archive = ZipArchive::new(BufReader::new(file))
        .map_err(|e| CoreError::Input(anyhow!("Not a valid zip file: {}", e)))?;

    let manifest = read_zip_manifest(&mut archive);

    // Build the optional allow-list of repo-relative paths the caller wants to import.
    // Sanitize each so the filter matches the same normalized paths we extract under.
    let filter: Option<HashSet<String>> = match &req.files {
        Some(list) if !list.is_empty() => {
            let mut set = HashSet::with_capacity(list.len());
            for raw in list {
                let rel = sanitize_repo_path(raw)?;
                if !rel.is_empty() {
                    set.insert(rel);
                }
            }
            Some(set)
        }
        _ => None,
    };
    let is_allowed = |rel: &str| -> bool {
        match &filter {
            Some(set) => set.contains(rel),
            None => true,
        }
    };

    // When a manifest is present it is authoritative for what the import is allowed
    // to touch — the preview is built from the manifest, so a zip that contains
    // extra entries beyond what the manifest advertises must NOT silently sneak
    // them onto disk. Build a set of manifest-listed (non-deleted) repo paths;
    // extraction skips anything outside it.
    let manifest_extract_allow: Option<HashSet<String>> = manifest.as_ref().map(|m| {
        m.entries
            .iter()
            .filter(|e| !matches!(e.state, FileState::Deleted))
            .filter_map(|e| sanitize_repo_path(&e.path).ok())
            .filter(|s| !s.is_empty())
            .collect()
    });

    let mut extracted = 0usize;
    let mut deleted = 0usize;

    // First, apply any deletions called out in the manifest. We rely on the
    // sanitizer to prevent traversal outside the repo.
    if let Some(m) = &manifest {
        for entry in &m.entries {
            if matches!(entry.state, FileState::Deleted) {
                let rel = sanitize_repo_path(&entry.path)?;
                if rel.is_empty() || !is_allowed(&rel) {
                    continue;
                }
                let abs = join_repo_path(&repo_root, &rel);
                if abs.is_file() {
                    fs::remove_file(&abs).map_err(|e| {
                        CoreError::Internal(anyhow!("Failed to remove {}: {}", rel, e))
                    })?;
                    deleted += 1;
                }
            }
        }
    }

    // Then extract every regular-file entry, skipping the manifest itself and any
    // entries not advertised by the manifest (when present).
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| CoreError::Internal(anyhow!("Failed to read zip entry: {}", e)))?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().to_string();
        if name == MANIFEST_NAME {
            continue;
        }
        let rel = sanitize_repo_path(&name)?;
        if rel.is_empty() || !is_allowed(&rel) {
            continue;
        }
        if let Some(allow) = &manifest_extract_allow {
            if !allow.contains(&rel) {
                warn!("Skipping zip entry {}: not advertised by manifest", rel);
                continue;
            }
        }

        let abs = join_repo_path(&repo_root, &rel);
        if let Some(parent) = abs.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                CoreError::Internal(anyhow!(
                    "Failed to create directory {}: {}",
                    parent.display(),
                    e
                ))
            })?;
        }

        let out = File::create(&abs)
            .map_err(|e| CoreError::Internal(anyhow!("Failed to create {}: {}", rel, e)))?;
        let mut out = BufWriter::new(out);
        std::io::copy(&mut entry, &mut out)
            .map_err(|e| CoreError::Internal(anyhow!("Failed to extract {}: {}", rel, e)))?;
        out.flush()
            .map_err(|e| CoreError::Internal(anyhow!("Failed to flush {}: {}", rel, e)))?;
        extracted += 1;
    }

    info!(
        "Imported zip from {}: extracted {}, deleted {}",
        req.source, extracted, deleted
    );

    Ok(Json(ImportZippedChangesResponse { extracted, deleted }))
}
