use axum::extract::{Query, State};
use axum::Json;
use ethos_core::clients::git::Opts;
use ethos_core::types::errors::CoreError;
use ethos_core::types::repo::CommitInfo;
use serde::Deserialize;
use tracing::instrument;

use super::is_valid_sha;
use crate::engine::EngineProvider;
use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct CommitInfoParams {
    pub sha: String,
}

// NUL-delimited metadata fields, message last. NUL (%x00) is used instead of a printable
// delimiter so author/committer names or emails that happen to contain characters like `|`
// don't corrupt the parse. Git guarantees no field contains a NUL byte.
//   %H=full, %h=short, %an/%ae/%aI=author, %cn/%ce/%cI=committer, %P=parents, %B=body
const GIT_SHOW_FORMAT: &str =
    "--pretty=format:%H%x00%h%x00%an%x00%ae%x00%aI%x00%cn%x00%ce%x00%cI%x00%P%x00%B";

/// Parses the NUL-delimited output of `git show --no-patch` with `GIT_SHOW_FORMAT` into a
/// `CommitInfo`. Extracted into its own function so it can be unit-tested without running git.
pub(crate) fn parse_git_show_output(output: &str) -> Result<CommitInfo, CoreError> {
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

    Ok(CommitInfo {
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
    })
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

    let output = state
        .git()
        .run_and_collect_output(
            &["show", "--no-patch", GIT_SHOW_FORMAT, &params.sha],
            Opts {
                skip_notify_frontend: true,
                should_log_stdout: false,
                ..Default::default()
            },
        )
        .await
        .map_err(|e| CoreError::Internal(anyhow::anyhow!("git show failed: {}", e)))?;

    Ok(Json(parse_git_show_output(&output)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_output(parts: &[&str]) -> String {
        parts.join("\0")
    }

    #[test]
    fn parses_full_commit() {
        let out = make_output(&[
            "a1b2c3d4e5f6789012345678901234567890abcd",
            "a1b2c3d",
            "Jane Doe",
            "jane@example.com",
            "2024-03-05T10:30:00+00:00",
            "John Committer",
            "john@example.com",
            "2024-03-05T11:00:00+00:00",
            "1111111 2222222",
            "Subject line\n\nBody paragraph with details.\n",
        ]);

        let info = parse_git_show_output(&out).expect("should parse");

        assert_eq!(info.sha, "a1b2c3d4e5f6789012345678901234567890abcd");
        assert_eq!(info.short_sha, "a1b2c3d");
        assert_eq!(info.author_name, "Jane Doe");
        assert_eq!(info.author_email, "jane@example.com");
        assert_eq!(info.committer_name, "John Committer");
        assert_eq!(info.committer_email, "john@example.com");
        assert_eq!(info.parents, vec!["1111111", "2222222"]);
        assert_eq!(info.subject, "Subject line");
        assert_eq!(info.message, "Subject line\n\nBody paragraph with details.");
    }

    #[test]
    fn preserves_delimiter_chars_inside_fields() {
        // Names/emails with `|` used to be a risk with printable delimiters; NUL-delimited
        // parsing must leave them intact.
        let out = make_output(&[
            "deadbeef",
            "dead",
            "A|B Name",
            "weird|addr@example.com",
            "2024-03-05T10:30:00+00:00",
            "A|B Name",
            "weird|addr@example.com",
            "2024-03-05T10:30:00+00:00",
            "",
            "Message | with pipes",
        ]);

        let info = parse_git_show_output(&out).expect("should parse");
        assert_eq!(info.author_name, "A|B Name");
        assert_eq!(info.author_email, "weird|addr@example.com");
        assert_eq!(info.parents, Vec::<String>::new());
        assert_eq!(info.subject, "Message | with pipes");
    }

    #[test]
    fn preserves_newlines_inside_message_body() {
        let out = make_output(&[
            "abc",
            "abc",
            "Dev",
            "dev@example.com",
            "2024-03-05T10:30:00+00:00",
            "Dev",
            "dev@example.com",
            "2024-03-05T10:30:00+00:00",
            "",
            "Title\n\nFirst paragraph.\nSecond line of first paragraph.\n\nSecond paragraph.",
        ]);

        let info = parse_git_show_output(&out).expect("should parse");
        assert_eq!(info.subject, "Title");
        assert!(info.message.contains("Second line of first paragraph."));
        assert!(info.message.contains("Second paragraph."));
    }

    #[test]
    fn rejects_truncated_output() {
        // Fewer than 10 NUL-separated fields — likely indicates git emitted something
        // unexpected, e.g. format string not applied, or a pre-format error.
        let out = make_output(&["a", "b", "c"]);
        let err = parse_git_show_output(&out).unwrap_err();
        assert!(err.to_string().contains("unexpected git show output"));
    }

    #[test]
    fn rejects_bad_author_date() {
        let out = make_output(&[
            "abc",
            "abc",
            "Dev",
            "dev@example.com",
            "not-a-date",
            "Dev",
            "dev@example.com",
            "2024-03-05T10:30:00+00:00",
            "",
            "Subject",
        ]);
        let err = parse_git_show_output(&out).unwrap_err();
        assert!(err.to_string().contains("bad author date"));
    }
}
