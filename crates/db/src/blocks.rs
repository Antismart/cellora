//! Queries against the `blocks` table.

use sqlx::{PgExecutor, PgPool};

use crate::error::DbResult;
use crate::models::{Block, BlockRow};

/// Insert a single block row.
pub async fn insert<'e, E>(executor: E, row: &BlockRow) -> DbResult<()>
where
    E: PgExecutor<'e>,
{
    sqlx::query!(
        r#"
        INSERT INTO blocks (
            number, hash, parent_hash, timestamp_ms, epoch,
            transactions_count, proposals_count, uncles_count, nonce, dao
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
        "#,
        row.number,
        &row.hash,
        &row.parent_hash,
        row.timestamp_ms,
        row.epoch,
        row.transactions_count,
        row.proposals_count,
        row.uncles_count,
        row.nonce,
        &row.dao,
    )
    .execute(executor)
    .await?;
    Ok(())
}

/// Highest block number currently stored, or `None` if the table is empty.
pub async fn latest_number(pool: &PgPool) -> DbResult<Option<i64>> {
    let rec = sqlx::query!("SELECT MAX(number) AS max FROM blocks")
        .fetch_one(pool)
        .await?;
    Ok(rec.max)
}

/// Fetch the highest-numbered block, or `None` if the table is empty.
pub async fn latest(pool: &PgPool) -> DbResult<Option<Block>> {
    let row = sqlx::query_as!(
        Block,
        r#"
        SELECT number, hash, parent_hash, timestamp_ms, epoch,
               transactions_count, proposals_count, uncles_count,
               nonce, dao, indexed_at
        FROM blocks
        ORDER BY number DESC
        LIMIT 1
        "#,
    )
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// Hash of the indexed block at the given height, or `None` if no
/// block at that height is stored. Used by the reorg detector to
/// compare against the chain's view at the same height.
pub async fn hash_at(pool: &PgPool, number: i64) -> DbResult<Option<Vec<u8>>> {
    let rec = sqlx::query!("SELECT hash FROM blocks WHERE number = $1", number,)
        .fetch_optional(pool)
        .await?;
    Ok(rec.map(|r| r.hash))
}

/// Delete every block whose number is strictly greater than `ancestor`.
/// `ON DELETE CASCADE` on `transactions` and `cells` removes their
/// associated rows. The caller is responsible for restoring `is_live`
/// state on cells that were consumed in the deleted range —
/// see [`crate::cells::restore_consumed_above`].
///
/// Returns the count of block rows removed (the rollback depth).
pub async fn delete_above(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ancestor: i64,
) -> DbResult<u64> {
    let result = sqlx::query!("DELETE FROM blocks WHERE number > $1", ancestor)
        .execute(&mut **tx)
        .await?;
    Ok(result.rows_affected())
}

/// Fetch a block by its block number, or `None` when no such block is
/// indexed yet.
pub async fn get_by_number(pool: &PgPool, number: i64) -> DbResult<Option<Block>> {
    let row = sqlx::query_as!(
        Block,
        r#"
        SELECT number, hash, parent_hash, timestamp_ms, epoch,
               transactions_count, proposals_count, uncles_count,
               nonce, dao, indexed_at
        FROM blocks
        WHERE number = $1
        "#,
        number,
    )
    .fetch_optional(pool)
    .await?;
    Ok(row)
}
