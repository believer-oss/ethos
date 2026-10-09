//! Persistent capture ledger (`client-capture.json`).

use std::fs::Metadata;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use chrono::{DateTime, Duration, Utc};
use parking_lot::Mutex;
use tracing::{debug, warn};

use super::types::{CaptureLedger, CaptureSession, InflightUpload, SessionState, UploadedEntry};

pub const LEDGER_FILE: &str = "client-capture.json";
pub const UPLOADED_CAP: usize = 2000;
const SUPPORTED_VERSION: u32 = 1;
const COALESCE_MS: i64 = 2000;

/// File mtime in whole milliseconds since the epoch; every `mtime_ms` comparison uses this precision.
pub fn mtime_ms(meta: &Metadata) -> Option<i64> {
    let modified = meta.modified().ok()?;
    Some(DateTime::<Utc>::from(modified).timestamp_millis())
}

/// True when `path` is a regular file whose current `(size, mtime_ms)` equal the given values.
pub fn stat_matches(path: &Path, size: u64, mtime: i64) -> bool {
    match std::fs::symlink_metadata(path) {
        Ok(meta) => meta.is_file() && meta.len() == size && mtime_ms(&meta) == Some(mtime),
        Err(_) => false,
    }
}

impl CaptureLedger {
    fn empty() -> Self {
        Self {
            version: SUPPORTED_VERSION,
            ..Default::default()
        }
    }

    /// Returns how many records were removed. Entries whose local file is still present and unchanged are pinned.
    pub fn prune(&mut self, now: DateTime<Utc>, retention_days: u32) -> usize {
        let cutoff = now - Duration::days(i64::from(retention_days));
        let before = self.sessions.len() + self.uploaded.len();

        self.sessions.retain(|session| {
            if session.state != SessionState::Closed {
                return true;
            }
            let closed_at = session.closed_at.unwrap_or(session.launched_at);
            let install_missing = matches!(session.install_dir.try_exists(), Ok(false));
            closed_at >= cutoff && !install_missing
        });

        let pinned =
            |entry: &UploadedEntry| stat_matches(&entry.local_path, entry.size, entry.mtime_ms);

        self.uploaded
            .retain(|_, entry| entry.uploaded_at >= cutoff || pinned(entry));

        if self.uploaded.len() > UPLOADED_CAP {
            let mut oldest: Vec<(DateTime<Utc>, String)> = self
                .uploaded
                .iter()
                .map(|(sha, entry)| (entry.uploaded_at, sha.clone()))
                .collect();
            oldest.sort();

            let mut remaining = self.uploaded.len();
            for (_, sha) in oldest {
                if remaining <= UPLOADED_CAP {
                    break;
                }
                if self.uploaded.get(&sha).is_some_and(pinned) {
                    continue;
                }
                self.uploaded.remove(&sha);
                remaining -= 1;
            }
        }

        before - (self.sessions.len() + self.uploaded.len())
    }
}

struct State {
    ledger: CaptureLedger,
    dirty: bool,
    last_write: Option<DateTime<Utc>>,
}

/// Single in-memory copy of the ledger. The lock is never held across an await; every helper is synchronous.
pub struct CaptureStore {
    path: PathBuf,
    state: Mutex<State>,
    persist_failed: AtomicBool,
}

impl CaptureStore {
    pub fn load(path: PathBuf, now: DateTime<Utc>) -> Self {
        let ledger = load_ledger(&path, now);
        Self {
            path,
            state: Mutex::new(State {
                ledger,
                dirty: false,
                last_write: None,
            }),
            persist_failed: AtomicBool::new(false),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn persist_failed(&self) -> bool {
        self.persist_failed.load(Ordering::Relaxed)
    }

    pub fn with<R>(&self, f: impl FnOnce(&CaptureLedger) -> R) -> R {
        f(&self.state.lock().ledger)
    }

    pub fn snapshot(&self) -> CaptureLedger {
        self.state.lock().ledger.clone()
    }

    pub fn add_session(&self, session: CaptureSession, now: DateTime<Utc>) {
        let mut state = self.state.lock();
        state.ledger.sessions.push(session);
        self.write_now(&mut state, now);
    }

    /// Returns false when no session has this id. Persists immediately when the session becomes `Closed`.
    pub fn update_session(
        &self,
        id: &str,
        now: DateTime<Utc>,
        f: impl FnOnce(&mut CaptureSession),
    ) -> bool {
        let mut state = self.state.lock();
        let Some(session) = state.ledger.sessions.iter_mut().find(|s| s.id == id) else {
            return false;
        };
        let was_closed = session.state == SessionState::Closed;
        f(session);
        let closed_now = !was_closed && session.state == SessionState::Closed;
        if closed_now {
            self.write_now(&mut state, now);
        } else {
            self.mark_dirty(&mut state, now);
        }
        true
    }

    /// Persists immediately when the row gains an `upload_id` it did not have.
    pub fn upsert_inflight(&self, row: InflightUpload, now: DateTime<Utc>) {
        let mut state = self.state.lock();
        let inflight = &mut state.ledger.inflight;
        let upload_id_created = match inflight.iter_mut().find(|r| r.local_path == row.local_path) {
            Some(existing) => {
                let created = row.upload_id.is_some() && existing.upload_id != row.upload_id;
                *existing = row;
                created
            }
            None => {
                let created = row.upload_id.is_some();
                inflight.push(row);
                created
            }
        };
        if upload_id_created {
            self.write_now(&mut state, now);
        } else {
            self.mark_dirty(&mut state, now);
        }
    }

    pub fn remove_inflight(&self, local_path: &Path, now: DateTime<Utc>) -> Option<InflightUpload> {
        let mut state = self.state.lock();
        let index = state
            .ledger
            .inflight
            .iter()
            .position(|r| r.local_path == local_path)?;
        let removed = state.ledger.inflight.remove(index);
        self.mark_dirty(&mut state, now);
        Some(removed)
    }

    /// Records the upload, drops the matching inflight row and persists immediately.
    pub fn record_uploaded(&self, sha256: String, entry: UploadedEntry, now: DateTime<Utc>) {
        let mut state = self.state.lock();
        state
            .ledger
            .inflight
            .retain(|r| r.local_path != entry.local_path);
        state.ledger.uploaded.insert(sha256, entry);
        self.write_now(&mut state, now);
    }

    pub fn prune(&self, now: DateTime<Utc>, retention_days: u32) -> usize {
        let mut state = self.state.lock();
        let removed = state.ledger.prune(now, retention_days);
        if removed > 0 {
            self.mark_dirty(&mut state, now);
        }
        removed
    }

    /// Marks the ledger dirty and writes now unless a write happened within the last 2 s.
    pub fn persist_soon(&self, now: DateTime<Utc>) {
        let mut state = self.state.lock();
        self.mark_dirty(&mut state, now);
    }

    /// Writes a dirty ledger when the coalescing window has passed; also the retry path after a failed write.
    pub fn flush_if_due(&self, now: DateTime<Utc>) -> bool {
        let mut state = self.state.lock();
        if state.dirty && Self::due(&state, now) {
            return self.write_now(&mut state, now);
        }
        false
    }

    /// Unconditional write (shutdown).
    pub fn flush(&self, now: DateTime<Utc>) -> bool {
        let mut state = self.state.lock();
        self.write_now(&mut state, now)
    }

    fn due(state: &State, now: DateTime<Utc>) -> bool {
        match state.last_write {
            Some(last) => (now - last).num_milliseconds() >= COALESCE_MS,
            None => true,
        }
    }

    fn mark_dirty(&self, state: &mut State, now: DateTime<Utc>) {
        state.dirty = true;
        if Self::due(state, now) {
            self.write_now(state, now);
        }
    }

    fn write_now(&self, state: &mut State, now: DateTime<Utc>) -> bool {
        state.last_write = Some(now);
        match write_ledger(&self.path, &state.ledger) {
            Ok(()) => {
                state.dirty = false;
                self.persist_failed.store(false, Ordering::Relaxed);
                true
            }
            Err(e) => {
                state.dirty = true;
                if !self.persist_failed.swap(true, Ordering::Relaxed) {
                    warn!(
                        "Could not save capture ledger at {:?}: {e}. Continuing from memory.",
                        self.path
                    );
                } else {
                    debug!("Capture ledger write still failing: {e}");
                }
                false
            }
        }
    }
}

fn write_ledger(path: &Path, ledger: &CaptureLedger) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(ledger)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, bytes)?;
    std::fs::rename(&temporary, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&temporary);
    })
}

fn load_ledger(path: &Path, now: DateTime<Utc>) -> CaptureLedger {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return CaptureLedger::empty(),
        Err(e) => {
            warn!("Could not read capture ledger at {path:?}: {e}");
            move_aside(path, now);
            return CaptureLedger::empty();
        }
    };

    match serde_json::from_slice::<CaptureLedger>(&bytes) {
        Ok(ledger) if ledger.version <= SUPPORTED_VERSION => ledger,
        Ok(ledger) => {
            warn!(
                "Capture ledger at {path:?} has unsupported version {}; starting empty",
                ledger.version
            );
            move_aside(path, now);
            CaptureLedger::empty()
        }
        Err(e) => {
            warn!("Capture ledger at {path:?} is unreadable: {e}; starting empty");
            move_aside(path, now);
            CaptureLedger::empty()
        }
    }
}

fn move_aside(path: &Path, now: DateTime<Utc>) {
    let Some(name) = path.file_name() else {
        return;
    };
    let mut aside = name.to_os_string();
    aside.push(format!(".corrupt-{}", now.timestamp_millis()));
    let target = path.with_file_name(aside);
    if let Err(e) = std::fs::rename(path, &target) {
        warn!("Could not move capture ledger aside to {target:?}: {e}");
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use chrono::TimeZone;

    use super::*;
    use crate::capture::types::CloseReason;

    fn at(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(1_700_000_000 + secs, 0).unwrap()
    }

    fn session(id: &str, install_dir: &Path) -> CaptureSession {
        CaptureSession {
            id: id.to_string(),
            install_dir: install_dir.to_path_buf(),
            sha: Some("abc".to_string()),
            user: "tester".to_string(),
            playtest: "pt".to_string(),
            launched_at: at(0),
            state: SessionState::Launched,
            exited_at: None,
            closed_at: None,
            close_reason: None,
            empty_polls: 0,
            first_empty_at: None,
        }
    }

    fn closed_session(id: &str, install_dir: &Path, closed_at: DateTime<Utc>) -> CaptureSession {
        CaptureSession {
            state: SessionState::Closed,
            closed_at: Some(closed_at),
            close_reason: Some(CloseReason::Uploaded),
            ..session(id, install_dir)
        }
    }

    fn entry(path: &Path, uploaded_at: DateTime<Utc>) -> UploadedEntry {
        let (size, mtime) = match fs::symlink_metadata(path) {
            Ok(meta) => (meta.len(), mtime_ms(&meta).unwrap()),
            Err(_) => (1, 1),
        };
        UploadedEntry {
            key: "k".to_string(),
            bucket: "b".to_string(),
            local_path: path.to_path_buf(),
            size,
            mtime_ms: mtime,
            uploaded_at,
            session_id: "s".to_string(),
        }
    }

    fn inflight(path: &Path, upload_id: Option<&str>) -> InflightUpload {
        InflightUpload {
            session_id: "s".to_string(),
            local_path: path.to_path_buf(),
            size: 10,
            mtime_ms: 5,
            sha256: "ff".to_string(),
            bucket: "b".to_string(),
            key: "k".to_string(),
            upload_id: upload_id.map(str::to_string),
            part_size: 8 << 20,
            started_at: at(0),
            attempts: 0,
            next_attempt_at: None,
        }
    }

    fn on_disk(path: &Path) -> serde_json::Value {
        serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
    }

    fn sessions_on_disk(path: &Path) -> usize {
        on_disk(path)["sessions"].as_array().unwrap().len()
    }

    #[test]
    fn missing_file_gives_empty_ledger() {
        let dir = tempfile::tempdir().unwrap();
        let store = CaptureStore::load(dir.path().join(LEDGER_FILE), at(0));
        store.with(|l| {
            assert_eq!(l.version, 1);
            assert!(l.sessions.is_empty() && l.uploaded.is_empty() && l.inflight.is_empty());
        });
        assert!(!store.persist_failed());
    }

    #[test]
    fn round_trip_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(LEDGER_FILE);
        let trace = dir.path().join("a.utrace");
        fs::write(&trace, b"data").unwrap();

        let store = CaptureStore::load(path.clone(), at(0));
        store.add_session(session("s1", dir.path()), at(0));
        store.upsert_inflight(inflight(&trace, Some("up-1")), at(1));
        store.record_uploaded("deadbeef".to_string(), entry(&trace, at(2)), at(2));
        store.upsert_inflight(inflight(&dir.path().join("b.log"), None), at(3));
        store.flush(at(10));
        let before = serde_json::to_value(store.snapshot()).unwrap();
        drop(store);

        let reloaded = CaptureStore::load(path.clone(), at(20));
        let after = serde_json::to_value(reloaded.snapshot()).unwrap();
        assert_eq!(before, after);
        reloaded.with(|l| {
            assert_eq!(l.sessions.len(), 1);
            assert!(l.uploaded.contains_key("deadbeef"));
            assert_eq!(l.inflight.len(), 1);
        });
        assert!(!dir.path().join("client-capture.json.tmp").exists());
    }

    #[test]
    fn corrupt_file_is_moved_aside() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(LEDGER_FILE);
        fs::write(&path, b"{ not json").unwrap();

        let now = at(5);
        let store = CaptureStore::load(path.clone(), now);
        store.with(|l| assert!(l.sessions.is_empty()));
        assert!(!path.exists());
        let aside = dir.path().join(format!(
            "client-capture.json.corrupt-{}",
            now.timestamp_millis()
        ));
        assert_eq!(fs::read(aside).unwrap(), b"{ not json");
    }

    #[test]
    fn future_version_is_moved_aside() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(LEDGER_FILE);
        fs::write(&path, br#"{"version":2,"sessions":[]}"#).unwrap();

        let now = at(7);
        let store = CaptureStore::load(path.clone(), now);
        store.with(|l| assert_eq!(l.version, 1));
        assert!(!path.exists());
        assert!(dir
            .path()
            .join(format!(
                "client-capture.json.corrupt-{}",
                now.timestamp_millis()
            ))
            .exists());
    }

    #[test]
    fn tmp_file_never_survives_a_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(LEDGER_FILE);
        let store = CaptureStore::load(path.clone(), at(0));
        store.add_session(session("s1", dir.path()), at(0));
        store.add_session(session("s2", dir.path()), at(1));

        assert!(path.exists());
        assert!(!dir.path().join("client-capture.json.tmp").exists());
        assert_eq!(sessions_on_disk(&path), 2);
    }

    #[test]
    fn coalesces_non_immediate_writes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(LEDGER_FILE);
        let store = CaptureStore::load(path.clone(), at(0));
        store.add_session(session("s1", dir.path()), at(0));
        assert_eq!(on_disk(&path)["sessions"][0]["state"], "launched");

        let running = |s: &mut CaptureSession| s.state = SessionState::Running;
        assert!(store.update_session("s1", at(1), running));
        assert_eq!(on_disk(&path)["sessions"][0]["state"], "launched");
        assert!(!store.flush_if_due(at(1)));
        assert_eq!(on_disk(&path)["sessions"][0]["state"], "launched");

        assert!(store.flush_if_due(at(2)));
        assert_eq!(on_disk(&path)["sessions"][0]["state"], "running");
        assert!(!store.flush_if_due(at(10)));
    }

    #[test]
    fn immediate_triggers_bypass_coalescing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(LEDGER_FILE);
        let trace = dir.path().join("a.utrace");
        fs::write(&trace, b"data").unwrap();
        let store = CaptureStore::load(path.clone(), at(0));
        store.add_session(session("s1", dir.path()), at(0));

        store.upsert_inflight(inflight(&trace, None), at(0));
        assert_eq!(on_disk(&path)["inflight"].as_array().unwrap().len(), 0);

        store.upsert_inflight(inflight(&trace, Some("up")), at(0));
        assert_eq!(on_disk(&path)["inflight"][0]["upload_id"], "up");

        store.update_session("s1", at(0), |s| {
            s.state = SessionState::Closed;
            s.closed_at = Some(at(0));
            s.close_reason = Some(CloseReason::Cancelled);
        });
        assert_eq!(on_disk(&path)["sessions"][0]["state"], "closed");

        store.record_uploaded("aa".to_string(), entry(&trace, at(0)), at(0));
        let json = on_disk(&path);
        assert!(json["uploaded"]["aa"].is_object());
        assert_eq!(json["inflight"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn persist_failure_sets_flag_and_retries_on_tick() {
        let dir = tempfile::tempdir().unwrap();
        let blocker = dir.path().join("blocker");
        fs::write(&blocker, b"i am a file").unwrap();
        let path = blocker.join(LEDGER_FILE);

        let store = CaptureStore::load(path.clone(), at(0));
        store.add_session(session("s1", dir.path()), at(0));
        assert!(store.persist_failed());
        store.with(|l| assert_eq!(l.sessions.len(), 1));

        assert!(!store.flush_if_due(at(2)));
        assert!(store.persist_failed());

        fs::remove_file(&blocker).unwrap();
        fs::create_dir(&blocker).unwrap();
        assert!(store.flush_if_due(at(4)));
        assert!(!store.persist_failed());
        assert_eq!(sessions_on_disk(&path), 1);
    }

    #[test]
    fn remove_inflight_returns_row() {
        let dir = tempfile::tempdir().unwrap();
        let store = CaptureStore::load(dir.path().join(LEDGER_FILE), at(0));
        let p = dir.path().join("a.log");
        store.upsert_inflight(inflight(&p, None), at(0));
        assert!(store.remove_inflight(&p, at(0)).is_some());
        assert!(store.remove_inflight(&p, at(0)).is_none());
        store.with(|l| assert!(l.inflight.is_empty()));
    }

    #[test]
    fn prunes_by_age() {
        let dir = tempfile::tempdir().unwrap();
        let mut ledger = CaptureLedger::empty();
        let day = |d: i64| at(d * 86_400);
        let now = day(40);

        ledger
            .sessions
            .push(closed_session("old", dir.path(), day(5)));
        ledger
            .sessions
            .push(closed_session("recent", dir.path(), day(30)));
        ledger.sessions.push(session("open", dir.path()));
        ledger
            .uploaded
            .insert("old".into(), entry(&dir.path().join("gone1"), day(5)));
        ledger
            .uploaded
            .insert("new".into(), entry(&dir.path().join("gone2"), day(30)));

        let removed = ledger.prune(now, 30);
        assert_eq!(removed, 2);
        let ids: Vec<_> = ledger.sessions.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["recent", "open"]);
        assert!(ledger.uploaded.contains_key("new") && !ledger.uploaded.contains_key("old"));
    }

    #[test]
    fn closed_session_with_missing_install_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("install-gone");
        let mut ledger = CaptureLedger::empty();
        ledger.sessions.push(closed_session("c", &missing, at(0)));
        ledger
            .sessions
            .push(closed_session("kept", dir.path(), at(0)));
        let mut open = session("open", &missing);
        open.state = SessionState::Exited;
        ledger.sessions.push(open);

        assert_eq!(ledger.prune(at(60), 30), 1);
        let ids: Vec<_> = ledger.sessions.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["kept", "open"]);
    }

    #[test]
    fn cap_drops_oldest_first() {
        let dir = tempfile::tempdir().unwrap();
        let mut ledger = CaptureLedger::empty();
        let total = UPLOADED_CAP + 5;
        for i in 0..total {
            ledger.uploaded.insert(
                format!("{i:05}"),
                entry(&dir.path().join(format!("gone{i}")), at(i as i64)),
            );
        }

        let now = at(total as i64);
        assert_eq!(ledger.prune(now, 30), 5);
        assert_eq!(ledger.uploaded.len(), UPLOADED_CAP);
        for i in 0..5 {
            assert!(!ledger.uploaded.contains_key(&format!("{i:05}")));
        }
        assert!(ledger.uploaded.contains_key("00005"));
    }

    #[test]
    fn pinned_entries_survive_age_and_cap() {
        let dir = tempfile::tempdir().unwrap();
        let live = dir.path().join("live.utrace");
        fs::write(&live, b"trace").unwrap();
        let changed = dir.path().join("changed.utrace");
        fs::write(&changed, b"trace").unwrap();

        let mut ledger = CaptureLedger::empty();
        ledger.uploaded.insert("live".into(), entry(&live, at(0)));
        let mut stale = entry(&changed, at(0));
        stale.size += 1;
        ledger.uploaded.insert("changed".into(), stale);
        assert_eq!(ledger.prune(at(100 * 86_400), 30), 1);
        assert!(ledger.uploaded.contains_key("live"));
        assert!(!ledger.uploaded.contains_key("changed"));

        for i in 0..UPLOADED_CAP + 3 {
            ledger.uploaded.insert(
                format!("x{i:05}"),
                entry(
                    &dir.path().join(format!("gone{i}")),
                    at(100 * 86_400 + i as i64),
                ),
            );
        }
        let removed = ledger.prune(at(100 * 86_400 + 5_000), 30);
        assert_eq!(removed, 4);
        assert!(ledger.uploaded.contains_key("live"));
        assert_eq!(ledger.uploaded.len(), UPLOADED_CAP);
    }

    #[test]
    fn pinned_entry_at_cap_boundary_is_skipped_not_counted() {
        let dir = tempfile::tempdir().unwrap();
        let live = dir.path().join("live.utrace");
        fs::write(&live, b"trace").unwrap();

        let mut ledger = CaptureLedger::empty();
        ledger.uploaded.insert("a-live".into(), entry(&live, at(0)));
        for i in 0..UPLOADED_CAP {
            ledger.uploaded.insert(
                format!("x{i:05}"),
                entry(&dir.path().join(format!("gone{i}")), at(1 + i as i64)),
            );
        }

        assert_eq!(ledger.prune(at(5_000), 30), 1);
        assert!(ledger.uploaded.contains_key("a-live"));
        assert!(!ledger.uploaded.contains_key("x00000"));
        assert_eq!(ledger.uploaded.len(), UPLOADED_CAP);
    }

    #[test]
    fn store_prune_persists() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(LEDGER_FILE);
        let store = CaptureStore::load(path.clone(), at(0));
        store.add_session(closed_session("old", dir.path(), at(0)), at(0));
        assert_eq!(store.prune(at(40 * 86_400), 30), 1);
        assert_eq!(sessions_on_disk(&path), 0);
    }

    #[test]
    fn stat_matches_compares_ms_precision() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("f.log");
        fs::write(&file, b"abc").unwrap();
        let meta = fs::symlink_metadata(&file).unwrap();
        let mtime = mtime_ms(&meta).unwrap();
        assert!(stat_matches(&file, 3, mtime));
        assert!(!stat_matches(&file, 4, mtime));
        assert!(!stat_matches(&file, 3, mtime + 1));
        assert!(!stat_matches(dir.path(), 3, mtime));
        assert!(!stat_matches(&dir.path().join("none"), 3, mtime));
    }
}
