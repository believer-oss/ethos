//! Watch directory resolution, session windows and deny rules.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf, Prefix};
use std::time::SystemTime;

use chrono::{DateTime, Utc};
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use tracing::{debug, warn};

use super::config::{CaptureWatchEntry, ClientCaptureUploadConfig};
use super::ledger::mtime_ms;
use super::types::{CaptureLedger, CaptureSession, FileKind, SessionState};

/// Files that must never be uploaded, whatever the watch config says.
const DENIED_FILE_NAMES: &[&str] = &["discord_token.txt"];
const MAX_FUTURE_MTIME_MS: i64 = 24 * 60 * 60 * 1000;
const PROTECTED_DIR: &str = "saved";

#[derive(Clone, Debug)]
pub struct ResolvedWatch {
    pub dir: PathBuf,
    pub globs: GlobSet,
    pub key_prefix: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub path: PathBuf,
    pub size: u64,
    pub mtime: SystemTime,
    pub mtime_ms: i64,
    pub kind: FileKind,
    pub key_prefix: String,
}

fn kind_for_extension(name: &str) -> Option<FileKind> {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".utrace") {
        Some(FileKind::Trace)
    } else if lower.ends_with(".log") {
        Some(FileKind::Log)
    } else {
        None
    }
}

fn is_denied(name: &str) -> bool {
    DENIED_FILE_NAMES
        .iter()
        .any(|denied| name.eq_ignore_ascii_case(denied))
}

/// Checks that need neither the install dir nor the environment.
fn entry_config_valid(entry: &CaptureWatchEntry) -> bool {
    if entry.dir.trim().is_empty()
        || entry.patterns.is_empty()
        || entry.key_prefix.trim_matches('/').is_empty()
    {
        return false;
    }
    if entry.dir.split(['/', '\\']).any(|part| part == "..") {
        return false;
    }
    entry
        .patterns
        .iter()
        .all(|p| kind_for_extension(p).is_some())
}

/// True when at least one watch entry passes the config-time checks.
pub fn usable_watch_entries(cfg: &ClientCaptureUploadConfig) -> bool {
    cfg.watch.iter().any(entry_config_valid)
}

fn expand_env(input: &str, env: &dyn Fn(&str) -> Option<String>) -> Option<String> {
    let mut out = String::new();
    let mut rest = input;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('%') {
            Some(end) if end > 0 => {
                let value = env(&after[..end]).filter(|v| !v.is_empty())?;
                out.push_str(&value);
                rest = &after[end + 1..];
            }
            _ => {
                out.push('%');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    Some(out)
}

/// Lowercased, lexically resolved path components used for ancestor comparisons.
fn comparison_components(path: &Path) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if out
                    .last()
                    .is_some_and(|last| !last.ends_with(':') && last != "/")
                {
                    out.pop();
                }
            }
            Component::Prefix(p) => out.push(match p.kind() {
                Prefix::VerbatimDisk(d) | Prefix::Disk(d) => {
                    format!("{}:", (d as char).to_ascii_lowercase())
                }
                _ => p.as_os_str().to_string_lossy().to_lowercase(),
            }),
            Component::RootDir => out.push("/".to_string()),
            Component::Normal(s) => out.push(s.to_string_lossy().to_lowercase()),
        }
    }
    out
}

fn resolve_entry(
    install_dir: &Path,
    entry: &CaptureWatchEntry,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<ResolvedWatch, String> {
    if !entry_config_valid(entry) {
        return Err("empty field, `..` component or pattern that is not .utrace/.log".to_string());
    }
    let expanded = expand_env(&entry.dir, env).ok_or("unset environment variable")?;
    let raw = PathBuf::from(expanded);
    if raw.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err("`..` component".to_string());
    }
    if (raw.has_root() || raw.components().any(|c| matches!(c, Component::Prefix(_))))
        && !raw.is_absolute()
    {
        return Err("drive-relative or root-relative path".to_string());
    }

    let joined = if raw.is_absolute() {
        raw
    } else {
        install_dir.join(raw)
    };
    let dir: PathBuf = joined
        .components()
        .filter(|c| !matches!(c, Component::CurDir))
        .collect();

    let dir_parts = comparison_components(&dir);
    let install_parts = comparison_components(install_dir);
    if dir_parts.last().map(String::as_str) == Some(PROTECTED_DIR) {
        return Err("directory is a Saved folder itself".to_string());
    }
    if install_parts.starts_with(&dir_parts) {
        return Err("directory is the install dir or an ancestor of it".to_string());
    }
    let inside_install =
        dir_parts.len() > install_parts.len() && dir_parts.starts_with(&install_parts);
    if inside_install {
        let below_install = &dir_parts[install_parts.len()..dir_parts.len() - 1];
        if !below_install.iter().any(|p| p == PROTECTED_DIR) {
            return Err("directory inside the install is not below a Saved folder".to_string());
        }
    }

    let mut builder = GlobSetBuilder::new();
    for pattern in &entry.patterns {
        let glob = GlobBuilder::new(pattern)
            .case_insensitive(true)
            .build()
            .map_err(|e| format!("invalid pattern {pattern:?}: {e}"))?;
        builder.add(glob);
    }
    let globs = builder.build().map_err(|e| e.to_string())?;

    Ok(ResolvedWatch {
        dir,
        globs,
        key_prefix: entry.key_prefix.trim_matches('/').to_string(),
    })
}

/// Resolves watch entries against `install_dir`, skipping invalid ones with a warning.
pub fn resolve_watch_dirs(install_dir: &Path, entries: &[CaptureWatchEntry]) -> Vec<ResolvedWatch> {
    resolve_watch_dirs_with_env(install_dir, entries, &|name| std::env::var(name).ok())
}

fn resolve_watch_dirs_with_env(
    install_dir: &Path,
    entries: &[CaptureWatchEntry],
    env: &dyn Fn(&str) -> Option<String>,
) -> Vec<ResolvedWatch> {
    entries
        .iter()
        .filter_map(|entry| match resolve_entry(install_dir, entry, env) {
            Ok(resolved) => Some(resolved),
            Err(reason) => {
                warn!(
                    "Skipping capture watch entry (dir {:?}): {reason}",
                    entry.dir
                );
                None
            }
        })
        .collect()
}

/// Files the uploader may start on now: in the session window, quiescent and not already known.
pub fn candidates(
    session: &CaptureSession,
    ledger: &CaptureLedger,
    watches: &[ResolvedWatch],
    quiescence_seconds: u64,
    now: DateTime<Utc>,
) -> Vec<Candidate> {
    scan(session, ledger, watches, quiescence_seconds, now, true)
}

/// Files that are or will become uploadable; the live trace of a running session is counted.
pub fn pending_candidates(
    session: &CaptureSession,
    ledger: &CaptureLedger,
    watches: &[ResolvedWatch],
    quiescence_seconds: u64,
    now: DateTime<Utc>,
) -> Vec<Candidate> {
    scan(session, ledger, watches, quiescence_seconds, now, false)
}

fn scan(
    session: &CaptureSession,
    ledger: &CaptureLedger,
    watches: &[ResolvedWatch],
    quiescence_seconds: u64,
    now: DateTime<Utc>,
    require_quiescent: bool,
) -> Vec<Candidate> {
    if session.state == SessionState::Closed {
        return Vec::new();
    }

    let quiescence_ms = i64::try_from(quiescence_seconds)
        .unwrap_or(i64::MAX / 1000)
        .saturating_mul(1000);
    let lower_ms = session.launched_at.timestamp_millis();
    let upper_ms = match (session.state, session.exited_at) {
        (SessionState::Exited, Some(exited_at)) => {
            Some(exited_at.timestamp_millis().saturating_add(quiescence_ms))
        }
        _ => None,
    };
    let now_ms = now.timestamp_millis();

    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut out = Vec::new();
    for watch in watches {
        let read_dir = match std::fs::read_dir(&watch.dir) {
            Ok(rd) => rd,
            Err(e) => {
                debug!("Cannot read capture watch dir {}: {e}", watch.dir.display());
                continue;
            }
        };
        for dirent in read_dir.flatten() {
            let name = dirent.file_name().to_string_lossy().into_owned();
            if is_denied(&name) || !watch.globs.is_match(&name) {
                continue;
            }
            let Some(kind) = kind_for_extension(&name) else {
                continue;
            };
            let path = dirent.path();
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if !meta.is_file() || meta.len() == 0 {
                continue;
            }
            let (Some(mtime_ms), Ok(mtime)) = (mtime_ms(&meta), meta.modified()) else {
                continue;
            };

            if mtime_ms > now_ms.saturating_add(MAX_FUTURE_MTIME_MS) {
                warn!(
                    "Ignoring capture file with an mtime more than 24h in the future: {}",
                    path.display()
                );
                continue;
            }
            if mtime_ms < lower_ms || upper_ms.is_some_and(|upper| mtime_ms > upper) {
                continue;
            }
            if require_quiescent && now_ms.saturating_sub(mtime_ms) < quiescence_ms {
                continue;
            }
            if is_known(ledger, &path, meta.len(), mtime_ms) {
                continue;
            }
            if !seen.insert(path.clone()) {
                continue;
            }
            out.push(Candidate {
                path,
                size: meta.len(),
                mtime,
                mtime_ms,
                kind,
                key_prefix: watch.key_prefix.clone(),
            });
        }
    }

    out.sort_by(|a, b| {
        let rank = |k: FileKind| u8::from(k == FileKind::Trace);
        rank(a.kind)
            .cmp(&rank(b.kind))
            .then(a.mtime_ms.cmp(&b.mtime_ms))
            .then_with(|| a.path.cmp(&b.path))
    });
    out
}

fn is_known(ledger: &CaptureLedger, path: &Path, size: u64, mtime_ms: i64) -> bool {
    ledger
        .uploaded
        .values()
        .any(|e| e.local_path == path && e.size == size && e.mtime_ms == mtime_ms)
        || ledger
            .inflight
            .iter()
            .any(|e| e.local_path == path && e.size == size && e.mtime_ms == mtime_ms)
}

#[cfg(test)]
mod tests {
    use std::fs::{self, OpenOptions};

    use chrono::TimeZone;
    use tempfile::TempDir;

    use super::*;
    use crate::capture::types::{InflightUpload, UploadedEntry};

    const Q: u64 = 30;

    fn entry(dir: &str, patterns: &[&str], prefix: &str) -> CaptureWatchEntry {
        CaptureWatchEntry {
            dir: dir.to_string(),
            patterns: patterns.iter().map(|p| p.to_string()).collect(),
            key_prefix: prefix.to_string(),
        }
    }

    fn no_env(_: &str) -> Option<String> {
        None
    }

    fn resolve(install: &Path, e: CaptureWatchEntry) -> Vec<ResolvedWatch> {
        resolve_watch_dirs_with_env(install, &[e], &no_env)
    }

    #[test]
    fn relative_dir_joins_install_dir() {
        let install = Path::new("install");
        let r = resolve(install, entry("Game/Saved/Logs", &["*.log"], "p/logs/"));
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].dir, install.join("Game/Saved/Logs"));
        assert_eq!(r[0].key_prefix, "p/logs");
        assert!(r[0].globs.is_match("A.LOG"));
    }

    #[test]
    fn absolute_dir_outside_install_is_accepted() {
        let tmp = TempDir::new().unwrap();
        let install = tmp.path().join("install");
        let other = tmp.path().join("elsewhere").join("Logs");
        let r = resolve(&install, entry(other.to_str().unwrap(), &["*.utrace"], "p"));
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].dir, other);
    }

    #[test]
    fn env_expansion_and_unset_variable() {
        let tmp = TempDir::new().unwrap();
        let install = tmp.path().join("install");
        let base = tmp.path().join("local");
        let env = |name: &str| (name == "LOCALAPPDATA").then(|| base.to_str().unwrap().to_string());

        let e = entry("%LOCALAPPDATA%/Game/Logs", &["*.log"], "p");
        let r = resolve_watch_dirs_with_env(&install, &[e], &env);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].dir, base.join("Game").join("Logs"));

        let e = entry("%NOT_SET%/Game/Logs", &["*.log"], "p");
        assert!(resolve_watch_dirs_with_env(&install, &[e], &env).is_empty());

        let empty = |_: &str| Some(String::new());
        let e = entry("%EMPTY%/Game/Logs", &["*.log"], "p");
        assert!(resolve_watch_dirs_with_env(&install, &[e], &empty).is_empty());
    }

    #[test]
    fn real_environment_lookup_skips_unset_variable() {
        let install = Path::new("install");
        let e = entry(
            "%ETHOS_CAPTURE_TEST_DEFINITELY_UNSET%/Saved/Logs",
            &["*.log"],
            "p",
        );
        assert!(resolve_watch_dirs(install, &[e]).is_empty());
    }

    #[test]
    fn parent_dir_components_are_rejected() {
        let install = Path::new("install");
        for dir in [
            "Saved/../Logs",
            "../Saved/Logs",
            "Saved\\..\\Logs",
            "Saved/Logs/..",
        ] {
            assert!(
                resolve(install, entry(dir, &["*.log"], "p")).is_empty(),
                "{dir}"
            );
        }
    }

    #[test]
    fn env_value_with_parent_dir_is_rejected() {
        let install = Path::new("install");
        let env = |_: &str| Some("../x".to_string());
        let e = entry("%V%/Saved/Logs", &["*.log"], "p");
        assert!(resolve_watch_dirs_with_env(install, &[e], &env).is_empty());
    }

    #[test]
    fn saved_install_and_ancestors_are_rejected() {
        let tmp = TempDir::new().unwrap();
        let install = tmp.path().join("Users").join("t").join("Game");
        let abs = |p: &Path| p.to_str().unwrap().to_string();

        for dir in [
            "Saved".to_string(),
            "game/saved".to_string(),
            "Game/SAVED".to_string(),
            ".".to_string(),
            abs(&install),
            abs(install.parent().unwrap()),
            abs(tmp.path()),
            abs(&install.join("Saved")),
            abs(&install.join("Binaries")),
            "Binaries".to_string(),
            "Saved/../".to_string(),
        ] {
            assert!(
                resolve(&install, entry(&dir, &["*.log"], "p")).is_empty(),
                "{dir}"
            );
        }

        assert_eq!(
            resolve(&install, entry("Saved/Logs", &["*.log"], "p")).len(),
            1
        );
        assert_eq!(
            resolve(&install, entry("Game/Saved/Logs/Sub", &["*.log"], "p")).len(),
            1
        );
        assert_eq!(
            resolve(&install, entry("saved/logs", &["*.log"], "p")).len(),
            1
        );
    }

    #[test]
    fn ancestor_check_is_component_wise_and_case_insensitive() {
        let install = Path::new("base/Game2/work");
        assert_eq!(
            resolve(install, entry("../x", &["*.log"], "p")).len(),
            0,
            "parent dir"
        );
        let abs_sibling = std::env::temp_dir().join("Game2-sibling").join("Logs");
        let install_abs = std::env::temp_dir().join("Game2");
        let r = resolve(
            &install_abs,
            entry(abs_sibling.to_str().unwrap(), &["*.log"], "p"),
        );
        assert_eq!(r.len(), 1, "string prefix must not count as ancestor");

        let upper = install_abs.to_str().unwrap().to_uppercase();
        assert!(resolve(&install_abs, entry(&upper, &["*.log"], "p")).is_empty());
    }

    #[test]
    fn pattern_and_field_rules() {
        let install = Path::new("install");
        let bad = [
            entry("Saved/Logs", &["*.txt"], "p"),
            entry("Saved/Logs", &["*.log", "*.txt"], "p"),
            entry("Saved/Logs", &["discord_token.txt"], "p"),
            entry("Saved/Logs", &["*"], "p"),
            entry("Saved/Logs", &["*.log.bak"], "p"),
            entry("Saved/Logs", &[], "p"),
            entry("", &["*.log"], "p"),
            entry("Saved/Logs", &["*.log"], ""),
            entry("Saved/Logs", &["*.log"], "//"),
            entry("Saved/Logs", &["[*.log"], "p"),
        ];
        for e in bad {
            assert!(resolve(install, e.clone()).is_empty(), "{e:?}");
        }
        let r = resolve(install, entry("Saved/Logs", &["*.LOG", "A*.Utrace"], "p"));
        assert_eq!(r.len(), 1);
        assert!(r[0].globs.is_match("x.log"));
        assert!(r[0].globs.is_match("a_b.UTRACE"));
    }

    #[test]
    fn invalid_entries_do_not_block_valid_ones() {
        let install = Path::new("install");
        let entries = [
            entry("Saved/Logs", &["*.txt"], "p"),
            entry("Saved/Profiling", &["*.utrace"], "q"),
        ];
        let r = resolve_watch_dirs_with_env(install, &entries, &no_env);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].key_prefix, "q");
    }

    #[test]
    fn usable_watch_entries_rules() {
        let mut cfg = ClientCaptureUploadConfig::default();
        assert!(!usable_watch_entries(&cfg));
        cfg.watch = vec![
            entry("Saved/Logs", &["*.txt"], "p"),
            entry("../Saved", &["*.log"], "p"),
            entry("", &["*.log"], "p"),
        ];
        assert!(!usable_watch_entries(&cfg));
        cfg.watch.push(entry("Saved/Logs", &["*.log"], "p"));
        assert!(usable_watch_entries(&cfg));
    }

    fn at(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_800_000_000 + secs, 0).unwrap()
    }

    fn write(dir: &Path, name: &str, body: &str, mtime: DateTime<Utc>) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, body).unwrap();
        set_mtime(&path, mtime);
        path
    }

    fn set_mtime(path: &Path, mtime: DateTime<Utc>) {
        let f = OpenOptions::new().write(true).open(path).unwrap();
        f.set_modified(mtime.into()).unwrap();
    }

    fn session(state: SessionState, exited: Option<DateTime<Utc>>) -> CaptureSession {
        CaptureSession {
            id: "s1".to_string(),
            install_dir: PathBuf::from("install"),
            sha: None,
            user: "u".to_string(),
            playtest: "pt".to_string(),
            launched_at: at(0),
            state,
            exited_at: exited,
            closed_at: None,
            close_reason: None,
            empty_polls: 0,
            first_empty_at: None,
        }
    }

    fn watch_for(dir: &Path, patterns: &[&str]) -> Vec<ResolvedWatch> {
        let mut builder = GlobSetBuilder::new();
        for p in patterns {
            builder.add(GlobBuilder::new(p).case_insensitive(true).build().unwrap());
        }
        vec![ResolvedWatch {
            dir: dir.to_path_buf(),
            globs: builder.build().unwrap(),
            key_prefix: "pre".to_string(),
        }]
    }

    fn names(c: &[Candidate]) -> Vec<String> {
        c.iter()
            .map(|c| c.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect()
    }

    fn exited_session() -> CaptureSession {
        session(SessionState::Exited, Some(at(1000)))
    }

    #[test]
    fn window_bounds_are_inclusive() {
        let tmp = TempDir::new().unwrap();
        let w = watch_for(tmp.path(), &["*.log"]);
        write(tmp.path(), "before.log", "x", at(-1));
        write(tmp.path(), "at_launch.log", "x", at(0));
        write(tmp.path(), "at_upper.log", "x", at(1000 + Q as i64));
        write(tmp.path(), "after.log", "x", at(1000 + Q as i64 + 1));
        let now = at(5000);

        let got = candidates(&exited_session(), &CaptureLedger::default(), &w, Q, now);
        let mut n = names(&got);
        n.sort();
        assert_eq!(n, vec!["at_launch.log", "at_upper.log"]);
    }

    #[test]
    fn window_is_compared_in_milliseconds() {
        let tmp = TempDir::new().unwrap();
        let w = watch_for(tmp.path(), &["*.log"]);
        let upper = at(1000 + Q as i64);
        write(
            tmp.path(),
            "just_over.log",
            "x",
            upper + chrono::Duration::milliseconds(1),
        );
        write(
            tmp.path(),
            "launch_minus_1ms.log",
            "x",
            at(0) - chrono::Duration::milliseconds(1),
        );
        let got = candidates(
            &exited_session(),
            &CaptureLedger::default(),
            &w,
            Q,
            at(5000),
        );
        assert!(got.is_empty(), "{got:?}");
    }

    #[test]
    fn quiescence_filter() {
        let tmp = TempDir::new().unwrap();
        let w = watch_for(tmp.path(), &["*.log"]);
        let now = at(500);
        write(
            tmp.path(),
            "settled.log",
            "x",
            now - chrono::Duration::seconds(30),
        );
        write(
            tmp.path(),
            "fresh.log",
            "x",
            now - chrono::Duration::seconds(29),
        );
        let s = session(SessionState::Exited, Some(at(600)));

        let got = candidates(&s, &CaptureLedger::default(), &w, Q, now);
        assert_eq!(names(&got), vec!["settled.log"]);

        let pending = pending_candidates(&s, &CaptureLedger::default(), &w, Q, now);
        let mut n = names(&pending);
        n.sort();
        assert_eq!(n, vec!["fresh.log", "settled.log"]);
    }

    #[test]
    fn running_and_launched_have_no_upper_bound() {
        let tmp = TempDir::new().unwrap();
        let w = watch_for(tmp.path(), &["*.utrace"]);
        write(tmp.path(), "live.utrace", "x", at(100_000));
        let now = at(100_000);

        for state in [SessionState::Launched, SessionState::Running] {
            let s = session(state, None);
            let pending = pending_candidates(&s, &CaptureLedger::default(), &w, Q, now);
            assert_eq!(names(&pending), vec!["live.utrace"]);
            let later = now + chrono::Duration::seconds(Q as i64);
            let got = candidates(&s, &CaptureLedger::default(), &w, Q, later);
            assert_eq!(names(&got), vec!["live.utrace"]);
        }
    }

    #[test]
    fn exited_upper_bound_applies_to_pending_too() {
        let tmp = TempDir::new().unwrap();
        let w = watch_for(tmp.path(), &["*.utrace"]);
        write(tmp.path(), "later.utrace", "x", at(1000 + Q as i64 + 1));
        let pending = pending_candidates(
            &exited_session(),
            &CaptureLedger::default(),
            &w,
            Q,
            at(5000),
        );
        assert!(pending.is_empty());
    }

    #[test]
    fn closed_session_yields_nothing() {
        let tmp = TempDir::new().unwrap();
        let w = watch_for(tmp.path(), &["*.log"]);
        write(tmp.path(), "a.log", "x", at(10));
        let s = session(SessionState::Closed, Some(at(1000)));
        assert!(candidates(&s, &CaptureLedger::default(), &w, Q, at(5000)).is_empty());
        assert!(pending_candidates(&s, &CaptureLedger::default(), &w, Q, at(5000)).is_empty());
    }

    #[test]
    fn non_recursive_and_pattern_filtered() {
        let tmp = TempDir::new().unwrap();
        let w = watch_for(tmp.path(), &["*.log"]);
        write(tmp.path(), "top.log", "x", at(10));
        write(tmp.path(), "top.txt", "x", at(10));
        let sub = tmp.path().join("sub");
        fs::create_dir(&sub).unwrap();
        write(&sub, "nested.log", "x", at(10));

        let got = candidates(
            &exited_session(),
            &CaptureLedger::default(),
            &w,
            Q,
            at(5000),
        );
        assert_eq!(names(&got), vec!["top.log"]);
    }

    #[test]
    fn zero_byte_and_directories_are_skipped() {
        let tmp = TempDir::new().unwrap();
        let w = watch_for(tmp.path(), &["*.log"]);
        write(tmp.path(), "empty.log", "", at(10));
        let dir = tmp.path().join("dir.log");
        fs::create_dir(&dir).unwrap();
        write(tmp.path(), "ok.log", "x", at(10));

        let got = candidates(
            &exited_session(),
            &CaptureLedger::default(),
            &w,
            Q,
            at(5000),
        );
        assert_eq!(names(&got), vec!["ok.log"]);
    }

    #[test]
    fn symlinks_are_skipped_where_supported() {
        let tmp = TempDir::new().unwrap();
        let w = watch_for(tmp.path(), &["*.log"]);
        let target = write(tmp.path(), "real.txt", "x", at(10));
        let link = tmp.path().join("link.log");

        #[cfg(unix)]
        let made = std::os::unix::fs::symlink(&target, &link).is_ok();
        #[cfg(windows)]
        let made = std::os::windows::fs::symlink_file(&target, &link).is_ok();
        if !made {
            return;
        }

        let got = candidates(
            &exited_session(),
            &CaptureLedger::default(),
            &w,
            Q,
            at(5000),
        );
        assert!(got.is_empty(), "{got:?}");
    }

    #[test]
    fn future_mtime_is_ignored() {
        let tmp = TempDir::new().unwrap();
        let w = watch_for(tmp.path(), &["*.log"]);
        let now = at(5000);
        write(
            tmp.path(),
            "far.log",
            "x",
            now + chrono::Duration::hours(25),
        );
        write(
            tmp.path(),
            "soon.log",
            "x",
            now + chrono::Duration::hours(1),
        );
        let s = session(SessionState::Running, None);

        let pending = pending_candidates(&s, &CaptureLedger::default(), &w, Q, now);
        assert_eq!(names(&pending), vec!["soon.log"]);
        assert!(candidates(&s, &CaptureLedger::default(), &w, Q, now).is_empty());
    }

    #[test]
    fn logs_come_before_traces_then_mtime_ascending() {
        let tmp = TempDir::new().unwrap();
        let w = watch_for(tmp.path(), &["*.log", "*.utrace"]);
        write(tmp.path(), "t_new.utrace", "x", at(40));
        write(tmp.path(), "t_old.utrace", "x", at(10));
        write(tmp.path(), "l_new.log", "x", at(30));
        write(tmp.path(), "l_old.log", "x", at(20));

        let got = candidates(
            &exited_session(),
            &CaptureLedger::default(),
            &w,
            Q,
            at(5000),
        );
        assert_eq!(
            names(&got),
            vec!["l_old.log", "l_new.log", "t_old.utrace", "t_new.utrace"]
        );
        assert_eq!(got[0].kind, FileKind::Log);
        assert_eq!(got[2].kind, FileKind::Trace);
        assert_eq!(got[0].key_prefix, "pre");
    }

    #[test]
    fn known_files_are_skipped_only_on_exact_match() {
        let tmp = TempDir::new().unwrap();
        let w = watch_for(tmp.path(), &["*.log"]);
        let up = write(tmp.path(), "uploaded.log", "abc", at(10));
        let fl = write(tmp.path(), "inflight.log", "abcd", at(11));
        let changed = write(tmp.path(), "changed.log", "abcde", at(12));
        write(tmp.path(), "new.log", "x", at(13));

        let stat = |p: &Path| {
            let m = fs::symlink_metadata(p).unwrap();
            (m.len(), mtime_ms(&m).unwrap())
        };
        let mut ledger = CaptureLedger::default();
        let (size, ms) = stat(&up);
        ledger.uploaded.insert(
            "aa".to_string(),
            UploadedEntry {
                key: "k".to_string(),
                bucket: "b".to_string(),
                local_path: up,
                size,
                mtime_ms: ms,
                uploaded_at: at(100),
                session_id: "s1".to_string(),
            },
        );
        let (size, ms) = stat(&fl);
        ledger.inflight.push(InflightUpload {
            session_id: "s1".to_string(),
            local_path: fl,
            size,
            mtime_ms: ms,
            sha256: "bb".to_string(),
            bucket: "b".to_string(),
            key: "k".to_string(),
            upload_id: None,
            part_size: 0,
            started_at: at(100),
            attempts: 0,
            next_attempt_at: None,
        });
        let (size, ms) = stat(&changed);
        ledger.uploaded.insert(
            "cc".to_string(),
            UploadedEntry {
                key: "k".to_string(),
                bucket: "b".to_string(),
                local_path: changed,
                size,
                mtime_ms: ms + 1,
                uploaded_at: at(100),
                session_id: "s1".to_string(),
            },
        );

        let got = candidates(&exited_session(), &ledger, &w, Q, at(5000));
        let mut n = names(&got);
        n.sort();
        assert_eq!(n, vec!["changed.log", "new.log"]);
    }

    #[test]
    fn discord_token_is_never_selectable_even_with_a_matching_glob() {
        let tmp = TempDir::new().unwrap();
        let w = watch_for(tmp.path(), &["*"]);
        write(tmp.path(), "discord_token.txt", "secret", at(10));
        write(tmp.path(), "Discord_Token.TXT", "secret", at(10));
        write(tmp.path(), "ok.log", "x", at(10));

        let got = candidates(
            &exited_session(),
            &CaptureLedger::default(),
            &w,
            Q,
            at(5000),
        );
        assert_eq!(names(&got), vec!["ok.log"]);
        assert!(is_denied("DISCORD_TOKEN.txt"));
        assert!(!is_denied("discord_token.log"));
    }

    #[test]
    fn resolved_globs_are_case_insensitive_end_to_end() {
        let tmp = TempDir::new().unwrap();
        let install = tmp.path().join("install");
        let logs = install.join("Game").join("Saved").join("Logs");
        fs::create_dir_all(&logs).unwrap();
        write(&logs, "Game.LOG", "x", at(10));
        write(&logs, "discord_token.txt", "secret", at(10));

        let w = resolve(&install, entry("Game/Saved/Logs", &["*.log"], "p"));
        let got = candidates(
            &exited_session(),
            &CaptureLedger::default(),
            &w,
            Q,
            at(5000),
        );
        assert_eq!(names(&got), vec!["Game.LOG"]);
    }

    #[test]
    fn missing_watch_dir_is_not_an_error() {
        let tmp = TempDir::new().unwrap();
        let w = watch_for(&tmp.path().join("nope"), &["*.log"]);
        assert!(candidates(
            &exited_session(),
            &CaptureLedger::default(),
            &w,
            Q,
            at(5000)
        )
        .is_empty());
    }

    #[test]
    fn candidate_mtime_matches_mtime_ms() {
        let tmp = TempDir::new().unwrap();
        let w = watch_for(tmp.path(), &["*.log"]);
        write(tmp.path(), "a.log", "x", at(10));
        let got = candidates(
            &exited_session(),
            &CaptureLedger::default(),
            &w,
            Q,
            at(5000),
        );
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].mtime_ms, at(10).timestamp_millis());
        let from_time = DateTime::<Utc>::from(got[0].mtime).timestamp_millis();
        assert_eq!(from_time, got[0].mtime_ms);
    }
}
