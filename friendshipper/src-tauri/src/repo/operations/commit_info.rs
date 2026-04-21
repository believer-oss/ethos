use axum::extract::{Query, State};
use axum::Json;
use ethos_core::clients::git::Opts;
use ethos_core::types::errors::CoreError;
use ethos_core::types::repo::CommitInfo;
use serde::Deserialize;
use tracing::instrument;

use crate::engine::EngineProvider;
use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct CommitInfoParams {
    pub sha: String,
}

fn is_valid_sha(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.chars().all(|c| c.is_ascii_hexdigit())
}

#[instrument(skip(state))]
pub async fn commit_info_handler<T>(
    State(state): State<AppState<T>>,
    Query(params): Query<CommitInfoParams>,
) -> Result<Json<CommitInfo>, CoreError>
where
    T: EngineProvider,
{
    if !is_valid_sha(&params.sha) {
        return Err(CoreError::Input(anyhow::anyhow!(
            "Invalid commit SHA: {}",
            params.sha
        )));
    }

    // Format: pipe-delimited metadata on line 1, raw body follows.
    // %H=full, %h=short, %an/%ae/%aI=author, %cn/%ce/%cI=committer, %P=parents, %B=body
    let format_spec = "--pretty=format:%H|%h|%an|%ae|%aI|%cn|%ce|%cI|%P%n%B";

    let output = state
        .git()
        .run_and_collect_output(
            &["show", "--no-patch", format_spec, &params.sha],
            Opts {
                skip_notify_frontend: true,
                should_log_stdout: false,
                ..Default::default()
            },
        )
        .await
        .map_err(|e| CoreError::Internal(anyhow::anyhow!("git show failed: {}", e)))?;

    let (first_line, message) = output.split_once('\n').unwrap_or((output.as_str(), ""));
    let parts: Vec<&str> = first_line.splitn(9, '|').collect();
    if parts.len() < 9 {
        return Err(CoreError::Internal(anyhow::anyhow!(
            "unexpected git show output: {}",
            first_line
        )));
    }

    let author_date = chrono::DateTime::parse_from_rfc3339(parts[4])
        .map_err(|e| CoreError::Internal(anyhow::anyhow!("bad author date: {}", e)))?
        .with_timezone(&chrono::Utc);
    let committer_date = chrono::DateTime::parse_from_rfc3339(parts[7])
        .map_err(|e| CoreError::Internal(anyhow::anyhow!("bad committer date: {}", e)))?
        .with_timezone(&chrono::Utc);

    let parents: Vec<String> = parts[8].split_whitespace().map(|s| s.to_string()).collect();

    let message = message.trim_end().to_string();
    let subject = message.lines().next().unwrap_or("").to_string();

    Ok(Json(CommitInfo {
        sha: parts[0].to_string(),
        short_sha: parts[1].to_string(),
        author_name: parts[2].to_string(),
        author_email: parts[3].to_string(),
        author_date,
        committer_name: parts[5].to_string(),
        committer_email: parts[6].to_string(),
        committer_date,
        parents,
        subject,
        message,
    }))
}
