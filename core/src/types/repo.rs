use crate::types::commits::Commit;
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
    Blocked,
    #[serde(other)]
    Unknown,
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

    /// Lockable files changed on both origin/<trunk> and this branch since their merge-base.
    /// Non-blocking. Always empty unless the current branch is in branch mode.
    #[serde(default)]
    pub will_conflict_on_rebase: Vec<String>,

    /// Locally modified tracked files also changed on origin/<branch>; Sync may leave conflict
    /// markers in them. Non-blocking. Always empty unless the current branch is in branch mode.
    #[serde(default)]
    pub will_conflict_on_sync: Vec<String>,

    /// A guided rebase is in progress, completed but unpushed, or its push failed.
    #[serde(default)]
    pub guided_rebase_active: bool,

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
            will_conflict_on_rebase: vec![],
            will_conflict_on_sync: vec![],
            guided_rebase_active: false,
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
    #[serde(
        rename = "expectedBranch",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub expected_branch: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RevertFilesRequest {
    pub files: Vec<String>,
    #[serde(rename = "skipEngineCheck")]
    pub skip_engine_check: bool,
    #[serde(rename = "takeSnapshot", default = "default_snapshot")]
    pub take_snapshot: bool,
}

fn default_snapshot() -> bool {
    true
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RebaseStatusResponse {
    pub rebase_merge_exists: bool,
    pub head_name_exists: bool,
    #[serde(default)]
    pub guided_rebase_active: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObjectCountResponse {
    pub in_pack_count: u64,
    pub is_healthy: bool,
    pub raw_output: String,

    /// Loose (unpacked) object count. Maintenance debt, as opposed to `in_pack_count`, which is
    /// repository size.
    pub loose_count: u64,

    /// When the repository was last packed by anyone — this app, the Diagnostics gc button, or a
    /// developer's own `git gc`. Derived from the commit-graph mtime, which (unlike pack mtimes) is
    /// not disturbed by fetching. `None` means never.
    pub last_packed: Option<DateTime<Utc>>,
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
#[serde(rename_all = "camelCase")]
pub struct PullResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conflicts: Option<Vec<String>>,
    #[serde(default)]
    pub already_up_to_date: bool,
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
#[serde(rename_all = "camelCase")]
pub struct FileHistoryResponse {
    /// OFPA-translated friendly name for the queried path, when the engine can resolve it.
    /// Empty when the editor isn't running or the path doesn't translate to an asset name.
    #[serde(default)]
    pub display_name: String,
    pub revisions: Vec<FileHistoryRevision>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum RepoFileKind {
    Directory,
    File,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum RepoFileState {
    Unmodified,
    Modified,
    Added,
    Untracked,
    Deleted,
    OutOfDate,
    Conflicted,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepoDirectoryEntry {
    pub name: String,
    pub path: String,
    pub kind: RepoFileKind,
    pub state: RepoFileState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepoDirectoryListing {
    pub path: String,
    pub entries: Vec<RepoDirectoryEntry>,
}

/// Prefix of branches created by Friendshipper's branch support.
pub const FEATURE_BRANCH_PREFIX: &str = "f11r-branch-";

pub fn is_feature_branch(name: &str) -> bool {
    name.starts_with(FEATURE_BRANCH_PREFIX)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchInfo {
    /// Bare name, e.g. `f11r-branch-foo` or `release/1.2`.
    pub name: String,
    /// Full object id.
    pub sha: String,
    pub short_sha: String,
    /// ISO 8601 strict, e.g. `2026-10-21T13:01:46-07:00`.
    pub commit_date: String,
    pub author: String,
    /// First line of the commit message.
    pub subject: String,
    /// Full commit message, trailing whitespace trimmed.
    pub message: String,
    pub is_feature_branch: bool,
    pub is_protected: bool,
    pub is_current: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateBranchRequest {
    pub slug: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SwitchBranchRequest {
    pub branch: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateBranchResponse {
    /// Full name, `f11r-branch-<slug>`.
    pub branch: String,
    /// True when HEAD is now on the new branch.
    pub switched: bool,
    /// Explains why when `switched` is false; null otherwise.
    pub message: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SwitchBranchResponse {
    pub branch: String,
    /// Set when the local branch diverged from origin.
    pub warning: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BranchUpstream {
    #[default]
    Present,
    Gone,
    NotPushed,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BranchLocalSync {
    #[default]
    InSync,
    Ahead,
    Behind,
    Diverged,
    Unknown,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileTouch {
    /// First 8 chars of the commit sha.
    pub sha: String,
    pub author: String,
    /// git `%aI`, RFC 3339.
    pub date: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LockableFileRow {
    /// Repo-relative, forward slashes.
    pub path: String,
    /// Empty when the engine has none cached; the UI falls back to `path`.
    pub display_name: String,
    /// Newest branch commit touching the path; null if beyond the 2000-commit scan.
    pub branch_commit: Option<FileTouch>,
    /// Path changed on origin/<trunk> since the merge-base.
    pub changed_on_main: bool,
    /// Newest trunk commit touching the path; null when !changed_on_main or beyond the scan.
    pub main_commit: Option<FileTouch>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchMergeQueueEntry {
    /// GraphQL `MergeQueueEntryState` name.
    pub state: String,
    pub position: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchPullRequest {
    pub number: i64,
    /// GraphQL node id.
    pub id: String,
    pub url: String,
    /// PR head commit sha.
    pub head_ref_oid: String,
    /// Branch the PR merges into.
    pub base_ref_name: String,
    /// `owner/name` of the head repository; `None` when GitHub no longer has it.
    pub head_repository: Option<String>,
    /// "OPEN" or "MERGED"; closed PRs are never returned.
    pub state: String,
    /// "MERGEABLE", "CONFLICTING" or "UNKNOWN"; unknown values are passed through.
    pub mergeable: String,
    /// Non-null when the PR is in the merge queue.
    pub merge_queue_entry: Option<BranchMergeQueueEntry>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchAnalysis {
    /// When false, every other field is a default and must be ignored.
    pub git_supported: bool,
    /// Token after "git version " (e.g. "2.47.1.windows.1"); trimmed raw output when unparseable.
    pub git_version: String,
    /// Current branch (never protected).
    pub branch: String,
    pub trunk_branch: String,
    pub uses_merge_queue: bool,
    pub upstream: BranchUpstream,
    /// Local <branch> vs origin/<branch>; Unknown unless upstream is Present.
    pub local_sync: BranchLocalSync,
    pub local_ahead_of_origin: u32,
    pub local_behind_origin: u32,
    /// origin/<branch> vs origin/<trunk>.
    pub commits_ahead: u32,
    pub commits_behind: u32,
    /// Full sha of merge-base(origin/<trunk>, origin/<branch>).
    pub merge_base: Option<String>,
    /// merge-tree conflicted paths; empty means clean.
    pub predicted_conflicts: Vec<String>,
    /// Capped at 1000, sorted changed_on_main first, then path (case-insensitive).
    pub lockable_files: Vec<LockableFileRow>,
    /// Total before the cap.
    pub lockable_file_count: u32,
    /// origin/<trunk>..origin/<branch>, newest first, max 200.
    pub commits: Vec<Commit>,
    /// A MERGED PR is returned only when it covers the branch tip or the upstream is gone.
    pub pr: Option<BranchPullRequest>,
    /// Set when the GitHub lookup failed or GitHub is unavailable; `pr` is then null.
    pub pr_error: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateBranchPullRequestRequest {
    pub title: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchPullRequestResult {
    pub number: i64,
    pub url: String,
    pub created: bool,
    pub already_queued: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchCleanupPr {
    pub number: i64,
    pub url: String,
    pub base_ref_name: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchCleanupCommit {
    pub sha: String,
    pub subject: String,
}

/// A local `f11r-branch-*` branch whose branch on origin was deleted.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchCleanupCandidate {
    pub name: String,
    /// Full sha of the local branch tip.
    pub sha: String,
    pub subject: String,
    pub author: String,
    /// Author date, ISO 8601.
    pub date: String,
    /// The tip is an ancestor of origin/<trunk>.
    pub merged_into_trunk: bool,
    /// Every non-merge commit not on origin/<trunk> has a patch-equivalent commit there.
    pub patch_equivalent: bool,
    /// A merged pull request for this branch name whose head contains the tip.
    pub merged_via_pr: Option<BranchCleanupPr>,
    /// The pull request lookup was needed but GitHub was unavailable.
    pub pr_unknown: bool,
    /// Commits not on origin/<trunk>: non-merge commits without an equivalent there, plus merges.
    pub unmerged_commit_count: u32,
    /// Newest first, at most 20.
    pub unmerged_commits: Vec<BranchCleanupCommit>,
    /// Merged into the trunk (or a target branch via PR); deleting it loses no commits.
    pub safe: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchCleanupCandidates {
    pub trunk_branch: String,
    pub candidates: Vec<BranchCleanupCandidate>,
    /// Set when GitHub could not be asked about some candidate's pull request.
    pub pr_error: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchCleanupTarget {
    pub name: String,
    /// The tip the user saw; the branch is deleted only if it still points here.
    pub sha: String,
    /// The user saw the branch as safe; unless that was via a merged PR, it must still be
    /// merged into the trunk by git checks.
    #[serde(default)]
    pub safe: bool,
    /// The branch was safe only because of a merged pull request.
    #[serde(default)]
    pub safe_via_pr: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchCleanupRequest {
    pub branches: Vec<BranchCleanupTarget>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchCleanupResult {
    pub name: String,
    pub sha: String,
    pub deleted: bool,
    pub error: Option<String>,
}

/// Where a guided rebase stands, derived from `f11r-rebase.json` and git's own rebase state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RebaseStateKind {
    /// No guided rebase.
    #[default]
    Idle,
    /// Git is stopped mid-rebase on a guided rebase's commits.
    InProgress,
    /// The rebase finished locally and has not been pushed.
    CompletedUnpushed,
    /// The force-with-lease push was rejected or failed.
    PushFailed,
    /// The record no longer matches the repository; it can only be dismissed.
    Stale,
    /// A rebase Friendshipper did not start is in progress.
    External,
}

/// Why a guided rebase record no longer matches the repository.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RebaseStaleReason {
    #[default]
    AbortedOutside,
    PushedOutside,
    BranchChanged,
    OtherRebase,
    Unreadable,
    Unknown,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RebaseCommitInfo {
    /// Full 40-hex sha.
    pub sha: String,
    pub subject: String,
    /// Author name.
    pub author: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ConflictKind {
    #[default]
    Content,
    ModifyDelete,
    /// Only aborting the rebase resolves it.
    Unsupported,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ConflictSideName {
    #[default]
    Main,
    Branch,
}

/// Which side of a conflict to keep.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RebaseKeep {
    #[default]
    Main,
    Branch,
    /// The file was edited by hand.
    Manual,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConflictFile {
    /// Repo-relative, `/` separators.
    pub path: String,
    pub display_name: String,
    pub kind: ConflictKind,
    /// Set only for `modifyDelete`.
    pub deleted_on: Option<ConflictSideName>,
    pub unsupported_reason: Option<String>,
    pub is_text: bool,
    pub resolved: bool,
    pub resolution: Option<RebaseKeep>,
    /// Latest commit on the main side that touched the file.
    pub main_side: Option<RebaseCommitInfo>,
    /// The branch commit being replayed.
    pub branch_side: RebaseCommitInfo,
}

/// A commit of the original branch with no replayed counterpart.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DroppedCommit {
    pub sha: String,
    pub subject: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RebaseState {
    pub state: RebaseStateKind,
    /// Empty when idle or external.
    pub branch: String,
    pub step: u32,
    pub total_steps: u32,
    pub current_commit: Option<RebaseCommitInfo>,
    pub conflicts: Vec<ConflictFile>,
    /// Git stopped without a conflict Friendshipper recorded; only abort is offered.
    pub step_incomplete: bool,
    pub dropped_commits: Vec<DroppedCommit>,
    pub stale_reason: Option<RebaseStaleReason>,
    /// The branch's commits touch C++ source or build files.
    pub touches_cpp: bool,
    /// Empty when none.
    pub backup_ref: String,
    /// Empty when none.
    pub backup_sha: String,
    pub push_failed_remote_sha: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolveConflictRequest {
    pub path: String,
    pub keep: RebaseKeep,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenConflictFileRequest {
    pub path: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnresolveConflictRequest {
    pub path: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RebaseFinishOutcome {
    #[default]
    Pushed,
    LeaseRejected,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RebaseFinishResponse {
    pub outcome: RebaseFinishOutcome,
    pub state: RebaseState,
    pub dropped_commits: Vec<DroppedCommit>,
    pub touches_cpp: bool,
    /// Updating editor binaries or the engine failed after the push succeeded.
    pub post_step_error: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RebasePreflight {
    pub touches_cpp: bool,
    /// First 20 paths.
    pub cpp_files: Vec<String>,
    pub commit_count: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EditorBinariesState {
    #[default]
    Match,
    Mismatch,
    Unknown,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditorBinariesStatus {
    pub state: EditorBinariesState,
    /// Empty if none.
    pub dll_commit: String,
    pub latest_cpp_commit: Option<RebaseCommitInfo>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommitInfo {
    pub sha: String,
    pub short_sha: String,
    pub author_name: String,
    pub author_email: String,
    pub author_date: DateTime<Utc>,
    pub committer_name: String,
    pub committer_email: String,
    pub committer_date: DateTime<Utc>,
    pub parents: Vec<String>,
    pub subject: String,
    pub message: String,
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

/// One entry the user would see in the restore-snapshot preview, mirroring the
/// shape of `ZipPreviewEntry` so the frontend can render it with the same
/// component logic.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotPreviewEntry {
    pub path: String,
    /// What the snapshot will do to this path on restore: Added means it will
    /// appear, Modified means content will be replaced, Deleted means it will
    /// be removed from the working tree.
    pub state: FileState,
    /// True if this path is currently in the user's modified or untracked
    /// list — restoring will overwrite their local change.
    pub conflicts_with_local: bool,
    /// True if a file currently exists at this path on disk.
    pub exists_on_disk: bool,
}

pub type ShowCommitFilesResponse = Vec<CommitFileInfo>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_status_serializes_will_conflict_on_sync() {
        let value = serde_json::to_value(RepoStatus::new()).unwrap();
        assert_eq!(value["willConflictOnSync"], serde_json::json!([]));

        let mut legacy = value;
        legacy.as_object_mut().unwrap().remove("willConflictOnSync");
        let status: RepoStatus = serde_json::from_value(legacy).unwrap();
        assert!(status.will_conflict_on_sync.is_empty());
    }

    #[test]
    fn repo_status_serializes_will_conflict_on_rebase() {
        let value = serde_json::to_value(RepoStatus::new()).unwrap();
        assert_eq!(value["willConflictOnRebase"], serde_json::json!([]));

        let mut legacy = value;
        legacy
            .as_object_mut()
            .unwrap()
            .remove("willConflictOnRebase");
        let status: RepoStatus = serde_json::from_value(legacy).unwrap();
        assert!(status.will_conflict_on_rebase.is_empty());
    }

    #[test]
    fn branch_analysis_wire_shape() {
        let value = serde_json::to_value(BranchAnalysis::default()).unwrap();
        assert_eq!(value["gitSupported"], serde_json::json!(false));
        assert_eq!(value["localAheadOfOrigin"], serde_json::json!(0));
        assert_eq!(value["mergeBase"], serde_json::Value::Null);
        assert_eq!(value["pr"], serde_json::Value::Null);
        assert!(value.as_object().unwrap().contains_key("prError"));
        assert_eq!(value["prError"], serde_json::Value::Null);
        assert_eq!(value["upstream"], serde_json::json!("present"));
        assert_eq!(value["localSync"], serde_json::json!("inSync"));

        assert_eq!(
            serde_json::to_value(BranchUpstream::NotPushed).unwrap(),
            serde_json::json!("notPushed")
        );

        let req: CreateBranchPullRequestRequest = serde_json::from_str(r#"{"title":"x"}"#).unwrap();
        assert_eq!(req.title, "x");
    }

    #[test]
    fn rebase_state_wire_shape() {
        let value = serde_json::to_value(RebaseState::default()).unwrap();
        assert_eq!(value["state"], serde_json::json!("idle"));
        assert_eq!(value["branch"], serde_json::json!(""));
        assert_eq!(value["step"], serde_json::json!(0));
        assert_eq!(value["totalSteps"], serde_json::json!(0));
        assert_eq!(value["currentCommit"], serde_json::Value::Null);
        assert_eq!(value["conflicts"], serde_json::json!([]));
        assert_eq!(value["stepIncomplete"], serde_json::json!(false));
        assert_eq!(value["droppedCommits"], serde_json::json!([]));
        assert_eq!(value["staleReason"], serde_json::Value::Null);
        assert_eq!(value["touchesCpp"], serde_json::json!(false));
        assert_eq!(value["backupRef"], serde_json::json!(""));
        assert_eq!(value["backupSha"], serde_json::json!(""));
        assert_eq!(value["pushFailedRemoteSha"], serde_json::Value::Null);

        assert_eq!(
            serde_json::to_value(RebaseStateKind::CompletedUnpushed).unwrap(),
            serde_json::json!("completedUnpushed")
        );
        assert_eq!(
            serde_json::to_value(RebaseStaleReason::PushedOutside).unwrap(),
            serde_json::json!("pushedOutside")
        );
        assert_eq!(
            serde_json::to_value(ConflictKind::ModifyDelete).unwrap(),
            serde_json::json!("modifyDelete")
        );
        assert_eq!(
            serde_json::to_value(EditorBinariesState::Match).unwrap(),
            serde_json::json!("match")
        );

        let keep: RebaseKeep = serde_json::from_str(r#""manual""#).unwrap();
        assert_eq!(keep, RebaseKeep::Manual);
        let req: ResolveConflictRequest =
            serde_json::from_str(r#"{"path":"a/b.txt","keep":"branch"}"#).unwrap();
        assert_eq!(req.keep, RebaseKeep::Branch);
    }

    #[test]
    fn repo_status_guided_rebase_active_defaults_false() {
        let value = serde_json::to_value(RepoStatus::new()).unwrap();
        assert_eq!(value["guidedRebaseActive"], serde_json::json!(false));

        let mut legacy = value;
        legacy.as_object_mut().unwrap().remove("guidedRebaseActive");
        let status: RepoStatus = serde_json::from_value(legacy).unwrap();
        assert!(!status.guided_rebase_active);

        let response: RebaseStatusResponse =
            serde_json::from_str(r#"{"rebaseMergeExists":false,"headNameExists":false}"#).unwrap();
        assert!(!response.guided_rebase_active);
    }

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
    fn test_submit_status_round_trips_known_variants() {
        let cases = [
            (SubmitStatus::Ok, "\"Ok\""),
            (SubmitStatus::CheckoutRequired, "\"CheckoutRequired\""),
            (
                SubmitStatus::CheckedOutByOtherUser,
                "\"CheckedOutByOtherUser\"",
            ),
            (SubmitStatus::Unmerged, "\"Unmerged\""),
            (SubmitStatus::Conflicted, "\"Conflicted\""),
            (SubmitStatus::Blocked, "\"Blocked\""),
            (SubmitStatus::Unknown, "\"Unknown\""),
        ];

        for (variant, expected_json) in cases {
            let serialized = serde_json::to_string(&variant).expect("serialize");
            assert_eq!(
                serialized, expected_json,
                "serialization changed for {variant:?}"
            );

            let deserialized: SubmitStatus =
                serde_json::from_str(&serialized).expect("deserialize");
            assert_eq!(
                deserialized, variant,
                "round trip did not return the same variant for {variant:?}"
            );
        }
    }

    #[test]
    fn test_submit_status_unrecognized_string_becomes_unknown() {
        let result: SubmitStatus = serde_json::from_str("\"SomeFutureVariantThatDoesNotExistYet\"")
            .expect("an unrecognized submitStatus string must deserialize successfully, not error");
        assert_eq!(result, SubmitStatus::Unknown);
    }

    #[test]
    fn test_submit_status_malformed_json_still_errors() {
        // Sanity check: tolerance is for unrecognized *variant names*, not for
        // malformed JSON in general. A non-string value must still fail.
        let result = serde_json::from_str::<SubmitStatus>("42");
        assert!(result.is_err());
    }

    #[test]
    fn test_changeset_payload_with_unknown_submit_status_loads() {
        let json = r#"[
            {
                "name": "default",
                "files": [
                    {
                        "path": "Content/Foo.uasset",
                        "displayName": "Foo.uasset",
                        "state": "Modified",
                        "isStaged": false,
                        "lockedBy": "",
                        "submitStatus": "SomeFutureVariantFromANewerBuild"
                    },
                    {
                        "path": "Source/Bar.cpp",
                        "displayName": "Bar.cpp",
                        "state": "Modified",
                        "isStaged": false,
                        "lockedBy": "",
                        "submitStatus": "Ok"
                    }
                ],
                "open": true,
                "checked": false,
                "indeterminate": false
            }
        ]"#;

        // This is the exact call made by `load_changeset` in
        // friendshipper/src-tauri/src/repo/operations/changeset.rs:162-168.
        let changesets: Vec<ChangeSet> = serde_json::from_str(json)
            .expect("changesets.json with an unknown variant must still load");

        assert_eq!(changesets.len(), 1);
        assert_eq!(changesets[0].files.len(), 2);
        assert_eq!(changesets[0].files[0].submit_status, SubmitStatus::Unknown);
        assert_eq!(changesets[0].files[1].submit_status, SubmitStatus::Ok);
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
