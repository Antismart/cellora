//! `GET /v1/stats` — indexer progress and lag.
//!
//! Values are read from the shared [`TipTracker`](crate::tip::TipTracker)
//! rather than queried per request, so this endpoint is essentially free
//! and cannot starve the database pool under load.

use axum::extract::State;
use axum::Json;
use serde::Serialize;

use crate::state::AppState;

/// Wire-format shape of `/v1/stats`.
#[derive(Debug, Serialize)]
pub struct StatsResponse {
    /// Highest block number Cellora has ingested, or `None` on a fresh DB.
    pub indexer_tip: Option<i64>,
    /// Last tip block number the CKB node returned.
    pub node_tip: Option<u64>,
    /// `node_tip − indexer_tip` when both are known.
    pub lag_blocks: Option<i64>,
    /// Age of the snapshot (seconds since it was observed). Lets clients
    /// tell a lagging indexer from a dead one.
    pub snapshot_age_seconds: u64,
    /// `true` when the snapshot is older than the internal staleness
    /// threshold — usually means the refresh task can't reach the node
    /// or the database.
    pub is_stale: bool,
}

/// Handler for `GET /v1/stats`.
pub async fn stats(State(state): State<AppState>) -> Json<StatsResponse> {
    let snap = state.tip.get();
    let snapshot_age_seconds = snap.observed_monotonic.elapsed().as_secs();

    Json(StatsResponse {
        indexer_tip: snap.indexer_tip,
        node_tip: snap.node_tip,
        lag_blocks: snap.lag_blocks(),
        snapshot_age_seconds,
        is_stale: snap.is_stale(),
    })
}
