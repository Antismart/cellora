//! Queries against the `reorg_log` audit table.
//!
//! Lifecycle: a reorg is detected, a row is inserted with
//! `status = 'in_progress'`, the rollback is performed in the same
//! transaction, and the row is updated to `'completed'` (or `'failed'`).
//! The `id` returned by [`insert`] is the handle the rollback uses for
//! the eventual update.

use sqlx::{Postgres, Transaction};

use crate::error::DbResult;
use crate::models::{ReorgLogEntry, ReorgStatus};

/// Insert a new `in_progress` reorg row. Returns the row id used by
/// [`mark_completed`] / [`mark_failed`] to advance its status.
pub async fn insert(
    tx: &mut Transaction<'_, Postgres>,
    divergence_block_number: i64,
    divergence_node_hash: &[u8],
    divergence_indexed_hash: &[u8],
    depth: i32,
) -> DbResult<i64> {
    let row = sqlx::query!(
        r#"
        INSERT INTO reorg_log (
            divergence_block_number,
            divergence_node_hash,
            divergence_indexed_hash,
            depth
        )
        VALUES ($1, $2, $3, $4)
        RETURNING id
        "#,
        divergence_block_number,
        divergence_node_hash,
        divergence_indexed_hash,
        depth,
    )
    .fetch_one(&mut **tx)
    .await?;
    Ok(row.id)
}

/// Mark a previously-inserted reorg row as `completed` and stamp
/// `completed_at`. Called inside the same transaction that performed
/// the rollback, immediately before commit.
pub async fn mark_completed(tx: &mut Transaction<'_, Postgres>, id: i64) -> DbResult<()> {
    sqlx::query!(
        r#"
        UPDATE reorg_log
        SET status = 'completed', completed_at = now()
        WHERE id = $1
        "#,
        id,
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Mark a reorg row as `failed`, recording the error message. Used
/// when the rollback transaction itself fails — the surrounding code
/// re-opens a fresh transaction to write this.
pub async fn mark_failed(tx: &mut Transaction<'_, Postgres>, id: i64, error: &str) -> DbResult<()> {
    sqlx::query!(
        r#"
        UPDATE reorg_log
        SET status = 'failed', completed_at = now(), error = $2
        WHERE id = $1
        "#,
        id,
        error,
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Read the most recent `limit` rows for an admin / debugging surface.
/// Returned newest first.
pub async fn list_recent(pool: &sqlx::PgPool, limit: i64) -> DbResult<Vec<ReorgLogEntry>> {
    let rows = sqlx::query_as!(
        ReorgLogEntry,
        r#"
        SELECT
            id,
            detected_at,
            divergence_block_number,
            divergence_node_hash,
            divergence_indexed_hash,
            depth,
            completed_at,
            status AS "status: ReorgStatus",
            error
        FROM reorg_log
        ORDER BY detected_at DESC
        LIMIT $1
        "#,
        limit,
    )
    .fetch_all(pool)
    .await?;
    Ok(rows)
}
