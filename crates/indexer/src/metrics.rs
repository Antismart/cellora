//! Prometheus metrics for the indexer binary.
//!
//! Exposes a separate registry from the API's. Each binary owns its own
//! `/metrics` endpoint because a Prometheus pull model only sees the
//! process it scrapes — there is no in-memory sharing across binaries
//! anyway.

use std::sync::Arc;

use prometheus::{
    Encoder, HistogramOpts, HistogramVec, IntCounter, IntGauge, Registry, TextEncoder,
};

/// Bundle of indexer Prometheus metrics. Cheap to clone — internally
/// an `Arc` of the underlying handles.
#[derive(Clone)]
pub struct Metrics {
    inner: Arc<MetricsInner>,
}

struct MetricsInner {
    registry: Registry,
    latest_block: IntGauge,
    blocks_indexed_total: IntCounter,
    block_indexing_duration_seconds: HistogramVec,
    reorg_total: IntCounter,
    reorg_oversized_total: IntCounter,
    reorg_depth: HistogramVec,
}

impl std::fmt::Debug for Metrics {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Metrics").finish_non_exhaustive()
    }
}

impl Metrics {
    /// Build a new metrics bundle.
    pub fn new() -> Self {
        // Each metric definition below is a literal name + label spec
        // we control. Construction is fallible only on clearly invalid
        // inputs that cannot occur with these literals.
        #[allow(clippy::expect_used)]
        let registry = Registry::new();

        #[allow(clippy::expect_used)]
        let latest_block = IntGauge::new(
            "indexer_latest_block",
            "Highest block number the indexer has committed to the database.",
        )
        .expect("latest_block");

        #[allow(clippy::expect_used)]
        let blocks_indexed_total = IntCounter::new(
            "indexer_blocks_indexed_total",
            "Total number of blocks successfully indexed since process start.",
        )
        .expect("blocks_indexed_total");

        let duration_buckets = vec![
            0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0,
        ];
        #[allow(clippy::expect_used)]
        let block_indexing_duration_seconds = HistogramVec::new(
            HistogramOpts::new(
                "indexer_block_indexing_duration_seconds",
                "Wall-clock time spent indexing a single block, including parse and commit.",
            )
            .buckets(duration_buckets),
            &[],
        )
        .expect("block_indexing_duration_seconds");

        #[allow(clippy::expect_used)]
        let reorg_total = IntCounter::new(
            "reorg_total",
            "Total number of chain reorgs detected and rolled back since process start.",
        )
        .expect("reorg_total");

        #[allow(clippy::expect_used)]
        let reorg_oversized_total = IntCounter::new(
            "reorg_oversized_total",
            "Reorgs whose depth exceeded `INDEXER_REORG_MAX_DEPTH`. Rolled back regardless; presence indicates an operational concern.",
        )
        .expect("reorg_oversized_total");

        let depth_buckets = vec![1.0, 2.0, 4.0, 8.0, 12.0, 24.0, 48.0, 100.0];
        #[allow(clippy::expect_used)]
        let reorg_depth = HistogramVec::new(
            HistogramOpts::new("reorg_depth", "Depth (in blocks) of every detected reorg.")
                .buckets(depth_buckets),
            &[],
        )
        .expect("reorg_depth");

        let registrations = [
            registry.register(Box::new(latest_block.clone())),
            registry.register(Box::new(blocks_indexed_total.clone())),
            registry.register(Box::new(block_indexing_duration_seconds.clone())),
            registry.register(Box::new(reorg_total.clone())),
            registry.register(Box::new(reorg_oversized_total.clone())),
            registry.register(Box::new(reorg_depth.clone())),
        ];
        for r in registrations {
            #[allow(clippy::expect_used)]
            r.expect("metric registration");
        }

        Self {
            inner: Arc::new(MetricsInner {
                registry,
                latest_block,
                blocks_indexed_total,
                block_indexing_duration_seconds,
                reorg_total,
                reorg_oversized_total,
                reorg_depth,
            }),
        }
    }

    /// Called after a block is successfully committed.
    pub fn observe_block_indexed(&self, height: i64, duration_secs: f64) {
        self.inner.latest_block.set(height);
        self.inner.blocks_indexed_total.inc();
        self.inner
            .block_indexing_duration_seconds
            .with_label_values(&[])
            .observe(duration_secs);
    }

    /// Update only the `indexer_latest_block` gauge. Used by the reorg
    /// path to bring the gauge in line with the new tip immediately,
    /// without contaminating the indexing-duration histogram with a
    /// zero observation.
    pub fn set_latest_block(&self, height: i64) {
        self.inner.latest_block.set(height);
    }

    /// Called after a reorg rollback completes. `depth` is the number
    /// of blocks rolled back; `oversized` is true when the reorg
    /// exceeded the configured upper bound.
    pub fn observe_reorg(&self, depth: i64, oversized: bool) {
        self.inner.reorg_total.inc();
        if oversized {
            self.inner.reorg_oversized_total.inc();
        }
        self.inner
            .reorg_depth
            .with_label_values(&[])
            .observe(depth as f64);
    }

    /// Render the registry in Prometheus text format.
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
