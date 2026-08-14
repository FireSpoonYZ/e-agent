//! Per-turn cancellation and progress shared by every tool.
//!
//! The host cancels a whole turn rather than one call, so the signal is process
//! wide instead of threaded through each tool's arguments.
//
// ponytail: one global turn scope, since the executor already serializes calls;
// move to a per-call handle if concurrent independent turns are ever needed.

use std::{
    future::Future,
    sync::{
        OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};

use tokio::sync::broadcast;

/// Error returned when the host cancels the current turn.
#[derive(Debug, thiserror::Error)]
#[error("operation cancelled")]
pub struct Cancelled;

static CANCELLED: AtomicBool = AtomicBool::new(false);
static NOTIFY: OnceLock<broadcast::Sender<()>> = OnceLock::new();

fn notify() -> &'static broadcast::Sender<()> {
    NOTIFY.get_or_init(|| broadcast::channel(1).0)
}

/// Cancel the current turn, waking every tool waiting on the signal.
pub fn cancel() {
    CANCELLED.store(true, Ordering::SeqCst);
    let _ = notify().send(());
}

/// Clear the cancellation flag before starting a new turn.
pub fn reset() {
    CANCELLED.store(false, Ordering::SeqCst);
}

/// Whether the current turn has been cancelled.
pub fn cancelled() -> bool {
    CANCELLED.load(Ordering::SeqCst)
}

/// Report progress for a running tool.
///
/// Emitted through `tracing`, which already crosses the cdylib boundary, so the
/// host sees extension progress without a second broadcast channel.
pub fn progress(tool: &'static str, message: &str) {
    tracing::info!(target: "e_agent_extension::progress", tool, message, "tool progress");
}

/// Subscribe to the cancellation signal for the current turn.
pub fn subscribe_cancel() -> broadcast::Receiver<()> {
    notify().subscribe()
}

/// Run `task` unless the turn is cancelled first.
pub(crate) async fn until_cancelled<T>(
    task: impl Future<Output = anyhow::Result<T>>,
) -> anyhow::Result<T> {
    if cancelled() {
        return Err(Cancelled.into());
    }
    let mut signal = notify().subscribe();
    tokio::select! {
        biased;
        result = task => result,
        _ = signal.recv() => Err(Cancelled.into()),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{cancel, cancelled, progress, reset, until_cancelled};

    #[test]
    fn cancels_a_running_tool_and_reports_progress() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            reset();
            progress("read", "started");

            let task = until_cancelled(async {
                tokio::time::sleep(Duration::from_secs(30)).await;
                Ok(())
            });
            let canceller = tokio::spawn(async {
                tokio::time::sleep(Duration::from_millis(10)).await;
                cancel();
            });
            assert!(task.await.is_err());
            canceller.await.unwrap();
            assert!(cancelled());

            reset();
            assert!(!cancelled());
            assert!(until_cancelled(async { Ok(7) }).await.unwrap() == 7);
        });
    }
}
