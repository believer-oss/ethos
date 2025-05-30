use crate::types::commits::Commit;
use std::collections::HashMap;
use std::sync::Arc;

pub fn format_commit(commit: &String, commit_map: &Arc<HashMap<String, Commit>>) -> String {
    let mut displayed_name = commit.clone();

    if let Some(local_commit) = commit_map.get(commit) {
        if let Some(message) = &local_commit.message {
            let mut message = message.to_owned();

            if message.len() > 50 {
                message.truncate(47);
                message += "...";
            }

            displayed_name = format!("{displayed_name} - {message}");
        }

        if let Some(author) = &local_commit.author {
            displayed_name = format!("{displayed_name} ({author})");
        }
    }

    displayed_name
}
