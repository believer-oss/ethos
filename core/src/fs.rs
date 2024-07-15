use directories_next::ProjectDirs;
use std::fs;
use std::path::PathBuf;
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
