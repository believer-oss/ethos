use std::str;
use std::sync::Arc;

use anyhow::anyhow;
use axum::extract::Query;
use axum::{async_trait, extract::State, Json};
use parking_lot::RwLock;
use serde::Deserialize;
use tracing::{debug, error, info, instrument, warn};

use crate::engine;
use crate::engine::EngineProvider;
use crate::state::AppState;
use ethos_core::clients::aws::ensure_aws_client;
use ethos_core::clients::git;
use ethos_core::storage::{
    config::Project, ArtifactBuildConfig, ArtifactConfig, ArtifactKind, ArtifactList,
    ArtifactStorage, Platform,
};
use ethos_core::types::config::AppConfigRef;
use ethos_core::types::config::RepoConfigRef;
use ethos_core::types::errors::CoreError;
use ethos_core::types::repo::{File, RepoStatus};
use ethos_core::worker::{Task, TaskSequence};
use ethos_core::AWSClient;

pub type RepoStatusRef = Arc<RwLock<RepoStatus>>;

/*
git status --porcelain=v2 --branch --ignored

# branch.oid 06a3b67b10cf37ed685fbb8df4a0dd31f0c7fb05
# branch.head ar/exe-party
# branch.upstream origin/ar/exe-party
# branch.ab +0 -0
1 M. N... 100644 100644 100644 232ff8b3124704a5cca2bfab1c065cef6e30418f 75adf399778e40238255a7d19b4f42918e63a40b friendshipper-svc/src/repo/operations.rs

First column:
    # = branch info
    1 = tracked file
    2 = renamed file
    u = unmerged file
    ? = untracked file
    ! = ignored file

If line is a file, second column:
    M = modified
    A = added
    D = deleted
    R = renamed
    C = copied

    . before means unstaged, . after means staged.

Third column is always N... unless it's a submodule.

From there: octal file mode at HEAD, octal file mode in index, octal file mode in worktree, object name in HEAD, object name in index, file path
*/

#[derive(Clone, Debug)]
pub struct StatusOp<T>
where
    T: EngineProvider,
{
    pub git_client: git::Git,
    pub repo_status: RepoStatusRef,
    pub app_config: AppConfigRef,
    pub repo_config: RepoConfigRef,
    pub engine: T,
    pub aws_client: AWSClient,
    pub storage: ArtifactStorage,
    pub skip_fetch: bool,
    pub skip_dll_check: bool,
    pub allow_offline_communication: bool,
}

#[async_trait]
impl<T> Task for StatusOp<T>
where
    T: EngineProvider,
{
    async fn execute(&self) -> anyhow::Result<()> {
        let _ = self.run().await?;

        Ok(())
    }

    fn get_name(&self) -> String {
        String::from("RepoStatus")
    }
}

impl<T> StatusOp<T>
where
    T: EngineProvider,
{
    #[instrument]
    pub(crate) async fn run(&self) -> anyhow::Result<RepoStatus> {
        if !self.skip_fetch {
            info!("Fetching latest for {:?}", self.git_client.repo_path);
            self.git_client
                .run(&["fetch", "--prune"], git::Opts::default())
                .await?;
        }

        info!("StatusOp: running git status...");
        let status_output = self.git_client.status().await?;
        let status_lines = status_output.lines().collect::<Vec<_>>();

        info!("StatusOp: parsing status state...");

        let mut status = RepoStatus::new();
        let pull_dlls = self.app_config.read().pull_dlls;

        // make sure we always copy over the last push/sync status
        {
            let current_status = self.repo_status.read();

            // because dll checking can be skipped, default to current values
            status.origin_has_new_dlls = current_status.origin_has_new_dlls;
            status.pull_dlls = pull_dlls;
            status
                .dll_commit_local
                .clone_from(&current_status.dll_commit_local);
            status
                .dll_commit_remote
                .clone_from(&current_status.dll_commit_remote);
        }

        for line in status_lines {
            if line.starts_with("##") {
                status.parse_branch_string(line);

                continue;
            }

            let file = File::from_status_line(line);

            if !file.index_state.is_empty() && !status.has_staged_changes {
                status.has_staged_changes = true;
            }

            if !file.working_state.is_empty() && !status.has_local_changes {
                status.has_local_changes = true;
            }

            if file.index_state == "?" && file.working_state == "?" {
                status.untracked_files.0.push(file);
            } else {
                status.modified_files.0.push(file);
            }
        }

        if status.detached_head {
            warn!(
                "Detached head state detected for {:?}",
                self.git_client.repo_path
            );
        }

        // check modified files in local commits
        let mut modified_committed: Vec<String> = vec![];
        if status.commits_ahead > 0 {
            let range = format!("HEAD~{}...HEAD", status.commits_ahead);

            let output = self.git_client.diff_filenames(&range).await?;
            for line in output.lines() {
                if !line.is_empty() {
                    modified_committed.push(line.to_owned());
                }
            }
        }

        // get display names if available for OFPA
        {
            info!("StatusOp: fetching OFPA filenames...");

            let mut combined_files = status.modified_files.0.clone();
            combined_files.append(&mut status.untracked_files.0.clone());

            let communication = if self.allow_offline_communication {
                engine::CommunicationType::OfflineFallback
            } else {
                engine::CommunicationType::IpcOnly
            };
            self.update_filelist_display_names(communication, &mut combined_files)
                .await;

            let num_modified = status.modified_files.0.len();

            status.modified_files.0 = combined_files[0..num_modified].to_vec();
            status.untracked_files.0 = combined_files[num_modified..].to_vec();
        }

        info!("StatusOp: checking HEAD SHA and remote URL info...");

        status.commit_head_origin = self
            .git_client
            .head_commit(git::CommitFormat::Long, git::CommitHead::Remote)
            .await?;

        let remote_url = match self
            .git_client
            .run_and_collect_output(
                &["config", "--get", "remote.origin.url"],
                git::Opts::default(),
            )
            .await
        {
            Ok(url) => url,
            Err(_) => {
                return Err(anyhow!("Failed to get remote URL for repo"));
            }
        };

        // https://github.com/owner/repository.git
        let parts = remote_url.split('/');
        if parts.count() < 4 {
            status.repo_owner = "".to_string();
            status.repo_name = "".to_string();
        } else {
            status.repo_owner = remote_url.split('/').nth(3).unwrap().to_string();
            status.repo_name = remote_url
                .split('/')
                .nth(4)
                .unwrap()
                .trim_end()
                .trim_end_matches(".git")
                .to_string();
        }

        // Since we aren't likely to have much contention on this lock, it's likely
        // cheaper to write than to read and then sometimes write.
        let new_selected_artifact_project = format!(
            "{}-{}",
            status.repo_owner.to_lowercase(),
            status.repo_name.to_lowercase()
        );
        {
            let mut app_config = self.app_config.write();
            app_config.selected_artifact_project = Some(new_selected_artifact_project);
        }

        if !self.skip_dll_check {
            info!("StatusOp: searching for remote DLL archives...");

            self.find_dll_archive_url_info(&mut status).await?;
            status.pull_dlls = pull_dlls;
        }

        info!("StatusOp: finding upstream modified files...");
        status.modified_upstream = self.get_modified_upstream(&status).await?;

        status.conflicts = self.get_upstream_conflicts(&modified_committed, &status);
        if !status.conflicts.is_empty() {
            status.conflict_upstream = true;
        }

        let mut repo_status = self.repo_status.write();
        *repo_status = status.clone();

        info!("StatusOp: done.");

        Ok(status)
    }

    async fn get_modified_upstream(
        &self,
        status: &RepoStatus,
    ) -> Result<Vec<String>, anyhow::Error> {
        // no commits upstream, no changes
        if status.commits_behind == 0 {
            return Ok(vec![]);
        }

        // check files modified upstream
        let mut modified_upstream: Vec<String> = vec![];
        let range = format!("HEAD...{}", status.remote_branch);

        let output = self.git_client.diff_filenames(&range).await?;

        for line in output.lines() {
            if !line.is_empty() {
                modified_upstream.push(line.to_owned());
            }
        }

        let filtered = modified_upstream
            .iter()
            .filter(|file| {
                file.ends_with(".uasset") || file.ends_with(".umap") || file.ends_with(".dll")
            })
            .cloned()
            .collect::<Vec<_>>();

        Ok(filtered)
    }

    fn get_upstream_conflicts(
        &self,
        modified_committed: &[String],
        repo_status: &RepoStatus,
    ) -> Vec<String> {
        repo_status
            .modified_upstream
            .iter()
            .filter(|file| {
                modified_committed.contains(file)
                    || repo_status.modified_files.contains(file)
                    || repo_status.untracked_files.contains(file)
            })
            .cloned()
            .collect::<Vec<_>>()
    }

    #[instrument(skip_all)]
    async fn find_dll_archive_url_info(&self, status: &mut RepoStatus) -> anyhow::Result<()> {
        debug!("parsing remote URL for repo id");

        let repo_id = if status.repo_owner.is_empty() || status.repo_name.is_empty() {
            warn!("No URL configured for this repo - this is likely a test. Using default url id 'friendshipper'");
            "friendshipper".to_string()
        } else {
            let repo_id = format!("{}-{}", status.repo_owner, status.repo_name);
            let repo_id = repo_id.replace('/', "-");
            repo_id.to_lowercase()
        };
        debug!("Parsed the repo id as {:?}", repo_id);

        debug!("fetching s3 editor entries list");

        self.aws_client.check_config().await?;

        let storage = &self.storage;

        let project = if status.repo_owner.is_empty() || status.repo_name.is_empty() {
            let app_config = self.app_config.read();
            let (owner, repo) = match app_config.selected_artifact_project {
                Some(ref project) => {
                    let (owner, repo) =
                        project.split_once('-').ok_or(anyhow!("Invalid project"))?;
                    (owner, repo)
                }
                None => return Err(anyhow!("No project selected")),
            };

            Project::new(owner, repo)
        } else {
            Project::new(&status.repo_owner, &status.repo_name)
        };

        let artifact_config = ArtifactConfig::new(
            project,
            ArtifactKind::Editor,
            ArtifactBuildConfig::Development,
            Platform::Win64,
        );

        let mut list = storage.artifact_list(artifact_config).await;

        let builds = list.sort_by_last_modified();

        debug!("s3 entries list: {:?}", builds);

        let git_opts = git::Opts::new_without_logs();
        let local_commit_shas: String = self
            .git_client
            .run_and_collect_output(&["log", "--format=\"%H\"", "-1000"], git_opts)
            .await
            .unwrap_or(String::new());
        let remote_commit_shas: String = self
            .git_client
            .run_and_collect_output(&["log", "--format=\"%H\"", "-1000", "FETCH_HEAD"], git_opts)
            .await
            .unwrap_or(String::new());

        let dll_commit_local = find_dll_commit(builds, &local_commit_shas, "local");
        let dll_commit_remote = find_dll_commit(builds, &remote_commit_shas, "remote");

        status.origin_has_new_dlls = dll_commit_local != dll_commit_remote;
        status.dll_commit_local = dll_commit_local;
        status.dll_commit_remote = dll_commit_remote;

        Ok(())
    }

    #[instrument]
    pub async fn update_filelist_display_names(
        &self,
        communication: engine::CommunicationType,
        files: &mut [File],
    ) {
        let filenames: Vec<String> = files.iter().map(|v| v.path.clone()).collect();

        let engine_path = self
            .app_config
            .read()
            .load_engine_path_from_repo(&self.repo_config.read())
            .unwrap_or_default();

        let asset_names: Vec<String> = self
            .engine
            .get_asset_display_names(communication, &engine_path, &filenames)
            .await;

        // OFPANameCache::get_names(
        //     self.ofpa_cache.clone(),
        //     &PathBuf::from(repo_path),
        //     &uproject_path,
        //     &engine_path,
        //     &filenames,
        //     CanUseCommandlet::FallbackOnly,
        // )
        // .await

        assert_eq!(files.len(), asset_names.len());

        // the suggested replacement to use an enumerator with an index and value is *more* complicated than
        // this simple parallel array code...
        #[allow(clippy::needless_range_loop)]
        for i in 0..files.len() {
            files[i].display_name.clone_from(&asset_names[i]);

            debug!(
                "updating file {} to have display name '{}'",
                files[i].path, asset_names[i]
            );
        }
    }
}

#[instrument]
fn find_dll_commit(files: &ArtifactList, long_shas: &str, context: &str) -> String {
    for sha in long_shas.lines() {
        let sha = sha.replace('"', "");
        debug!("checking sha {} against s3 entries...", sha);
        if files.iter().any(|entry| entry.key.0.contains(&sha)) {
            return sha.to_string();
        }
    }

    warn!(
        "Failed to find editor binaries matching any commits for context {}",
        context
    );
    String::new()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusParams {
    #[serde(default)]
    pub skip_fetch: bool,
    #[serde(default)]
    pub skip_dll_check: bool,
    #[serde(default)]
    pub allow_offline_communication: bool,
}

pub async fn status_handler<T>(
    State(state): State<AppState<T>>,
    params: Query<StatusParams>,
) -> Result<Json<RepoStatus>, CoreError>
where
    T: EngineProvider,
{
    let aws_client = ensure_aws_client(state.aws_client.read().await.clone())?;

    let storage = match state.storage.read().clone() {
        Some(storage) => storage,
        None => {
            return Err(CoreError(anyhow!(
                "No storage configured for this app. AWS may still be initializing."
            )))
        }
    };

    let status_op = {
        StatusOp {
            repo_status: state.repo_status.clone(),
            app_config: state.app_config.clone(),
            repo_config: state.repo_config.clone(),
            engine: state.engine.clone(),
            git_client: state.git(),
            aws_client: aws_client.clone(),
            storage,
            skip_fetch: params.skip_fetch,
            skip_dll_check: params.skip_dll_check,
            allow_offline_communication: params.allow_offline_communication,
        }
    };

    // Make sure AWS credentials still valid
    aws_client.check_config().await?;

    // make sure this status operation is executed behind any queued operations
    let (tx, rx) = tokio::sync::oneshot::channel::<Option<anyhow::Error>>();

    let mut sequence = TaskSequence::new().with_completion_tx(tx);
    sequence.push(Box::new(status_op));

    state.operation_tx.send(sequence).await?;

    match rx.await {
        Ok(e) => {
            if let Some(e) = e {
                error!("Status operation failed: {}", e);
                return Err(CoreError(e));
            }

            let status = state.repo_status.read();

            Ok(Json(status.clone()))
        }
        Err(_) => Err(CoreError(anyhow!("Error executing status operation"))),
    }
}

#[cfg(test)]
mod tests {
    use ethos_core::storage::ArtifactEntry;

    use super::*;

    #[test]
    fn test_find_dll_commit() {
        let base = std::time::SystemTime::now();
        let ac = ArtifactConfig::new(
            "fake-project".into(),
            ArtifactKind::Editor,
            ArtifactBuildConfig::Development,
            Platform::Win64,
        );
        let mut list = ArtifactList::new(ac, "s3://fakebucket/".into());
        let mut entry = ArtifactEntry::new("v1/believerco-gameprototypemp/editor/win64/development/0266eafeecd51b155d3621469ac689bcd485020d.json".to_string());
        entry.last_modified = base - std::time::Duration::from_secs(5);
        list.entries.push(entry);
        let mut entry = ArtifactEntry::new("v1/believerco-gameprototypemp/editor/win64/development/9c351d7dacd6c412f55a825d77727761d9c1268b.json".to_string());
        entry.last_modified = base - std::time::Duration::from_secs(10);
        list.entries.push(entry);
        let mut entry = ArtifactEntry::new("v1/believerco-gameprototypemp/editor/win64/development/de0bb3ad8454d29083665ebb3db0dd0c29a2d1d0.json".to_string());
        entry.last_modified = base - std::time::Duration::from_secs(20);
        list.entries.push(entry);
        list.sort_by_last_modified();

        let long_shas = [
            "0123456789abcde0123456789abcde0123456789",
            "9c351d7dacd6c412f55a825d77727761d9c1268b",
            "1123456789abcde0123456789abcde0123456789",
            "2123456789abcde0123456789abcde0123456789",
            "3123456789abcde0123456789abcde0123456789",
        ]
            .join("\n");

        println!("{:?}", long_shas);
        let sha = find_dll_commit(&list, &long_shas, "test");
        assert_eq!(sha, "9c351d7dacd6c412f55a825d77727761d9c1268b");
    }
}
