//! Lightweight HTTP server that exposes the indexer's `/metrics`.
//!
//! The indexer otherwise has no HTTP surface, but Prometheus's scrape
//! model requires every instrumented service to serve its own endpoint.
//! This module spins up a minimal axum router on a configurable bind
//! address and tears it down when the supplied cancellation token
//! fires.

use std::net::SocketAddr;

use anyhow::Context;
use axum::extract::State;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::metrics::Metrics;

/// Spawn the metrics HTTP server on `bind_addr`. Returns the join
/// handle so `main` can await it during shutdown.
pub fn spawn(
    metrics: Metrics,
    bind_addr: SocketAddr,
    cancel: CancellationToken,
) -> JoinHandle<anyhow::Result<()>> {
    tokio::spawn(async move {
        let app = Router::new()
            .route("/metrics", get(metrics_handler))
            .route("/health", get(health_handler))
            .with_state(metrics);
        let listener = TcpListener::bind(bind_addr)
            .await
            .with_context(|| format!("bind metrics listener on {bind_addr}"))?;
        info!(addr = %bind_addr, "indexer metrics server listening");
        let serve = axum::serve(listener, app).with_graceful_shutdown(async move {
            cancel.cancelled().await;
        });
        if let Err(err) = serve.await {
            warn!(error = %err, "indexer metrics server stopped with error");
        } else {
            info!("indexer metrics server stopped cleanly");
        }
        Ok::<(), anyhow::Error>(())
    })
}

async fn metrics_handler(State(metrics): State<Metrics>) -> impl IntoResponse {
    (
        [("content-type", "text/plain; version=0.0.4")],
        metrics.render(),
    )
}

async fn health_handler() -> &'static str {
    "ok"
}
