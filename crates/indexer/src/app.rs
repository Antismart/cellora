//! Service wiring for the indexer binary.

use cellora_common::{ckb::CkbClient, config::Config};
use redis::aio::ConnectionManager;
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;

use crate::metrics::Metrics;
use crate::poller::{Poller, PollerError};

/// Root service object that owns every long-lived handle (pool, RPC client,
/// configuration, optional Redis publisher, metrics) and dispatches to the
/// poller.
pub struct Service {
    pool: PgPool,
    ckb: CkbClient,
    config: Config,
    redis: Option<ConnectionManager>,
    metrics: Metrics,
}

impl Service {
    /// Construct a new service from its dependencies. A fresh metrics
    /// bundle is created internally; share it externally via
    /// [`Service::with_metrics`] if you also want to expose it over
    /// HTTP.
    pub fn new(pool: PgPool, ckb: CkbClient, config: Config) -> Self {
        Self {
            pool,
            ckb,
            config,
            redis: None,
            metrics: Metrics::new(),
        }
    }

    /// Attach a Redis connection so the poller can publish reorg events
    /// on `cellora:reorg`. The indexer runs without one — publishing is
    /// best-effort and skipped silently when absent.
    pub fn with_redis(mut self, redis: ConnectionManager) -> Self {
        self.redis = Some(redis);
        self
    }

    /// Replace the default metrics bundle so the metrics server and the
    /// poller observe the same registry.
    pub fn with_metrics(mut self, metrics: Metrics) -> Self {
        self.metrics = metrics;
        self
    }

    /// Run the indexer loop until `cancel` fires or a fatal error occurs.
    pub async fn run(self, cancel: CancellationToken) -> Result<(), PollerError> {
        let mut poller = Poller::new(self.pool, self.ckb, self.config).with_metrics(self.metrics);
        if let Some(redis) = self.redis {
            poller = poller.with_redis(redis);
        }
        poller.run(cancel).await
    }
}
