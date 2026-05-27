use anyhow::{anyhow, bail};
use axum::extract::State;
use axum::{async_trait, Json};
use octocrab::models::pulls::{MergeableState, PullRequest};
use octocrab::{params, Octocrab};
use std::io::Write;
use std::path::PathBuf;
use std::sync::mpsc::Sender;
use tracing::{debug, error, info, instrument, warn};

use crate::engine::CommunicationType;
use crate::engine::EngineProvider;
use crate::repo::operations::pull::PullOp;
use crate::repo::operations::validate::validate_repo_state;
use crate::repo::operations::StatusOp;
use crate::repo::RepoStatusRef;
use crate::state::AppState;
use crate::state::Notification;
use ethos_core::clients::git;
use ethos_core::clients::github;
use ethos_core::longtail::Longtail;
use ethos_core::msg::LongtailMsg;
use ethos_core::operations::{AddOp, CommitOp, LockOp, RestoreOp};
use ethos_core::storage::ArtifactStorage;
use ethos_core::types::config::AppConfigRef;
use ethos_core::types::config::RepoConfigRef;
use ethos_core::types::errors::CoreError;
use ethos_core::types::github::TokenNotFoundError;
use ethos_core::types::locks::LockOperation;
use ethos_core::types::repo::SubmitStatus;
use ethos_core::types::repo::{File, PushRequest};
use ethos_core::worker::{Task, TaskSequence};
use ethos_core::AWSClient;

#[derive(Clone)]
pub struct GitHubSubmitOp {
    pub head_branch: String,
    pub base_branch: String,
    pub commit_message: String,
    pub repo_status: RepoStatusRef,
    pub token: String,
    pub client: github::GraphQLClient,
    pub use_merge_queue: bool,
}

#[derive(Clone)]
pub struct SubmitOp<T>
where
    T: EngineProvider,
{
    pub files: Vec<String>,
    pub commit_message: String,

    pub app_config: AppConfigRef,
    pub repo_config: RepoConfigRef,
    pub engine: T,
    pub aws_client: Option<AWSClient>,
    pub storage: Option<ArtifactStorage>,
    pub repo_status: RepoStatusRef,

    pub longtail: Longtail,
    pub longtail_tx: Sender<LongtailMsg>,
    pub notification_tx: Sender<Notification>,
    /// Forwarded into the auto-sync `PullOp` kicked off after a successful
    /// quicksubmit merge so the pulling modal shows the same phase labels
    /// a standalone Sync would.
    pub sync_phase_tx: Sender<String>,

    pub git_client: git::Git,
    pub token: String,
    pub github_client: github::GraphQLClient,
}

const SUBMIT_PREFIX: &str = "[quick submit]";

/// Ask git which of the given paths are NOT LFS-tracked or declared non-text
/// in `.gitattributes`, returning only paths that could plausibly contain
/// human-readable conflict markers.
///
/// `git diff --check` skips binary files via its own classification, so this
/// pre-filter doesn't change correctness — it just avoids staging files that
/// would force a full working-tree read + LFS clean filter for each entry.
/// On a 15k-asset submit that turns ~15 minutes of LFS hashing into a single
/// fast `check-attr` lookup.
///
/// We classify by two attributes:
/// - `filter=lfs` — git-lfs tracked, content lives outside the tree
/// - `text=unset` — `-text` set (directly or via the `binary` macro)
///
/// Both cases mean "treat as binary", and `--check` would skip them anyway.
/// Files with `text=auto` or `text=unspecified` are kept; `--check` will run
/// its own NUL-byte heuristic on them at diff time. That costs an extra
/// staging round-trip for those files but doesn't change the result.
async fn filter_to_textlike_paths(
    git_client: &git::Git,
    paths: &[String],
) -> anyhow::Result<Vec<String>> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }

    // Build NUL-separated input for `git check-attr -z --stdin`. Streaming
    // via stdin keeps us under the OS command-line cap regardless of submit
    // size.
    let mut input: Vec<u8> = Vec::with_capacity(paths.iter().map(|p| p.len() + 1).sum());
    for path in paths {
        input.extend_from_slice(path.as_bytes());
        input.push(0u8);
    }

    let mut cmd = tokio::process::Command::new("git");
    cmd.args([
        "-c",
        "core.quotePath=false",
        "check-attr",
        "-z",
        "--stdin",
        "filter",
        "text",
    ]);
    cmd.env("GIT_CLONE_PROTECTION_ACTIVE", "false");
    cmd.env("LC_ALL", "C");
    if !git_client.repo_path.as_os_str().is_empty() {
        cmd.current_dir(git_client.repo_path.canonicalize()?);
    }
    #[cfg(windows)]
    cmd.creation_flags(crate::repo::CREATE_NO_WINDOW);

    // `run_with_stdin` feeds the (potentially >1 MB) path list to check-attr
    // from a dedicated task while draining stdout, so a large submit can't
    // deadlock on the pipe buffers. See its docs for the gory details.
    let output = ethos_core::utils::process::run_with_stdin(cmd, input).await?;
    if !output.status.success() {
        bail!(
            "git check-attr failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    // Output is `<path>\0<attr>\0<info>\0` repeating — one record per
    // requested attribute per input path. Collect the paths to exclude
    // into a set, then return the input list minus those.
    let mut excluded: std::collections::HashSet<String> = std::collections::HashSet::new();
    let fields: Vec<&[u8]> = output
        .stdout
        .split(|&b| b == 0)
        .filter(|s| !s.is_empty())
        .collect();
    for record in fields.chunks_exact(3) {
        let [path, attr, value] = record else {
            unreachable!("chunks_exact(3) yields 3-element slices");
        };
        let attr = std::str::from_utf8(attr).unwrap_or("");
        let value = std::str::from_utf8(value).unwrap_or("");
        let exclude = (attr == "filter" && value == "lfs") || (attr == "text" && value == "unset");
        if exclude {
            if let Ok(path) = std::str::from_utf8(path) {
                excluded.insert(path.to_string());
            }
        }
    }

    Ok(paths
        .iter()
        .filter(|p| !excluded.contains(p.as_str()))
        .cloned()
        .collect())
}

/// Ask git itself which submit paths have unresolved conflict markers.
///
/// We can't just run `git diff --check HEAD -- <paths>` because untracked
/// files aren't part of `git diff`'s view, so a brand-new file pasted with
/// conflict markers would slip through. Instead we seed a **temp index**
/// from HEAD, stage the submit set into it (so new files become "added"
/// entries), and then run `git diff --check --cached HEAD` against the
/// temp index. The user's real index is never touched.
///
/// Using `--check` (the same check `git rebase` runs and the sample
/// `pre-commit` hook uses) means we honor `merge.conflictMarkerSize`,
/// git's binary/text detection, and `.gitattributes` filters — LFS-tracked
/// assets are skipped automatically. We only collect lines tagged
/// `leftover conflict marker` and ignore the whitespace-error lines that
/// `--check` also emits.
///
/// `--check` flags each marker line independently, so it can't distinguish a
/// real conflict from a lone `=======` (a Markdown/RST section underline) or
/// a stray `>>>>>>> ...` banner. A second pass (`confirm_paired_markers`)
/// therefore keeps only files whose introduced lines carry both an opening
/// and a closing marker — the shape a genuine conflict always has.
///
/// # Caveats
///
/// - **The diff needs no pathspec.** The temp index is seeded from HEAD's
///   full tree and only the submit set is re-staged, so `git diff --check
///   --cached HEAD` already reports exactly the submit-set paths and nothing
///   else. We deliberately pass no `-- <paths>`, which also sidesteps the OS
///   command-line length cap (~32 KB on Windows) no matter how large the
///   submit grows.
/// - **Parser depends on git's English output strings** (`"leftover
///   conflict marker"`). `run_git_with_index` pins `LC_ALL=C` so this
///   holds regardless of the user's locale.
/// - **Output paths must round-trip cleanly.** `run_git_with_index` passes
///   `-c core.quotePath=false` so non-ASCII paths come back as raw UTF-8
///   instead of double-quoted with octal escapes.
/// - **A document that literally demonstrates conflict syntax can still
///   flag.** The pairing pass narrows to open+close, but a file that
///   deliberately contains both a `<<<<<<<` and a `>>>>>>>` line (e.g. a
///   merge tutorial) carries the full shape and will be caught. That's far
///   rarer than the lone-divider case and is the accepted residual.
async fn find_files_with_conflict_markers(
    git_client: &git::Git,
    paths: &[String],
) -> Result<Vec<String>, CoreError> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }

    // Drop paths that git already classifies as LFS-tracked or binary
    // before doing any I/O. `git diff --check` would skip them anyway via
    // its own binary detection, and staging them via `git add` would
    // trigger a full working-tree read + LFS clean filter per file. On a
    // 15k-asset submit this turns minutes into seconds.
    let paths = filter_to_textlike_paths(git_client, paths).await?;
    if paths.is_empty() {
        return Ok(Vec::new());
    }

    // Write the pathspec list once and reuse it for both `git add` and
    // `git diff` so they stay scoped to the same files.
    let mut pathspec = tempfile::NamedTempFile::new()?;
    for path in &paths {
        writeln!(pathspec, "{path}")?;
    }
    pathspec.flush()?;
    let pathspec_arg = pathspec
        .path()
        .to_str()
        .ok_or_else(|| anyhow!("pathspec temp file path is not valid UTF-8"))?
        .to_string();

    let temp_dir = tempfile::tempdir()?;
    let temp_index = temp_dir.path().join("conflict-check-index");

    // 1. Resolve HEAD to a SHA so a concurrent ref move can't desync the
    //    seed tree from the comparison base.
    let head_sha_out = run_git_with_index(git_client, &["rev-parse", "HEAD"], &temp_index).await?;
    if !head_sha_out.status.success() {
        return Err(CoreError::Internal(anyhow!(
            "git rev-parse HEAD failed: {}",
            String::from_utf8_lossy(&head_sha_out.stderr).trim()
        )));
    }
    let head_sha = String::from_utf8_lossy(&head_sha_out.stdout)
        .trim()
        .to_string();

    // 2. Seed the temp index with HEAD's tree.
    let read_tree_out =
        run_git_with_index(git_client, &["read-tree", &head_sha], &temp_index).await?;
    if !read_tree_out.status.success() {
        return Err(CoreError::Internal(anyhow!(
            "git read-tree HEAD failed: {}",
            String::from_utf8_lossy(&read_tree_out.stderr).trim()
        )));
    }

    // 3. Stage the submit set into the temp index. `-A` covers
    //    additions/modifications/deletions in one shot.
    let add_out = run_git_with_index(
        git_client,
        &["add", "-A", "--pathspec-from-file", &pathspec_arg],
        &temp_index,
    )
    .await?;
    if !add_out.status.success() {
        // User-actionable: the selected file list is stale relative to the
        // working tree. Surface as Input (400) so the message reaches the
        // user instead of being logged as an internal 500.
        return Err(CoreError::Input(anyhow!(
            "Could not stage one or more selected files for the pre-submit \
             conflict-marker check — your file list may be stale (a file may \
             have been deleted or moved since you opened the submit modal). \
             Refresh Friendshipper and try again.\n\nGit said: {}",
            String::from_utf8_lossy(&add_out.stderr).trim()
        )));
    }

    // 4. Diff HEAD vs the temp index with `--check`. No pathspec needed: the
    //    temp index equals HEAD except for the submit set we just staged, so
    //    the diff is already scoped to exactly those files. `git diff
    //    --check` exits non-zero when it finds issues, which is information,
    //    not an error — we inspect stdout regardless of exit status.
    let check_out = run_git_with_index(
        git_client,
        &["diff", "--check", "--cached", "HEAD"],
        &temp_index,
    )
    .await?;

    let code = check_out.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&check_out.stdout);
    let candidates = parse_conflict_check(code, &stdout).map_err(|e| {
        CoreError::Internal(anyhow!(
            "{e} (git diff --check stderr: {})",
            String::from_utf8_lossy(&check_out.stderr).trim()
        ))
    })?;
    if candidates.is_empty() {
        return Ok(candidates);
    }

    // 5. `git diff --check` flags every marker line on its own and can't tell
    //    a real conflict from a lone `=======` section divider or a stray
    //    `>>>>>>> ...` banner line. Narrow to files that contain a *coherent*
    //    conflict — both an opening (`<<<<<<<`) and a closing (`>>>>>>>`)
    //    marker among the introduced (added) lines. Requiring both ends can
    //    only drop unpaired false positives; a genuine conflict always
    //    carries an open and a close, so this never hides one. No pathspec on
    //    the diff for the same reason as step 4 — the temp index already
    //    scopes it to the submit set; we filter to `candidates` while parsing.
    let marker_size = read_conflict_marker_size(git_client, &temp_index).await;
    let diff_out =
        run_git_with_index(git_client, &["diff", "--cached", "HEAD"], &temp_index).await?;
    if !diff_out.status.success() {
        // The diff failed unexpectedly. Fall back to the stricter `--check`
        // result rather than risk letting a real conflict slip through.
        return Ok(candidates);
    }
    let diff = String::from_utf8_lossy(&diff_out.stdout);
    Ok(confirm_paired_markers(&diff, &candidates, marker_size))
}

/// Count the leading run of byte `c` at the start of `s`. Used to recognize a
/// conflict marker line (`<<<<<<<`, `>>>>>>>`) by its opening run length.
fn leading_run(s: &str, c: u8) -> usize {
    s.bytes().take_while(|&b| b == c).count()
}

/// Read the effective `merge.conflictMarkerSize` (default 7) so the pairing
/// pass recognizes markers the same way `git diff --check` did. `git config
/// --get` exits non-zero when the key is unset, which falls through to the
/// default.
async fn read_conflict_marker_size(git_client: &git::Git, index_file: &std::path::Path) -> usize {
    const DEFAULT: usize = 7;
    match run_git_with_index(
        git_client,
        &["config", "--get", "merge.conflictMarkerSize"],
        index_file,
    )
    .await
    {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout)
            .trim()
            .parse::<usize>()
            .ok()
            .filter(|n| *n > 0)
            .unwrap_or(DEFAULT),
        _ => DEFAULT,
    }
}

/// From a `git diff --cached HEAD` unified diff, return the subset of
/// `candidates` whose introduced (added) lines contain both an opening
/// (`<<<<<<<`) and a closing (`>>>>>>>`) conflict marker of at least
/// `marker_size` characters. Files with only a `=======` divider or a single
/// stray marker — section underlines, banners — are dropped.
///
/// Pure (no I/O) so the pairing rule can be unit tested directly.
fn confirm_paired_markers(diff: &str, candidates: &[String], marker_size: usize) -> Vec<String> {
    let candidate_set: std::collections::HashSet<&str> =
        candidates.iter().map(String::as_str).collect();
    // path -> (saw opening marker, saw closing marker), among added lines.
    // Keyed by the candidate's own `&str` so the per-line scan allocates
    // nothing; we only materialize Strings for the paths we keep.
    let mut seen: std::collections::BTreeMap<&str, (bool, bool)> =
        std::collections::BTreeMap::new();
    let mut current: Option<&str> = None;
    for line in diff.lines() {
        // Reset at each file header so a deletion's `+++ /dev/null` can't
        // bleed added lines onto the previous file.
        if line.starts_with("diff --git ") {
            current = None;
            continue;
        }
        if let Some(path) = line.strip_prefix("+++ b/") {
            current = candidate_set.get(path).copied();
            continue;
        }
        let Some(cur) = current else {
            continue;
        };
        // Added content lines start with a single '+' (the '+++' header was
        // handled above and won't reach here).
        let Some(added) = line.strip_prefix('+') else {
            continue;
        };
        let entry = seen.entry(cur).or_insert((false, false));
        if leading_run(added, b'<') >= marker_size {
            entry.0 = true;
        } else if leading_run(added, b'>') >= marker_size {
            entry.1 = true;
        }
    }
    seen.into_iter()
        .filter(|(_, (open, close))| *open && *close)
        .map(|(path, _)| path.to_string())
        .collect()
}

/// Classify a `git diff --check` result and extract the conflicted paths.
///
/// Exit conventions, empirically:
///   0 — clean
///   1 — whitespace warnings only (no conflict markers)
///   2 — conflict markers found (and/or whitespace)
/// Anything outside `0..=2` is a fatal error (bad ref, usage, repo problem) —
/// we error rather than parse an empty stdout and let the submit through.
/// This is the guard that would have caught the `--pathspec-from-file`
/// regression: that bug returned exit 129 with an empty stdout, which the old
/// code treated as "no markers found".
///
/// Pure (no I/O) so the exit-code guard and the line parser can be unit
/// tested directly without standing up a git repo.
fn parse_conflict_check(code: i32, stdout: &str) -> anyhow::Result<Vec<String>> {
    if !matches!(code, 0..=2) {
        bail!("conflict-marker check failed (git diff --check exit {code})");
    }

    // Lines look like:
    //   path/to/file:7: leftover conflict marker
    // De-dup via BTreeSet so files with multiple marker lines are reported
    // once, and the final list comes out sorted.
    let mut conflicted: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for line in stdout.lines() {
        let Some(rest_idx) = line.rfind(": leftover conflict marker") else {
            continue;
        };
        let before_marker = &line[..rest_idx];
        // before_marker is "path/to/file:linenum"; strip the trailing ":linenum".
        if let Some((path, _line_num)) = before_marker.rsplit_once(':') {
            if !path.is_empty() {
                conflicted.insert(path.to_string());
            }
        }
    }
    Ok(conflicted.into_iter().collect())
}

/// Spawn a git command with `GIT_INDEX_FILE` pointing at the given path.
/// Returns the raw `Output` (including non-success exit statuses) because
/// callers want to inspect stdout/stderr regardless.
///
/// Two invariants this helper enforces for every call:
/// - `-c core.quotePath=false` so output paths come back as raw UTF-8 bytes
///   instead of double-quoted with octal escapes whenever a byte ≥ 0x80
///   shows up. The conflict-marker parser depends on path strings round-
///   tripping cleanly; without this a single asset with a non-ASCII name
///   would slip past the gate.
/// - `LC_ALL=C` so git's user-facing messages (notably the
///   "leftover conflict marker" string our parser scans for) come back in
///   English regardless of the user's locale. Otherwise a Friendshipper
///   user with a translated git would silently bypass the entire check.
async fn run_git_with_index(
    git_client: &git::Git,
    args: &[&str],
    index_file: &std::path::Path,
) -> anyhow::Result<std::process::Output> {
    let mut cmd = tokio::process::Command::new("git");
    cmd.arg("-c").arg("core.quotePath=false");
    cmd.args(args);
    cmd.env("GIT_CLONE_PROTECTION_ACTIVE", "false");
    cmd.env("GIT_INDEX_FILE", index_file);
    cmd.env("LC_ALL", "C");
    if !git_client.repo_path.as_os_str().is_empty() {
        cmd.current_dir(git_client.repo_path.canonicalize()?);
    }
    #[cfg(windows)]
    cmd.creation_flags(crate::repo::CREATE_NO_WINDOW);
    Ok(cmd.output().await?)
}

impl<T> SubmitOp<T>
where
    T: EngineProvider,
{
    /// Post a high-level phase label to the submitting modal. Mirrors
    /// `PullOp::emit_phase`. Send errors are ignored: the receiver lives
    /// for the app's lifetime, so a failed send means the listener has
    /// gone away — not worth bubbling up and failing the submit over.
    fn emit_phase(&self, phase: impl Into<String>) {
        let _ = self.sync_phase_tx.send(phase.into());
    }
}

#[async_trait]
impl Task for GitHubSubmitOp {
    #[instrument(name = "GitHubSubmitOp::execute", skip(self))]
    async fn execute(&self) -> Result<(), CoreError> {
        let octocrab = Octocrab::builder()
            .personal_token(self.token.clone())
            .build()?;

        let truncated_message = if self.commit_message.len() > 50 {
            format!("{}...", &self.commit_message[..50])
        } else {
            self.commit_message.clone()
        };

        let owner: String;
        let repo: String;
        {
            let status = self.repo_status.read();
            owner = status.repo_owner.clone();
            repo = status.repo_name.clone();
        }

        let pr = octocrab
            .pulls(owner.clone(), repo.clone())
            .create(
                format!("{SUBMIT_PREFIX} {truncated_message}"),
                self.head_branch.clone(),
                self.base_branch.clone(),
            )
            .send()
            .await?;

        if self.use_merge_queue {
            // Fire-and-forget for merge queue path
            let self_clone = self.clone();
            let octocrab_clone = octocrab.clone();
            let owner_clone = owner.clone();
            let repo_clone = repo.clone();

            tokio::spawn(async move {
                match self_clone
                    .poll_for_mergeable(
                        octocrab_clone.clone(),
                        pr,
                        owner_clone.clone(),
                        repo_clone.clone(),
                    )
                    .await
                {
                    Ok(updated_pr) => {
                        if let Ok(id) = self_clone
                            .client
                            .get_pull_request_id(
                                owner_clone.clone(),
                                repo_clone.clone(),
                                updated_pr.number as i64,
                            )
                            .await
                        {
                            if let Err(e) = self_clone.client.enqueue_pull_request(id).await {
                                warn!("Failed to enqueue pull request: {:?}", e);
                            }
                        } else {
                            warn!("Failed to get pull request ID");
                        }
                    }
                    Err(e) => warn!("Failed to poll for mergeable state: {:?}", e),
                }
            });

            Ok(())
        } else {
            // Non-merge-queue: wait inline for merge to complete with timeout
            let result = tokio::time::timeout(std::time::Duration::from_secs(120), async {
                let updated_pr = self
                    .poll_for_mergeable(octocrab.clone(), pr, owner.clone(), repo.clone())
                    .await?;

                octocrab
                    .pulls(owner, repo)
                    .merge(updated_pr.number)
                    .method(params::pulls::MergeMethod::Rebase)
                    .send()
                    .await
                    .map_err(|e| {
                        CoreError::Internal(anyhow!("Failed to merge pull request: {}", e))
                    })?;

                Ok::<(), CoreError>(())
            })
            .await;

            match result {
                Ok(Ok(())) => Ok(()),
                Ok(Err(e)) => Err(e),
                Err(_) => Err(CoreError::Internal(anyhow!(
                    "Timed out waiting for PR merge (120s)"
                ))),
            }
        }
    }

    fn get_name(&self) -> String {
        "GitHubSubmitOp".to_string()
    }
}

impl GitHubSubmitOp {
    #[instrument(name = "GitHubSubmitOp::poll_for_mergeable", skip(self))]
    async fn poll_for_mergeable(
        &self,
        octocrab: Octocrab,
        pr: PullRequest,
        owner: String,
        repo: String,
    ) -> Result<PullRequest, CoreError> {
        let mut pr = pr.clone();
        while let Some(state) = pr.mergeable_state.clone() {
            match state {
                MergeableState::Blocked | MergeableState::Behind | MergeableState::Unknown => {
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    pr = octocrab
                        .pulls(owner.clone(), repo.clone())
                        .get(pr.clone().number)
                        .await?;
                }
                MergeableState::Dirty => {
                    return Err(CoreError::Input(anyhow!(
                        "PR state is 'dirty'. It's likely a commit check has failed."
                    )));
                }
                _ => {
                    info!("mergeable state: {:?}", state);
                    break;
                }
            }
        }

        Ok(pr)
    }
}

// Quick submit is a workflow that submits changes via GitHub Pull Requests, taking advantage of the GitHub merge queue to avoid having
// to sync latest first.
// When a commit goes through the merge queue, it becomes a different commit due to how the commit is merged into/
// rebased onto main. When making a successive change, GitHub isn't smart enough to detect that the previous commit is the same one is
// now in main, so it complains that there is a conflict, due to 2 "different" commits touching the same files, even though they have
// the exact same contents. To overcome this limitation, quick submits leverage the concept of git worktrees to resolve local changes
// with the latest changes in main.
// The general logic for quick submit pushes go like this:
// 1. User initiates quick submit
// 2. If the current branch has an existing quick submit change in the merge queue, cancel it. We'll just reuse the current branch.
//    We need to cancel the in-flight one since if it lands in main, it will conflict with what we try to put in the merge queue, so
//    instead we just resolve all the changes locally again, push them all up to the same branch, and resubmit to the merge queue.
// 3. Make a new f11r-<timestamp> branch to contain the changes if needed.
// 4. Commit new changes
// 5. If a scratch worktree folder doesn't exist, make one.
// 6. In the workree directory:
//    a. Make a branch called f11r-<timestamp>-wt and ensure it's up to date with exactly what's on f11r-<timestamp>.
//    b. Resolve local changes with latest main
//    c. Push changes to the remote
// 7. Trigger PR via github
#[async_trait]
impl<T> Task for SubmitOp<T>
where
    T: EngineProvider,
{
    #[instrument(name = "SubmitOp::execute", skip(self))]
    async fn execute(&self) -> Result<(), CoreError> {
        // abort if there are no files to submit
        if self.files.is_empty() {
            return Err(CoreError::Input(anyhow!("No files to submit")));
        }

        // Validate git state before proceeding
        {
            let repo_path = PathBuf::from(self.app_config.read().repo_path.clone());
            let repo_status = self.repo_status.read().clone();
            validate_repo_state(&repo_path, &repo_status)?;
        }

        self.emit_phase("Checking repository status");
        let status_op = StatusOp {
            repo_status: self.repo_status.clone(),
            app_config: self.app_config.clone(),
            repo_config: self.repo_config.clone(),
            engine: self.engine.clone(),
            git_client: self.git_client.clone(),
            github_username: self.github_client.username.clone(),
            aws_client: None,
            storage: None,
            allow_offline_communication: false,
            skip_display_names: true,

            // we'll make sure this gets done at the end
            skip_engine_update: true,
        };

        // We're moving this call from the frontend to the backend so we can customize
        // some submit-specific behavior.
        status_op.execute().await?;

        // abort if we are trying to submit any conflicted files, or files that should be locked, but aren't
        {
            let repo_status = self.repo_status.read().clone();
            let mut unsubmittable_files: Vec<File> = vec![];

            for file in self.files.iter() {
                let mut all_modified_iter = repo_status
                    .modified_files
                    .0
                    .iter()
                    .chain(repo_status.untracked_files.0.iter());
                if let Some(file) = all_modified_iter.find(|x| x.path == *file) {
                    match file.submit_status {
                        SubmitStatus::Ok => {}
                        _ => unsubmittable_files.push(file.clone()),
                    }
                }
            }

            if !unsubmittable_files.is_empty() {
                let engine_path = self
                    .app_config
                    .read()
                    .load_engine_path_from_repo(&self.repo_config.read())
                    .unwrap_or_default();
                let unsubmittable_file_paths: Vec<String> =
                    unsubmittable_files.iter().map(|x| x.path.clone()).collect();

                let unsubmittable_display_names = self
                    .engine
                    .get_asset_display_names(
                        CommunicationType::None,
                        &engine_path,
                        &unsubmittable_file_paths,
                    )
                    .await;

                for (file, display_name) in unsubmittable_files
                    .iter()
                    .zip(unsubmittable_display_names.iter())
                {
                    let name_formatted: String = if display_name.is_empty() {
                        file.path.clone()
                    } else {
                        format!("{} ({})", display_name, file.path)
                    };
                    let reason = match file.submit_status {
                        SubmitStatus::Ok => panic!("should have been filtered out by above code"),
                        SubmitStatus::CheckoutRequired => "This file is an asset and must be checked out (locked) before submitting",
                        SubmitStatus::CheckedOutByOtherUser => "This file is an asset and must be checked out (locked) before submitting, but it is locked by another user",
                        SubmitStatus::Unmerged => "This file is unmerged and must be reverted to continue",
                        SubmitStatus::Conflicted => "A newer version of this file exists; this file must be reverted to continue",
                    };
                    tracing::error!("{}: {}", reason, name_formatted);
                }
                return Err(CoreError::Input(anyhow!("Some files are not allowed to be submitted. Check the log for specific errors.")));
            }
        }

        // Refuse to ship files with unresolved git conflict markers. We
        // delegate to `git diff --check`, which is what git itself uses
        // (rebase, sample pre-commit hook) — so it respects
        // `merge.conflictMarkerSize` and git's own binary/text detection,
        // and only complains about markers in changes being introduced (not
        // pre-existing markers in unrelated content). Runs before the
        // snapshot so a doomed submit doesn't waste one.
        {
            let conflicted =
                find_files_with_conflict_markers(&self.git_client, &self.files).await?;
            if !conflicted.is_empty() {
                for path in &conflicted {
                    tracing::error!("Unresolved conflict markers in {}", path);
                }
                return Err(CoreError::Input(anyhow!(
                    "Cannot submit: {} file(s) still contain unresolved merge conflict markers:\n  - {}\n\nResolve the conflict markers (the <<<<<<<, =======, >>>>>>> lines) before submitting.",
                    conflicted.len(),
                    conflicted.join("\n  - ")
                )));
            }
        }

        // save a snapshot before submitting with all modified/added files
        // make sure we have a temp dir for copying our files
        let status = self.repo_status.read().clone();
        let modified_files = status.modified_files.0.clone();
        let untracked_files = status.untracked_files.0.clone();
        let all_files: Vec<String> = modified_files
            .into_iter()
            .chain(untracked_files.into_iter())
            .map(|file| file.path.clone())
            .collect();
        if !all_files.is_empty() {
            self.emit_phase("Snapshotting local changes");
        }
        let snapshot = self
            .git_client
            .save_snapshot("pre-submit", all_files)
            .await?;

        match self.execute_internal().await {
            Ok(_) => Ok(()),
            Err(e) => {
                // can't touch the working tree unless the engine isn't running
                if self.engine.check_ready_to_sync_repo().await.is_ok() {
                    // attempt to reset to original branch and restore snapshot
                    // if this fails for any reason, we should simply log, then return the original error
                    let branch = self.repo_status.read().branch.clone();
                    self.git_client.hard_reset(&branch).await?;

                    match self
                        .git_client
                        .restore_snapshot_via_cherry_pick(&snapshot.commit, vec![])
                        .await
                    {
                        Ok(_) => {}
                        Err(e) => {
                            // log the error, but don't return it
                            warn!("Failed to restore snapshot after failed submit: {}", e);
                        }
                    }
                } else {
                    warn!("Unable to automatically restore pre-submit state due to editor running.")
                }
                Err(e)
            }
        }
    }

    fn get_name(&self) -> String {
        "SubmitOp".to_string()
    }
}

impl<T> SubmitOp<T>
where
    T: EngineProvider,
{
    #[instrument(name = "SubmitOp::execute_internal", skip(self))]
    pub async fn execute_internal(&self) -> Result<(), CoreError> {
        let target_branch = self.app_config.read().target_branch.clone();
        let prev_branch = self.repo_status.read().branch.clone();

        // Validate target branch exists on remote before proceeding
        if !self.git_client.has_remote_branch(&target_branch).await? {
            return Err(CoreError::Input(anyhow!(
                "Target branch '{}' does not exist on remote. Check your target branch configuration.",
                target_branch
            )));
        }

        let target_branch_configs = self.repo_config.read().target_branches.clone();
        let use_merge_queue = target_branch_configs
            .iter()
            .find(|config| config.name == target_branch)
            .ok_or_else(|| {
                CoreError::Input(anyhow!(
                    "Target branch `{}` not found in repo config",
                    target_branch
                ))
            })?
            .uses_merge_queue;

        let mut f11r_branch = {
            let display_name = &self.app_config.read().user_display_name;
            let santized_display_name = display_name.replace(' ', "-");
            format!(
                "f11r-{}-{}-{}",
                target_branch,
                santized_display_name,
                chrono::Utc::now().timestamp()
            )
        };

        // If the target branch uses merge queue, it's possible there's an inflight quicksubmit.
        // Cancel it - we can be reasonably sure
        let mut needs_new_pr = true;
        let mut quicksubmit_pr_id: Option<String> = None;
        if use_merge_queue && is_quicksubmit_branch(&prev_branch) {
            let owner: String;
            let repo: String;
            {
                let status = self.repo_status.read();
                owner = status.repo_owner.clone();
                repo = status.repo_name.clone();
            }

            // Skip merge queue check if repo owner/name are not set (during startup)
            if owner.is_empty() || repo.is_empty() {
                debug!("Skipping merge queue check: repo owner/name not yet configured (owner='{}', repo='{}')", owner, repo);
                return Err(CoreError::Internal(anyhow::anyhow!("Repository information not yet available - please wait for repo status to initialize")));
            }

            let merge_queue = self
                .github_client
                .get_merge_queue_no_retry(&owner, &repo)
                .await?;
            if let Some(entries) = merge_queue.entries {
                if let Some(nodes) = entries.nodes {
                    for node in nodes.into_iter().flatten() {
                        if let Some(commit) = node.head_commit {
                            if let Some(author) = commit.author {
                                if let Some(pr) = node.pull_request {
                                    if let Some(user) = author.user {
                                        if user.login == self.github_client.username
                                            && pr.title.starts_with(SUBMIT_PREFIX)
                                        {
                                            // There should only be one quicksubmit PR in merge queue at a time
                                            quicksubmit_pr_id = Some(pr.id);
                                            break;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }

            if let Some(id) = &quicksubmit_pr_id {
                // Silently absorb errors - the PR may have been already merged in this case
                let res = self
                    .github_client
                    .dequeue_pull_request(id.to_string())
                    .await;
                match res {
                    Ok(_) => {
                        needs_new_pr = false;
                    }
                    Err(e) => {
                        warn!(
                            "Failed to cancel existing pull request {}. Reason: {}",
                            id, e
                        );
                    }
                }
            }
        }

        if needs_new_pr {
            // If we're currently on a quicksubmit branch, we need to first checkout
            // the target branch to ensure the new f11r branch is created from the
            // latest target branch, not from the old quicksubmit branch.
            // This prevents including commits from the previous quicksubmit that may
            // have already been merged.
            if is_quicksubmit_branch(&prev_branch) {
                // A branch swap is about to happen — block if the engine is running,
                // since swapping files out from under Unreal can cause issues.
                if self.engine.check_ready_to_sync_repo().await.is_err() {
                    return Err(CoreError::Input(anyhow!(
                        "Unreal Editor must be closed before submitting. Your previous submit branch has been merged, so a branch swap is required."
                    )));
                }
                self.emit_phase(format!("Switching to {target_branch}"));
                // Ensure target branch is available locally before checkout
                if !self.git_client.has_local_branch(&target_branch).await? {
                    info!(
                        "Target branch '{}' not found locally, fetching from remote",
                        target_branch
                    );
                    self.git_client
                        .run(
                            &["checkout", "--track", &format!("origin/{}", target_branch)],
                            Default::default(),
                        )
                        .await?;
                } else {
                    self.git_client
                        .run(&["checkout", &target_branch], Default::default())
                        .await?;
                }

                // Pull latest changes from target branch
                self.git_client
                    .run(&["pull", "origin", &target_branch], Default::default())
                    .await?;
            }

            self.git_client
                .run(&["checkout", "-b", &f11r_branch], Default::default())
                .await?;

            // Clean up the old f11r branch, if it was one
            if is_quicksubmit_branch(&prev_branch) {
                self.git_client
                    .delete_branch(&prev_branch, git::BranchType::Local)
                    .await?;
            }
        } else {
            f11r_branch.clone_from(&prev_branch);
        }

        // commit changes
        self.emit_phase("Committing changes");
        {
            let add_op = AddOp {
                files: self.files.clone(),
                git_client: self.git_client.clone(),
            };

            add_op.execute().await?;

            // unstage any files that are staged but not in the request
            let mut staged_files = Vec::new();
            {
                let repo_status = self.repo_status.read();
                let modified = repo_status.modified_files.clone();
                for file in modified.into_iter() {
                    if file.is_staged {
                        staged_files.push(file.path.clone());
                    }
                }
            }

            let files_to_unstage: Vec<String> = staged_files
                .into_iter()
                .filter(|file| !self.files.contains(file))
                .collect();

            if !files_to_unstage.is_empty() {
                let restore_op = RestoreOp {
                    files: files_to_unstage,
                    git_client: self.git_client.clone(),
                };

                restore_op.execute().await?;
            }

            // Debug logging to diagnose commit failures
            let current_branch = self.git_client.current_branch().await?;
            debug!("About to commit on branch: {}", current_branch);

            // Check what's actually staged before committing
            let status_output = self
                .git_client
                .run_and_collect_output(&["status", "--short"], git::Opts::default())
                .await
                .unwrap_or_else(|_| "Failed to get status".to_string());
            debug!("Git status before commit:\n{}", status_output);

            // We can skip the status check because we know for a fact that there are staged files
            let commit_op = CommitOp {
                message: self.commit_message.clone(),
                repo_status: self.repo_status.clone(),
                git_client: self.git_client.clone(),
                skip_status_check: true,
            };

            match commit_op.execute().await {
                Ok(_) => debug!("Commit succeeded"),
                Err(e) => {
                    error!(
                        "Commit failed on branch '{}' with error: {}",
                        current_branch, e
                    );
                    error!("Status was:\n{}", status_output);
                    return Err(e);
                }
            }
        }

        self.emit_phase("Pushing to GitHub");
        let worktree_path: PathBuf = 'path: {
            let repo_path = PathBuf::from(self.app_config.read().repo_path.clone());

            let worktrees = self.git_client.list_worktrees().await?;
            for tree in worktrees.iter() {
                if tree.directory != repo_path {
                    // if the directory exists on disk, break
                    if tree.directory.exists() {
                        break 'path tree.directory.clone();
                    }

                    // if the directory doesn't exist, remove the worktree
                    self.git_client
                        .run(
                            &[
                                "worktree",
                                "remove",
                                tree.directory.to_string_lossy().as_ref(),
                            ],
                            Default::default(),
                        )
                        .await?;
                }
            }

            let repo_folder_name: String = repo_path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();

            // create worktree if it doesn't exist yet
            let mut worktree_path = repo_path.clone();
            worktree_path.pop();
            worktree_path.push(format!(".{repo_folder_name}-wt"));

            self.git_client
                .run(
                    &[
                        "worktree",
                        "add",
                        "--detach",
                        &worktree_path.to_string_lossy(),
                    ],
                    git::Opts::default().with_lfs_stubs(),
                )
                .await?;

            worktree_path.clone()
        };

        let worktree_branch = format!("{f11r_branch}-wt");

        let mut git_client_worktree = self.git_client.clone();
        git_client_worktree.repo_path.clone_from(&worktree_path);

        // To make the worktree as cheap as possible, we need to make sure no LFS files are checked out and
        // they remain stubs
        let git_opts_lfs_stubs = git::Opts::default().with_lfs_stubs();

        // Abort any in-progress rebase from a previous failed submit
        _ = git_client_worktree
            .run(
                &["rebase", "--abort"],
                git::Opts::new_with_ignored(&["no rebase in progress"]).with_lfs_stubs(),
            )
            .await;

        // make sure the worktree is hard reset
        git_client_worktree
            .run(&["reset", "--hard"], git_opts_lfs_stubs)
            .await?;
        git_client_worktree
            .run(&["clean", "-fd"], git_opts_lfs_stubs)
            .await?;

        // resolve changes with latest main and push up to the remote
        {
            let worktree_prev_branch = git_client_worktree.current_branch().await?;

            // delete the worktree branch if it exists - we need to make one that matches the state of
            // f11r_branch exactly, and the old worktree branch will likely have changes from main mixed
            // up into it.
            if worktree_branch == worktree_prev_branch {
                cleanup_worktree_branch(&git_client_worktree, &worktree_branch, git_opts_lfs_stubs)
                    .await;
            }

            // Checkout a new branch for the worktree in the same state as the f11r branch
            self.git_client
                .run(
                    &["branch", &worktree_branch, &f11r_branch],
                    git::Opts::default(),
                )
                .await?;

            // now we can resolve any new changes in main with the current changes and push up to the remote
            git_client_worktree
                .run(&["checkout", &worktree_branch], git_opts_lfs_stubs)
                .await?;
            git_client_worktree
                .run(&["fetch", "origin", &*target_branch], git_opts_lfs_stubs)
                .await?;
            git_client_worktree
                .run(
                    &["rebase", &format!("origin/{target_branch}")],
                    git_opts_lfs_stubs,
                )
                .await?;

            // force is needed when pushing changes because we may be reusing a remote branch
            git_client_worktree
                .run(
                    &["push", "-f", "origin", &worktree_branch],
                    git::Opts::default(),
                )
                .await?;

            // cleanup old branch
            if worktree_branch != worktree_prev_branch
                && is_quicksubmit_branch(&worktree_prev_branch)
            {
                git_client_worktree
                    .delete_branch(&worktree_prev_branch, git::BranchType::Local)
                    .await?;
            }
        }

        // If we already have a PR, we must be using the merge queue so just requeue it.
        // Otherwise create a whole new Github submit op.
        match quicksubmit_pr_id {
            // We have a PR, this is a requeue
            Some(pr_id) => {
                self.emit_phase("Re-queuing pull request");
                info!("Reusing existing PR with ID: {}", pr_id);

                // Wait for GitHub to process the force push and run checks before enqueueing.
                // GitHub may silently reject the enqueue if the PR isn't ready yet.
                info!(
                    "Waiting 15 seconds for GitHub to process force push before re-enqueueing..."
                );
                tokio::time::sleep(std::time::Duration::from_secs(15)).await;

                info!("Re-enqueueing PR {} to merge queue", pr_id);
                match self.github_client.enqueue_pull_request(pr_id.clone()).await {
                    Ok(_) => {
                        info!("Successfully re-enqueued PR {}", pr_id);
                    }
                    Err(e) => {
                        error!("Failed to re-enqueue PR {}: {}", pr_id, e);
                        return Err(e.into());
                    }
                }
                cleanup_worktree_branch(&git_client_worktree, &worktree_branch, git_opts_lfs_stubs)
                    .await;
            }
            // We have no active PR in queue, lets setup a new one
            None => {
                let gh_op = GitHubSubmitOp {
                    head_branch: worktree_branch.clone(),
                    base_branch: target_branch.clone(),
                    token: self.token.clone(),
                    commit_message: self.commit_message.clone(),
                    repo_status: self.repo_status.clone(),
                    client: self.github_client.clone(),
                    use_merge_queue,
                };

                if use_merge_queue {
                    self.emit_phase("Adding to merge queue");
                    gh_op.execute().await?;

                    cleanup_worktree_branch(
                        &git_client_worktree,
                        &worktree_branch,
                        git_opts_lfs_stubs,
                    )
                    .await;

                    return Ok(());
                } else {
                    self.emit_phase("Submitting pull request");
                    // Non-merge-queue: gh_op.execute() blocks until merge completes
                    let merge_result = gh_op.execute().await;

                    cleanup_worktree_branch(
                        &git_client_worktree,
                        &worktree_branch,
                        git_opts_lfs_stubs,
                    )
                    .await;

                    match merge_result {
                        Ok(()) => {
                            self.emit_phase("Releasing file locks");
                            info!("PR merge confirmed, unlocking files");
                            let github_username = self.github_client.username.clone();
                            let lock_op = LockOp {
                                git_client: self.git_client.clone(),
                                paths: self.files.clone(),
                                op: LockOperation::Unlock,
                                response_tx: None,
                                github_pat: self.token.clone(),
                                repo_status: self.repo_status.clone(),
                                github_username,
                                force: false,
                            };
                            lock_op.execute().await?;

                            // Autosync if editor is not running
                            if self.engine.check_ready_to_sync_repo().await.is_ok() {
                                if let (Some(aws_client), Some(storage)) =
                                    (self.aws_client.clone(), self.storage.clone())
                                {
                                    info!(
                                        "Auto-syncing back to target branch after quicksubmit merge"
                                    );
                                    let pull_op = PullOp {
                                        app_config: self.app_config.clone(),
                                        repo_config: self.repo_config.clone(),
                                        repo_status: self.repo_status.clone(),
                                        longtail: self.longtail.clone(),
                                        longtail_tx: self.longtail_tx.clone(),
                                        aws_client,
                                        storage,
                                        git_client: self.git_client.clone(),
                                        github_client: Some(self.github_client.clone()),
                                        engine: self.engine.clone(),
                                        sync_phase_tx: self.sync_phase_tx.clone(),
                                        // The pre-submit snapshot already covers the working
                                        // tree, so skip the expensive duplicate snapshot inside
                                        // PullOp. git pull --autostash still protects dirty
                                        // files during the rebase.
                                        skip_snapshot: true,
                                    };
                                    if let Err(e) = pull_op.execute().await {
                                        // Don't return Err here — the commit/push/merge all
                                        // succeeded, and returning Err would trigger the
                                        // destructive snapshot-restore in execute().
                                        let msg = format!(
                                            "Your changes were submitted successfully! \
                                            Auto-sync back to the target branch failed: {}. \
                                            If you see conflicts with your local files, you can \
                                            restore them from the pre-submit snapshot in the \
                                            Snapshots tab. Otherwise, just Sync manually when ready.",
                                            e
                                        );
                                        error!("{}", msg);
                                        let _ = self.notification_tx.send(Notification::Error(msg));
                                        return Ok(());
                                    }
                                    let _ = self.notification_tx.send(Notification::Success(
                                        "Changes submitted and auto-sync complete.".to_string(),
                                    ));
                                } else {
                                    // Note: don't expect this to be hit since we shouldn't be able to get here, but logging error if we do.
                                    error!("AWS client or storage not available, skipping autosync after quicksubmit");
                                    let _ = self.notification_tx.send(
                                        Notification::Error("AWS client or storage not available, unable to auto-sync. Please sync manually.".to_string())
                                    );
                                }
                            } else {
                                let msg = "Quicksubmit merge complete, but skipping auto-sync since Editor is running".to_string();
                                info!("{}", msg);
                                let _ = self.notification_tx.send(Notification::Success(msg));
                            }

                            return Ok(());
                        }
                        Err(e) => {
                            // Merge failed or timed out. Files stay locked.
                            // Don't return Err — the commit/push/PR all succeeded, and
                            // returning Err would trigger the destructive snapshot-restore
                            // in execute().
                            warn!("PR created but merge did not complete: {}", e);
                            if self.engine.check_ready_to_sync_repo().await.is_ok() {
                                let msg = format!(
                                    "PR was created but merge did not confirm: {}. Files remain locked.",
                                    e
                                );
                                error!("{}", msg);
                                let _ = self.notification_tx.send(Notification::Error(msg));
                            } else {
                                info!("Editor running, merge timeout is not critical. PR may still merge via GitHub.");
                                let _ = self.notification_tx.send(
                                    Notification::Error(format!("PR was created but merge did not confirm: {}. PR should be available to merge manually via GitHub.", e))
                                );
                            }
                            return Ok(());
                        }
                    }
                }
            }
        }

        Ok(())
    }
}

/// Detach the worktree HEAD and delete the given local branch.
/// Errors are intentionally swallowed — this is best-effort cleanup.
async fn cleanup_worktree_branch(git_client: &git::Git, branch: &str, opts: git::Opts<'_>) {
    _ = git_client.run(&["checkout", "--detach"], opts).await;
    _ = git_client
        .delete_branch(branch, git::BranchType::Local)
        .await;
}

#[instrument(skip(state))]
pub async fn submit_handler<T>(
    State(state): State<AppState<T>>,
    Json(request): Json<PushRequest>,
) -> Result<Json<String>, CoreError>
where
    T: EngineProvider,
{
    let token = state
        .app_config
        .read()
        .github_pat
        .clone()
        .ok_or(CoreError::Input(anyhow!(
            "GitHub PAT is not configured. Please configure it in the settings."
        )))?;

    if request.files.is_empty() {
        return Err(CoreError::Input(anyhow!("No files to submit")));
    }

    let github_client = match state.github_client.read().clone() {
        Some(client) => client.clone(),
        None => return Err(CoreError::Internal(anyhow!(TokenNotFoundError))),
    };

    let aws_client = state.aws_client.read().await.clone();
    let storage = state.storage.read().clone();

    let submit_op = SubmitOp {
        files: request.files.clone(),
        commit_message: request.commit_message.clone(),

        app_config: state.app_config.clone(),
        repo_config: state.repo_config.clone(),
        engine: state.engine.clone(),
        aws_client,
        storage,
        repo_status: state.repo_status.clone(),

        longtail: state.longtail.clone(),
        longtail_tx: state.longtail_tx.clone(),
        notification_tx: state.notification_tx.clone(),
        sync_phase_tx: state.sync_phase_tx.clone(),

        git_client: state.git(),
        token: token.to_string(),
        github_client,
    };

    let (tx, rx) = tokio::sync::oneshot::channel::<Option<CoreError>>();
    let mut sequence = TaskSequence::new().with_completion_tx(tx);
    sequence.push(Box::new(submit_op));

    state.operation_tx.send(sequence).await?;

    match rx.await {
        Ok(Some(e)) => {
            return Err(e);
        }
        Ok(None) => {}
        Err(e) => return Err(e.into()),
    }

    Ok(Json("ok".to_string()))
}

pub fn is_quicksubmit_branch(branch: &str) -> bool {
    branch.starts_with("f11r")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::mpsc;

    /// Initialize an empty git repo with one initial commit so HEAD resolves.
    /// Returns the temp dir guard (drop = cleanup) and a `Git` client pointed
    /// at it.
    async fn make_test_repo() -> (tempfile::TempDir, git::Git) {
        let dir = tempfile::tempdir().expect("create tempdir");
        let (tx, _rx) = mpsc::channel::<String>();
        // Leak the receiver — the channel just needs to stay open for the
        // duration of the test. Dropping `_rx` is fine; the sender's send()
        // calls will fail silently but git's run() doesn't propagate that.
        let git = git::Git::new(dir.path().to_path_buf(), tx);

        git.run(&["init", "-q"], git::Opts::default())
            .await
            .expect("git init");
        git.run(
            &["config", "user.email", "test@example.com"],
            git::Opts::default(),
        )
        .await
        .expect("set user.email");
        git.run(&["config", "user.name", "Test"], git::Opts::default())
            .await
            .expect("set user.name");

        fs::write(dir.path().join("seed.txt"), "seed\n").expect("write seed");
        git.run(&["add", "seed.txt"], git::Opts::default())
            .await
            .expect("git add seed");
        git.run(&["commit", "-qm", "seed"], git::Opts::default())
            .await
            .expect("initial commit");

        (dir, git)
    }

    #[tokio::test]
    async fn flags_untracked_new_file_with_markers() {
        // The regression case: a brand-new file (not in HEAD, not staged)
        // with conflict markers — the kind that previously slipped through.
        let (dir, git) = make_test_repo().await;
        let path = "Config/DummyTestWithConflicts.ini";
        fs::create_dir_all(dir.path().join("Config")).unwrap();
        fs::write(
            dir.path().join(path),
            "test\n<<<<<<< Updated upstream\ntest\n=======\ntest\n>>>>>>> Stashed changes\ntest\n",
        )
        .unwrap();

        let result = find_files_with_conflict_markers(&git, &[path.to_string()])
            .await
            .expect("check ran");
        assert_eq!(result, vec![path.to_string()]);
    }

    #[tokio::test]
    async fn flags_tracked_file_modified_with_markers() {
        let (dir, git) = make_test_repo().await;
        let path = "conflicted.txt";
        // First commit it clean...
        fs::write(dir.path().join(path), "before\n").unwrap();
        git.run(&["add", path], git::Opts::default()).await.unwrap();
        git.run(&["commit", "-qm", "add"], git::Opts::default())
            .await
            .unwrap();
        // ...then modify in place to add a conflict.
        fs::write(
            dir.path().join(path),
            "ok\n<<<<<<< HEAD\nmine\n=======\ntheirs\n>>>>>>> branch\nok\n",
        )
        .unwrap();

        let result = find_files_with_conflict_markers(&git, &[path.to_string()])
            .await
            .expect("check ran");
        assert_eq!(result, vec![path.to_string()]);
    }

    #[tokio::test]
    async fn skips_binary_file_even_with_marker_bytes() {
        // A binary file (NUL byte in the first 8 KB) that happens to contain
        // bytes spelling out conflict markers should not be flagged — that's
        // how LFS-tracked smudged assets stay out of the check.
        let (dir, git) = make_test_repo().await;
        let path = "binary.dat";
        let mut content: Vec<u8> = b"<<<<<<< HEAD\nbefore-nul\n".to_vec();
        content.push(0u8); // NUL forces git's binary classifier
        content.extend_from_slice(b"after-nul\n=======\nstuff\n>>>>>>> branch\n");
        fs::write(dir.path().join(path), content).unwrap();

        let result = find_files_with_conflict_markers(&git, &[path.to_string()])
            .await
            .expect("check ran");
        assert!(
            result.is_empty(),
            "binary content with marker-shaped bytes should be skipped, got {result:?}"
        );
    }

    #[tokio::test]
    async fn skips_deletion() {
        // A file tracked in HEAD that's been removed from the working tree
        // has no `+` lines to scan — `--check` (correctly) finds nothing.
        let (dir, git) = make_test_repo().await;
        let path = "to-delete.txt";
        fs::write(dir.path().join(path), "original\n").unwrap();
        git.run(&["add", path], git::Opts::default()).await.unwrap();
        git.run(&["commit", "-qm", "add"], git::Opts::default())
            .await
            .unwrap();
        fs::remove_file(dir.path().join(path)).unwrap();

        let result = find_files_with_conflict_markers(&git, &[path.to_string()])
            .await
            .expect("check ran");
        assert!(
            result.is_empty(),
            "deletions cannot carry conflict markers, got {result:?}"
        );
    }

    #[tokio::test]
    async fn dedups_multiple_marker_lines_per_file() {
        // A single file with two separate conflict regions should appear in
        // the result list exactly once.
        let (dir, git) = make_test_repo().await;
        let path = "multi.txt";
        fs::write(
            dir.path().join(path),
            "a\n<<<<<<< HEAD\nm1\n=======\nt1\n>>>>>>> b\n\
             b\n<<<<<<< HEAD\nm2\n=======\nt2\n>>>>>>> b\nc\n",
        )
        .unwrap();

        let result = find_files_with_conflict_markers(&git, &[path.to_string()])
            .await
            .expect("check ran");
        assert_eq!(result, vec![path.to_string()]);
    }

    #[tokio::test]
    async fn passes_clean_file() {
        let (dir, git) = make_test_repo().await;
        let path = "clean.txt";
        fs::write(dir.path().join(path), "all good\nno markers here\n").unwrap();

        let result = find_files_with_conflict_markers(&git, &[path.to_string()])
            .await
            .expect("check ran");
        assert!(result.is_empty(), "clean file flagged: {result:?}");
    }

    #[tokio::test]
    async fn early_returns_on_empty_input() {
        let (_dir, git) = make_test_repo().await;
        let result = find_files_with_conflict_markers(&git, &[])
            .await
            .expect("check ran");
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn prefilter_skips_binary_per_gitattributes() {
        // A path declared `-text` (or via the `binary` macro) in
        // `.gitattributes` should be filtered out before we even stage it.
        // This is the optimization that keeps 15k-asset submits fast: the
        // file never gets read, never gets hashed, never gets piped through
        // an LFS clean filter.
        let (dir, git) = make_test_repo().await;
        fs::write(dir.path().join(".gitattributes"), "*.dat -text\n").unwrap();
        git.run(&["add", ".gitattributes"], git::Opts::default())
            .await
            .unwrap();
        git.run(&["commit", "-qm", "attrs"], git::Opts::default())
            .await
            .unwrap();

        // Even though the file's bytes spell out a conflict marker, the
        // -text attribute pre-filters it out. `git diff --check` would
        // skip it anyway via binary detection; we just save the I/O.
        let evil = "evil.dat";
        fs::write(
            dir.path().join(evil),
            "<<<<<<< HEAD\nm\n=======\nt\n>>>>>>> b\n",
        )
        .unwrap();

        let result = find_files_with_conflict_markers(&git, &[evil.to_string()])
            .await
            .expect("check ran");
        assert!(
            result.is_empty(),
            "pre-filter should have excluded the binary path, got {result:?}"
        );
    }

    #[tokio::test]
    async fn prefilter_keeps_textlike_paths() {
        // Sanity check that the pre-filter doesn't accidentally exclude
        // normal source files. A text file with no .gitattributes entry
        // should pass through and get scanned for markers.
        let (dir, git) = make_test_repo().await;
        let path = "regular.txt";
        fs::write(
            dir.path().join(path),
            "ok\n<<<<<<< HEAD\nm\n=======\nt\n>>>>>>> b\nok\n",
        )
        .unwrap();

        let result = find_files_with_conflict_markers(&git, &[path.to_string()])
            .await
            .expect("check ran");
        assert_eq!(result, vec![path.to_string()]);
    }

    #[tokio::test]
    async fn stale_file_list_is_input_error() {
        // A selected path that is neither tracked nor present in the working
        // tree makes `git add --pathspec-from-file` fail with "pathspec did
        // not match". That's user-actionable (stale file list), so it must
        // surface as CoreError::Input (400) — not Internal (500).
        let (_dir, git) = make_test_repo().await;
        let result =
            find_files_with_conflict_markers(&git, &["does/not/exist.txt".to_string()]).await;
        match result {
            Err(CoreError::Input(_)) => {}
            other => panic!("expected CoreError::Input for a stale file list, got {other:?}"),
        }
    }

    #[test]
    fn parse_conflict_check_errors_on_unexpected_exit() {
        // Exit codes outside 0..=2 (e.g. 129 from a usage error) must be an
        // error, not silently treated as "clean" — the regression guard.
        assert!(parse_conflict_check(129, "").is_err());
        assert!(parse_conflict_check(-1, "").is_err());
    }

    #[test]
    fn parse_conflict_check_extracts_and_dedups_paths() {
        let stdout = "a/b.txt:7: leftover conflict marker\n\
                      a/b.txt:42: leftover conflict marker\n\
                      c.ini:1: leftover conflict marker\n";
        assert_eq!(
            parse_conflict_check(2, stdout).unwrap(),
            vec!["a/b.txt".to_string(), "c.ini".to_string()]
        );
    }

    #[test]
    fn parse_conflict_check_ignores_whitespace_warnings() {
        // `git diff --check` also emits whitespace-error lines (exit 1); only
        // "leftover conflict marker" lines should count.
        assert!(parse_conflict_check(1, "x.txt:3: trailing whitespace.\n")
            .unwrap()
            .is_empty());
        assert!(parse_conflict_check(0, "").unwrap().is_empty());
    }

    // A minimal `git diff --cached HEAD`-shaped unified diff adding `body` as
    // the contents of a single new file.
    fn diff_adding(path: &str, body: &str) -> String {
        let mut out = format!(
            "diff --git a/{path} b/{path}\nnew file mode 100644\nindex 0000000..1111111\n--- /dev/null\n+++ b/{path}\n@@ -0,0 +1,9 @@\n"
        );
        for l in body.lines() {
            out.push('+');
            out.push_str(l);
            out.push('\n');
        }
        out
    }

    #[test]
    fn confirm_paired_keeps_real_conflict() {
        let diff = diff_adding("a.txt", "x\n<<<<<<< HEAD\nm\n=======\nt\n>>>>>>> b\ny");
        let candidates = vec!["a.txt".to_string()];
        assert_eq!(
            confirm_paired_markers(&diff, &candidates, 7),
            vec!["a.txt".to_string()]
        );
    }

    #[test]
    fn confirm_paired_drops_lone_divider() {
        // The real-world false positive: a `=======` section underline with
        // no opening/closing marker.
        let diff = diff_adding("doc.txt", "SUMMARY\n=======\nall good");
        let candidates = vec!["doc.txt".to_string()];
        assert!(confirm_paired_markers(&diff, &candidates, 7).is_empty());
    }

    #[test]
    fn confirm_paired_drops_single_ended_markers() {
        // Opening-only (a `<<<<<<< note` banner) and closing-only are both
        // incoherent and must be dropped.
        let open_only = diff_adding("o.txt", "head\n<<<<<<< not a conflict\ntail");
        let close_only = diff_adding("c.txt", "head\n>>>>>>> end of section\ntail");
        assert!(confirm_paired_markers(&open_only, &["o.txt".to_string()], 7).is_empty());
        assert!(confirm_paired_markers(&close_only, &["c.txt".to_string()], 7).is_empty());
    }

    #[test]
    fn confirm_paired_honors_marker_size() {
        // A 7-char marker is not a marker when the configured size is 8.
        let diff = diff_adding("a.txt", "<<<<<<< h\n=======\n>>>>>>> b");
        assert!(confirm_paired_markers(&diff, &["a.txt".to_string()], 8).is_empty());
        assert_eq!(
            confirm_paired_markers(&diff, &["a.txt".to_string()], 7),
            vec!["a.txt".to_string()]
        );
    }

    #[test]
    fn confirm_paired_ignores_files_not_in_candidates() {
        let diff = diff_adding("other.txt", "<<<<<<< h\n=======\n>>>>>>> b");
        assert!(confirm_paired_markers(&diff, &["a.txt".to_string()], 7).is_empty());
    }

    #[tokio::test]
    async fn lone_divider_is_not_flagged_end_to_end() {
        // Full path through find_files: a new file with a lone `=======`
        // divider trips `git diff --check`, but the pairing pass clears it,
        // so a legitimate submit is not blocked. This is the
        // VALIDATION_ANALYSIS.txt case from the fellowship repo.
        let (dir, git) = make_test_repo().await;
        let path = "NOTES.txt";
        fs::write(dir.path().join(path), "SUMMARY\n=======\nLooks good.\n").unwrap();

        let result = find_files_with_conflict_markers(&git, &[path.to_string()])
            .await
            .expect("check ran");
        assert!(
            result.is_empty(),
            "lone === divider should not be flagged, got {result:?}"
        );
    }
}
