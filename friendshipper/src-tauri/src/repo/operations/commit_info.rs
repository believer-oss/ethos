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

    // NUL-delimited metadata fields, message last. NUL (%x00) is used instead of a printable
    // delimiter so author/committer names or emails that happen to contain characters like `|`
    // don't corrupt the parse. Git guarantees no field contains a NUL byte.
    //   %H=full, %h=short, %an/%ae/%aI=author, %cn/%ce/%cI=committer, %P=parents, %B=body
    let format_spec =
        "--pretty=format:%H%x00%h%x00%an%x00%ae%x00%aI%x00%cn%x00%ce%x00%cI%x00%P%x00%B";

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

    // First 9 NUL splits are the metadata fields; the 10th is the raw message body (which may
    // contain its own newlines — we preserve them).
    let parts: Vec<&str> = output.splitn(10, '\0').collect();
    if parts.len() < 10 {
        return Err(CoreError::Internal(anyhow::anyhow!(
            "unexpected git show output (got {} fields)",
            parts.len()
        )));
    }

    let author_date = chrono::DateTime::parse_from_rfc3339(parts[4])
        .map_err(|e| CoreError::Internal(anyhow::anyhow!("bad author date: {}", e)))?
        .with_timezone(&chrono::Utc);
    let committer_date = chrono::DateTime::parse_from_rfc3339(parts[7])
        .map_err(|e| CoreError::Internal(anyhow::anyhow!("bad committer date: {}", e)))?
        .with_timezone(&chrono::Utc);

    let parents: Vec<String> = parts[8].split_whitespace().map(|s| s.to_string()).collect();

    let message = parts[9].trim_end().to_string();
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
