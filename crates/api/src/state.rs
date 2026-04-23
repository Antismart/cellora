//! Shared application state handed to every Axum handler.
//!
//! `AppState` is cloned per-request by Axum. The fields inside it are either
//! cheap-to-clone (e.g. [`sqlx::PgPool`] holds an `Arc` internally) or wrapped
//! in `Arc` so cloning only touches refcounts.

use std::sync::Arc;

use cellora_common::config::Config;
use sqlx::PgPool;

use crate::tip::TipTracker;

/// Application state injected into every handler.
#[derive(Debug, Clone)]
pub struct AppState {
    /// Postgres connection pool.
    pub db: PgPool,
    /// Shared runtime configuration.
    pub config: Arc<Config>,
    /// Lock-free view of the latest indexer / node tip snapshot.
    pub tip: TipTracker,
}

impl AppState {
    /// Build a new [`AppState`] with a fresh (empty) [`TipTracker`].
    /// The tracker remains empty until the refresh task spawned by
    /// `cellora_api::tip::spawn_refresh_task` publishes a snapshot.
    pub fn new(db: PgPool, config: Config) -> Self {
        Self {
            db,
            config: Arc::new(config),
            tip: TipTracker::new(),
        }
    }

    /// Build a state with a caller-supplied tracker. Used by tests that
    /// want to poke a snapshot in before issuing requests, and by main
    /// when the tracker needs to be shared with the refresh task.
    pub fn with_tip(db: PgPool, config: Config, tip: TipTracker) -> Self {
        Self {
            db,
            config: Arc::new(config),
            tip,
        }
    }
}
