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
use chrono::Utc;
use redis::aio::ConnectionManager;
use sqlx::PgPool;
use thiserror::Error;
use tokio::time::{sleep, Instant};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use crate::parser::{parse_block, ParseError};
use crate::reorg::{self, ReorgError};

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
    #[error("reorg handling failed: {0}")]
    Reorg(#[from] ReorgError),
}

/// Owning handle for the polling loop.
pub struct Poller {
    pool: PgPool,
    ckb: CkbClient,
    config: Config,
    redis: Option<ConnectionManager>,
}

impl Poller {
    /// Construct a poller with its external dependencies.
    pub fn new(pool: PgPool, ckb: CkbClient, config: Config) -> Self {
        Self {
            pool,
            ckb,
            config,
            redis: None,
        }
    }

    /// Attach an optional Redis connection for publishing reorg events
    /// on the `cellora:reorg` channel. The poller works without one —
    /// publishing is best-effort and skipped silently when absent.
    pub fn with_redis(mut self, redis: ConnectionManager) -> Self {
        self.redis = Some(redis);
        self
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
                Ok(StepOutcome::ReorgHandled { new_tip }) => {
                    // Rollback succeeded; resume from the block after the
                    // common ancestor on the next loop iteration.
                    next_block = u64::try_from(new_tip).unwrap_or(0).saturating_add(1);
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

        // Reorg detection: when we already have a block at `block_number - 1`,
        // verify the new block's parent_hash matches our stored hash there.
        // If they disagree, the chain has reorganized and our stored chain
        // tip is no longer canonical — handle it before attempting to insert.
        if block_number > 0 {
            let signed_height = i64::try_from(block_number).unwrap_or(i64::MAX);
            let prev_height = signed_height - 1;
            if let Some(stored_prev_hash) = blocks::hash_at(&self.pool, prev_height).await? {
                let parent_hash = block.header.inner.parent_hash.0.to_vec();
                if parent_hash != stored_prev_hash {
                    self.handle_reorg(prev_height, &stored_prev_hash).await?;
                    // Re-poll the new tip on the next iteration; do not
                    // attempt to insert this block into the rolled-back
                    // chain — the next height after the ancestor may be
                    // different from `block_number`.
                    return Ok(StepOutcome::ReorgHandled {
                        new_tip: prev_height - 1,
                    });
                }
            }
        }

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

    /// Walk back to find the common ancestor and run the rollback.
    /// `suspect_height` is the height at which we just observed the
    /// disagreement (typically `tip - 1` because the new block's
    /// parent is at that height).
    async fn handle_reorg(
        &self,
        suspect_height: i64,
        indexed_hash_at_suspect: &[u8],
    ) -> Result<(), PollerError> {
        let pool = self.pool.clone();
        let ancestor = reorg::find_common_ancestor(&self.ckb, suspect_height, |h| {
            let pool = pool.clone();
            async move { blocks::hash_at(&pool, h).await }
        })
        .await?;

        let depth = suspect_height - ancestor.block_number + 1;
        let target = i64::from(self.config.indexer_reorg_target_depth);
        let max = i64::from(self.config.indexer_reorg_max_depth);

        if depth > max {
            // Past the upper bound — log loudly but still complete.
            // Failing closed would leave the database stuck on the
            // orphaned chain, which is worse.
            error!(
                depth,
                ancestor = ancestor.block_number,
                max,
                "reorg deeper than upper bound; rolling back anyway"
            );
        } else if depth > target {
            warn!(
                depth,
                ancestor = ancestor.block_number,
                target,
                "reorg deeper than target depth"
            );
        } else {
            info!(
                depth,
                ancestor = ancestor.block_number,
                "reorg detected; rolling back"
            );
        }

        let outcome = reorg::rollback_to(
            &self.pool,
            &ancestor,
            suspect_height,
            indexed_hash_at_suspect,
        )
        .await?;

        info!(
            log_id = outcome.log_id,
            depth = outcome.depth,
            deleted_blocks = outcome.deleted_blocks,
            restored_cells = outcome.restored_cells,
            new_tip = outcome.ancestor_height,
            "reorg rollback completed"
        );

        let event = reorg::ReorgEvent {
            ancestor_block_number: ancestor.block_number,
            ancestor_hash: format!("0x{}", hex::encode(&ancestor.node_hash)),
            depth: outcome.depth,
            completed_at: Utc::now(),
        };
        reorg::publish_reorg(self.redis.as_ref(), &event).await;

        Ok(())
    }
}

enum StepOutcome {
    Indexed,
    WaitingForTip,
    ReorgHandled {
        /// New checkpoint height after the rollback. The poller resumes
        /// from `new_tip + 1`.
        new_tip: i64,
    },
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
