use directories_next::ProjectDirs;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Debug, Clone)]
pub struct FsEntry {
    pub name: String,
    pub last_modified: SystemTime,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct LocalDownloadPath(pub PathBuf);

impl std::ops::Deref for LocalDownloadPath {
    type Target = PathBuf;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl LocalDownloadPath {
    pub fn new(app_name: &str) -> Self {
        if let Some(proj_dirs) = ProjectDirs::from("", "", app_name) {
            return LocalDownloadPath(proj_dirs.data_dir().to_path_buf());
        };
        // Fallback path off of root, since we don't know where we are?
        LocalDownloadPath(PathBuf::from("/believer"))
    }
}

pub fn check_dir(path: PathBuf) -> Option<FsEntry> {
    let md = fs::metadata(path.clone());
    let basename = path.file_name().unwrap_or_default();
    match md {
        Ok(md) => {
            let last_modified = md.modified().unwrap_or(SystemTime::now());
            Some(FsEntry {
                name: basename.to_string_lossy().to_string(),
                last_modified,
            })
        }
        Err(_) => None,
    }
}

/// Clears the read-only attribute on `path` so the file can be overwritten or
/// removed.
///
/// Working-tree files land read-only through several routes — git-lfs marks
/// lockable files read-only when the user doesn't hold the lock (see the
/// `GIT_LFS_SET_LOCKABLE_READONLY` handling in [`crate::operations`]), and
/// content copied in from other tooling often carries the attribute too. On
/// Windows, opening such a file for writing fails with "Access is denied".
///
/// A missing file is not an error, so this can be called as a precondition
/// before writing a path that may not exist yet.
pub fn clear_readonly(path: &Path) -> std::io::Result<()> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };

    let mut perms = metadata.permissions();
    if !perms.readonly() {
        return Ok(());
    }

    // `set_readonly(false)` hands out write bits to group and other as well on
    // unix; the owner bit is all that's needed to rewrite the file.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(perms.mode() | 0o200);
    }
    // Clippy's lint targets the unix meaning of this call, which the cfg rules out.
    #[cfg(not(unix))]
    #[allow(clippy::permissions_set_readonly_false)]
    perms.set_readonly(false);

    fs::set_permissions(path, perms)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set_readonly(path: &Path) {
        let mut perms = fs::metadata(path).unwrap().permissions();
        perms.set_readonly(true);
        fs::set_permissions(path, perms).unwrap();
    }

    #[test]
    fn clear_readonly_allows_overwrite_and_removal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("locked.uasset");
        fs::write(&path, "original").unwrap();
        set_readonly(&path);

        clear_readonly(&path).unwrap();

        assert!(!fs::metadata(&path).unwrap().permissions().readonly());
        fs::File::create(&path).unwrap();
        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn clear_readonly_leaves_writable_files_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("writable.txt");
        fs::write(&path, "original").unwrap();
        let before = fs::metadata(&path).unwrap().permissions();

        clear_readonly(&path).unwrap();

        assert_eq!(fs::metadata(&path).unwrap().permissions(), before);
    }

    #[test]
    fn clear_readonly_ignores_missing_files() {
        let dir = tempfile::tempdir().unwrap();
        clear_readonly(&dir.path().join("never-existed")).unwrap();
    }
}
