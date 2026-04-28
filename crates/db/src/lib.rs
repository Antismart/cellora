//! Database layer for the Cellora indexer.
//!
//! All SQL queries are compile-time checked via [`sqlx::query!`] /
//! [`sqlx::query_as!`]. The repository modules (`blocks`, `transactions`,
//! `cells`, `checkpoint`) take a `sqlx::Postgres` executor, so callers decide
//! whether to run a statement on a pool or inside an open transaction.

pub mod api_keys;
pub mod blocks;
pub mod cells;
pub mod checkpoint;
pub mod error;
pub mod migrate;
pub mod models;
pub mod pool;
pub mod reorg_log;
pub mod transactions;

pub use error::{DbError, DbResult};
pub use pool::connect;
