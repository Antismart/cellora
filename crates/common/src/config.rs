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
}

fn default_poll_interval_ms() -> u64 {
    2000
}

fn default_log_level() -> String {
    "info".to_owned()
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
