use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct File {
    pub path: String,

    #[serde(rename = "displayName")]
    pub display_name: String,
    #[serde(rename = "indexState")]
    pub index_state: String,
    #[serde(rename = "workingState")]
    pub working_state: String,
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

        // skip the space
        chars.next();
        let path = chars.collect::<String>();

        // remove leading and trailing quote escapes
        let path = path.trim_matches('"').to_owned();

        Self {
            path,
            display_name: String::new(),
            index_state,
            working_state,
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
    pub commits_ahead: u32,
    pub commits_behind: u32,
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
}

impl RepoStatus {
    pub fn new() -> Self {
        Self {
            detached_head: false,
            last_updated: chrono::Utc::now(),
            branch: String::new(),
            remote_branch: String::new(),
            repo_owner: String::new(),
            repo_name: String::new(),
            commits_ahead: 0,
            commits_behind: 0,
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
pub struct CommitFileInfo {
    pub action: String,
    pub file: String,
}

#[derive(Clone, Default, Deserialize, Serialize)]
pub struct Snapshot {
    pub commit: String,
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

        assert_eq!(file.index_state, "?");
        assert_eq!(file.working_state, "?");
        assert_eq!(file.path, "test/test.dll");
    }

    #[test]
    fn test_parse_file_staged() {
        let file = File::from_status_line("A  test-foo/test-foo.dll");

        assert_eq!(file.index_state, "A");
        assert_eq!(file.working_state, "");
        assert_eq!(file.path, "test-foo/test-foo.dll");
    }

    #[test]
    fn test_parse_file_unstaged() {
        let file = File::from_status_line(" M test-bar/test-bar.png");

        assert_eq!(file.index_state, "");
        assert_eq!(file.working_state, "M");
        assert_eq!(file.path, "test-bar/test-bar.png");
    }
}
