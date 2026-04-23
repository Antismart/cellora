//! Queries against the `cells` table.

use sqlx::{PgPool, Postgres, Transaction};

use crate::error::DbResult;
use crate::models::{Cell, CellRow, ConsumedCellRef};

/// Filter on liveness. `None` returns both live and consumed cells.
#[derive(Debug, Clone, Copy, Default)]
pub enum LivenessFilter {
    /// Only cells with `consumed_by_tx_hash IS NULL`.
    OnlyLive,
    /// Only cells with `consumed_by_tx_hash IS NOT NULL`.
    OnlyConsumed,
    /// No liveness filter applied.
    #[default]
    Any,
}

impl LivenessFilter {
    fn as_bool(self) -> Option<bool> {
        match self {
            Self::OnlyLive => Some(true),
            Self::OnlyConsumed => Some(false),
            Self::Any => None,
        }
    }
}

/// Keyset cursor for cell list pagination. Encodes the last `(block_number,
/// tx_hash, output_index)` returned in the previous page. Ordering is
/// `DESC` on all three columns so the single tuple `<` comparison produces
/// the correct next page.
#[derive(Debug, Clone)]
pub struct CellCursor {
    /// Block number of the last row returned.
    pub block_number: i64,
    /// Transaction hash of the last row returned.
    pub tx_hash: Vec<u8>,
    /// Output index of the last row returned.
    pub output_index: i32,
}

/// Insert a batch of cells belonging to a single block. Written row-by-row for
/// Week 1; can be moved to `UNNEST`-based bulk insert as a later optimisation.
pub async fn insert_batch(tx: &mut Transaction<'_, Postgres>, rows: &[CellRow]) -> DbResult<()> {
    for row in rows {
        sqlx::query!(
            r#"
            INSERT INTO cells (
                tx_hash, output_index, block_number, capacity_shannons,
                lock_code_hash, lock_hash_type, lock_args, lock_hash,
                type_code_hash, type_hash_type, type_args, type_hash,
                data
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
            "#,
            &row.tx_hash,
            row.output_index,
            row.block_number,
            row.capacity_shannons,
            &row.lock_code_hash,
            row.lock_hash_type.as_i16(),
            &row.lock_args,
            &row.lock_hash,
            row.type_code_hash.as_deref(),
            row.type_hash_type.map(|h| h.as_i16()),
            row.type_args.as_deref(),
            row.type_hash.as_deref(),
            &row.data,
        )
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

/// Mark each referenced cell as consumed by the given input.
pub async fn mark_consumed(
    tx: &mut Transaction<'_, Postgres>,
    refs: &[ConsumedCellRef],
) -> DbResult<()> {
    for r in refs {
        sqlx::query!(
            r#"
            UPDATE cells
               SET consumed_by_tx_hash      = $3,
                   consumed_by_input_index  = $4,
                   consumed_at_block_number = $5
             WHERE tx_hash = $1 AND output_index = $2
            "#,
            &r.tx_hash,
            r.output_index,
            &r.consumed_by_tx_hash,
            r.consumed_by_input_index,
            r.consumed_at_block_number,
        )
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

/// Page of cells filtered by lock hash.
///
/// Results are ordered `(block_number DESC, tx_hash DESC, output_index DESC)`
/// and joined with the `blocks` table so each row carries `block_hash`.
/// The cursor encodes the last row seen; passing `None` returns the first
/// page.
pub async fn query_by_lock_hash(
    pool: &PgPool,
    lock_hash: &[u8],
    liveness: LivenessFilter,
    cursor: Option<&CellCursor>,
    limit: i64,
) -> DbResult<Vec<Cell>> {
    let is_live = liveness.as_bool();
    let (cursor_bn, cursor_tx, cursor_oi) = cursor_fields(cursor);

    let rows = sqlx::query_as!(
        Cell,
        r#"
        SELECT
            c.tx_hash,
            c.output_index,
            c.block_number,
            b.hash AS block_hash,
            c.capacity_shannons,
            c.lock_code_hash,
            c.lock_hash_type,
            c.lock_args,
            c.lock_hash,
            c.type_code_hash,
            c.type_hash_type,
            c.type_args,
            c.type_hash,
            c.data,
            c.consumed_by_tx_hash,
            c.consumed_by_input_index,
            c.consumed_at_block_number
        FROM cells c
        INNER JOIN blocks b ON b.number = c.block_number
        WHERE c.lock_hash = $1
          AND ($2::boolean IS NULL OR (c.consumed_by_tx_hash IS NULL) = $2)
          AND ($3::bigint IS NULL
               OR (c.block_number, c.tx_hash, c.output_index) < ($3, $4::bytea, $5::integer))
        ORDER BY c.block_number DESC, c.tx_hash DESC, c.output_index DESC
        LIMIT $6
        "#,
        lock_hash,
        is_live,
        cursor_bn,
        cursor_tx,
        cursor_oi,
        limit,
    )
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Page of cells filtered by type hash. See [`query_by_lock_hash`] for the
/// ordering and cursor semantics.
pub async fn query_by_type_hash(
    pool: &PgPool,
    type_hash: &[u8],
    liveness: LivenessFilter,
    cursor: Option<&CellCursor>,
    limit: i64,
) -> DbResult<Vec<Cell>> {
    let is_live = liveness.as_bool();
    let (cursor_bn, cursor_tx, cursor_oi) = cursor_fields(cursor);

    let rows = sqlx::query_as!(
        Cell,
        r#"
        SELECT
            c.tx_hash,
            c.output_index,
            c.block_number,
            b.hash AS block_hash,
            c.capacity_shannons,
            c.lock_code_hash,
            c.lock_hash_type,
            c.lock_args,
            c.lock_hash,
            c.type_code_hash,
            c.type_hash_type,
            c.type_args,
            c.type_hash,
            c.data,
            c.consumed_by_tx_hash,
            c.consumed_by_input_index,
            c.consumed_at_block_number
        FROM cells c
        INNER JOIN blocks b ON b.number = c.block_number
        WHERE c.type_hash = $1
          AND ($2::boolean IS NULL OR (c.consumed_by_tx_hash IS NULL) = $2)
          AND ($3::bigint IS NULL
               OR (c.block_number, c.tx_hash, c.output_index) < ($3, $4::bytea, $5::integer))
        ORDER BY c.block_number DESC, c.tx_hash DESC, c.output_index DESC
        LIMIT $6
        "#,
        type_hash,
        is_live,
        cursor_bn,
        cursor_tx,
        cursor_oi,
        limit,
    )
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Destructure an optional cursor into the three parameter slots used by
/// the paginated queries. Using `Option<T>` per column lets sqlx's
/// macro-level type inference pick up the placeholder types.
fn cursor_fields(cursor: Option<&CellCursor>) -> (Option<i64>, Option<&[u8]>, Option<i32>) {
    match cursor {
        Some(c) => (Some(c.block_number), Some(&c.tx_hash), Some(c.output_index)),
        None => (None, None, None),
    }
}
