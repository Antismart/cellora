//! Migration runner thin-wrapper around [`sqlx::migrate!`].

use sqlx::PgPool;

use crate::error::DbResult;

/// Apply every pending migration embedded at compile time from the workspace
/// `migrations/` directory. Idempotent.
pub async fn run(pool: &PgPool) -> DbResult<()> {
    sqlx::migrate!("../../migrations").run(pool).await?;
    Ok(())
}
