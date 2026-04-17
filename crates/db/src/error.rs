//! Database error type used by the repository modules.

use thiserror::Error;

/// Convenience alias for operations returning a [`DbError`].
pub type DbResult<T> = std::result::Result<T, DbError>;

/// Errors surfaced by the `cellora-db` crate.
#[allow(missing_docs)]
#[derive(Debug, Error)]
pub enum DbError {
    /// Low-level sqlx / driver failure.
    #[error("sqlx error: {0}")]
    Sqlx(#[from] sqlx::Error),

    /// The migrator failed to apply a migration.
    #[error("migration error: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),

    /// An invariant between rows was violated (e.g. batch length mismatch).
    #[error("{0}")]
    Invariant(&'static str),
}
