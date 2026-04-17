//! Block polling loop.
//!
//! The poller asks the CKB node for the next block after its current
//! checkpoint, parses it into database rows, and commits everything (block,
//! transactions, cells, consumed cells, new checkpoint) in a single Postgres
//! transaction. It sleeps between polls when the node has no new block, and
//! applies capped exponential backoff on transient errors.

use std::time::Duration;

use cellora_common::{ckb::CkbClient, config::Config, error::Error as CommonError};
use cellora_db::{blocks, cells, checkpoint, transactions, DbError};
use sqlx::PgPool;
use thiserror::Error;
use tokio::time::{sleep, Instant};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::parser::{parse_block, ParseError};

/// Errors that can terminate the poller.
#[allow(missing_docs)]
#[derive(Debug, Error)]
pub enum PollerError {
    #[error("ckb rpc error: {0}")]
    Rpc(#[from] CommonError),
    #[error("database error: {0}")]
    Db(#[from] DbError),
    #[error("parse error at block {block}: {source}")]
    Parse {
        block: u64,
        #[source]
        source: ParseError,
    },
}

/// Owning handle for the polling loop.
pub struct Poller {
    pool: PgPool,
    ckb: CkbClient,
    config: Config,
}

impl Poller {
    /// Construct a poller with its external dependencies.
    pub fn new(pool: PgPool, ckb: CkbClient, config: Config) -> Self {
        Self { pool, ckb, config }
    }

    /// Drive the loop until `cancel` fires or a fatal error bubbles up.
    ///
    /// A fatal error is something that cannot be retried: invalid configuration
    /// or a bug in the parser. Transient RPC / DB errors are logged and retried
    /// with capped exponential backoff.
    pub async fn run(self, cancel: CancellationToken) -> Result<(), PollerError> {
        let poll_interval = Duration::from_millis(self.config.poll_interval_ms);
        let mut next_block = match checkpoint::read(&self.pool).await? {
            Some(cp) => cp.last_indexed_block.saturating_add(1) as u64,
            None => self.config.indexer_start_block,
        };
        info!(next_block, "starting poll loop");

        let mut backoff = Backoff::new();

        while !cancel.is_cancelled() {
            match self.step(next_block).await {
                Ok(StepOutcome::Indexed) => {
                    next_block = next_block.saturating_add(1);
                    backoff.reset();
                }
                Ok(StepOutcome::WaitingForTip) => {
                    select_sleep(&cancel, poll_interval).await;
                }
                Err(err) => {
                    warn!(block = next_block, error = %err, "poll step failed; backing off");
                    let delay = backoff.next_delay();
                    select_sleep(&cancel, delay).await;
                }
            }
        }
        info!("shutdown requested; poll loop exiting");
        Ok(())
    }

    async fn step(&self, block_number: u64) -> Result<StepOutcome, PollerError> {
        let start = Instant::now();
        let Some(block) = self.ckb.get_block_by_number(block_number).await? else {
            debug!(block = block_number, "node has no block at this height yet");
            return Ok(StepOutcome::WaitingForTip);
        };

        let parsed = parse_block(&block).map_err(|source| PollerError::Parse {
            block: block_number,
            source,
        })?;

        let mut tx = self.pool.begin().await.map_err(DbError::from)?;
        blocks::insert(&mut *tx, &parsed.block).await?;
        transactions::insert_batch(&mut tx, &parsed.transactions).await?;
        cells::insert_batch(&mut tx, &parsed.cells).await?;
        cells::mark_consumed(&mut tx, &parsed.consumed).await?;
        checkpoint::upsert(&mut tx, parsed.block.number, &parsed.block.hash).await?;
        tx.commit().await.map_err(DbError::from)?;

        info!(
            block = parsed.block.number,
            hash = %hex::encode(&parsed.block.hash),
            txs = parsed.transactions.len(),
            cells = parsed.cells.len(),
            consumed = parsed.consumed.len(),
            elapsed_ms = start.elapsed().as_millis() as u64,
            "indexed block"
        );
        Ok(StepOutcome::Indexed)
    }
}

enum StepOutcome {
    Indexed,
    WaitingForTip,
}

/// Capped exponential backoff starting at 1 s, doubling up to 30 s.
struct Backoff {
    current: Duration,
}

impl Backoff {
    fn new() -> Self {
        Self {
            current: Duration::from_secs(1),
        }
    }

    fn next_delay(&mut self) -> Duration {
        let delay = self.current;
        self.current = (self.current * 2).min(Duration::from_secs(30));
        delay
    }

    fn reset(&mut self) {
        self.current = Duration::from_secs(1);
    }
}

async fn select_sleep(cancel: &CancellationToken, delay: Duration) {
    tokio::select! {
        _ = sleep(delay) => {}
        _ = cancel.cancelled() => {}
    }
}
