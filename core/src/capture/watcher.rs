//! Game process watcher and the pure session state transitions.
//!
//! Self-contained on purpose: no `AppState`, ledger or S3, so it can move to a launcher.
//!
//! `Exited` -> `Closed(Uploaded)` is not decided here. It needs the ledger and file
//! selection, so the service (stage 016) decides it using [`StepContext::quiescence_elapsed`]
//! plus its own "nothing left to upload" check.

use std::collections::HashSet;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};
use tracing::warn;

use super::types::{CaptureSession, CloseReason, SessionState};
use crate::utils::process::processes_running_from;

pub const POLL: Duration = Duration::from_secs(5);
pub const LAUNCH_GRACE: TimeDelta = TimeDelta::seconds(120);
pub const EXIT_DEBOUNCE: u8 = 2;
pub const SESSION_MAX_AGE: TimeDelta = TimeDelta::days(7);

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PollResult {
    ProcessSeen,
    Empty,
}

impl PollResult {
    fn seen(self) -> bool {
        self == PollResult::ProcessSeen
    }
}

#[derive(Copy, Clone, Debug)]
pub struct StepContext {
    pub first_poll_after_start: bool,
    pub quiescence: TimeDelta,
    pub has_uploading_file: bool,
}

impl StepContext {
    pub fn quiescence_elapsed(&self, session: &CaptureSession, now: DateTime<Utc>) -> bool {
        session.state == SessionState::Exited
            && session
                .exited_at
                .is_some_and(|exited_at| now >= exited_at + self.quiescence)
    }
}

/// The session fields a poll may change, as new values.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Transition {
    pub state: SessionState,
    pub exited_at: Option<DateTime<Utc>>,
    pub close_reason: Option<CloseReason>,
    pub empty_polls: u8,
    pub first_empty_at: Option<DateTime<Utc>>,
}

impl Transition {
    fn unchanged(session: &CaptureSession) -> Self {
        Self {
            state: session.state,
            exited_at: session.exited_at,
            close_reason: session.close_reason,
            empty_polls: session.empty_polls,
            first_empty_at: session.first_empty_at,
        }
    }

    fn exit(&mut self, session: &CaptureSession, at: DateTime<Utc>) {
        self.state = SessionState::Exited;
        self.exited_at = Some(at.max(session.launched_at));
        self.empty_polls = 0;
        self.first_empty_at = None;
    }

    /// Returns whether a persisted field changed.
    pub fn apply(&self, session: &mut CaptureSession, now: DateTime<Utc>) -> bool {
        let persisted_changed = session.state != self.state
            || session.exited_at != self.exited_at
            || session.close_reason != self.close_reason;
        if self.state == SessionState::Closed && session.state != SessionState::Closed {
            session.closed_at = Some(now);
        }
        session.state = self.state;
        session.exited_at = self.exited_at;
        session.close_reason = self.close_reason;
        session.empty_polls = self.empty_polls;
        session.first_empty_at = self.first_empty_at;
        persisted_changed
    }
}

pub fn step(
    session: &CaptureSession,
    poll: PollResult,
    now: DateTime<Utc>,
    ctx: &StepContext,
) -> Transition {
    let mut next = Transition::unchanged(session);
    if session.state == SessionState::Closed {
        return next;
    }

    if now - session.launched_at > SESSION_MAX_AGE && !ctx.has_uploading_file {
        next.state = SessionState::Closed;
        next.close_reason = Some(CloseReason::Expired);
        return next;
    }

    match session.state {
        SessionState::Launched => {
            if poll.seen() {
                next.state = SessionState::Running;
                next.empty_polls = 0;
                next.first_empty_at = None;
            } else if ctx.first_poll_after_start || now - session.launched_at > LAUNCH_GRACE {
                next.exit(session, now);
            }
        }
        SessionState::Running => {
            if poll.seen() {
                next.empty_polls = 0;
                next.first_empty_at = None;
            } else if ctx.first_poll_after_start {
                next.exit(session, now);
            } else {
                let first_empty_at = session.first_empty_at.unwrap_or(now);
                next.empty_polls = session.empty_polls.saturating_add(1);
                next.first_empty_at = Some(first_empty_at);
                if next.empty_polls >= EXIT_DEBOUNCE {
                    next.exit(session, first_empty_at);
                }
            }
        }
        SessionState::Exited | SessionState::Closed => {}
    }
    next
}

pub trait ProcessScanner: Send + Sync + 'static {
    fn running_from(&self, root: &Path) -> bool;
}

pub struct SysinfoScanner;

impl ProcessScanner for SysinfoScanner {
    fn running_from(&self, root: &Path) -> bool {
        !processes_running_from(root).is_empty()
    }
}

pub trait Clock: Send + Sync + 'static {
    fn now(&self) -> DateTime<Utc>;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// Roots that had a process on the last scan.
#[derive(Clone, Debug, Default)]
pub struct ScanSnapshot {
    running: HashSet<PathBuf>,
}

impl ScanSnapshot {
    pub fn any_running(&self) -> bool {
        !self.running.is_empty()
    }

    pub fn poll_for(&self, session: &CaptureSession) -> PollResult {
        if self.running.contains(&session.install_dir) {
            PollResult::ProcessSeen
        } else {
            PollResult::Empty
        }
    }
}

/// Distinct `install_dir` of every non-`Closed` session, plus `extra_root`.
pub fn scan_roots(sessions: &[CaptureSession], extra_root: Option<&Path>) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    let mut roots = Vec::new();
    let candidates = sessions
        .iter()
        .filter(|s| s.state != SessionState::Closed)
        .map(|s| s.install_dir.as_path())
        .chain(extra_root);
    for root in candidates {
        if seen.insert(root.to_path_buf()) {
            roots.push(root.to_path_buf());
        }
    }
    roots
}

/// A session still in `Launched` is inside its launch grace; past it, `step` exits it.
pub fn game_running(
    snapshot: &ScanSnapshot,
    sessions: &[CaptureSession],
    now: DateTime<Utc>,
) -> bool {
    snapshot.any_running()
        || sessions
            .iter()
            .any(|s| s.state == SessionState::Launched && now - s.launched_at < LAUNCH_GRACE)
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct PollOutcome {
    pub game_running: bool,
    /// A persisted session field changed; the caller should write the ledger.
    pub changed: bool,
}

pub struct GameProcessWatcher<S = SysinfoScanner, C = SystemClock> {
    scanner: Arc<S>,
    clock: C,
}

impl GameProcessWatcher {
    pub fn system() -> Self {
        Self::new(SysinfoScanner, SystemClock)
    }
}

impl<S: ProcessScanner, C: Clock> GameProcessWatcher<S, C> {
    pub fn new(scanner: S, clock: C) -> Self {
        Self {
            scanner: Arc::new(scanner),
            clock,
        }
    }

    /// One `running_from` call per root: the underlying scan dedups by process name.
    pub async fn scan(&self, roots: Vec<PathBuf>) -> ScanSnapshot {
        let scanner = self.scanner.clone();
        let result = tokio::task::spawn_blocking(move || {
            roots
                .into_iter()
                .filter(|root| scanner.running_from(root))
                .collect::<HashSet<_>>()
        })
        .await;
        match result {
            Ok(running) => ScanSnapshot { running },
            Err(e) => {
                warn!("capture process scan failed: {e}");
                ScanSnapshot::default()
            }
        }
    }

    pub async fn game_running(
        &self,
        sessions: &[CaptureSession],
        extra_root: Option<&Path>,
    ) -> bool {
        let snapshot = self.scan(scan_roots(sessions, extra_root)).await;
        game_running(&snapshot, sessions, self.clock.now())
    }

    pub async fn poll(
        &self,
        sessions: &mut [CaptureSession],
        extra_root: Option<&Path>,
        first_poll_after_start: bool,
        quiescence: TimeDelta,
        has_uploading_file: impl Fn(&CaptureSession) -> bool,
    ) -> PollOutcome {
        let snapshot = self.scan(scan_roots(sessions, extra_root)).await;
        let now = self.clock.now();
        let mut changed = false;
        for session in sessions.iter_mut() {
            if session.state == SessionState::Closed {
                continue;
            }
            let ctx = StepContext {
                first_poll_after_start,
                quiescence,
                has_uploading_file: has_uploading_file(session),
            };
            let transition = step(session, snapshot.poll_for(session), now, &ctx);
            changed |= transition.apply(session, now);
        }
        PollOutcome {
            game_running: game_running(&snapshot, sessions, now),
            changed,
        }
    }
}

/// Exit detection for one launch, with the same grace and debounce as the sessions.
#[derive(Clone, Debug)]
struct ExitWaiter {
    started_at: DateTime<Utc>,
    seen: bool,
    empty_polls: u8,
}

impl ExitWaiter {
    fn new(started_at: DateTime<Utc>) -> Self {
        Self {
            started_at,
            seen: false,
            empty_polls: 0,
        }
    }

    fn observe(&mut self, process_seen: bool, now: DateTime<Utc>) -> bool {
        if process_seen {
            self.seen = true;
            self.empty_polls = 0;
            return false;
        }
        if !self.seen {
            return now - self.started_at > LAUNCH_GRACE;
        }
        self.empty_polls = self.empty_polls.saturating_add(1);
        self.empty_polls >= EXIT_DEBOUNCE
    }
}

pub async fn wait_for_exit_with<S, C, Sleep, Fut>(
    watcher: &GameProcessWatcher<S, C>,
    install_dir: PathBuf,
    mut sleep: Sleep,
) where
    S: ProcessScanner,
    C: Clock,
    Sleep: FnMut(Duration) -> Fut,
    Fut: Future<Output = ()>,
{
    let mut waiter = ExitWaiter::new(watcher.clock.now());
    loop {
        let snapshot = watcher.scan(vec![install_dir.clone()]).await;
        if waiter.observe(snapshot.any_running(), watcher.clock.now()) {
            return;
        }
        sleep(POLL).await;
    }
}

/// Returns once no game process runs from `install_dir`, or none appeared within the launch grace.
pub async fn wait_for_exit(install_dir: PathBuf) {
    wait_for_exit_with(
        &GameProcessWatcher::system(),
        install_dir,
        tokio::time::sleep,
    )
    .await;
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use PollResult::{Empty, ProcessSeen};

    use super::*;

    fn t(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000 + secs, 0).unwrap()
    }

    fn session(state: SessionState) -> CaptureSession {
        CaptureSession {
            id: "s".to_string(),
            install_dir: PathBuf::from("install"),
            sha: None,
            user: "u".to_string(),
            playtest: "pt".to_string(),
            launched_at: t(0),
            state,
            exited_at: None,
            closed_at: None,
            close_reason: None,
            empty_polls: 0,
            first_empty_at: None,
        }
    }

    fn ctx() -> StepContext {
        StepContext {
            first_poll_after_start: false,
            quiescence: TimeDelta::seconds(30),
            has_uploading_file: false,
        }
    }

    fn first_poll_ctx() -> StepContext {
        StepContext {
            first_poll_after_start: true,
            ..ctx()
        }
    }

    fn run(session: &mut CaptureSession, polls: &[(i64, PollResult)]) -> Vec<Transition> {
        polls
            .iter()
            .map(|&(secs, poll)| {
                let tr = step(session, poll, t(secs), &ctx());
                tr.apply(session, t(secs));
                tr
            })
            .collect()
    }

    #[test]
    fn launched_with_process_becomes_running() {
        let mut s = session(SessionState::Launched);
        run(&mut s, &[(5, ProcessSeen)]);
        assert_eq!(s.state, SessionState::Running);
        assert_eq!(s.empty_polls, 0);
    }

    #[test]
    fn launch_grace_holds_until_exceeded() {
        let mut s = session(SessionState::Launched);
        run(&mut s, &[(5, Empty), (120, Empty)]);
        assert_eq!(s.state, SessionState::Launched);
        assert_eq!(s.exited_at, None);

        run(&mut s, &[(125, Empty)]);
        assert_eq!(s.state, SessionState::Exited);
        assert_eq!(s.exited_at, Some(t(125)));
    }

    #[test]
    fn single_empty_poll_does_not_exit() {
        let mut s = session(SessionState::Running);
        run(
            &mut s,
            &[
                (5, Empty),
                (10, ProcessSeen),
                (15, Empty),
                (20, ProcessSeen),
            ],
        );
        assert_eq!(s.state, SessionState::Running);
        assert_eq!(s.empty_polls, 0);
        assert_eq!(s.first_empty_at, None);
    }

    #[test]
    fn exit_uses_first_empty_poll_time() {
        let mut s = session(SessionState::Running);
        let trs = run(&mut s, &[(100, Empty), (105, Empty)]);
        assert_eq!(trs[0].state, SessionState::Running);
        assert_eq!(trs[0].empty_polls, 1);
        assert_eq!(s.state, SessionState::Exited);
        assert_eq!(s.exited_at, Some(t(100)));
    }

    #[test]
    fn counter_restarts_after_non_empty_poll() {
        let mut s = session(SessionState::Running);
        run(
            &mut s,
            &[(100, Empty), (105, ProcessSeen), (110, Empty), (115, Empty)],
        );
        assert_eq!(s.state, SessionState::Exited);
        assert_eq!(s.exited_at, Some(t(110)));
    }

    #[test]
    fn exited_at_is_clamped_to_launched_at() {
        let mut s = session(SessionState::Running);
        run(&mut s, &[(-50, Empty), (-45, Empty)]);
        assert_eq!(s.state, SessionState::Exited);
        assert_eq!(s.exited_at, Some(t(0)));
    }

    #[test]
    fn clock_backwards_does_not_exit_launched_or_expire() {
        let s = session(SessionState::Launched);
        let tr = step(&s, Empty, t(-1000), &ctx());
        assert_eq!(tr.state, SessionState::Launched);
    }

    #[test]
    fn restart_rule_applies_to_first_poll_only() {
        for state in [SessionState::Running, SessionState::Launched] {
            let mut s = session(state);
            step(&s, Empty, t(50), &first_poll_ctx()).apply(&mut s, t(50));
            assert_eq!(s.state, SessionState::Exited, "{state:?}");
            assert_eq!(s.exited_at, Some(t(50)));
        }

        let mut s = session(SessionState::Running);
        run(&mut s, &[(50, Empty)]);
        assert_eq!(s.state, SessionState::Running);
    }

    #[test]
    fn restart_rule_with_process_resumes_normally() {
        for state in [SessionState::Running, SessionState::Launched] {
            let mut s = session(state);
            step(&s, ProcessSeen, t(50), &first_poll_ctx()).apply(&mut s, t(50));
            assert_eq!(s.state, SessionState::Running, "{state:?}");
        }
    }

    #[test]
    fn expiry_only_without_uploading_file() {
        let late = t(0) + SESSION_MAX_AGE + TimeDelta::seconds(1);
        for state in [
            SessionState::Launched,
            SessionState::Running,
            SessionState::Exited,
        ] {
            let mut s = session(state);
            let blocked = StepContext {
                has_uploading_file: true,
                ..ctx()
            };
            let tr = step(&s, ProcessSeen, late, &blocked);
            assert_ne!(tr.state, SessionState::Closed, "{state:?}");

            let tr = step(&s, ProcessSeen, late, &ctx());
            assert_eq!(tr.state, SessionState::Closed, "{state:?}");
            assert_eq!(tr.close_reason, Some(CloseReason::Expired));
            assert!(tr.apply(&mut s, late));
            assert_eq!(s.closed_at, Some(late));
        }
    }

    #[test]
    fn exactly_max_age_does_not_expire() {
        let s = session(SessionState::Running);
        let tr = step(&s, ProcessSeen, t(0) + SESSION_MAX_AGE, &ctx());
        assert_eq!(tr.state, SessionState::Running);
    }

    #[test]
    fn closed_sessions_are_never_transitioned() {
        let mut s = session(SessionState::Closed);
        s.close_reason = Some(CloseReason::Uploaded);
        let late = t(0) + SESSION_MAX_AGE * 2;
        for poll in [ProcessSeen, Empty] {
            for c in [ctx(), first_poll_ctx()] {
                let tr = step(&s, poll, late, &c);
                assert_eq!(tr, Transition::unchanged(&s));
            }
        }
    }

    #[test]
    fn exited_is_left_for_the_service_to_close() {
        let mut s = session(SessionState::Exited);
        s.exited_at = Some(t(10));
        for poll in [Empty, ProcessSeen] {
            let tr = step(&s, poll, t(1000), &ctx());
            assert_eq!(tr, Transition::unchanged(&s));
        }
    }

    #[test]
    fn quiescence_elapsed_is_inclusive_and_needs_exited() {
        let mut s = session(SessionState::Exited);
        s.exited_at = Some(t(100));
        let c = ctx();
        assert!(!c.quiescence_elapsed(&s, t(129)));
        assert!(c.quiescence_elapsed(&s, t(130)));
        s.state = SessionState::Running;
        assert!(!c.quiescence_elapsed(&s, t(500)));
    }

    #[test]
    fn apply_reports_only_persisted_changes() {
        let mut s = session(SessionState::Running);
        let tr = step(&s, Empty, t(5), &ctx());
        assert!(!tr.apply(&mut s, t(5)));
        assert_eq!(s.empty_polls, 1);
        let tr = step(&s, Empty, t(10), &ctx());
        assert!(tr.apply(&mut s, t(10)));
    }

    #[derive(Clone, Default)]
    struct FakeScanner {
        running: Arc<Mutex<HashSet<PathBuf>>>,
        calls: Arc<Mutex<Vec<PathBuf>>>,
    }

    impl FakeScanner {
        fn set(&self, roots: &[&str]) {
            *self.running.lock().unwrap() = roots.iter().map(PathBuf::from).collect();
        }
    }

    impl ProcessScanner for FakeScanner {
        fn running_from(&self, root: &Path) -> bool {
            self.calls.lock().unwrap().push(root.to_path_buf());
            self.running.lock().unwrap().contains(root)
        }
    }

    #[derive(Clone)]
    struct FakeClock(Arc<Mutex<DateTime<Utc>>>);

    impl FakeClock {
        fn new(at: DateTime<Utc>) -> Self {
            Self(Arc::new(Mutex::new(at)))
        }

        fn advance(&self, by: TimeDelta) {
            *self.0.lock().unwrap() += by;
        }

        fn set(&self, at: DateTime<Utc>) {
            *self.0.lock().unwrap() = at;
        }
    }

    impl Clock for FakeClock {
        fn now(&self) -> DateTime<Utc> {
            *self.0.lock().unwrap()
        }
    }

    fn session_at(dir: &str, state: SessionState, launched: i64) -> CaptureSession {
        let mut s = session(state);
        s.install_dir = PathBuf::from(dir);
        s.launched_at = t(launched);
        s
    }

    fn watcher(clock: &FakeClock) -> (GameProcessWatcher<FakeScanner, FakeClock>, FakeScanner) {
        let scanner = FakeScanner::default();
        (
            GameProcessWatcher::new(scanner.clone(), clock.clone()),
            scanner,
        )
    }

    #[test]
    fn scan_roots_are_distinct_non_closed_plus_extra() {
        let sessions = vec![
            session_at("a", SessionState::Running, 0),
            session_at("a", SessionState::Launched, 1),
            session_at("b", SessionState::Closed, 2),
            session_at("c", SessionState::Exited, 3),
        ];
        let roots = scan_roots(&sessions, Some(Path::new("data")));
        assert_eq!(
            roots,
            vec![
                PathBuf::from("a"),
                PathBuf::from("c"),
                PathBuf::from("data")
            ]
        );
        assert_eq!(
            scan_roots(&sessions, Some(Path::new("a"))),
            vec![PathBuf::from("a"), PathBuf::from("c")]
        );
    }

    #[tokio::test]
    async fn game_running_true_inside_launch_grace_without_process() {
        let clock = FakeClock::new(t(60));
        let (w, _) = watcher(&clock);
        let sessions = vec![session_at("a", SessionState::Launched, 0)];
        assert!(w.game_running(&sessions, None).await);
    }

    #[tokio::test]
    async fn game_running_false_after_grace_without_process() {
        let clock = FakeClock::new(t(120));
        let (w, _) = watcher(&clock);
        let sessions = vec![session_at("a", SessionState::Launched, 0)];
        assert!(!w.game_running(&sessions, None).await);
        clock.advance(TimeDelta::seconds(30));
        assert!(!w.game_running(&sessions, None).await);
    }

    #[tokio::test]
    async fn game_running_false_for_running_session_with_no_process() {
        let clock = FakeClock::new(t(10));
        let (w, _) = watcher(&clock);
        let sessions = vec![session_at("a", SessionState::Running, 0)];
        assert!(!w.game_running(&sessions, None).await);
    }

    #[tokio::test]
    async fn game_running_true_when_any_root_has_a_process() {
        let clock = FakeClock::new(t(1000));
        let (w, scanner) = watcher(&clock);
        let sessions = vec![session_at("a", SessionState::Exited, 0)];
        assert!(!w.game_running(&sessions, Some(Path::new("data"))).await);

        scanner.set(&["data"]);
        assert!(w.game_running(&sessions, Some(Path::new("data"))).await);
        assert!(!w.game_running(&sessions, None).await);
    }

    #[tokio::test]
    async fn scan_calls_the_scanner_once_per_root() {
        let clock = FakeClock::new(t(0));
        let (w, scanner) = watcher(&clock);
        let sessions = vec![
            session_at("a", SessionState::Running, 0),
            session_at("a", SessionState::Launched, 1),
            session_at("b", SessionState::Running, 2),
        ];
        w.game_running(&sessions, Some(Path::new("data"))).await;
        let mut calls = scanner.calls.lock().unwrap().clone();
        calls.sort();
        assert_eq!(
            calls,
            vec![
                PathBuf::from("a"),
                PathBuf::from("b"),
                PathBuf::from("data")
            ]
        );
    }

    #[tokio::test]
    async fn poll_walks_a_session_through_its_lifecycle() {
        let clock = FakeClock::new(t(5));
        let (w, scanner) = watcher(&clock);
        let mut sessions = vec![session_at("a", SessionState::Launched, 0)];
        let q = TimeDelta::seconds(30);

        scanner.set(&["a"]);
        let out = w.poll(&mut sessions, None, false, q, |_| false).await;
        assert_eq!(sessions[0].state, SessionState::Running);
        assert!(out.changed && out.game_running);

        scanner.set(&[]);
        clock.set(t(100));
        let out = w.poll(&mut sessions, None, false, q, |_| false).await;
        assert_eq!(sessions[0].state, SessionState::Running);
        assert!(!out.changed && !out.game_running);

        clock.set(t(105));
        let out = w.poll(&mut sessions, None, false, q, |_| false).await;
        assert_eq!(sessions[0].state, SessionState::Exited);
        assert_eq!(sessions[0].exited_at, Some(t(100)));
        assert!(out.changed && !out.game_running);
    }

    #[tokio::test]
    async fn poll_skips_closed_sessions_and_keeps_installs_independent() {
        let clock = FakeClock::new(t(10));
        let (w, scanner) = watcher(&clock);
        scanner.set(&["a"]);
        let mut closed = session_at("c", SessionState::Closed, 0);
        closed.close_reason = Some(CloseReason::Cancelled);
        let mut sessions = vec![
            session_at("a", SessionState::Launched, 0),
            session_at("b", SessionState::Running, 0),
            closed,
        ];
        w.poll(&mut sessions, None, true, TimeDelta::seconds(30), |_| false)
            .await;
        assert_eq!(sessions[0].state, SessionState::Running);
        assert_eq!(sessions[1].state, SessionState::Exited);
        assert_eq!(sessions[2].state, SessionState::Closed);
        assert!(sessions[2].exited_at.is_none());
    }

    #[test]
    fn exit_waiter_gives_up_after_grace_when_nothing_appears() {
        let mut waiter = ExitWaiter::new(t(0));
        assert!(!waiter.observe(false, t(5)));
        assert!(!waiter.observe(false, t(120)));
        assert!(waiter.observe(false, t(121)));
    }

    #[test]
    fn exit_waiter_debounces_after_a_process_was_seen() {
        let mut waiter = ExitWaiter::new(t(0));
        assert!(!waiter.observe(true, t(5)));
        assert!(!waiter.observe(false, t(500)));
        assert!(!waiter.observe(true, t(505)));
        assert!(!waiter.observe(false, t(510)));
        assert!(waiter.observe(false, t(515)));
    }

    #[tokio::test]
    async fn wait_for_exit_returns_after_debounced_exit() {
        let clock = FakeClock::new(t(0));
        let (w, scanner) = watcher(&clock);
        scanner.set(&["install"]);
        let sleeps = Arc::new(Mutex::new(0u32));

        let (clock2, scanner2, sleeps2) = (clock.clone(), scanner.clone(), sleeps.clone());
        wait_for_exit_with(&w, PathBuf::from("install"), move |d| {
            let n = {
                let mut n = sleeps2.lock().unwrap();
                *n += 1;
                *n
            };
            clock2.advance(TimeDelta::from_std(d).unwrap());
            if n == 3 {
                scanner2.set(&[]);
            }
            async {}
        })
        .await;

        // seen x3, empty x2: the fourth poll is the first empty one, the fifth confirms.
        assert_eq!(*sleeps.lock().unwrap(), 4);
    }

    #[tokio::test]
    async fn wait_for_exit_gives_up_when_nothing_appears() {
        let clock = FakeClock::new(t(0));
        let (w, _) = watcher(&clock);
        let clock2 = clock.clone();
        wait_for_exit_with(&w, PathBuf::from("install"), move |d| {
            clock2.advance(TimeDelta::from_std(d).unwrap());
            async {}
        })
        .await;
        let waited = clock.now() - t(0);
        assert!(waited > LAUNCH_GRACE);
        assert!(waited <= LAUNCH_GRACE + TimeDelta::seconds(5));
    }
}
