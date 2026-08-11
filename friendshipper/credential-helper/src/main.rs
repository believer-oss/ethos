//! Git credential helper backed by Friendshipper's keyring entries.
//!
//! Friendshipper configures this binary as the repo-local credential.helper,
//! replacing Git Credential Manager for repos it manages. Both plain git and
//! git-lfs invoke it, so one token covers fetch/push, LFS transfers, and the
//! lfs-rs lock server.
//!
//! Keyring entry names are a contract with friendshipper/src-tauri (see
//! auth/github.rs and lib.rs). The GitHub App access token is preferred; the
//! legacy PAT entry is the fallback for users who haven't migrated.

use std::io::Read;

use chrono::{DateTime, Utc};

const APP_NAME: &str = "Friendshipper";
const OAUTH_TOKENS_ENTRY: &str = "github_oauth_tokens";
const LEGACY_PAT_ENTRY: &str = "github_pat";

fn oauth_access_token() -> Option<String> {
    let blob = keyring::Entry::new(APP_NAME, OAUTH_TOKENS_ENTRY)
        .ok()?
        .get_password()
        .ok()?;
    let tokens: serde_json::Value = serde_json::from_str(&blob).ok()?;

    let expires_at = tokens.get("expiresAt")?.as_str()?;
    let expires_at = DateTime::parse_from_rfc3339(expires_at).ok()?;
    if expires_at < Utc::now() {
        return None;
    }

    Some(tokens.get("accessToken")?.as_str()?.to_string())
}

fn legacy_pat() -> Option<String> {
    keyring::Entry::new(APP_NAME, LEGACY_PAT_ENTRY)
        .ok()?
        .get_password()
        .ok()
        .filter(|pat| !pat.is_empty())
}

fn main() {
    let operation = std::env::args().nth(1).unwrap_or_default();

    // The protocol delivers key=value lines on stdin; we match on any host the
    // repo points us at (github.com and the LFS server), so the input only
    // needs to be drained.
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);

    // store/erase are lifecycle hints from git; token lifecycle belongs to
    // Friendshipper, so both are no-ops.
    if operation != "get" {
        return;
    }

    // Print nothing when no credential is available: git treats that as this
    // helper passing, and moves on to the next configured helper (if any).
    if let Some(token) = oauth_access_token().or_else(legacy_pat) {
        print!("username=x-access-token\npassword={token}\n");
    }
}
