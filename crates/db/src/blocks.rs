//! Queries against the `blocks` table.

use sqlx::{PgExecutor, PgPool};

use crate::error::DbResult;
use crate::models::BlockRow;

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
