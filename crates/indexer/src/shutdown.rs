//! Listen for SIGINT / SIGTERM and trigger graceful shutdown of the indexer.

use tokio::signal;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

/// Spawn a task that flips `cancel` the first time the process receives
/// SIGINT, SIGTERM, or the platform-equivalent interrupt. Returns a handle
/// that callers can `await` after cancelling the token themselves.
pub fn spawn(cancel: CancellationToken) -> JoinHandle<()> {
    tokio::spawn(async move {
        tokio::select! {
            _ = wait_for_ctrl_c() => {
                info!("received interrupt; initiating shutdown");
            }
            _ = wait_for_terminate() => {
                info!("received terminate; initiating shutdown");
            }
            _ = cancel.cancelled() => {
                // Cancellation came from elsewhere (e.g. service error).
            }
        }
        cancel.cancel();
    })
}

async fn wait_for_ctrl_c() {
    if let Err(err) = signal::ctrl_c().await {
        warn!(error = %err, "ctrl_c handler failed to install");
    }
}

#[cfg(unix)]
async fn wait_for_terminate() {
    use tokio::signal::unix::{signal, SignalKind};
    match signal(SignalKind::terminate()) {
        Ok(mut stream) => {
            stream.recv().await;
        }
        Err(err) => {
            warn!(error = %err, "SIGTERM handler failed to install");
            std::future::pending::<()>().await;
        }
    }
}

#[cfg(not(unix))]
async fn wait_for_terminate() {
    // On non-unix platforms only Ctrl-C triggers shutdown; block forever here.
    std::future::pending::<()>().await;
}
