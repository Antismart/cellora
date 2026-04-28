//! Service wiring for the indexer binary.

use cellora_common::{ckb::CkbClient, config::Config};
use redis::aio::ConnectionManager;
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;

use crate::poller::{Poller, PollerError};

/// Root service object that owns every long-lived handle (pool, RPC client,
/// configuration, optional Redis publisher) and dispatches to the poller.
pub struct Service {
    pool: PgPool,
    ckb: CkbClient,
    config: Config,
    redis: Option<ConnectionManager>,
}

impl Service {
    /// Construct a new service from its dependencies.
    pub fn new(pool: PgPool, ckb: CkbClient, config: Config) -> Self {
        Self {
            pool,
            ckb,
            config,
            redis: None,
        }
    }

    /// Attach a Redis connection so the poller can publish reorg events
    /// on `cellora:reorg`. The indexer runs without one — publishing is
    /// best-effort and skipped silently when absent.
    pub fn with_redis(mut self, redis: ConnectionManager) -> Self {
        self.redis = Some(redis);
        self
    }

    /// Run the indexer loop until `cancel` fires or a fatal error occurs.
    pub async fn run(self, cancel: CancellationToken) -> Result<(), PollerError> {
        let mut poller = Poller::new(self.pool, self.ckb, self.config);
        if let Some(redis) = self.redis {
            poller = poller.with_redis(redis);
        }
        poller.run(cancel).await
    }
}
