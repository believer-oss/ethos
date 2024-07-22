use crate::types::config::AppConfigRef;
use async_trait::async_trait;
use tokio::sync::mpsc::Receiver;
use tracing::{debug, error, info, instrument, span};

#[async_trait]
pub trait Task {
    async fn execute(&self) -> anyhow::Result<()>;
    fn get_name(&self) -> String;
}

pub struct NoOp;

#[async_trait]
impl Task for NoOp {
    async fn execute(&self) -> anyhow::Result<()> {
        Ok(())
    }

    fn get_name(&self) -> String {
        String::from("NoOp")
    }
}

pub struct TaskSequence {
    pub tasks: Vec<Box<dyn Task + Send + Sync>>,
    pub completion_tx: Option<tokio::sync::oneshot::Sender<Option<anyhow::Error>>>,

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
        tx: tokio::sync::oneshot::Sender<Option<anyhow::Error>>,
    ) -> Self {
        self.completion_tx = Some(tx);
        self
    }

    pub fn push(&mut self, op: Box<dyn Task + Send + Sync>) {
        self.tasks.push(op);
    }
}

pub struct RepoWorker {
    config: AppConfigRef,
    queue: Receiver<TaskSequence>,
}

impl RepoWorker {
    pub fn new(config: AppConfigRef, tx: Receiver<TaskSequence>) -> Self {
        RepoWorker { config, queue: tx }
    }

    // For running git tasks that could take a while, like pulling or pushing.
    pub async fn run(&mut self) {
        while let Some(sequence) = self.queue.recv().await {
            {
                let config = self.config.read();
                if config.repo_path.is_empty() {
                    debug!("Repo path not set, skipping Task");
                    continue;
                }
            }

            let mut err: Option<anyhow::Error> = None;
            let span = sequence.span.clone();
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

            if sequence.completion_tx.is_some() {
                let _ = sequence.completion_tx.unwrap().send(err);
            }
        }
    }

    #[instrument(parent = _span, skip_all)]
    async fn run_task(
        &self,
        op: Box<dyn Task + Send + Sync>,
        _span: &tracing::Span,
    ) -> anyhow::Result<()> {
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
