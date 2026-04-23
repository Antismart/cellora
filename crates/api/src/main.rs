//! Cellora REST API — entry point.
//!
//! Responsibilities:
//! - Load configuration from the environment.
//! - Initialise structured logging.
//! - Connect to Postgres (migrations are owned by the indexer, not the API).
//! - Build the router and bind the listener.
//! - Serve until SIGINT / SIGTERM, then shut down gracefully.

use std::net::SocketAddr;

use anyhow::Context;
use cellora_api::{build_app, AppState};
use cellora_common::{config::Config, logging};
use cellora_db::connect;
use tokio::net::TcpListener;
use tokio::signal;
use tracing::info;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // `.env` is a developer convenience; production runs with real env vars only.
    let _ = dotenvy::dotenv();

    let config = Config::from_env().context("load configuration")?;
    logging::init(&config.log_level, config.log_format).context("initialise logging")?;

    info!(
        version = env!("CARGO_PKG_VERSION"),
        bind_addr = %config.api_bind_addr,
        "cellora api starting",
    );

    let pool = connect(&config.database_url)
        .await
        .context("connect to postgres")?;

    let bind_addr: SocketAddr = config
        .api_bind_addr
        .parse()
        .with_context(|| format!("parse bind address '{}'", config.api_bind_addr))?;

    let state = AppState::new(pool, config);
    let app = build_app(state);

    let listener = TcpListener::bind(bind_addr)
        .await
        .with_context(|| format!("bind tcp listener on {bind_addr}"))?;

    info!(addr = %bind_addr, "cellora api listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("serve http")?;

    info!("cellora api stopped cleanly");
    Ok(())
}

/// Future that completes on SIGINT or SIGTERM. Used by `axum::serve` to
/// drain in-flight requests before exiting.
async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(err) = signal::ctrl_c().await {
            tracing::error!(error = %err, "failed to install ctrl-c handler");
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match signal::unix::signal(signal::unix::SignalKind::terminate()) {
            Ok(mut stream) => {
                stream.recv().await;
            }
            Err(err) => {
                tracing::error!(error = %err, "failed to install sigterm handler");
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => info!("received SIGINT, shutting down"),
        _ = terminate => info!("received SIGTERM, shutting down"),
    }
}
