use crate::types::locks::Lock;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum SubmitStatus {
    #[default]
    Ok,
    CheckoutRequired,
    CheckedOutByOtherUser,
    Unmerged,
    Conflicted,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum FileState {
    #[default]
    Unknown,
    Added,
    Modified,
    Deleted,
    Unmerged,
}

impl FileState {
    fn parse(index_state: &str, working_state: &str) -> FileState {
        if working_state == "?" {
            return FileState::Added;
        }
        if working_state == "M" {
            return FileState::Modified;
        }
        if working_state == "D" {
            return FileState::Deleted;
        }

        if working_state == "U" {
            return FileState::Unmerged;
        }

        if index_state == "A" {
            return FileState::Added;
        }
        if index_state == "M" {
            return FileState::Modified;
        }
        if index_state == "D" {
            return FileState::Deleted;
        }
        if index_state == "U" {
            return FileState::Unmerged;
        }

        FileState::Unknown
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct File {
    pub path: String,
    pub display_name: String,
    pub state: FileState,
    pub is_staged: bool,
    pub locked_by: String,
    pub submit_status: SubmitStatus,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ChangeSet {
    pub name: String,
    pub files: Vec<File>,
    pub open: bool,
    pub checked: bool,
    pub indeterminate: bool,
}

impl File {
    pub fn from_status_line(line: &str) -> Self {
        //  M test/test.dll
        // ?? test/test.dll
        // A  test/test.dll

        let mut chars = line.chars();
        let index_state = match chars.next() {
            Some(c) => c.to_string().trim().to_owned(),
            None => String::new(),
        };
        let working_state = match chars.next() {
            Some(c) => c.to_string().trim().to_owned(),
            None => String::new(),
        };
        let state = FileState::parse(&index_state, &working_state);

        // skip the space
        chars.next();
        let path = chars.collect::<String>();

        // remove leading and trailing quote escapes
        let path = path.trim_matches('"').to_owned();

        Self {
            path,
            display_name: String::new(),
            state,
            is_staged: !index_state.is_empty(),
            locked_by: String::new(),
            submit_status: SubmitStatus::Ok,
            url: None,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct FileList(pub Vec<File>);

impl FileList {
    pub fn contains(&self, file: &str) -> bool {
        self.0.iter().any(|f| f.path == *file)
    }

    pub fn get(&self, file: &str) -> Option<&File> {
        self.0.iter().find(|f| f.path == *file)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl IntoIterator for FileList {
    type Item = File;
    type IntoIter = std::vec::IntoIter<Self::Item>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

pub type RepoStatusRef = std::sync::Arc<parking_lot::RwLock<RepoStatus>>;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepoStatus {
    pub detached_head: bool,
    pub last_updated: DateTime<Utc>,

    // branches
    pub branch: String,
    pub remote_branch: String,

    // remote info
    pub repo_owner: String,
    pub repo_name: String,

    // commits
    pub commit_head: String,
    pub commits_ahead: u32,
    pub commits_behind: u32,
    pub commits_ahead_of_trunk: u32,
    pub commits_behind_trunk: u32,
    pub commit_head_origin: String,

    // DLLs
    pub origin_has_new_dlls: bool,
    pub pull_dlls: bool,
    pub dll_commit_local: String,
    pub dll_commit_remote: String,

    // file paths
    pub untracked_files: FileList,
    pub modified_files: FileList,

    // helpers for detecting changes
    pub has_staged_changes: bool,
    pub has_local_changes: bool,

    // upstream
    pub conflict_upstream: bool,
    pub conflicts: Vec<String>,

    pub modified_upstream: Vec<String>,

    // locks
    pub lock_user: String,
    pub locks_ours: Vec<Lock>,
    pub locks_theirs: Vec<Lock>,
}

impl RepoStatus {
    pub fn new() -> Self {
        Self {
            detached_head: false,
            last_updated: Utc::now(),
            branch: String::new(),
            remote_branch: String::new(),
            repo_owner: String::new(),
            repo_name: String::new(),
            commit_head: String::new(),
            commits_ahead: 0,
            commits_behind: 0,
            commits_ahead_of_trunk: 0,
            commits_behind_trunk: 0,
            commit_head_origin: String::new(),
            origin_has_new_dlls: false,
            pull_dlls: false,
            dll_commit_local: String::new(),
            dll_commit_remote: String::new(),
            untracked_files: FileList::default(),
            modified_files: FileList::default(),
            has_staged_changes: false,
            has_local_changes: false,
            conflict_upstream: false,
            conflicts: vec![],
            modified_upstream: vec![],
            lock_user: String::new(),
            locks_ours: vec![],
            locks_theirs: vec![],
        }
    }

    pub fn parse_file_line(&mut self, line: &str) {
        if line.starts_with("##") {
            self.parse_branch_string(line);

            return;
        }

        let file = File::from_status_line(line);

        if file.is_staged {
            self.has_staged_changes = true;
        }

        if file.state == FileState::Added {
            if !self.untracked_files.contains(&file.path) {
                self.untracked_files.0.push(file);
            }
        } else if !self.modified_files.contains(&file.path) {
            self.modified_files.0.push(file);
        }
    }

    pub fn parse_branch_string(&mut self, line: &str) {
        // ## ar/friendshipper-git...origin/ar/friendshipper-git [ahead 1, behind 1]
        // ## ar/friendshipper-git...origin/ar/friendshipper-git [ahead 1]
        // ## HEAD (no branch)
        // ## test-branch

        // detached head
        if line == "## HEAD (no branch)" {
            self.detached_head = true;

            return;
        }

        let parts = line.split(' ').collect::<Vec<_>>();

        let branch_parts = parts[1].split("...").collect::<Vec<_>>();
        if branch_parts.len() == 2 {
            branch_parts[0].clone_into(&mut self.branch);
            branch_parts[1].clone_into(&mut self.remote_branch);
        } else {
            // local branch with no remote
            parts[1].clone_into(&mut self.branch);

            return;
        }

        if line.contains("ahead") && line.contains("behind") {
            let ahead = parts[3].replace(',', "").parse::<u32>().unwrap();
            let behind = parts[5].replace(']', "").parse::<u32>().unwrap();

            self.commits_ahead = ahead;
            self.commits_behind = behind;
        } else if line.contains("ahead") {
            let ahead = parts[3].replace(']', "").parse::<u32>().unwrap();

            self.commits_ahead = ahead;
            self.commits_behind = 0;
        } else if line.contains("behind") {
            let behind = parts[3].replace(']', "").parse::<u32>().unwrap();

            self.commits_behind = behind;
            self.commits_ahead = 0;
        } else {
            self.commits_ahead = 0;
            self.commits_behind = 0;
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CloneRequest {
    pub url: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushRequest {
    #[serde(rename = "commitMessage")]
    pub commit_message: String,
    pub files: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RevertFilesRequest {
    pub files: Vec<String>,
    #[serde(rename = "skipEngineCheck")]
    pub skip_engine_check: bool,
    #[serde(rename = "takeSnapshot")]
    pub take_snapshot: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RebaseStatusResponse {
    pub rebase_merge_exists: bool,
    pub head_name_exists: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigureUserRequest {
    pub name: String,
    pub email: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LockRequest {
    pub paths: Vec<String>,
    pub force: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PullResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conflicts: Option<Vec<String>>,
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileHistoryRevision {
    pub filename: String,
    pub commit_id: String,
    pub short_commit_id: String,
    pub commit_id_number: u32,
    pub revision_number: u32,
    pub file_hash: String,
    pub description: String,
    pub user_name: String,
    pub action: String,
    pub date: DateTime<Utc>,
    pub file_size: u32,
}

#[derive(Clone, Default, Deserialize, Serialize)]
pub struct FileHistoryResponse {
    pub revisions: Vec<FileHistoryRevision>,
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommitFileInfo {
    pub action: String,
    pub file: String,
    pub display_name: String,
}

#[derive(Clone, Default, Deserialize, Serialize)]
pub struct Snapshot {
    pub commit: String,
    pub message: String,
    pub timestamp: DateTime<Utc>,

    #[serde(skip)]
    pub stash_index: String,
}

pub type ShowCommitFilesResponse = Vec<CommitFileInfo>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_branch_success() {
        let mut status = RepoStatus::new();
        status.parse_branch_string("## my-branch...origin/my-branch [ahead 1, behind 1]");

        assert_eq!(status.branch, "my-branch");
        assert_eq!(status.remote_branch, "origin/my-branch");
        assert_eq!(status.commits_ahead, 1);
        assert_eq!(status.commits_behind, 1);
    }

    #[test]
    fn test_parse_branch_detached_head() {
        let mut status = RepoStatus::new();
        status.parse_branch_string("## HEAD (no branch)");

        assert!(status.detached_head);
    }

    #[test]
    fn test_parse_branch_no_remote() {
        let mut status = RepoStatus::new();
        status.parse_branch_string("## test-branch");

        assert_eq!(status.branch, "test-branch");
        assert_eq!(status.remote_branch, "");
        assert_eq!(status.commits_ahead, 0);
        assert_eq!(status.commits_behind, 0);
    }

    #[test]
    fn test_parse_file_untracked() {
        let file = File::from_status_line("?? test/test.dll");

        assert_eq!(file.state, FileState::Added);
        assert_eq!(file.path, "test/test.dll");
    }

    #[test]
    fn test_parse_file_staged() {
        let file = File::from_status_line("A  test-foo/test-foo.dll");

        assert_eq!(file.state, FileState::Added);
        assert_eq!(file.path, "test-foo/test-foo.dll");
    }

    #[test]
    fn test_parse_file_unstaged() {
        let file = File::from_status_line(" M test-bar/test-bar.png");

        assert_eq!(file.state, FileState::Modified);
        assert_eq!(file.path, "test-bar/test-bar.png");
    }
}
