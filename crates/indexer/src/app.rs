//! Service wiring for the indexer binary.

use cellora_common::{ckb::CkbClient, config::Config};
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;

use crate::poller::{Poller, PollerError};

/// Root service object that owns every long-lived handle (pool, RPC client,
/// configuration) and dispatches to the poller.
pub struct Service {
    pool: PgPool,
    ckb: CkbClient,
    config: Config,
}

impl Service {
    /// Construct a new service from its dependencies.
    pub fn new(pool: PgPool, ckb: CkbClient, config: Config) -> Self {
        Self { pool, ckb, config }
    }

    /// Run the indexer loop until `cancel` fires or a fatal error occurs.
    pub async fn run(self, cancel: CancellationToken) -> Result<(), PollerError> {
        let poller = Poller::new(self.pool, self.ckb, self.config);
        poller.run(cancel).await
    }
}
