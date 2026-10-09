use std::sync::mpsc::{channel, Sender as STDSender};
use std::thread;

use ethos_core::capture::service::CaptureNotification;
use ethos_core::capture::types::PendingSummary;
use ethos_core::types::errors::CoreError;

use crate::engine::EngineProvider;
use crate::state::{AppState, Notification};

pub use router::router;

pub mod router;

/// Bridges the core's capture toasts onto the app's notification channel.
pub fn notification_forwarder(tx: STDSender<Notification>) -> STDSender<CaptureNotification> {
    let (capture_tx, capture_rx) = channel::<CaptureNotification>();
    thread::spawn(move || {
        while let Ok(notification) = capture_rx.recv() {
            let mapped = match notification {
                CaptureNotification::Success(msg) => Notification::Success(msg),
                CaptureNotification::Error(msg) => Notification::Error(msg),
            };
            if tx.send(mapped).is_err() {
                break;
            }
        }
    });
    capture_tx
}

/// `pending_all` reads the watch directories, so it runs off the async runtime.
pub async fn pending_all<T>(state: &AppState<T>) -> Result<Option<PendingSummary>, CoreError>
where
    T: EngineProvider,
{
    let capture = state.capture.clone();
    tokio::task::spawn_blocking(move || capture.pending_all())
        .await
        .map_err(|e| CoreError::Internal(e.into()))
}
