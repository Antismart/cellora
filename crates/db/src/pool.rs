//! Postgres connection pool construction.

use std::time::Duration;

use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;

use crate::error::DbResult;

/// Build a pool using production-sane defaults.
///
/// * `max_connections = 16`
/// * `acquire_timeout = 5 s`
/// * `test_before_acquire = true` (cheap liveness check; protects against
///   stale connections after Postgres restarts).
pub async fn connect(database_url: &str) -> DbResult<PgPool> {
    let pool = PgPoolOptions::new()
        .max_connections(16)
        .acquire_timeout(Duration::from_secs(5))
        .test_before_acquire(true)
        .connect(database_url)
        .await?;
    Ok(pool)
}
