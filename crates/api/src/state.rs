//! Shared application state handed to every Axum handler.
//!
//! `AppState` is cloned per-request by Axum. The fields inside it are either
//! cheap-to-clone (e.g. [`sqlx::PgPool`] holds an `Arc` internally) or wrapped
//! in `Arc` so cloning only touches refcounts.

use std::sync::Arc;
use std::time::Duration;

use cellora_common::config::Config;
use sqlx::PgPool;

use crate::auth::AuthCache;
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
    /// In-process cache for resolved API keys. Bypasses Argon2
    /// verification for repeat-presented bearer tokens.
    pub auth_cache: AuthCache,
}

impl AppState {
    /// Build a new [`AppState`] with a fresh (empty) [`TipTracker`] and
    /// an auth cache sized from the supplied [`Config`].
    pub fn new(db: PgPool, config: Config) -> Self {
        let auth_cache = AuthCache::new(
            config.api_auth_cache_capacity,
            Duration::from_secs(config.api_auth_cache_ttl_seconds),
        );
        Self {
            db,
            config: Arc::new(config),
            tip: TipTracker::new(),
            auth_cache,
        }
    }

    /// Build a state with a caller-supplied tracker. Used by tests that
    /// want to poke a snapshot in before issuing requests, and by main
    /// when the tracker needs to be shared with the refresh task.
    pub fn with_tip(db: PgPool, config: Config, tip: TipTracker) -> Self {
        let auth_cache = AuthCache::new(
            config.api_auth_cache_capacity,
            Duration::from_secs(config.api_auth_cache_ttl_seconds),
        );
        Self {
            db,
            config: Arc::new(config),
            tip,
            auth_cache,
        }
    }
}
