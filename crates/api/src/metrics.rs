//! Prometheus metrics for the API binary.
//!
//! A single [`Metrics`] holds every counter / histogram / gauge the API
//! emits, registered against a private [`prometheus::Registry`]. The
//! registry is exposed by [`Metrics::render`] in the standard text
//! format, served from the public `/metrics` route.
//!
//! Cardinality is bounded by construction: `path` labels use the matched
//! axum route (e.g. `/v1/blocks/:number`), not the raw request URI, so
//! a malicious client cannot create unbounded label combinations by
//! generating distinct paths.

use std::sync::Arc;

use prometheus::{
    Encoder, HistogramOpts, HistogramVec, IntCounterVec, IntGauge, Registry, TextEncoder,
};

/// Bundle of all Prometheus metrics the API emits. Cheap to clone —
/// internally an `Arc` of the underlying handles.
#[derive(Clone)]
pub struct Metrics {
    inner: Arc<MetricsInner>,
}

struct MetricsInner {
    registry: Registry,
    requests_total: IntCounterVec,
    request_duration_seconds: HistogramVec,
    rate_limit_decisions_total: IntCounterVec,
    db_connections_active: IntGauge,
    db_connections_idle: IntGauge,
}

impl std::fmt::Debug for Metrics {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Metrics").finish_non_exhaustive()
    }
}

impl Metrics {
    /// Build a new metrics bundle and register every metric with the
    /// supplied registry.
    pub fn new() -> Self {
        // Histogram buckets for sub-second-to-second-scale request
        // durations. The defaults from prometheus' helpers are biased
        // toward longer requests; these match the latency budget for an
        // indexed-data API.
        let duration_buckets = vec![
            0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0,
        ];

        // The unwraps below are unreachable: each metric definition is
        // a literal name + label spec we control, validated by the
        // `prometheus` crate at construction time. Wrapping them in
        // `Result` plumbing would be ceremony with no upside.
        #[allow(clippy::expect_used)]
        let registry = Registry::new();

        #[allow(clippy::expect_used)]
        let requests_total = IntCounterVec::new(
            prometheus::Opts::new(
                "api_requests_total",
                "Total number of API requests served, labelled by method, matched path, and response status.",
            ),
            &["method", "path", "status"],
        )
        .expect("requests_total");

        #[allow(clippy::expect_used)]
        let request_duration_seconds = HistogramVec::new(
            HistogramOpts::new(
                "api_request_duration_seconds",
                "API request handling time in seconds, labelled by method and matched path.",
            )
            .buckets(duration_buckets),
            &["method", "path"],
        )
        .expect("request_duration_seconds");

        #[allow(clippy::expect_used)]
        let rate_limit_decisions_total = IntCounterVec::new(
            prometheus::Opts::new(
                "api_rate_limit_decisions_total",
                "Rate-limit decisions taken, labelled by surface (rest|graphql), tier, and outcome (allowed|limited|fail_open|fail_closed).",
            ),
            &["surface", "tier", "outcome"],
        )
        .expect("rate_limit_decisions_total");

        #[allow(clippy::expect_used)]
        let db_connections_active = IntGauge::new(
            "db_pool_connections_active",
            "Currently checked-out connections in the Postgres pool.",
        )
        .expect("db_connections_active");

        #[allow(clippy::expect_used)]
        let db_connections_idle = IntGauge::new(
            "db_pool_connections_idle",
            "Currently idle connections in the Postgres pool.",
        )
        .expect("db_connections_idle");

        let registrations = [
            registry.register(Box::new(requests_total.clone())),
            registry.register(Box::new(request_duration_seconds.clone())),
            registry.register(Box::new(rate_limit_decisions_total.clone())),
            registry.register(Box::new(db_connections_active.clone())),
            registry.register(Box::new(db_connections_idle.clone())),
        ];
        for r in registrations {
            #[allow(clippy::expect_used)]
            r.expect("metric registration");
        }

        Self {
            inner: Arc::new(MetricsInner {
                registry,
                requests_total,
                request_duration_seconds,
                rate_limit_decisions_total,
                db_connections_active,
                db_connections_idle,
            }),
        }
    }

    /// Record a completed request.
    pub fn observe_request(&self, method: &str, matched_path: &str, status: u16, seconds: f64) {
        let status_label = status.to_string();
        self.inner
            .requests_total
            .with_label_values(&[method, matched_path, &status_label])
            .inc();
        self.inner
            .request_duration_seconds
            .with_label_values(&[method, matched_path])
            .observe(seconds);
    }

    /// Record a rate-limit decision against `(surface, tier, outcome)`.
    pub fn observe_rate_limit(&self, surface: &str, tier: &str, outcome: RateLimitOutcome) {
        self.inner
            .rate_limit_decisions_total
            .with_label_values(&[surface, tier, outcome.as_str()])
            .inc();
    }

    /// Snapshot the connection pool's active / idle counts. Called on
    /// every request inside the metrics middleware so the gauges reflect
    /// near-real-time state without a separate background task.
    pub fn record_pool(&self, active: u32, idle: u32) {
        self.inner.db_connections_active.set(i64::from(active));
        self.inner.db_connections_idle.set(i64::from(idle));
    }

    /// Render the registry as Prometheus text format.
    pub fn render(&self) -> String {
        let metric_families = self.inner.registry.gather();
        let mut buffer = Vec::new();
        let encoder = TextEncoder::new();
        if encoder.encode(&metric_families, &mut buffer).is_err() {
            return String::new();
        }
        String::from_utf8(buffer).unwrap_or_default()
    }
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}

/// Outcome label for `api_rate_limit_decisions_total`.
#[derive(Debug, Clone, Copy)]
pub enum RateLimitOutcome {
    /// Request was allowed by the limiter.
    Allowed,
    /// Request was refused with 429.
    Limited,
    /// Limiter call failed and the limiter is configured to fail open.
    FailOpen,
    /// Limiter call failed and the limiter is configured to fail closed.
    FailClosed,
}

impl RateLimitOutcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::Allowed => "allowed",
            Self::Limited => "limited",
            Self::FailOpen => "fail_open",
            Self::FailClosed => "fail_closed",
        }
    }
}
