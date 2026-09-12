use crate::types::errors::CoreError;
use async_trait::async_trait;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use tokio::sync::mpsc::Receiver;
use tracing::{error, info, instrument, span};

#[async_trait]
pub trait Task {
    async fn execute(&self) -> Result<(), CoreError>;
    fn get_name(&self) -> String;
}

pub struct NoOp;

#[async_trait]
impl Task for NoOp {
    async fn execute(&self) -> Result<(), CoreError> {
        Ok(())
    }

    fn get_name(&self) -> String {
        String::from("NoOp")
    }
}

pub struct TaskSequence {
    pub tasks: Vec<Box<dyn Task + Send + Sync>>,
    pub completion_tx: Option<tokio::sync::oneshot::Sender<Option<CoreError>>>,

    span: tracing::Span,
}

impl Default for TaskSequence {
    fn default() -> Self {
        TaskSequence::new()
    }
}

impl TaskSequence {
    pub fn new() -> Self {
        TaskSequence {
            tasks: Vec::new(),
            completion_tx: None,
            span: span!(tracing::Level::INFO, "TaskSequence"),
        }
    }

    pub fn with_completion_tx(
        mut self,
        tx: tokio::sync::oneshot::Sender<Option<CoreError>>,
    ) -> Self {
        self.completion_tx = Some(tx);
        self
    }

    pub fn push(&mut self, op: Box<dyn Task + Send + Sync>) {
        self.tasks.push(op);
    }
}

pub struct RepoWorker {
    queue: Receiver<TaskSequence>,
    pause_file_watcher: Arc<AtomicBool>,
}

impl RepoWorker {
    pub fn new(tx: Receiver<TaskSequence>, pause_file_watcher: Arc<AtomicBool>) -> Self {
        RepoWorker {
            queue: tx,
            pause_file_watcher,
        }
    }

    // For running git tasks that could take a while, like pulling or pushing.
    pub async fn run(&mut self) {
        while let Some(sequence) = self.queue.recv().await {
            let mut err: Option<CoreError> = None;
            let span = sequence.span.clone();

            self.pause_file_watcher
                .store(true, std::sync::atomic::Ordering::Relaxed);
            for task in sequence.tasks {
                match self.run_task(task, &span).await {
                    Ok(_) => {}
                    Err(e) => {
                        error!("caught error running task: {}", &e);
                        err = Some(e);
                        break;
                    }
                }
            }
            self.pause_file_watcher
                .store(false, std::sync::atomic::Ordering::Relaxed);

            if let Some(tx) = sequence.completion_tx {
                let _ = tx.send(err);
            }
        }
    }

    #[instrument(parent = _span, skip_all)]
    async fn run_task(
        &self,
        op: Box<dyn Task + Send + Sync>,
        _span: &tracing::Span,
    ) -> Result<(), CoreError> {
        info!("Running: {:?}", op.get_name());
        match op.execute().await {
            Ok(_) => {}
            Err(e) => {
                return Err(e);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;
    use std::sync::Mutex;
    use std::time::Duration;

    /// Records `start:<name>` / `end:<name>` around a short sleep, so interleaving between
    /// sequences is visible in the log ordering.
    struct RecordingTask {
        name: String,
        log: Arc<Mutex<Vec<String>>>,
        /// Sampled inside `execute`, so a test can see what the pause flag was during the run.
        pause_seen: Option<Arc<AtomicBool>>,
        observed_pause: Arc<Mutex<Vec<bool>>>,
        fail: bool,
    }

    impl RecordingTask {
        fn new(name: &str, log: Arc<Mutex<Vec<String>>>) -> Self {
            RecordingTask {
                name: name.to_string(),
                log,
                pause_seen: None,
                observed_pause: Arc::new(Mutex::new(Vec::new())),
                fail: false,
            }
        }
    }

    #[async_trait]
    impl Task for RecordingTask {
        async fn execute(&self) -> Result<(), CoreError> {
            self.log
                .lock()
                .unwrap()
                .push(format!("start:{}", self.name));
            if let Some(flag) = &self.pause_seen {
                self.observed_pause
                    .lock()
                    .unwrap()
                    .push(flag.load(Ordering::Relaxed));
            }
            tokio::time::sleep(Duration::from_millis(80)).await;
            self.log.lock().unwrap().push(format!("end:{}", self.name));

            if self.fail {
                return Err(CoreError::Internal(anyhow::anyhow!("boom")));
            }
            Ok(())
        }

        fn get_name(&self) -> String {
            self.name.clone()
        }
    }

    /// Two sequences queued back to back must run one after the other, never overlapping.
    ///
    /// This is the property `run_maintenance_handler` depends on: maintenance is queued rather than
    /// executed directly precisely so it cannot interleave with a sync, submit or checkout. If this
    /// test fails, the frontend's locking modal is decorative.
    #[tokio::test(flavor = "multi_thread")]
    async fn test_worker_serializes_sequences() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        let pause = Arc::new(AtomicBool::new(false));

        let mut worker = RepoWorker::new(rx, pause);
        let handle = tokio::spawn(async move { worker.run().await });

        for name in ["first", "second"] {
            let mut seq = TaskSequence::new();
            seq.push(Box::new(RecordingTask::new(name, log.clone())));
            tx.send(seq).await.expect("queue sequence");
        }

        drop(tx); // closes the queue so run() returns once drained
        handle.await.expect("worker finished");

        let entries = log.lock().unwrap().clone();
        assert_eq!(
            entries,
            vec!["start:first", "end:first", "start:second", "end:second"],
            "sequences must not interleave"
        );
    }

    /// The worker must hold `pause_file_watcher` for the whole sequence and release it afterwards.
    ///
    /// Since Stage 005 this flag also suppresses the periodic fetch and maintenance tasks, so it is
    /// what stops background git work from racing a queued operation.
    #[tokio::test(flavor = "multi_thread")]
    async fn test_worker_pauses_file_watcher_during_sequence() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let observed = Arc::new(Mutex::new(Vec::new()));
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        let pause = Arc::new(AtomicBool::new(false));

        let mut task = RecordingTask::new("op", log.clone());
        task.pause_seen = Some(pause.clone());
        task.observed_pause = observed.clone();

        let mut worker = RepoWorker::new(rx, pause.clone());
        let handle = tokio::spawn(async move { worker.run().await });

        let mut seq = TaskSequence::new();
        seq.push(Box::new(task));
        tx.send(seq).await.expect("queue sequence");

        drop(tx);
        handle.await.expect("worker finished");

        assert_eq!(
            observed.lock().unwrap().clone(),
            vec![true],
            "pause flag must be set while a task runs"
        );
        assert!(
            !pause.load(Ordering::Relaxed),
            "pause flag must be cleared after the sequence completes"
        );
    }

    /// A failing task must stop its sequence and report the error to the completion channel — the
    /// path `run_maintenance_handler` turns into an HTTP error for the frontend.
    #[tokio::test(flavor = "multi_thread")]
    async fn test_worker_reports_error_and_stops_sequence() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        let pause = Arc::new(AtomicBool::new(false));

        let mut worker = RepoWorker::new(rx, pause.clone());
        let handle = tokio::spawn(async move { worker.run().await });

        let mut failing = RecordingTask::new("failing", log.clone());
        failing.fail = true;

        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        let mut seq = TaskSequence::new().with_completion_tx(done_tx);
        seq.push(Box::new(failing));
        seq.push(Box::new(RecordingTask::new("never_runs", log.clone())));
        tx.send(seq).await.expect("queue sequence");

        let result = done_rx.await.expect("completion signalled");
        assert!(
            result.is_some(),
            "the failure must be reported, not swallowed"
        );

        drop(tx);
        handle.await.expect("worker finished");

        let entries = log.lock().unwrap().clone();
        assert!(
            !entries.iter().any(|e| e.contains("never_runs")),
            "a sequence must stop at its first failing task, got {entries:?}"
        );
        assert!(
            !pause.load(Ordering::Relaxed),
            "pause flag must be cleared even when a sequence fails"
        );
    }
}
