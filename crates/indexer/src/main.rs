//! Cellora block indexer — entry point.
//!
//! Responsibilities:
//! - Load configuration from the environment.
//! - Initialise structured logging.
//! - Connect to Postgres and apply pending migrations.
//! - Poll the CKB node for new blocks and persist them.
//! - Shut down gracefully on SIGINT / SIGTERM.

use std::net::SocketAddr;

use anyhow::Context;
use cellora_common::{ckb::CkbClient, config::Config, logging};
use cellora_db::{connect, migrate};
use cellora_indexer::metrics::Metrics;
use cellora_indexer::{app, metrics_server, shutdown};
use tokio_util::sync::CancellationToken;
use tracing::info;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // `.env` is a developer convenience; production runs with real env vars only.
    let _ = dotenvy::dotenv();

    let config = Config::from_env().context("load configuration")?;
    logging::init(&config.log_level, config.log_format).context("initialise logging")?;

    info!(
        version = env!("CARGO_PKG_VERSION"),
        ckb_rpc = %config.ckb_rpc_url,
        poll_interval_ms = config.poll_interval_ms,
        start_block = config.indexer_start_block,
        "cellora indexer starting"
    );

    let pool = connect(&config.database_url)
        .await
        .context("connect to postgres")?;
    migrate::run(&pool).await.context("run migrations")?;

    let ckb = CkbClient::new(config.ckb_rpc_url.clone()).context("construct ckb client")?;
    let cancel = CancellationToken::new();
    let shutdown_handle = shutdown::spawn(cancel.clone());

    // Best-effort Redis connection for publishing reorg events. A
    // failure here is logged and the indexer carries on without
    // publishing — the API's tip cache will eventually re-poll.
    let redis = match redis::Client::open(config.redis_url.as_str()) {
        Ok(client) => match redis::aio::ConnectionManager::new(client).await {
            Ok(manager) => Some(manager),
            Err(err) => {
                tracing::warn!(error = %err, "redis connection manager failed; reorg events will not publish");
                None
            }
        },
        Err(err) => {
            tracing::warn!(error = %err, "redis client construction failed; reorg events will not publish");
            None
        }
    };

    // One shared metrics bundle so the HTTP server and the poller see
    // the same counters. A failure to bind the metrics listener is
    // logged and the indexer carries on — observability shouldn't
    // block ingestion.
    let metrics = Metrics::new();
    let metrics_bind: SocketAddr = config.indexer_metrics_bind_addr.parse().with_context(|| {
        format!(
            "parse indexer metrics bind address '{}'",
            config.indexer_metrics_bind_addr
        )
    })?;
    let metrics_handle = metrics_server::spawn(metrics.clone(), metrics_bind, cancel.clone());

    let mut service = app::Service::new(pool, ckb, config.clone()).with_metrics(metrics);
    if let Some(manager) = redis {
        service = service.with_redis(manager);
    }
    let result = service.run(cancel.clone()).await;

    // Ensure the signal listener and metrics server exit even if the
    // poller returned first.
    cancel.cancel();
    let _ = shutdown_handle.await;
    if let Err(err) = metrics_handle.await {
        tracing::warn!(error = %err, "metrics server task join failed");
    }

    match &result {
        Ok(()) => info!("cellora indexer stopped cleanly"),
        Err(err) => tracing::error!(error = %err, "cellora indexer stopped with error"),
    }
    result.map_err(anyhow::Error::from)
}
