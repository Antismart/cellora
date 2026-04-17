//! Queries against the `transactions` table.

use sqlx::{Postgres, Transaction};

use crate::error::DbResult;
use crate::models::TransactionRow;

/// Insert a batch of transactions belonging to a single block. Runs one
/// statement per row for Week 1 simplicity; bulk inserts via `UNNEST` are a
/// performance concern revisited once we index mainnet volume.
pub async fn insert_batch(
    tx: &mut Transaction<'_, Postgres>,
    rows: &[TransactionRow],
) -> DbResult<()> {
    for row in rows {
        sqlx::query!(
            r#"
            INSERT INTO transactions (
                hash, block_number, tx_index, version,
                cell_deps, header_deps, witnesses,
                inputs_count, outputs_count
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
            "#,
            &row.hash,
            row.block_number,
            row.tx_index,
            row.version,
            row.cell_deps,
            row.header_deps,
            row.witnesses,
            row.inputs_count,
            row.outputs_count,
        )
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}
