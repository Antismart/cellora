//! Structured logging and (optional) OpenTelemetry trace export.
//!
//! [`init`] installs the global `tracing` subscriber. It always wires a
//! `tracing-subscriber` formatter (JSON in production, pretty in
//! development) plus an `EnvFilter`. When [`OtelConfig::endpoint`] is
//! `Some`, an additional `tracing-opentelemetry` layer is attached and
//! a `tracing-opentelemetry` bridge forwards spans to an OTLP HTTP
//! collector.
//!
//! The function returns a [`TracingGuard`]. Holding the guard keeps
//! the OTel SDK alive; dropping it on graceful shutdown flushes any
//! pending spans. The guard's `Drop` is best-effort — a hung exporter
//! cannot block process exit.

use opentelemetry::trace::TracerProvider as _;
use opentelemetry::KeyValue;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::trace::{Sampler, TracerProvider};
use opentelemetry_sdk::Resource;
use tracing_subscriber::{fmt, prelude::*, EnvFilter, Registry};

use crate::config::LogFormat;
use crate::error::{Error, Result};

/// Optional OpenTelemetry exporter configuration. Constructed by the
/// caller from [`crate::config::Config`]; the binary supplies its own
/// `service_name` default when the operator hasn't set one.
#[derive(Debug, Clone)]
pub struct OtelConfig {
    /// OTLP HTTP endpoint (e.g. `http://collector:4318`). When `None`,
    /// no exporter is wired and the OTel layer is omitted.
    pub endpoint: Option<String>,
    /// Trace sample ratio in `[0.0, 1.0]`.
    pub sample_ratio: f64,
    /// `service.name` resource attribute attached to every span.
    pub service_name: String,
}

impl OtelConfig {
    /// Build an [`OtelConfig`] from the global [`crate::config::Config`].
    /// `default_service_name` is used when the operator has not
    /// supplied `CELLORA_OTEL_SERVICE_NAME`.
    pub fn from_config(config: &crate::config::Config, default_service_name: &str) -> Self {
        Self {
            endpoint: config.otel_otlp_endpoint.clone(),
            sample_ratio: config.otel_sample_ratio,
            service_name: config
                .otel_service_name
                .clone()
                .unwrap_or_else(|| default_service_name.to_owned()),
        }
    }
}

/// Owns the OTel SDK resources. Holding the guard keeps the exporter
/// task alive; dropping it on shutdown flushes any pending spans.
pub struct TracingGuard {
    provider: Option<TracerProvider>,
}

impl Drop for TracingGuard {
    fn drop(&mut self) {
        if let Some(provider) = self.provider.take() {
            // Best-effort flush. A hung exporter must not block exit.
            // The shutdown signature is fallible per the SDK; we drop
            // the result to keep the guard's `Drop` infallible.
            let _ = provider.shutdown();
        }
    }
}

impl std::fmt::Debug for TracingGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TracingGuard")
            .field("otel_attached", &self.provider.is_some())
            .finish_non_exhaustive()
    }
}

/// Install the global tracing subscriber and (optionally) an OTLP
/// exporter. Returns a [`TracingGuard`] the binary should hold for the
/// lifetime of the process and drop on shutdown.
///
/// * `filter` — `tracing_subscriber::EnvFilter` expression, e.g.
///   `"info"` or `"cellora_api=debug,sqlx=warn"`.
/// * `format` — JSON for production, pretty for local development.
/// * `otel` — when `Some` and `otel.endpoint` is `Some`, the OTel layer
///   is attached.
pub fn init(filter: &str, format: LogFormat, otel: Option<OtelConfig>) -> Result<TracingGuard> {
    let env_filter = EnvFilter::try_new(filter)
        .map_err(|err| Error::Logging(format!("invalid log filter '{filter}': {err}")))?;

    // The OTel layer's generic parameters resist composition with
    // `Option<_>` plus the rest of the layer stack, so we branch
    // explicitly. Verbose, but every arm is straightforwardly typed.
    let provider = match otel {
        Some(cfg) if cfg.endpoint.is_some() => {
            let (otel_layer, provider) = build_otel_layer(&cfg)?;
            install_with_otel(env_filter, format, otel_layer)?;
            Some(provider)
        }
        _ => {
            install_without_otel(env_filter, format)?;
            None
        }
    };

    Ok(TracingGuard { provider })
}

/// Install the subscriber stack including the OTel layer.
fn install_with_otel(
    env_filter: EnvFilter,
    format: LogFormat,
    otel_layer: tracing_opentelemetry::OpenTelemetryLayer<
        Registry,
        opentelemetry_sdk::trace::Tracer,
    >,
) -> Result<()> {
    let result = match format {
        LogFormat::Json => Registry::default()
            .with(otel_layer)
            .with(env_filter)
            .with(fmt::layer().json().with_current_span(false))
            .try_init(),
        LogFormat::Pretty => Registry::default()
            .with(otel_layer)
            .with(env_filter)
            .with(fmt::layer().pretty())
            .try_init(),
    };
    result.map_err(|err| Error::Logging(err.to_string()))
}

/// Install the subscriber stack without OTel.
fn install_without_otel(env_filter: EnvFilter, format: LogFormat) -> Result<()> {
    let result = match format {
        LogFormat::Json => Registry::default()
            .with(env_filter)
            .with(fmt::layer().json().with_current_span(false))
            .try_init(),
        LogFormat::Pretty => Registry::default()
            .with(env_filter)
            .with(fmt::layer().pretty())
            .try_init(),
    };
    result.map_err(|err| Error::Logging(err.to_string()))
}

/// Construct the OTLP HTTP exporter, the SDK tracer provider, and the
/// `tracing-opentelemetry` layer that bridges `tracing` spans to OTel.
fn build_otel_layer(
    cfg: &OtelConfig,
) -> Result<(
    tracing_opentelemetry::OpenTelemetryLayer<Registry, opentelemetry_sdk::trace::Tracer>,
    TracerProvider,
)> {
    let endpoint = cfg
        .endpoint
        .as_ref()
        .ok_or_else(|| Error::Logging("OTel layer requested but no endpoint set".to_owned()))?;

    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_endpoint(endpoint)
        .build()
        .map_err(|err| Error::Logging(format!("OTLP exporter init failed: {err}")))?;

    let resource = Resource::new(vec![KeyValue::new(
        "service.name",
        cfg.service_name.clone(),
    )]);

    let provider = TracerProvider::builder()
        .with_batch_exporter(exporter, opentelemetry_sdk::runtime::Tokio)
        .with_resource(resource)
        .with_sampler(Sampler::ParentBased(Box::new(Sampler::TraceIdRatioBased(
            cfg.sample_ratio.clamp(0.0, 1.0),
        ))))
        .build();

    let tracer = provider.tracer(cfg.service_name.clone());
    let layer = tracing_opentelemetry::layer().with_tracer(tracer);
    Ok((layer, provider))
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn otel_config_defaults_to_supplied_service_name() {
        let config = crate::config::Config {
            database_url: "x".into(),
            ckb_rpc_url: "x".into(),
            poll_interval_ms: 1,
            indexer_start_block: 0,
            indexer_reorg_target_depth: 12,
            indexer_reorg_max_depth: 100,
            indexer_metrics_bind_addr: "0.0.0.0:0".into(),
            log_level: "info".into(),
            log_format: LogFormat::Pretty,
            api_bind_addr: "0.0.0.0:0".into(),
            api_default_page_size: 50,
            api_max_page_size: 500,
            api_request_timeout_ms: 1,
            api_tip_cache_refresh_ms: 1,
            api_auth_cache_ttl_seconds: 1,
            api_auth_cache_capacity: 1,
            redis_url: "redis://x".into(),
            api_rate_limit_fail_open: true,
            api_rate_limit_free_rest_burst: 1,
            api_rate_limit_free_rest_refill_per_sec: 1.0,
            api_rate_limit_starter_rest_burst: 1,
            api_rate_limit_starter_rest_refill_per_sec: 1.0,
            api_rate_limit_pro_rest_burst: 1,
            api_rate_limit_pro_rest_refill_per_sec: 1.0,
            api_rate_limit_free_graphql_burst: 1,
            api_rate_limit_free_graphql_refill_per_sec: 1.0,
            api_rate_limit_starter_graphql_burst: 1,
            api_rate_limit_starter_graphql_refill_per_sec: 1.0,
            api_rate_limit_pro_graphql_burst: 1,
            api_rate_limit_pro_graphql_refill_per_sec: 1.0,
            otel_otlp_endpoint: None,
            otel_sample_ratio: 0.1,
            otel_service_name: None,
        };
        let cfg = OtelConfig::from_config(&config, "cellora-test");
        assert_eq!(cfg.service_name, "cellora-test");
        assert!(cfg.endpoint.is_none());
    }
}
