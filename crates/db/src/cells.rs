//! Queries against the `cells` table.

use sqlx::{Postgres, Transaction};

use crate::error::DbResult;
use crate::models::{CellRow, ConsumedCellRef};

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
