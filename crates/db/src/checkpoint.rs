//! Read and write the singleton `indexer_state` row that tracks how far the
//! indexer has progressed.

use sqlx::{PgPool, Postgres, Transaction};

use crate::error::DbResult;
use crate::models::Checkpoint;

/// Read the current checkpoint, or `None` on a freshly migrated database.
pub async fn read(pool: &PgPool) -> DbResult<Option<Checkpoint>> {
    let rec = sqlx::query!(
        r#"
        SELECT last_indexed_block, last_indexed_hash
        FROM indexer_state
        WHERE id = 1
        "#
    )
    .fetch_optional(pool)
    .await?;

    Ok(rec.map(|r| Checkpoint {
        last_indexed_block: r.last_indexed_block,
        last_indexed_hash: r.last_indexed_hash,
    }))
}

/// Upsert the checkpoint, setting it to the given block number / hash.
pub async fn upsert(
    tx: &mut Transaction<'_, Postgres>,
    last_indexed_block: i64,
    last_indexed_hash: &[u8],
) -> DbResult<()> {
    sqlx::query!(
        r#"
        INSERT INTO indexer_state (id, last_indexed_block, last_indexed_hash, updated_at)
        VALUES (1, $1, $2, now())
        ON CONFLICT (id) DO UPDATE
            SET last_indexed_block = EXCLUDED.last_indexed_block,
                last_indexed_hash  = EXCLUDED.last_indexed_hash,
                updated_at         = now()
        "#,
        last_indexed_block,
        last_indexed_hash,
    )
    .execute(&mut **tx)
    .await?;
    Ok(())
}
