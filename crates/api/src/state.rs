//! Shared application state handed to every Axum handler.
//!
//! `AppState` is cloned per-request by Axum. The fields inside it are either
//! cheap-to-clone (e.g. [`sqlx::PgPool`] holds an `Arc` internally) or wrapped
//! in `Arc` so cloning only touches refcounts.

use std::sync::Arc;

use cellora_common::config::Config;
use sqlx::PgPool;

/// Application state injected into every handler.
#[derive(Debug, Clone)]
pub struct AppState {
    /// Postgres connection pool.
    pub db: PgPool,
    /// Shared runtime configuration.
    pub config: Arc<Config>,
}

impl AppState {
    /// Build a new [`AppState`] from an existing pool and config.
    pub fn new(db: PgPool, config: Config) -> Self {
        Self {
            db,
            config: Arc::new(config),
        }
    }
}
