use std::path::Path;

use anyhow::anyhow;
use axum::async_trait;
use tracing::error;

use super::rebase_state::{
    derive_rebase_state, json_path, rebase_dir_exists, try_resolve_git_dir, REBASE_OP_LOCK,
};
use crate::engine::EngineProvider;
use crate::state::AppState;
use ethos_core::clients::git;
use ethos_core::types::errors::CoreError;
use ethos_core::types::repo::RebaseStateKind;
use ethos_core::worker::Task;

pub(crate) const GUARD_IN_PROGRESS: &str =
    "A rebase is in progress — finish or abort it in Friendshipper's Branch Management page.";
pub(crate) const GUARD_COMPLETED_UNPUSHED: &str = "A rebased branch is waiting to be pushed — push or reset it in Friendshipper's Branch Management page.";
pub(crate) const GUARD_PUSH_FAILED: &str = "Pushing the rebased branch failed — redo the rebase or reset to backup in Friendshipper's Branch Management page.";
pub(crate) const GUARD_UNKNOWN: &str = "Friendshipper could not check for a rebase in progress — try again, or open Friendshipper's Branch Management page.";

/// Whether a rebase record exists (one stat, no git). A failed stat counts as a record, so the
/// read that follows reports the error.
fn has_rebase_record(repo_path: &Path) -> anyhow::Result<bool> {
    let git_dir = try_resolve_git_dir(repo_path)?;
    Ok(!matches!(json_path(&git_dir).try_exists(), Ok(false)))
}

/// The derived guided rebase state, or `None` without a rebase record.
pub(crate) async fn recorded_rebase_kind(
    git: &git::Git,
    repo_path: &Path,
) -> anyhow::Result<Option<RebaseStateKind>> {
    if !has_rebase_record(repo_path)? {
        return Ok(None);
    }
    Ok(Some(derive_rebase_state(git, repo_path).await?.kind))
}

fn unknown_state(e: anyhow::Error) -> CoreError {
    error!("Could not determine the guided rebase state: {e:#}");
    CoreError::Internal(anyhow!(GUARD_UNKNOWN))
}

/// Refuses while a guided rebase is active, or when its state can't be determined. Never waits
/// for a rebase lock.
pub(crate) async fn ensure_no_guided_rebase(
    git: &git::Git,
    repo_path: &Path,
) -> Result<(), CoreError> {
    guard_with_op_lock(git, repo_path, &REBASE_OP_LOCK).await
}

/// [`ensure_no_guided_rebase`] against `op_lock` in place of `REBASE_OP_LOCK`.
pub(super) async fn guard_with_op_lock(
    git: &git::Git,
    repo_path: &Path,
    op_lock: &tokio::sync::Mutex<()>,
) -> Result<(), CoreError> {
    if !has_rebase_record(repo_path).map_err(unknown_state)? {
        return Ok(());
    }
    // A running rebase op may have written the record before git created its own rebase
    // state, which derives as stale in between.
    if op_lock.try_lock().is_err() {
        return Err(CoreError::Input(anyhow!(GUARD_IN_PROGRESS)));
    }
    let derived = derive_rebase_state(git, repo_path)
        .await
        .map_err(unknown_state)?;
    let sentence = match derived.kind {
        RebaseStateKind::Idle | RebaseStateKind::Stale | RebaseStateKind::External => return Ok(()),
        RebaseStateKind::InProgress => GUARD_IN_PROGRESS,
        RebaseStateKind::CompletedUnpushed => GUARD_COMPLETED_UNPUSHED,
        RebaseStateKind::PushFailed => GUARD_PUSH_FAILED,
    };
    Err(CoreError::Input(anyhow!(sentence)))
}

/// `ensure_no_guided_rebase` for the configured repo.
pub(crate) async fn guard_state<T>(state: &AppState<T>) -> Result<(), CoreError>
where
    T: EngineProvider,
{
    let git = state.git();
    ensure_no_guided_rebase(&git, &git.repo_path).await
}

/// Runs the guard again when its sequence reaches the worker, after everything queued before
/// it, such as a rebase start.
#[derive(Clone)]
pub(crate) struct GuidedRebaseGuardOp {
    pub git_client: git::Git,
}

#[async_trait]
impl Task for GuidedRebaseGuardOp {
    async fn execute(&self) -> Result<(), CoreError> {
        ensure_no_guided_rebase(&self.git_client, &self.git_client.repo_path).await
    }

    fn get_name(&self) -> String {
        String::from("GuidedRebaseGuardOp")
    }
}

/// The guard as the first task of a sequence that changes the repo.
pub(crate) fn guard_op<T>(state: &AppState<T>) -> Box<dyn Task + Send + Sync>
where
    T: EngineProvider,
{
    Box::new(GuidedRebaseGuardOp {
        git_client: state.git(),
    })
}

/// Whether startup may delete a leftover `index.lock`: not while git's rebase state exists, as
/// a rebase in a git process that outlived Friendshipper may still hold it.
pub(crate) fn may_remove_stale_index_lock(git_dir: &Path) -> bool {
    !rebase_dir_exists(git_dir)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use super::*;
    use crate::repo::operations::branch::rebase_state::tests::{
        diverge_on_a_txt, guided_stop_on_a_txt, new_git, sample_file_for, start_raw_rebase,
        Fixture, BRANCH, OTHER_BRANCH,
    };
    use crate::repo::operations::branch::rebase_state::{
        write_rebase_file, GuidedRebaseFile, GuidedRebasePhase, REBASE_FILE_LOCK,
    };
    use ethos_core::types::repo::RebaseStaleReason;
    use ethos_core::worker::{RepoWorker, TaskSequence};

    /// The guard against a private op lock, so rebase ops running in other tests can't hold it.
    async fn guard(fx: &Fixture) -> Result<(), CoreError> {
        guard_with_op_lock(&fx.git, fx.path(), &tokio::sync::Mutex::new(())).await
    }

    fn assert_unknown(result: Result<(), CoreError>) {
        assert!(
            matches!(&result, Err(CoreError::Internal(e)) if e.to_string() == GUARD_UNKNOWN),
            "{result:?}"
        );
    }

    fn assert_refused(result: Result<(), CoreError>, sentence: &str) {
        match result {
            Err(CoreError::Input(e)) => assert_eq!(e.to_string(), sentence),
            other => panic!("expected Input error, got {other:?}"),
        }
    }

    async fn assert_state(fx: &Fixture, kind: RebaseStateKind, reason: Option<RebaseStaleReason>) {
        let derived = fx.derive().await;
        assert_eq!((derived.kind, derived.stale_reason), (kind, reason));
    }

    async fn assert_passes_stale(fx: &Fixture, reason: RebaseStaleReason) {
        assert_state(fx, RebaseStateKind::Stale, Some(reason)).await;
        guard(fx).await.expect("stale records are not guarded");
    }

    #[test]
    fn refusals_are_single_sentences() {
        for sentence in [
            GUARD_IN_PROGRESS,
            GUARD_COMPLETED_UNPUSHED,
            GUARD_PUSH_FAILED,
            GUARD_UNKNOWN,
        ] {
            assert!(sentence.ends_with("Branch Management page."), "{sentence}");
            assert_eq!(sentence.matches('.').count(), 1, "{sentence}");
        }
    }

    #[tokio::test]
    async fn idle_passes_without_running_git() {
        let fx = Fixture::new().await;
        assert_state(&fx, RebaseStateKind::Idle, None).await;
        guard(&fx).await.expect("idle");

        let broken = new_git(&fx.path().join("missing"));
        ensure_no_guided_rebase(&broken, fx.path())
            .await
            .expect("no record means no git");
        // The same git fails once a record exists.
        fx.guided_complete().await;
        assert!(derive_rebase_state(&broken, fx.path()).await.is_err());
    }

    #[tokio::test]
    async fn in_progress_is_refused() {
        let fx = Fixture::new().await;
        guided_stop_on_a_txt(&fx).await;
        assert_state(&fx, RebaseStateKind::InProgress, None).await;
        assert_refused(guard(&fx).await, GUARD_IN_PROGRESS);
    }

    #[tokio::test]
    async fn completed_unpushed_is_refused() {
        let fx = Fixture::new().await;
        fx.guided_complete().await;
        assert_state(&fx, RebaseStateKind::CompletedUnpushed, None).await;
        assert_refused(guard(&fx).await, GUARD_COMPLETED_UNPUSHED);
    }

    #[tokio::test]
    async fn push_failed_is_refused() {
        let fx = Fixture::new().await;
        let mut file = fx.guided_complete().await;
        file.phase = GuidedRebasePhase::PushFailed;
        file.push_failed_remote_sha = Some(file.backup_sha.clone());
        fx.write_file_json(&file);
        assert_state(&fx, RebaseStateKind::PushFailed, None).await;
        assert_refused(guard(&fx).await, GUARD_PUSH_FAILED);
    }

    #[tokio::test]
    async fn unreadable_record_passes() {
        let fx = Fixture::new().await;
        fs::write(json_path(&fx.git_dir()), "{").unwrap();
        assert_passes_stale(&fx, RebaseStaleReason::Unreadable).await;
    }

    #[tokio::test]
    async fn other_rebase_passes() {
        let fx = Fixture::new().await;
        guided_stop_on_a_txt(&fx).await;
        fx.write_file_json(&sample_file_for(OTHER_BRANCH));
        assert_passes_stale(&fx, RebaseStaleReason::OtherRebase).await;
    }

    #[tokio::test]
    async fn branch_changed_passes() {
        let fx = Fixture::new().await;
        fx.guided_complete().await;
        fx.checkout("main").await;
        assert_passes_stale(&fx, RebaseStaleReason::BranchChanged).await;
    }

    #[tokio::test]
    async fn aborted_outside_passes() {
        let fx = Fixture::new().await;
        diverge_on_a_txt(&fx).await;
        let file = fx.guided_file("main").await;
        fx.write_file_json(&file);
        assert_passes_stale(&fx, RebaseStaleReason::AbortedOutside).await;
    }

    #[tokio::test]
    async fn pushed_outside_passes() {
        let fx = Fixture::new().await;
        fx.guided_complete().await;
        fx.push(BRANCH).await;
        assert_passes_stale(&fx, RebaseStaleReason::PushedOutside).await;
    }

    #[tokio::test]
    async fn changed_outside_passes() {
        let fx = Fixture::new().await;
        let file = fx.guided_complete().await;
        fx.run(&["reset", "-q", "--hard", &file.merge_base]).await;
        assert_passes_stale(&fx, RebaseStaleReason::Unknown).await;
    }

    #[tokio::test]
    async fn external_rebase_passes() {
        let fx = Fixture::new().await;
        diverge_on_a_txt(&fx).await;
        assert_eq!(fx.start_raw_rebase("main").await, 1);
        assert_state(&fx, RebaseStateKind::External, None).await;
        guard(&fx).await.expect("external");

        fs::write(json_path(&fx.git_dir()), "{").unwrap();
        assert_state(
            &fx,
            RebaseStateKind::External,
            Some(RebaseStaleReason::Unreadable),
        )
        .await;
        guard(&fx)
            .await
            .expect("external with an unreadable record");
    }

    #[tokio::test]
    async fn unknown_state_is_refused() {
        let fx = Fixture::new().await;
        fx.guided_complete().await;
        let broken = new_git(&fx.path().join("missing"));
        let op_lock = tokio::sync::Mutex::new(());
        assert_unknown(guard_with_op_lock(&broken, fx.path(), &op_lock).await);

        // A record that exists but can't be read.
        let json = json_path(&fx.git_dir());
        fs::remove_file(&json).unwrap();
        fs::create_dir(&json).unwrap();
        assert_unknown(guard(&fx).await);
    }

    #[tokio::test]
    async fn broken_gitdir_file_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let git = new_git(&dir.path().join("missing"));
        fs::write(dir.path().join(".git"), "not a gitdir line\n").unwrap();
        assert_unknown(ensure_no_guided_rebase(&git, dir.path()).await);
        assert!(recorded_rebase_kind(&git, dir.path()).await.is_err());

        let git_dir = tempfile::tempdir().unwrap();
        let gitdir_line = format!("gitdir: {}\n", git_dir.path().display());
        fs::write(dir.path().join(".git"), gitdir_line).unwrap();
        ensure_no_guided_rebase(&git, dir.path())
            .await
            .expect("a readable gitdir file without a record");
    }

    #[tokio::test]
    async fn record_written_by_a_running_rebase_op_is_refused() {
        let fx = Fixture::new().await;
        diverge_on_a_txt(&fx).await;
        // Start's window between writing the record and git creating its rebase state.
        let file = fx.guided_file("main").await;
        fx.write_file_json(&file);
        let op_lock = tokio::sync::Mutex::new(());
        let held = op_lock.lock().await;
        let _file_lock = REBASE_FILE_LOCK.lock().await;
        let result = tokio::time::timeout(
            Duration::from_secs(60),
            guard_with_op_lock(&fx.git, fx.path(), &op_lock),
        )
        .await
        .expect("the guard must not wait for a rebase lock");
        assert_refused(result, GUARD_IN_PROGRESS);

        drop(held);
        assert_passes_stale(&fx, RebaseStaleReason::AbortedOutside).await;
        assert!(op_lock.try_lock().is_ok(), "the guard keeps no lock");
    }

    #[tokio::test]
    async fn held_op_lock_without_a_record_passes() {
        // The state endpoint holds the op lock briefly, also while idle.
        let fx = Fixture::new().await;
        let op_lock = tokio::sync::Mutex::new(());
        let _held = op_lock.lock().await;
        guard_with_op_lock(&fx.git, fx.path(), &op_lock)
            .await
            .expect("idle");
    }

    /// Writes the record and starts the rebase on the worker, as Start does.
    struct StartRebaseTask {
        fx_git: git::Git,
        file: GuidedRebaseFile,
    }

    #[async_trait]
    impl Task for StartRebaseTask {
        async fn execute(&self) -> Result<(), CoreError> {
            write_rebase_file(&self.fx_git.repo_path.join(".git"), &self.file)?;
            assert_eq!(start_raw_rebase(&self.fx_git, &self.file.onto).await, 1);
            Ok(())
        }

        fn get_name(&self) -> String {
            String::from("StartRebaseTask")
        }
    }

    struct MarkerTask(Arc<AtomicBool>);

    #[async_trait]
    impl Task for MarkerTask {
        async fn execute(&self) -> Result<(), CoreError> {
            self.0.store(true, Ordering::SeqCst);
            Ok(())
        }

        fn get_name(&self) -> String {
            String::from("MarkerTask")
        }
    }

    /// Queues `tasks` as one sequence on `queue`; returns its completion.
    async fn queue(
        queue: &tokio::sync::mpsc::Sender<TaskSequence>,
        tasks: Vec<Box<dyn Task + Send + Sync>>,
    ) -> tokio::sync::oneshot::Receiver<Option<CoreError>> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let mut sequence = TaskSequence::new().with_completion_tx(tx);
        for task in tasks {
            sequence.push(task);
        }
        queue.send(sequence).await.expect("queue sequence");
        rx
    }

    fn start_worker() -> tokio::sync::mpsc::Sender<TaskSequence> {
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        let mut worker = RepoWorker::new(rx, Arc::new(AtomicBool::new(false)));
        tokio::spawn(async move { worker.run().await });
        tx
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn queued_guard_sees_a_rebase_started_ahead_of_it() {
        let fx = Fixture::new().await;
        diverge_on_a_txt(&fx).await;
        let file = fx.guided_file("main").await;
        // The handler's check passes: nothing has started yet.
        ensure_no_guided_rebase(&fx.git, fx.path())
            .await
            .expect("idle");

        let worker = start_worker();
        let started = queue(
            &worker,
            vec![Box::new(StartRebaseTask {
                fx_git: fx.git.clone(),
                file,
            })],
        )
        .await;
        let ran = Arc::new(AtomicBool::new(false));
        let guarded = queue(
            &worker,
            vec![
                Box::new(GuidedRebaseGuardOp {
                    git_client: fx.git.clone(),
                }),
                Box::new(MarkerTask(ran.clone())),
            ],
        )
        .await;

        assert!(started.await.unwrap().is_none());
        match guarded.await.unwrap() {
            Some(CoreError::Input(e)) => assert_eq!(e.to_string(), GUARD_IN_PROGRESS),
            other => panic!("expected the guard's refusal, got {other:?}"),
        }
        assert!(!ran.load(Ordering::SeqCst), "the guarded op must not run");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn queued_guard_passes_when_idle() {
        let fx = Fixture::new().await;
        let worker = start_worker();
        let ran = Arc::new(AtomicBool::new(false));
        let done = queue(
            &worker,
            vec![
                Box::new(GuidedRebaseGuardOp {
                    git_client: fx.git.clone(),
                }),
                Box::new(MarkerTask(ran.clone())),
            ],
        )
        .await;
        assert!(done.await.unwrap().is_none());
        assert!(ran.load(Ordering::SeqCst));
    }

    #[test]
    fn stale_index_lock_is_kept_during_a_rebase() {
        let dir = tempfile::tempdir().unwrap();
        let git_dir = dir.path();
        assert!(may_remove_stale_index_lock(git_dir));

        for marker in ["rebase-merge", "rebase-apply"] {
            fs::create_dir(git_dir.join(marker)).unwrap();
            assert!(!may_remove_stale_index_lock(git_dir), "{marker}");
            fs::remove_dir(git_dir.join(marker)).unwrap();
        }

        // A record alone means no git process can still be rebasing.
        fs::write(json_path(git_dir), "{").unwrap();
        assert!(may_remove_stale_index_lock(git_dir));
    }
}
