//! Runtime configuration loaded from environment variables.

use figment::{providers::Env, Figment};
use serde::Deserialize;

use crate::error::{Error, Result};

/// Prefix for all cellora-owned environment variables.
const ENV_PREFIX: &str = "CELLORA_";

/// Log output format selector.
#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    /// JSON lines — one structured event per line. Default in production.
    #[default]
    Json,
    /// Human-readable coloured output — used in local development.
    Pretty,
}

/// Runtime configuration for every cellora service.
#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    /// Postgres connection string (libpq format).
    pub database_url: String,
    /// CKB JSON-RPC endpoint (`http://host:port`).
    pub ckb_rpc_url: String,
    /// Delay between polls when the indexer has caught up to the tip.
    #[serde(default = "default_poll_interval_ms")]
    pub poll_interval_ms: u64,
    /// Block number to start indexing from on a fresh database.
    #[serde(default)]
    pub indexer_start_block: u64,
    /// Tracing `EnvFilter` string.
    #[serde(default = "default_log_level")]
    pub log_level: String,
    /// Log output format.
    #[serde(default)]
    pub log_format: LogFormat,

    /// Socket address the API binary binds to (e.g. `0.0.0.0:8080`).
    #[serde(default = "default_api_bind_addr")]
    pub api_bind_addr: String,
    /// Default page size applied when a request does not specify `limit`.
    #[serde(default = "default_api_default_page_size")]
    pub api_default_page_size: u32,
    /// Upper bound on `limit` accepted from clients.
    #[serde(default = "default_api_max_page_size")]
    pub api_max_page_size: u32,
    /// Per-request timeout enforced by the HTTP middleware stack.
    #[serde(default = "default_api_request_timeout_ms")]
    pub api_request_timeout_ms: u64,
    /// Refresh interval for the cached `(indexer_tip, node_tip)` snapshot.
    #[serde(default = "default_api_tip_cache_refresh_ms")]
    pub api_tip_cache_refresh_ms: u64,

    /// Time-to-live for entries in the in-process auth verification cache.
    /// Keeps Argon2 verification off the hot path for repeat-presented
    /// keys; revocation is best-effort within this window.
    #[serde(default = "default_api_auth_cache_ttl_seconds")]
    pub api_auth_cache_ttl_seconds: u64,
    /// Maximum number of entries in the auth verification cache.
    #[serde(default = "default_api_auth_cache_capacity")]
    pub api_auth_cache_capacity: u64,

    /// Redis URL used for the per-key rate limiter (and, in later weeks,
    /// reorg pub/sub and webhook delivery).
    #[serde(default = "default_redis_url")]
    pub redis_url: String,
    /// When `true`, rate limiting is bypassed if Redis is unreachable.
    /// Failing closed on a Redis outage would take the API down with no
    /// upside; failing open trades a bounded period of unmetered traffic
    /// for continued availability. Operators who want fail-closed flip
    /// this to `false`.
    #[serde(default = "default_api_rate_limit_fail_open")]
    pub api_rate_limit_fail_open: bool,

    /// Free-tier REST burst capacity (max tokens in the bucket).
    #[serde(default = "default_free_rest_burst")]
    pub api_rate_limit_free_rest_burst: u32,
    /// Free-tier REST refill rate, tokens per second.
    #[serde(default = "default_free_rest_refill")]
    pub api_rate_limit_free_rest_refill_per_sec: f64,
    /// Starter-tier REST burst capacity.
    #[serde(default = "default_starter_rest_burst")]
    pub api_rate_limit_starter_rest_burst: u32,
    /// Starter-tier REST refill rate, tokens per second.
    #[serde(default = "default_starter_rest_refill")]
    pub api_rate_limit_starter_rest_refill_per_sec: f64,
    /// Pro-tier REST burst capacity.
    #[serde(default = "default_pro_rest_burst")]
    pub api_rate_limit_pro_rest_burst: u32,
    /// Pro-tier REST refill rate, tokens per second.
    #[serde(default = "default_pro_rest_refill")]
    pub api_rate_limit_pro_rest_refill_per_sec: f64,

    /// Free-tier GraphQL burst capacity.
    #[serde(default = "default_free_graphql_burst")]
    pub api_rate_limit_free_graphql_burst: u32,
    /// Free-tier GraphQL refill rate, tokens per second.
    #[serde(default = "default_free_graphql_refill")]
    pub api_rate_limit_free_graphql_refill_per_sec: f64,
    /// Starter-tier GraphQL burst capacity.
    #[serde(default = "default_starter_graphql_burst")]
    pub api_rate_limit_starter_graphql_burst: u32,
    /// Starter-tier GraphQL refill rate, tokens per second.
    #[serde(default = "default_starter_graphql_refill")]
    pub api_rate_limit_starter_graphql_refill_per_sec: f64,
    /// Pro-tier GraphQL burst capacity.
    #[serde(default = "default_pro_graphql_burst")]
    pub api_rate_limit_pro_graphql_burst: u32,
    /// Pro-tier GraphQL refill rate, tokens per second.
    #[serde(default = "default_pro_graphql_refill")]
    pub api_rate_limit_pro_graphql_refill_per_sec: f64,
}

fn default_poll_interval_ms() -> u64 {
    2000
}

fn default_log_level() -> String {
    "info".to_owned()
}

fn default_api_bind_addr() -> String {
    "0.0.0.0:8080".to_owned()
}

fn default_api_default_page_size() -> u32 {
    50
}

fn default_api_max_page_size() -> u32 {
    500
}

fn default_api_request_timeout_ms() -> u64 {
    10_000
}

fn default_api_tip_cache_refresh_ms() -> u64 {
    1_000
}

fn default_api_auth_cache_ttl_seconds() -> u64 {
    60
}

fn default_api_auth_cache_capacity() -> u64 {
    10_000
}

fn default_redis_url() -> String {
    "redis://localhost:6379".to_owned()
}

fn default_api_rate_limit_fail_open() -> bool {
    true
}

fn default_free_rest_burst() -> u32 {
    30
}
fn default_free_rest_refill() -> f64 {
    1.0
}
fn default_starter_rest_burst() -> u32 {
    300
}
fn default_starter_rest_refill() -> f64 {
    20.0
}
fn default_pro_rest_burst() -> u32 {
    3_000
}
fn default_pro_rest_refill() -> f64 {
    200.0
}

fn default_free_graphql_burst() -> u32 {
    10
}
fn default_free_graphql_refill() -> f64 {
    0.5
}
fn default_starter_graphql_burst() -> u32 {
    100
}
fn default_starter_graphql_refill() -> f64 {
    10.0
}
fn default_pro_graphql_burst() -> u32 {
    1_000
}
fn default_pro_graphql_refill() -> f64 {
    100.0
}

impl Config {
    /// Load configuration from environment variables prefixed with `CELLORA_`.
    pub fn from_env() -> Result<Self> {
        Figment::new()
            .merge(Env::prefixed(ENV_PREFIX).split("__"))
            .extract()
            .map_err(|err| Error::Config(err.to_string()))
    }
}
