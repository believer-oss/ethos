use anyhow::bail;
use axum::extract::State;
use axum::{async_trait, Json};
use std::fs;
use std::io::Write;
use tokio::sync::oneshot::error::RecvError;
use tracing::info;
use tracing::{error, instrument};

use crate::engine::EngineProvider;
use crate::state::AppState;
use ethos_core::clients::git;
use ethos_core::operations::LockOp;
use ethos_core::types::errors::CoreError;
use ethos_core::types::locks::ForceUnlock;
use ethos_core::types::repo::RevertFilesRequest;
use ethos_core::worker::{Task, TaskSequence};

use super::{File, RepoStatusRef};

#[derive(Clone)]
pub struct RevertFilesOp<T> {
    pub files: Vec<String>,
    pub git_client: git::Git,
    pub repo_status: RepoStatusRef,
    pub engine: T,
}

// Note: This is not "git revert", it's "git checkout -- <files>"
#[async_trait]
impl<T> Task for RevertFilesOp<T>
where
    T: EngineProvider,
{
    #[instrument(skip(self), name = "RevertOp::execute", fields(files = ?self.files))]
    async fn execute(&self) -> anyhow::Result<()> {
        self.engine.check_ready_to_sync_repo().await?;

        if self.files.is_empty() {
            bail!("no files provided");
        }

        let mut num_chars = 0;
        for f in self.files.iter() {
            num_chars += f.len();
        }

        let branch = self.repo_status.read().branch.clone();

        // windows command line length limit is 8191, so if we're close to that, checking using a file instead
        if num_chars > 8000 {
            match self.revert_with_listfile(&branch).await {
                Ok(_) => return Ok(()),
                Err(e) => {
                    error!(
                        "Revert files with listfile failed, falling back to chunked batches: {}",
                        e
                    )
                }
            }
        }

        for chunk in self.files.chunks(50) {
            let mut args: Vec<&str> = vec!["checkout", &branch, "--"];
            for file in chunk {
                args.push(file);
            }
            self.git_client.run(&args, git::Opts::default()).await?;
        }

        Ok(())
    }

    fn get_name(&self) -> String {
        "RevertFilesOp".to_string()
    }
}

impl<T> RevertFilesOp<T>
where
    T: EngineProvider,
{
    async fn revert_with_listfile(&self, branch: &str) -> anyhow::Result<()> {
        let mut listfile_path = std::env::temp_dir();
        listfile_path.push("Friendshipper");

        if !listfile_path.exists() {
            if let Err(e) = fs::create_dir(&listfile_path) {
                bail!(
                    "Failed to create directory for storing temp file: {:?}. Reason: {}",
                    listfile_path,
                    e
                );
            }
        }

        listfile_path.push("revert_files.txt");

        match fs::File::create(&listfile_path) {
            Err(e) => {
                bail!(
                    "Failed to create listfile '{:?}' for RevertFilesOp: {}",
                    listfile_path,
                    e
                );
            }
            Ok(file) => {
                let mut writer = std::io::BufWriter::new(file);
                for path in &self.files {
                    if let Err(e) = writeln!(writer, "{}", &path) {
                        bail!(
                            "Failed to write '{}' to file {:?}: {}",
                            path,
                            listfile_path,
                            e
                        );
                    }
                }
                match writer.flush() {
                    Ok(_) => {}
                    Err(e) => {
                        bail!(
                            "Failed to write listfile '{:?}' for RevertFilesOp: {}",
                            listfile_path,
                            e
                        );
                    }
                }
            }
        }

        let pathspec_arg = format!("--pathspec-from-file={}", listfile_path.to_string_lossy());
        let args: Vec<&str> = vec!["checkout", &branch, &pathspec_arg];

        self.git_client.run(&args, git::Opts::default()).await?;

        Ok(())
    }
}

#[instrument(skip(state))]
pub async fn revert_files_handler<T>(
    State(state): State<AppState<T>>,
    Json(request): Json<RevertFilesRequest>,
) -> Result<Json<String>, CoreError>
where
    T: EngineProvider,
{
    let (tx, rx) = tokio::sync::oneshot::channel::<Option<anyhow::Error>>();
    let mut sequence = TaskSequence::new().with_completion_tx(tx);

    let repo_path = state.app_config.read().repo_path.clone();

    let modified_files = {
        let repo_status = state.repo_status.read();
        repo_status.modified_files.clone()
    };

    let untracked_files = {
        let repo_status = state.repo_status.read();
        repo_status.untracked_files.clone()
    };

    let modified: Vec<&File> = request
        .files
        .clone()
        .iter()
        .filter_map(|f| {
            if modified_files.contains(f) {
                modified_files.get(f)
            } else {
                None
            }
        })
        .collect();

    let added: Vec<&File> = request
        .files
        .clone()
        .iter()
        .filter_map(|f| {
            if untracked_files.contains(f) {
                untracked_files.get(f)
            } else {
                None
            }
        })
        .collect();

    info!("Added: {:?}, Modified: {:?}", added, modified);

    if !added.is_empty() {
        for file in &added {
            let path = repo_path.clone() + "/" + &file.path;
            fs::remove_file(&path)?;
        }
    }

    if !modified.is_empty() {
        let op = RevertFilesOp {
            git_client: state.git(),
            repo_status: state.repo_status.clone(),
            files: modified.iter().map(|f| f.path.clone()).collect(),
            engine: state.engine.clone(),
        };

        sequence.push(Box::new(op));
    }

    // unlock reverted files
    if !request.files.is_empty() {
        let lock_paths = request.files.to_vec();
        let github_pat = state.app_config.read().ensure_github_pat()?;

        let op = LockOp::unlock(state.git(), lock_paths, github_pat, ForceUnlock::False);
        sequence.push(Box::new(op));
    }

    let _ = state.operation_tx.send(sequence).await;

    let res: Result<Option<anyhow::Error>, RecvError> = rx.await;
    if let Ok(Some(e)) = res {
        return Err(CoreError(e));
    }

    Ok(Json(String::from("OK")))
}
