//! Cellora REST API — entry point.
//!
//! Responsibilities:
//! - Load configuration from the environment.
//! - Initialise structured logging.
//! - Connect to Postgres (migrations are owned by the indexer, not the API).
//! - Build the router and bind the listener.
//! - Serve until SIGINT / SIGTERM, then shut down gracefully.

use std::net::SocketAddr;
use std::time::Duration;

use anyhow::Context;
use cellora_api::admin::{self, Cli, Command};
use cellora_api::ratelimit::RateLimiter;
use cellora_api::tip::{spawn_refresh_task, TipTracker};
use cellora_api::{build_app, AppState};
use cellora_common::{ckb::CkbClient, config::Config, logging};
use cellora_db::connect;
use clap::Parser;
use tokio::net::TcpListener;
use tokio::signal;
use tokio_util::sync::CancellationToken;
use tracing::info;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // `.env` is a developer convenience; production runs with real env vars only.
    let _ = dotenvy::dotenv();

    let cli = Cli::parse();
    let config = Config::from_env().context("load configuration")?;
    logging::init(&config.log_level, config.log_format).context("initialise logging")?;

    match cli.command {
        Some(Command::Admin { action }) => {
            let pool = connect(&config.database_url)
                .await
                .context("connect to postgres")?;
            admin::run(&pool, action).await?;
            return Ok(());
        }
        None => {
            // Fall through to the default "serve" path below.
        }
    }

    info!(
        version = env!("CARGO_PKG_VERSION"),
        bind_addr = %config.api_bind_addr,
        tip_refresh_ms = config.api_tip_cache_refresh_ms,
        "cellora api starting",
    );

    let pool = connect(&config.database_url)
        .await
        .context("connect to postgres")?;

    let ckb = CkbClient::new(config.ckb_rpc_url.clone()).context("construct ckb client")?;

    let bind_addr: SocketAddr = config
        .api_bind_addr
        .parse()
        .with_context(|| format!("parse bind address '{}'", config.api_bind_addr))?;

    let refresh_interval = Duration::from_millis(config.api_tip_cache_refresh_ms);
    let tip = TipTracker::new();
    let cancel = CancellationToken::new();
    let refresh_handle = spawn_refresh_task(
        tip.clone(),
        pool.clone(),
        ckb,
        refresh_interval,
        cancel.clone(),
    );

    // Construct the Redis-backed rate limiter. A failed connection at
    // boot is logged but does not abort startup — the API serves
    // fail-open until Redis is reachable.
    let rate_limiter = build_rate_limiter(&config).await;

    let mut state = AppState::with_tip(pool, config, tip);
    if let Some(limiter) = rate_limiter {
        state = state.with_rate_limiter(limiter);
    }
    let app = build_app(state);

    let listener = TcpListener::bind(bind_addr)
        .await
        .with_context(|| format!("bind tcp listener on {bind_addr}"))?;

    info!(addr = %bind_addr, "cellora api listening");

    let serve_result = axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("serve http");

    // Stop the refresh task and wait for it to drain before exiting.
    cancel.cancel();
    if let Err(err) = refresh_handle.await {
        tracing::warn!(error = %err, "tip refresh task join failure");
    }

    serve_result?;
    info!("cellora api stopped cleanly");
    Ok(())
}

/// Best-effort construction of the rate limiter. Returns `None` when
/// Redis is unreachable at boot — the service starts anyway and the
/// limiter middleware fails open.
async fn build_rate_limiter(config: &Config) -> Option<RateLimiter> {
    let client = match redis::Client::open(config.redis_url.as_str()) {
        Ok(c) => c,
        Err(err) => {
            tracing::warn!(error = %err, url = %config.redis_url, "redis client construction failed");
            return None;
        }
    };
    match redis::aio::ConnectionManager::new(client).await {
        Ok(manager) => Some(RateLimiter::new(manager, config.api_rate_limit_fail_open)),
        Err(err) => {
            tracing::warn!(error = %err, "redis connection manager failed at startup");
            None
        }
    }
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
