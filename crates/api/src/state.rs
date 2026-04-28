//! Shared application state handed to every Axum handler.
//!
//! `AppState` is cloned per-request by Axum. The fields inside it are either
//! cheap-to-clone (e.g. [`sqlx::PgPool`] holds an `Arc` internally) or wrapped
//! in `Arc` so cloning only touches refcounts.

use std::sync::Arc;
use std::time::Duration;

use cellora_common::ckb::CkbClient;
use cellora_common::config::Config;
use redis::aio::ConnectionManager;
use sqlx::PgPool;

use crate::auth::AuthCache;
use crate::metrics::Metrics;
use crate::ratelimit::RateLimiter;
use crate::tip::TipTracker;

/// Application state injected into every handler.
#[derive(Clone)]
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
    /// Per-key Redis-backed rate limiter, or `None` when the limiter
    /// could not be initialised at startup. A missing limiter is treated
    /// as fail-open by the middleware.
    pub rate_limiter: Option<RateLimiter>,
    /// Prometheus metrics handles. Cheap to clone, shared across
    /// middleware and the `/metrics` route.
    pub metrics: Metrics,
    /// Direct handle to Redis used by the readiness probe (the rate
    /// limiter holds its own copy internally; that one is not exposed
    /// for this purpose). `None` skips the probe.
    pub redis: Option<ConnectionManager>,
    /// CKB JSON-RPC client used by the readiness probe to check that
    /// the upstream node is reachable and not in initial block
    /// download. `None` skips the probe.
    pub ckb: Option<CkbClient>,
}

impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .field("config", &self.config)
            .field("rate_limiter_attached", &self.rate_limiter.is_some())
            .field("redis_attached", &self.redis.is_some())
            .field("ckb_attached", &self.ckb.is_some())
            .finish_non_exhaustive()
    }
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
            rate_limiter: None,
            metrics: Metrics::new(),
            redis: None,
            ckb: None,
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
            rate_limiter: None,
            metrics: Metrics::new(),
            redis: None,
            ckb: None,
        }
    }

    /// Replace the rate limiter on an existing state. The limiter is
    /// initialised after construction in `main` because building it
    /// requires an async Redis connection.
    pub fn with_rate_limiter(mut self, limiter: RateLimiter) -> Self {
        self.rate_limiter = Some(limiter);
        self
    }

    /// Attach a Redis connection so the readiness probe can ping it.
    /// Independent of the rate limiter — both share the underlying
    /// connection-manager `Arc`, but each has its own handle.
    pub fn with_redis(mut self, redis: ConnectionManager) -> Self {
        self.redis = Some(redis);
        self
    }

    /// Attach a CKB JSON-RPC client so the readiness probe can verify
    /// the upstream node is reachable and synced.
    pub fn with_ckb(mut self, ckb: CkbClient) -> Self {
        self.ckb = Some(ckb);
        self
    }
}
