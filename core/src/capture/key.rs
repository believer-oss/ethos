//! S3 key construction for uploaded capture files.

use std::path::Path;
use std::time::SystemTime;

use chrono::{DateTime, NaiveDate, Utc};

const SLUG_MAX: usize = 32;
const FILE_MAX: usize = 64;
const EXT_MAX: usize = 8;
const SHA_LEN: usize = 12;
const METADATA_USER_MAX_SCALARS: usize = 128;

/// Builds `<keyPrefix>/<YYYY-MM-DD>/<slug>/<sha12>_<slug>_<mtimeUTC>_<file>.<ext>`.
///
/// `upload_date_utc` is fixed by the caller when the upload first starts and is never recomputed.
pub fn object_key(
    key_prefix: &str,
    upload_date_utc: NaiveDate,
    user_raw: &str,
    sha: Option<&str>,
    mtime: SystemTime,
    path: &Path,
) -> String {
    let user = slug(user_raw);
    let sha12 = sha12(sha);
    let mtime_utc = DateTime::<Utc>::from(mtime).format("%Y%m%dT%H%M%SZ");
    let file = file_part(path);
    let ext = ext_part(path);
    let dot = if ext.is_empty() { "" } else { "." };
    format!(
        "{}/{}/{user}/{sha12}_{user}_{mtime_utc}_{file}{dot}{ext}",
        key_prefix.trim_matches('/'),
        upload_date_utc.format("%Y-%m-%d"),
    )
}

/// Deterministic, Unicode-safe user slug: ASCII `A-Za-z0-9_-` kept, every other run becomes `-`.
pub fn slug(raw: &str) -> String {
    let mut collapsed = String::new();
    for c in raw.chars() {
        let c = if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
            c
        } else {
            '-'
        };
        if c == '-' && collapsed.ends_with('-') {
            continue;
        }
        collapsed.push(c);
    }

    let trim = |s: &str| s.trim_matches(|c| c == '-' || c == '_').to_string();
    let mut trimmed = trim(&collapsed);
    if trimmed.len() > SLUG_MAX {
        trimmed.truncate(SLUG_MAX);
        trimmed = trim(&trimmed);
    }
    if trimmed.is_empty() {
        "unknown-user".to_string()
    } else {
        trimmed
    }
}

fn sha12(sha: Option<&str>) -> String {
    let lowered: String = sha
        .unwrap_or_default()
        .chars()
        .take(SHA_LEN)
        .collect::<String>()
        .to_ascii_lowercase();
    if lowered.is_empty() {
        "unknown".to_string()
    } else {
        lowered
    }
}

fn file_part(path: &Path) -> String {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut out = String::new();
    let mut in_other_run = false;
    for c in stem.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
            out.push(c);
            in_other_run = false;
        } else if !in_other_run {
            out.push('-');
            in_other_run = true;
        }
    }
    out.truncate(FILE_MAX);
    if out.is_empty() {
        "file".to_string()
    } else {
        out
    }
}

fn ext_part(path: &Path) -> String {
    path.extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
        .chars()
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        .take(EXT_MAX)
        .collect()
}

/// Percent-encodes UTF-8 bytes, leaving only `A-Za-z0-9-._~`. ASCII is always encoded too.
pub fn metadata_encode(value: &str) -> String {
    urlencoding::encode(value).into_owned()
}

/// Raw display name for the `user` metadata header: 128 scalars, percent-encoded; `None` if empty.
pub fn metadata_user(raw: &str) -> Option<String> {
    let truncated: String = raw.chars().take(METADATA_USER_MAX_SCALARS).collect();
    if truncated.is_empty() {
        None
    } else {
        Some(metadata_encode(&truncated))
    }
}

/// Full session sha for the `sha` metadata header, or `unknown`.
pub fn metadata_sha(sha: Option<&str>) -> String {
    match sha {
        Some(s) if !s.is_empty() => metadata_encode(s),
        _ => "unknown".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use chrono::{FixedOffset, TimeZone};

    use super::*;

    const SHA40: &str = "3f9a1c7d2e4b5a6978c0d1e2f3a4b5c6d7e8f901";

    fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> SystemTime {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, s).unwrap().into()
    }

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    fn key(user: &str, sha: Option<&str>, mtime: SystemTime, file: &str) -> String {
        object_key(
            "p",
            date(2026, 10, 8),
            user,
            sha,
            mtime,
            &PathBuf::from(file),
        )
    }

    #[test]
    fn c_example_trace() {
        let k = object_key(
            "friendshipper/client-utrace",
            date(2026, 10, 8),
            "Jane Doe",
            Some(SHA40),
            utc(2026, 10, 8, 14, 30, 5),
            Path::new("Saved/Profiling/FellowshipTrace_20261008_103001.utrace"),
        );
        assert_eq!(
            k,
            "friendshipper/client-utrace/2026-10-08/Jane-Doe/3f9a1c7d2e4b_Jane-Doe_20261008T143005Z_FellowshipTrace_20261008_103001.utrace"
        );
    }

    #[test]
    fn c_example_log() {
        let k = object_key(
            "friendshipper/client-logs",
            date(2026, 10, 8),
            "Jane Doe",
            Some(SHA40),
            utc(2026, 10, 8, 14, 35, 12),
            Path::new("Saved/Logs/Fellowship.log"),
        );
        assert_eq!(
            k,
            "friendshipper/client-logs/2026-10-08/Jane-Doe/3f9a1c7d2e4b_Jane-Doe_20261008T143512Z_Fellowship.log"
        );
    }

    #[test]
    fn slug_basic_and_fallbacks() {
        assert_eq!(slug("Jane Doe"), "Jane-Doe");
        assert_eq!(slug("  "), "unknown-user");
        assert_eq!(slug("\u{c0ac}\u{c6a9}\u{c790}"), "unknown-user");
        assert_eq!(slug(""), "unknown-user");
        assert_eq!(slug("---___"), "unknown-user");
    }

    #[test]
    fn slug_walks_scalars_not_bytes() {
        assert_eq!(slug("a\u{c0ac}\u{c6a9}\u{c790}b"), "a-b");
        assert_eq!(slug("Jos\u{e9}"), "Jos");
        assert_eq!(slug("a\u{0301}b"), "a-b");
    }

    #[test]
    fn slug_runs_and_trim() {
        assert_eq!(slug("a!!!@@@b"), "a-b");
        assert_eq!(slug("a - b"), "a-b");
        assert_eq!(slug("a..b//c"), "a-b-c");
        assert_eq!(slug("--_a_--"), "a");
        assert_eq!(slug("_ a b _"), "a-b");
        assert_eq!(slug("a__b"), "a__b");
    }

    #[test]
    fn slug_truncates_then_retrims() {
        let long = "a".repeat(40);
        assert_eq!(slug(&long), "a".repeat(32));
        let cut_at_dash = format!("{}-{}", "a".repeat(31), "b".repeat(10));
        assert_eq!(slug(&cut_at_dash), "a".repeat(31));
        let cut_at_underscore = format!("{}_{}", "a".repeat(31), "b".repeat(10));
        assert_eq!(slug(&cut_at_underscore), "a".repeat(31));
        let exact = "a".repeat(32);
        assert_eq!(slug(&exact), exact);
    }

    #[test]
    fn sha_rules() {
        let t = utc(2026, 10, 8, 0, 0, 0);
        assert!(key("u", Some("ABCDEF12"), t, "f.log").contains("/abcdef12_u_"));
        assert!(key("u", None, t, "f.log").contains("/unknown_u_"));
        assert!(key("u", Some(""), t, "f.log").contains("/unknown_u_"));
        assert!(key("u", Some(&SHA40.to_uppercase()), t, "f.log").contains("/3f9a1c7d2e4b_u_"));
    }

    #[test]
    fn mtime_is_truncated_not_rounded() {
        let t: SystemTime = Utc
            .with_ymd_and_hms(2026, 10, 8, 14, 30, 5)
            .unwrap()
            .checked_add_signed(chrono::Duration::milliseconds(999))
            .unwrap()
            .into();
        assert!(key("u", None, t, "f.log").contains("_20261008T143005Z_"));
    }

    #[test]
    fn file_sanitising() {
        let t = utc(2026, 10, 8, 0, 0, 0);
        assert!(key("u", None, t, "my file (1).log").ends_with("_my-file-1-.log"));
        assert!(key("u", None, t, "a.b_c-d.log").ends_with("_a.b_c-d.log"));
        assert!(key("u", None, t, "\u{c0ac}\u{c6a9}\u{c790}.log").ends_with("_-.log"));
        let long = format!("{}.log", "x".repeat(100));
        let expected = format!("_{}.log", "x".repeat(64));
        assert!(key("u", None, t, &long).ends_with(&expected));
        assert_eq!(file_part(Path::new("")), "file");
        assert_eq!(file_part(Path::new("dir/archive.tar.gz")), "archive.tar");
    }

    #[test]
    fn ext_filtering() {
        let t = utc(2026, 10, 8, 0, 0, 0);
        assert!(key("u", None, t, "f.UTRACE").ends_with("_f.utrace"));
        assert!(key("u", None, t, "f.l-o_g").ends_with("_f.log"));
        assert!(key("u", None, t, "f.abcdefghijkl").ends_with("_f.abcdefgh"));
        assert!(key("u", None, t, "f").ends_with("_f"));
        assert!(key("u", None, t, "f.---").ends_with("_f"));
        assert!(key("u", None, t, "f.\u{c0ac}\u{c6a9}").ends_with("_f"));
    }

    #[test]
    fn upload_date_is_the_supplied_utc_date() {
        let local = FixedOffset::west_opt(5 * 3600)
            .unwrap()
            .with_ymd_and_hms(2026, 10, 8, 23, 30, 0)
            .unwrap();
        let utc_date = local.with_timezone(&Utc).date_naive();
        assert_eq!(utc_date, date(2026, 10, 9));
        let t = utc(2026, 10, 9, 4, 30, 0);
        let k = object_key("p", utc_date, "u", None, t, Path::new("f.log"));
        assert!(k.starts_with("p/2026-10-09/u/"));

        let a = object_key("p", date(2026, 10, 8), "u", None, t, Path::new("f.log"));
        let b = object_key("p", date(2026, 10, 8), "u", None, t, Path::new("f.log"));
        assert_eq!(a, b);
    }

    #[test]
    fn key_prefix_slashes_are_trimmed() {
        let t = utc(2026, 10, 8, 0, 0, 0);
        let k = object_key("/a/b/", date(2026, 10, 8), "u", None, t, Path::new("f.log"));
        assert!(k.starts_with("a/b/2026-10-08/u/"));
    }

    #[test]
    fn metadata_encoding() {
        assert_eq!(metadata_user("Jane Doe").as_deref(), Some("Jane%20Doe"));
        assert_eq!(metadata_user("a-b._~9").as_deref(), Some("a-b._~9"));
        assert_eq!(metadata_user("a/b").as_deref(), Some("a%2Fb"));
        assert_eq!(metadata_user("\u{c0ac}").as_deref(), Some("%EC%82%AC"));
        assert_eq!(metadata_user(""), None);
        let long = "\u{c0ac}".repeat(200);
        let encoded = metadata_user(&long).unwrap();
        assert_eq!(encoded.len(), 128 * 9);
    }

    #[test]
    fn metadata_sha_values() {
        assert_eq!(metadata_sha(Some("abc123")), "abc123");
        assert_eq!(metadata_sha(None), "unknown");
        assert_eq!(metadata_sha(Some("")), "unknown");
        assert_eq!(metadata_sha(Some("a b")), "a%20b");
    }
}
