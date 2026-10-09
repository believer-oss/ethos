//! Count-based deletion of uploaded local traces.

use std::collections::BTreeMap;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use tracing::{debug, warn};

use super::ledger::stat_matches;
use super::types::{CaptureLedger, UploadedEntry};

#[derive(Debug, Default, PartialEq, Eq)]
pub struct CleanupReport {
    pub deleted: Vec<PathBuf>,
    pub failed: Vec<PathBuf>,
    /// Uploaded traces still on disk after the pass.
    pub remaining: usize,
    pub skipped_persist_failed: bool,
}

fn is_trace(entry: &UploadedEntry) -> bool {
    entry
        .local_path
        .to_string_lossy()
        .to_ascii_lowercase()
        .ends_with(".utrace")
}

/// Deletes the oldest uploaded `.utrace` files (across all installs) until at most `keep` remain.
///
/// Only files recorded in `uploaded` whose `(size, mtime_ms)` still match are ever candidates.
pub fn cleanup_local_traces(
    ledger: &CaptureLedger,
    keep: u32,
    persist_failed: bool,
) -> CleanupReport {
    if persist_failed {
        debug!("Skipping local trace cleanup: capture ledger is not persisting");
        return CleanupReport {
            skipped_persist_failed: true,
            ..Default::default()
        };
    }

    let mut by_path: BTreeMap<&PathBuf, (DateTime<Utc>, &UploadedEntry)> = BTreeMap::new();
    for entry in ledger.uploaded.values().filter(|e| is_trace(e)) {
        if !stat_matches(&entry.local_path, entry.size, entry.mtime_ms) {
            continue;
        }
        by_path
            .entry(&entry.local_path)
            .and_modify(|current| {
                if entry.uploaded_at > current.0 {
                    *current = (entry.uploaded_at, entry);
                }
            })
            .or_insert((entry.uploaded_at, entry));
    }

    let mut candidates: Vec<(DateTime<Utc>, &PathBuf, &UploadedEntry)> = by_path
        .into_iter()
        .map(|(path, (at, entry))| (at, path, entry))
        .collect();
    candidates.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(b.1)));

    let excess = candidates.len().saturating_sub(keep as usize);
    let mut report = CleanupReport {
        remaining: candidates.len(),
        ..Default::default()
    };

    for (_, path, entry) in candidates.into_iter().take(excess) {
        if !stat_matches(path, entry.size, entry.mtime_ms) {
            report.remaining -= 1;
            continue;
        }
        match std::fs::remove_file(path) {
            Ok(()) => {
                report.deleted.push(path.clone());
                report.remaining -= 1;
            }
            Err(e) => {
                warn!("Could not delete uploaded trace {path:?}: {e}; will retry");
                report.failed.push(path.clone());
            }
        }
    }

    report
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use chrono::TimeZone;

    use super::*;
    use crate::capture::ledger::mtime_ms;

    fn at(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_700_000_000 + secs, 0).unwrap()
    }

    fn ledger() -> CaptureLedger {
        CaptureLedger {
            version: 1,
            ..Default::default()
        }
    }

    fn write(path: &Path, body: &[u8]) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    fn upload(ledger: &mut CaptureLedger, path: &Path, uploaded_at: DateTime<Utc>) {
        let meta = fs::symlink_metadata(path).unwrap();
        ledger.uploaded.insert(
            format!("sha-{}", path.display()),
            UploadedEntry {
                key: "k".to_string(),
                bucket: "b".to_string(),
                local_path: path.to_path_buf(),
                size: meta.len(),
                mtime_ms: mtime_ms(&meta).unwrap(),
                uploaded_at,
                session_id: "s".to_string(),
            },
        );
    }

    #[test]
    fn keeps_newest_five_across_two_installs() {
        let dir = tempfile::tempdir().unwrap();
        let mut l = ledger();
        let mut paths = Vec::new();
        for i in 0..7 {
            let install = if i % 2 == 0 { "install-a" } else { "install-b" };
            let p = dir.path().join(install).join(format!("t{i}.utrace"));
            write(&p, format!("trace {i}").as_bytes());
            upload(&mut l, &p, at(i));
            paths.push(p);
        }

        let report = cleanup_local_traces(&l, 5, false);
        assert_eq!(report.deleted, vec![paths[0].clone(), paths[1].clone()]);
        assert_eq!(report.remaining, 5);
        assert!(!paths[0].exists() && !paths[1].exists());
        assert!(paths[2..].iter().all(|p| p.exists()));

        let again = cleanup_local_traces(&l, 5, false);
        assert!(again.deleted.is_empty());
        assert_eq!(again.remaining, 5);
    }

    #[test]
    fn ties_break_by_path() {
        let dir = tempfile::tempdir().unwrap();
        let mut l = ledger();
        let b = dir.path().join("b.utrace");
        let a = dir.path().join("a.utrace");
        for p in [&b, &a] {
            write(p, b"x");
            upload(&mut l, p, at(0));
        }
        let report = cleanup_local_traces(&l, 1, false);
        assert_eq!(report.deleted, vec![a]);
        assert!(b.exists());
    }

    #[test]
    fn keep_zero_deletes_all_uploaded_traces() {
        let dir = tempfile::tempdir().unwrap();
        let mut l = ledger();
        let p = dir.path().join("only.UTRACE");
        write(&p, b"x");
        upload(&mut l, &p, at(0));
        let report = cleanup_local_traces(&l, 0, false);
        assert_eq!(report.deleted, vec![p.clone()]);
        assert!(!p.exists());
    }

    #[test]
    fn never_touches_unuploaded_logs_or_changed_files() {
        let dir = tempfile::tempdir().unwrap();
        let mut l = ledger();

        let not_uploaded = dir.path().join("pending.utrace");
        write(&not_uploaded, b"pending");

        let log = dir.path().join("old.log");
        write(&log, b"log");
        upload(&mut l, &log, at(0));

        let replaced = dir.path().join("replaced.utrace");
        write(&replaced, b"first");
        upload(&mut l, &replaced, at(1));
        write(&replaced, b"a longer replacement");

        let newest = dir.path().join("newest.utrace");
        write(&newest, b"n");
        upload(&mut l, &newest, at(2));

        let report = cleanup_local_traces(&l, 1, false);
        assert!(report.deleted.is_empty());
        assert_eq!(report.remaining, 1);
        for p in [&not_uploaded, &log, &replaced, &newest] {
            assert!(p.exists());
        }
        assert_eq!(fs::read(&replaced).unwrap(), b"a longer replacement");
    }

    #[test]
    fn mtime_change_with_same_size_is_not_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let mut l = ledger();
        let p = dir.path().join("t.utrace");
        write(&p, b"same");
        upload(&mut l, &p, at(0));
        l.uploaded.values_mut().for_each(|e| e.mtime_ms += 1);

        let report = cleanup_local_traces(&l, 0, false);
        assert!(report.deleted.is_empty());
        assert!(p.exists());
    }

    #[test]
    fn disabled_when_persist_failed() {
        let dir = tempfile::tempdir().unwrap();
        let mut l = ledger();
        let p = dir.path().join("t.utrace");
        write(&p, b"x");
        upload(&mut l, &p, at(0));

        let report = cleanup_local_traces(&l, 0, true);
        assert!(report.skipped_persist_failed);
        assert!(report.deleted.is_empty());
        assert!(p.exists());
    }

    #[cfg(windows)]
    #[test]
    fn locked_file_is_skipped_then_retried() {
        use std::os::windows::fs::OpenOptionsExt;

        let dir = tempfile::tempdir().unwrap();
        let mut l = ledger();
        let oldest = dir.path().join("t0.utrace");
        let newest = dir.path().join("t1.utrace");
        for (i, p) in [&oldest, &newest].into_iter().enumerate() {
            write(p, b"x");
            upload(&mut l, p, at(i as i64));
        }

        let lock = fs::OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(&oldest)
            .unwrap();
        let report = cleanup_local_traces(&l, 1, false);
        assert!(report.deleted.is_empty());
        assert_eq!(report.failed, vec![oldest.clone()]);
        assert!(oldest.exists());
        drop(lock);

        let retry = cleanup_local_traces(&l, 1, false);
        assert_eq!(retry.deleted, vec![oldest.clone()]);
        assert!(!oldest.exists() && newest.exists());
    }
}
