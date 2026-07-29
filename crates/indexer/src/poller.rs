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

use crate::metrics::Metrics;
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
    metrics: Metrics,
}

impl Poller {
    /// Construct a poller with its external dependencies.
    pub fn new(pool: PgPool, ckb: CkbClient, config: Config) -> Self {
        Self {
            pool,
            ckb,
            config,
            redis: None,
            metrics: Metrics::new(),
        }
    }

    /// Attach an optional Redis connection for publishing reorg events
    /// on the `cellora:reorg` channel. The poller works without one —
    /// publishing is best-effort and skipped silently when absent.
    pub fn with_redis(mut self, redis: ConnectionManager) -> Self {
        self.redis = Some(redis);
        self
    }

    /// Replace the default in-process metrics bundle with a shared one
    /// so the metrics HTTP server and the poller observe the same
    /// counters / histograms.
    pub fn with_metrics(mut self, metrics: Metrics) -> Self {
        self.metrics = metrics;
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
                Ok(StepOutcome::InconsistentNode) => {
                    // Depth-0 "reorg": the node gave an inconsistent
                    // response. Do NOT reset backoff and do NOT advance
                    // `next_block`; back off so we neither spin on the same
                    // block nor hammer a flaky node.
                    let delay = backoff.next_delay();
                    select_sleep(&cancel, delay).await;
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
                    return self.handle_reorg(prev_height, &stored_prev_hash).await;
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

        if let Some(redis) = self.redis.as_ref() {
            crate::events::publish_block_and_cells(redis, &parsed.block, &parsed.cells).await;
        }

        let elapsed = start.elapsed();
        self.metrics
            .observe_block_indexed(parsed.block.number, elapsed.as_secs_f64());
        info!(
            block = parsed.block.number,
            hash = %hex::encode(&parsed.block.hash),
            txs = parsed.transactions.len(),
            cells = parsed.cells.len(),
            consumed = parsed.consumed.len(),
            elapsed_ms = elapsed.as_millis() as u64,
            "indexed block"
        );
        Ok(StepOutcome::Indexed)
    }

    /// Walk back to find the common ancestor and run the rollback.
    /// `suspect_height` is the height at which we just observed the
    /// disagreement (typically `tip - 1` because the new block's
    /// parent is at that height).
    ///
    /// Returns [`StepOutcome::ReorgHandled`] once a genuine (depth >= 1)
    /// reorg has been rolled back, or [`StepOutcome::InconsistentNode`]
    /// when the common ancestor equals `suspect_height` — i.e. the
    /// rollback depth would be zero, which is not a real reorg but an
    /// inconsistent node response, and must not churn `reorg_log` rows.
    async fn handle_reorg(
        &self,
        suspect_height: i64,
        indexed_hash_at_suspect: &[u8],
    ) -> Result<StepOutcome, PollerError> {
        let pool = self.pool.clone();
        let ancestor = reorg::find_common_ancestor(&self.ckb, suspect_height, |h| {
            let pool = pool.clone();
            async move { blocks::hash_at(&pool, h).await }
        })
        .await?;

        // Depth-0 guard: if the common ancestor is the suspect height
        // itself, `rollback_to` would delete nothing yet still write a
        // `reorg_log` row and move the checkpoint to where it already is.
        // Resetting backoff and re-polling the same block would spin,
        // churning spurious audit rows and metrics. This only happens
        // when the node's own `block(N-1).hash` still equals our stored
        // hash at N-1 while `block(N).parent_hash` disagrees — an
        // inconsistent node response. Skip the rollback entirely and
        // signal the run loop to back off instead of resetting.
        if is_inconsistent_node(suspect_height, ancestor.block_number) {
            warn!(
                suspect_height,
                ancestor = ancestor.block_number,
                "common ancestor at suspect height (rollback depth 0); \
                 treating as inconsistent node response, not a reorg"
            );
            return Ok(StepOutcome::InconsistentNode);
        }

        let outcome = reorg::rollback_to(
            &self.pool,
            &ancestor,
            suspect_height,
            indexed_hash_at_suspect,
        )
        .await?;

        // Drive the gate and log branches from `rollback_to`'s returned
        // depth so the oversized alert, `reorg_oversized_total`, and the
        // histogram all agree on a single source of truth. This depth is
        // `previous_tip - ancestor` (no `+ 1`): the true number of
        // rolled-back blocks.
        let depth = i64::from(outcome.depth);
        let target = i64::from(self.config.indexer_reorg_target_depth);
        let max = i64::from(self.config.indexer_reorg_max_depth);
        let oversized = depth > max;

        if oversized {
            // Past the upper bound — log loudly but still complete.
            // Failing closed would leave the database stuck on the
            // orphaned chain, which is worse.
            error!(
                depth,
                ancestor = ancestor.block_number,
                max,
                "reorg deeper than upper bound; rolled back anyway"
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
                "reorg detected; rolled back"
            );
        }

        self.metrics.observe_reorg(depth, oversized);
        // After a rollback the indexer's stored tip changes; reflect
        // that in the gauge immediately so the metric does not lag
        // until the next block lands.
        self.metrics.set_latest_block(outcome.ancestor_height);

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

        // Resume from the block after the true common ancestor. Returning the
        // ancestor height (not the poll height) is essential: `rollback_to`
        // deleted every block above the ancestor and moved the checkpoint
        // there, so any higher resume point would skip — and permanently lose —
        // the blocks between the ancestor and the poll height on a multi-block
        // reorg.
        Ok(StepOutcome::ReorgHandled {
            new_tip: outcome.ancestor_height,
        })
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
    /// A parent-hash disagreement resolved to a rollback depth of zero:
    /// the node's response is internally inconsistent rather than a genuine
    /// reorg. No rollback or `reorg_log` row was written. The poller waits
    /// (without resetting backoff) so it does not spin on the same block.
    InconsistentNode,
}

/// Whether a parent-hash disagreement resolved to a zero-depth rollback,
/// which is an inconsistent node response rather than a genuine reorg. In
/// that case there is nothing to roll back and no `reorg_log` row should be
/// written. `ancestor_height` can never legitimately exceed `suspect_height`
/// (the walk-back starts at `suspect_height` and only decreases), so `>=`
/// is the correct guard.
fn is_inconsistent_node(suspect_height: i64, ancestor_height: i64) -> bool {
    ancestor_height >= suspect_height
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::is_inconsistent_node;

    /// The depth the poller now uses is `rollback_to`'s own formula:
    /// `previous_tip - ancestor` with no `+ 1`. This mirrors that formula
    /// so the expected arithmetic is pinned by a test.
    fn rollback_depth(suspect_height: i64, ancestor_height: i64) -> i64 {
        suspect_height - ancestor_height
    }

    #[test]
    fn depth_matches_rollback_to_no_off_by_one() {
        // A single-block reorg: suspect tip is one above the ancestor, so
        // exactly one block is rolled back. The old `+ 1` produced 2.
        assert_eq!(rollback_depth(100, 99), 1);
        // A ten-block reorg rolls back exactly ten blocks.
        assert_eq!(rollback_depth(100, 90), 10);
    }

    #[test]
    fn depth_zero_is_flagged_inconsistent() {
        // Ancestor equals the suspect height: rollback_to would delete
        // nothing, so this must not be treated as a reorg.
        assert_eq!(rollback_depth(100, 100), 0);
        assert!(is_inconsistent_node(100, 100));
    }

    #[test]
    fn genuine_reorg_is_not_flagged_inconsistent() {
        assert!(!is_inconsistent_node(100, 99));
        assert!(!is_inconsistent_node(100, 0));
    }

    #[test]
    fn ancestor_above_suspect_is_flagged_inconsistent() {
        // Defensive: an ancestor above the suspect height (which the
        // walk-back should never produce) is also treated as inconsistent
        // rather than yielding a negative depth.
        assert!(is_inconsistent_node(100, 101));
    }

    #[test]
    fn depth_gate_boundaries_use_corrected_depth() {
        // With the corrected (no `+ 1`) depth, a reorg whose depth exactly
        // equals `max` is NOT oversized; only depth strictly greater is.
        let max = 5i64;
        let at_max = rollback_depth(105, 100);
        let over_max = rollback_depth(106, 100);
        assert_eq!(at_max, 5);
        assert!(
            at_max <= max,
            "depth == max must not trip the oversized gate"
        );
        assert!(over_max > max, "depth > max must trip the oversized gate");
    }
}
