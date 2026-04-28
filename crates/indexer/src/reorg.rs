//! Reorg detection and transactional rollback.
//!
//! When the canonical chain diverges from what the indexer has stored,
//! [`find_common_ancestor`] walks back from the suspected divergence
//! height comparing the node's hash at each height against our stored
//! hash, until they agree. That height is the common ancestor `A`.
//!
//! [`rollback_to`] then performs the rollback inside a single
//! Postgres transaction:
//!
//! 1. Insert a `reorg_log` row in `in_progress`.
//! 2. Restore `consumed_*` columns on cells consumed in any block
//!    above `A` so they return to live status.
//! 3. Delete the affected blocks (cascading to transactions and cells
//!    they created).
//! 4. Advance `indexer_state` to `(A, hash(A))`.
//! 5. Mark the `reorg_log` row `completed`.
//! 6. Commit.
//!
//! Either every step lands or none of them do — the database can never
//! observe a partial rollback.
//!
//! After commit, [`publish_reorg`] emits a JSON payload on the
//! `cellora:reorg` Redis channel so the query plane can invalidate
//! caches. Publishing is fire-and-forget; a Redis outage does not
//! roll back the rollback.

use cellora_common::ckb::CkbClient;
use cellora_db::{blocks, cells, checkpoint, reorg_log, DbError, DbResult};
use chrono::{DateTime, Utc};
use redis::aio::ConnectionManager;
use redis::AsyncCommands;
use serde::Serialize;
use sqlx::PgPool;
use thiserror::Error;
use tracing::{error, info, warn};

/// Channel name used for reorg events.
pub const REORG_CHANNEL: &str = "cellora:reorg";

/// Errors raised during reorg handling.
#[allow(missing_docs)]
#[derive(Debug, Error)]
pub enum ReorgError {
    #[error("ckb rpc error: {0}")]
    Rpc(#[from] cellora_common::error::Error),
    #[error("database error: {0}")]
    Db(#[from] DbError),
    /// The walk-back ran past block 0 without finding a match. Either
    /// the indexed data is corrupt or the node returned an inconsistent
    /// chain — both are operational problems we cannot recover from
    /// without human intervention.
    #[error("no common ancestor found between indexer and node")]
    NoCommonAncestor,
}

/// What [`find_common_ancestor`] returns: the height and node-canonical
/// hash at the divergence point, plus the hash we had stored there
/// (used for the audit log entry).
#[derive(Debug, Clone)]
pub struct Ancestor {
    /// Block number of the common ancestor.
    pub block_number: i64,
    /// Hash the canonical chain has at that height.
    pub node_hash: Vec<u8>,
}

/// Walk back from `(suspect_height, indexed_hash_at_suspect)` comparing
/// the node's hash at each height against our stored hash. Returns the
/// first height where they agree.
///
/// `suspect_height` is the height at which we just observed a
/// disagreement — typically `tip - 1`. This function does not consult
/// the database; the caller passes the indexed hashes via
/// `indexed_hash_at`.
pub async fn find_common_ancestor<F, Fut>(
    ckb: &CkbClient,
    suspect_height: i64,
    indexed_hash_at: F,
) -> Result<Ancestor, ReorgError>
where
    F: Fn(i64) -> Fut,
    Fut: std::future::Future<Output = DbResult<Option<Vec<u8>>>>,
{
    let mut height = suspect_height;
    while height >= 0 {
        let block = ckb
            .get_block_by_number(u64::try_from(height).unwrap_or(0))
            .await?
            .ok_or_else(|| {
                // Node lost the block we just had — treat as divergence
                // we cannot resolve. Caller logs and bails.
                cellora_common::error::Error::CkbRpc {
                    code: -32000,
                    message: format!("node missing block {height} during reorg walk"),
                }
            })?;
        let node_hash = block.header.hash.0.to_vec();

        let indexed = indexed_hash_at(height).await?;
        if indexed.as_deref() == Some(node_hash.as_slice()) {
            return Ok(Ancestor {
                block_number: height,
                node_hash,
            });
        }
        height -= 1;
    }
    Err(ReorgError::NoCommonAncestor)
}

/// Run the rollback transaction. Returns the new tip height after
/// rollback (i.e., `ancestor.block_number`).
///
/// `previous_tip` is the height at which the indexer was committed
/// before detection — used to compute `depth` for the audit row.
pub async fn rollback_to(
    pool: &PgPool,
    ancestor: &Ancestor,
    previous_tip: i64,
    indexed_hash_at_divergence: &[u8],
) -> Result<RollbackOutcome, ReorgError> {
    // The "divergence height" recorded in the log is the first height
    // *above* the ancestor where we disagree with the chain — i.e.,
    // `ancestor.block_number + 1`. Using that lets an operator reading
    // the log answer "which block did our chain part ways at?" directly.
    let divergence_height = ancestor.block_number.saturating_add(1);
    let depth = i32::try_from(previous_tip - ancestor.block_number).unwrap_or(i32::MAX);

    let mut tx = pool.begin().await.map_err(DbError::from)?;

    let log_id = reorg_log::insert(
        &mut tx,
        divergence_height,
        &ancestor.node_hash, // canonical hash at the ancestor — caller can pull (ancestor+1) if they want
        indexed_hash_at_divergence,
        depth,
    )
    .await?;

    let restored_cells = cells::restore_consumed_above(&mut tx, ancestor.block_number).await?;
    let deleted_blocks = blocks::delete_above(&mut tx, ancestor.block_number).await?;
    checkpoint::upsert(&mut tx, ancestor.block_number, &ancestor.node_hash).await?;
    reorg_log::mark_completed(&mut tx, log_id).await?;

    tx.commit().await.map_err(DbError::from)?;

    Ok(RollbackOutcome {
        log_id,
        depth,
        deleted_blocks,
        restored_cells,
        ancestor_height: ancestor.block_number,
    })
}

/// Stats returned from a successful rollback. Logged at INFO and used
/// by metrics in slice 2.
#[derive(Debug)]
pub struct RollbackOutcome {
    /// `reorg_log.id` for the row written by this rollback.
    pub log_id: i64,
    /// Number of blocks rolled back (i.e., `previous_tip - ancestor`).
    pub depth: i32,
    /// Number of block rows deleted from `blocks`.
    pub deleted_blocks: u64,
    /// Number of cells whose `consumed_*` columns were restored.
    pub restored_cells: u64,
    /// New tip height after the rollback.
    pub ancestor_height: i64,
}

/// JSON payload broadcast on `cellora:reorg`.
#[derive(Debug, Serialize)]
pub struct ReorgEvent {
    /// New canonical tip height after the rollback.
    pub ancestor_block_number: i64,
    /// Hex-encoded canonical hash at the ancestor.
    pub ancestor_hash: String,
    /// Number of blocks rolled back.
    pub depth: i32,
    /// When the rollback completed (server wall clock).
    pub completed_at: DateTime<Utc>,
}

/// Best-effort publish of a reorg event on the `cellora:reorg`
/// channel. Errors are logged at WARN and otherwise swallowed — a
/// Redis outage cannot un-do a successful rollback, and the API's tip
/// cache will eventually re-poll.
pub async fn publish_reorg(redis: Option<&ConnectionManager>, event: &ReorgEvent) {
    let Some(conn) = redis else {
        return;
    };
    let payload = match serde_json::to_string(event) {
        Ok(s) => s,
        Err(err) => {
            error!(error = %err, "serialise reorg event failed");
            return;
        }
    };
    let mut conn = conn.clone();
    let publish: redis::RedisResult<i64> = conn.publish(REORG_CHANNEL, payload).await;
    match publish {
        Ok(receivers) => {
            info!(receivers, channel = REORG_CHANNEL, "published reorg event");
        }
        Err(err) => {
            warn!(error = %err, "publish reorg event failed");
        }
    }
}
