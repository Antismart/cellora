//! Shared snapshot of indexer and node tip heights.
//!
//! [`TipTracker`] is constructed once at startup and cloned into every
//! component that needs to read the tip: the stats endpoint, the
//! `X-Indexer-Tip` response-header middleware, and the cells metadata
//! block. Reading is lock-free — the underlying [`ArcSwap`] makes
//! `Arc<TipSnapshot>` loads wait-free and cheap.
//!
//! The snapshot is refreshed by the background task spawned from
//! [`spawn_refresh_task`]. That task queries `indexer_state` in Postgres
//! for the indexer tip and `get_tip_block_number` on the CKB node for the
//! node tip, then publishes a fresh snapshot. Failures of either source
//! are logged but do not abort the loop — stale data is preferable to a
//! crashed API.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use arc_swap::ArcSwap;
use cellora_common::ckb::CkbClient;
use sqlx::PgPool;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// How long after an update a snapshot is considered stale. Above this
/// threshold, responses are tagged with `X-Indexer-Tip-Stale: true` so
/// operators can debug lag from the client side.
const STALE_AFTER: Duration = Duration::from_secs(5);

/// Latest known tip heights and the wall-clock moment they were observed.
///
/// Values are `Option` because a fresh service has not yet run its first
/// refresh; the API must still serve under those conditions.
#[derive(Debug, Clone)]
pub struct TipSnapshot {
    /// Highest block number currently stored in Cellora's database.
    pub indexer_tip: Option<i64>,
    /// Tip block number reported by the CKB node on the last poll.
    pub node_tip: Option<u64>,
    /// System time at which this snapshot was published. Used for the
    /// `lag_seconds` calculation and the staleness threshold.
    pub observed_at: SystemTime,
    /// Monotonic clock reading alongside `observed_at`. Kept so the
    /// staleness check cannot be fooled by wall-clock changes.
    pub observed_monotonic: Instant,
}

impl TipSnapshot {
    /// Empty snapshot used before the first refresh has run. `observed_at`
    /// is set to the Unix epoch so the staleness check immediately flags
    /// it — clients see the header until real data arrives.
    pub fn empty() -> Self {
        Self {
            indexer_tip: None,
            node_tip: None,
            observed_at: UNIX_EPOCH,
            observed_monotonic: Instant::now()
                .checked_sub(STALE_AFTER * 2)
                .unwrap_or_else(Instant::now),
        }
    }

    /// `true` when the snapshot is older than [`STALE_AFTER`].
    pub fn is_stale(&self) -> bool {
        self.observed_monotonic.elapsed() > STALE_AFTER
    }

    /// `node_tip − indexer_tip` in blocks, when both are known.
    pub fn lag_blocks(&self) -> Option<i64> {
        match (self.indexer_tip, self.node_tip) {
            (Some(indexer), Some(node)) => {
                i64::try_from(node).ok().map(|n| n.saturating_sub(indexer))
            }
            _ => None,
        }
    }
}

/// Shared, lock-free holder of the latest [`TipSnapshot`].
///
/// Cheap to clone: internally an `Arc<ArcSwap<TipSnapshot>>`. Every clone
/// shares the same underlying slot so updates from the refresh task are
/// visible everywhere immediately.
#[derive(Debug, Clone)]
pub struct TipTracker {
    inner: Arc<ArcSwap<TipSnapshot>>,
}

impl TipTracker {
    /// Build a new tracker initialised with an empty snapshot.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(ArcSwap::from_pointee(TipSnapshot::empty())),
        }
    }

    /// Read the current snapshot. Returns an `Arc` so callers don't pay
    /// for a clone of the data.
    pub fn get(&self) -> Arc<TipSnapshot> {
        self.inner.load_full()
    }

    /// Publish a new snapshot. Replaces the old one atomically.
    pub fn set(&self, snapshot: TipSnapshot) {
        self.inner.store(Arc::new(snapshot));
    }
}

impl Default for TipTracker {
    fn default() -> Self {
        Self::new()
    }
}

/// Spawn the background task that keeps the tracker up to date. Returns
/// the `JoinHandle` so `main.rs` can await it during shutdown.
///
/// The task runs until `cancel` fires. Errors from either tip source are
/// logged at `WARN` and the prior snapshot is kept; a transient outage
/// does not take the API down.
pub fn spawn_refresh_task(
    tracker: TipTracker,
    pool: PgPool,
    ckb: CkbClient,
    refresh_interval: Duration,
    cancel: CancellationToken,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        // Run a refresh immediately on startup so the first request does
        // not see the empty placeholder.
        refresh_once(&tracker, &pool, &ckb).await;

        loop {
            tokio::select! {
                () = cancel.cancelled() => {
                    tracing::debug!("tip refresh task cancelled");
                    return;
                }
                () = tokio::time::sleep(refresh_interval) => {
                    refresh_once(&tracker, &pool, &ckb).await;
                }
            }
        }
    })
}

/// One refresh iteration. Reads both tips; on success publishes a new
/// snapshot, on failure logs and leaves the previous snapshot in place.
async fn refresh_once(tracker: &TipTracker, pool: &PgPool, ckb: &CkbClient) {
    let indexer_tip = match cellora_db::checkpoint::read(pool).await {
        Ok(row) => row.map(|c| c.last_indexed_block),
        Err(err) => {
            tracing::warn!(error = %err, "tip refresh: indexer tip query failed");
            tracker.get().indexer_tip
        }
    };

    let node_tip = match ckb.tip_block_number().await {
        Ok(n) => Some(n),
        Err(err) => {
            tracing::warn!(error = %err, "tip refresh: node tip query failed");
            tracker.get().node_tip
        }
    };

    tracker.set(TipSnapshot {
        indexer_tip,
        node_tip,
        observed_at: SystemTime::now(),
        observed_monotonic: Instant::now(),
    });
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn empty_snapshot_is_stale() {
        let snap = TipSnapshot::empty();
        assert!(snap.is_stale());
        assert!(snap.indexer_tip.is_none());
        assert!(snap.node_tip.is_none());
        assert!(snap.lag_blocks().is_none());
    }

    #[test]
    fn fresh_snapshot_is_not_stale() {
        let snap = TipSnapshot {
            indexer_tip: Some(10),
            node_tip: Some(12),
            observed_at: SystemTime::now(),
            observed_monotonic: Instant::now(),
        };
        assert!(!snap.is_stale());
        assert_eq!(snap.lag_blocks(), Some(2));
    }

    #[test]
    fn tracker_updates_are_visible() {
        let tracker = TipTracker::new();
        assert!(tracker.get().indexer_tip.is_none());

        tracker.set(TipSnapshot {
            indexer_tip: Some(42),
            node_tip: Some(42),
            observed_at: SystemTime::now(),
            observed_monotonic: Instant::now(),
        });
        assert_eq!(tracker.get().indexer_tip, Some(42));
        assert_eq!(tracker.get().node_tip, Some(42));
        assert_eq!(tracker.get().lag_blocks(), Some(0));
    }
}
