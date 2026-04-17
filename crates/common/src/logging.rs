//! Structured logging initialisation for every cellora binary.

use tracing_subscriber::{fmt, prelude::*, EnvFilter};

use crate::config::LogFormat;
use crate::error::{Error, Result};

/// Install the global tracing subscriber.
///
/// * `filter` — a `tracing_subscriber::EnvFilter` expression, e.g. `"info"`
///   or `"cellora_indexer=debug,sqlx=warn"`.
/// * `format` — JSON for production, Pretty for local development.
///
/// Calling this more than once will return an error on the second call.
pub fn init(filter: &str, format: LogFormat) -> Result<()> {
    let env_filter = EnvFilter::try_new(filter)
        .map_err(|err| Error::Logging(format!("invalid log filter '{filter}': {err}")))?;

    let registry = tracing_subscriber::registry().with(env_filter);

    match format {
        LogFormat::Json => registry
            .with(fmt::layer().json().with_current_span(false))
            .try_init()
            .map_err(|err| Error::Logging(err.to_string())),
        LogFormat::Pretty => registry
            .with(fmt::layer().pretty())
            .try_init()
            .map_err(|err| Error::Logging(err.to_string())),
    }
}
