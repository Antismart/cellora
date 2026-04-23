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

impl Config {
    /// Load configuration from environment variables prefixed with `CELLORA_`.
    pub fn from_env() -> Result<Self> {
        Figment::new()
            .merge(Env::prefixed(ENV_PREFIX).split("__"))
            .extract()
            .map_err(|err| Error::Config(err.to_string()))
    }
}
