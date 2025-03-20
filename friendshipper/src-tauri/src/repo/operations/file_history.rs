use axum::extract::{Query, State};
use axum::Json;
use ethos_core::types::repo::{FileHistoryResponse, FileHistoryRevision};
use serde::Deserialize;
use tracing::instrument;
use futures::future::{join_all};

use crate::engine::EngineProvider;
use ethos_core::types::errors::CoreError;

use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct FileHistoryParams {
    pub path: String,
}

pub async fn get_revision<T>(
    state: &AppState<T>,
    chunk: &[&str],
) -> Result<FileHistoryRevision, CoreError>
where
    T: EngineProvider,
{
    if chunk.len() != 2 {
        return Err(CoreError::Input(anyhow::anyhow!(
                            "chunk length is incorrect"
                        )));
    }

    let commit_line = chunk[0];
    let file_line = chunk[1];

    // Parse commit line parts
    let parts: Vec<&str> = commit_line.split('|').collect();
    if parts.len() < 4 {
        return Err(CoreError::Input(anyhow::anyhow!(
                            "parts length is incorrect"
                        )));
    }

    let commit_id = parts[0].to_string();
    let short_commit_id = commit_id[..8].to_string();
    let user_name = parts[1].to_string();

    // Parse timestamp
    let timestamp = parts[2]
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .parse::<i64>()
        .unwrap_or_default();
    let date = chrono::DateTime::from_timestamp(timestamp, 0)
        .unwrap_or_default()
        .with_timezone(&chrono::Utc);

    // Get description
    let description = parts[3].to_string();

    // Parse file line
    let file_parts: Vec<&str> = file_line.split_whitespace().collect();
    if file_parts.len() != 2 {
        return Err(CoreError::Input(anyhow::anyhow!(
                            "file parts length is incorrect"
                        )));
    }

    let action = translate_action(file_parts[0]);
    let filename = file_parts[1].to_string();

    // set commit_id_number to hex of short commit id
    let commit_id_number = u32::from_str_radix(&short_commit_id, 16).unwrap_or_default();

    let output = state
        .git()
        .run_and_collect_output(
            &["ls-tree", "--long", &commit_id, "--", &filename],
            Default::default(),
        )
        .await?;

    let mut file_hash = String::new();
    let mut file_size = 0;

    // Parse ls-tree output if we got any
    if !output.is_empty() {
        let parts: Vec<&str> = output.split_whitespace().collect();
        if parts.len() >= 4 {
            file_hash = parts[2].to_string();
            file_size = parts[3].parse::<u32>().unwrap_or_default();
        }
    }

    return Ok(FileHistoryRevision {
        filename,
        commit_id,
        short_commit_id,
        commit_id_number,
        revision_number: 0, // Not provided in git log output
        file_hash,
        description,
        user_name,
        action,
        date,
        file_size,
    });
}

#[instrument(skip(state))]
pub async fn file_history_handler<T>(
    State(state): State<AppState<T>>,
    Query(params): Query<FileHistoryParams>,
) -> Result<Json<FileHistoryResponse>, CoreError>
where
    T: EngineProvider,
{
    /*
    commit 98994d03db00ff6922d6a70442ba1ee6568fce48
    Author: Person Name <person@example.com>
    Date:   1728091003 -0700

    chore(foo): do thing

    A       Content/__ExternalActors__/Foo.uasset
     */
    // run git log --pretty=format:"%h %an %ad %s" --date=raw --name-status -- <path>
    let output = state
        .git()
        .run_and_collect_output(
            &[
                "log",
                "--pretty=format:%H|%an|%ad|%s",
                "--date=raw",
                "--name-status",
                "--",
                &params.path,
            ],
            Default::default(),
        )
        .await?;

    let lines: Vec<&str> = output.split('\n').collect();
    let chunks: Vec<&[&str]> = lines.chunks(2).collect();
    let mut revision_futures = Vec::new();
    let mut revisions = Vec::new();

    for chunk in chunks {
        revision_futures.push(get_revision(&state, chunk));
    }

    let results = join_all(revision_futures)
        .await;

    for result in results {
        match result {
            Ok(revision) => {
                revisions.push(revision);
            },
            // Show only the revisions that succeeded and ignore any that failed
            Err(_) => {}
        }
    }
    // for each, set revision number to length - index
    let len = revisions.len();
    for (index, revision) in revisions.iter_mut().enumerate() {
        revision.revision_number = (len - index) as u32;
    }

    Ok(Json(FileHistoryResponse { revisions }))
}

// We do have separate code for this conversion in the core library, but this is meant to
// be a nicer string for editor UI.
fn translate_action(action: &str) -> String {
    match action {
        " " => "unmodified".to_string(),
        "M" => "modified".to_string(),
        "A" => "add".to_string(),
        "D" => "delete".to_string(),
        "R" => "branch".to_string(),
        "C" => "branch".to_string(),
        "T" => "type changed".to_string(),
        "U" => "unmerged".to_string(),
        "X" => "unknown".to_string(),
        "B" => "broken pairing".to_string(),
        _ => "unknown".to_string(),
    }
}
