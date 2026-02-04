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
