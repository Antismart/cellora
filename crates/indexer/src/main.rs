//! Cellora block indexer — entry point.
//!
//! Responsibilities:
//! - Load configuration from the environment.
//! - Initialise structured logging.
//! - Connect to Postgres and apply pending migrations.
//! - Poll the CKB node for new blocks and persist them.
//! - Shut down gracefully on SIGINT / SIGTERM.

use anyhow::Context;
use cellora_common::{ckb::CkbClient, config::Config, logging};
use cellora_db::{connect, migrate};
use cellora_indexer::{app, shutdown};
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

    let service = app::Service::new(pool, ckb, config.clone());
    let result = service.run(cancel.clone()).await;

    // Ensure the signal listener exits even if the poller returned first.
    cancel.cancel();
    let _ = shutdown_handle.await;

    match &result {
        Ok(()) => info!("cellora indexer stopped cleanly"),
        Err(err) => tracing::error!(error = %err, "cellora indexer stopped with error"),
    }
    result.map_err(anyhow::Error::from)
}
