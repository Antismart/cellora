//! Liveness and readiness endpoints.
//!
//! * `GET /v1/health` — process is up. Always 200; used by container
//!   orchestrators for liveness probes.
//! * `GET /v1/health/ready` — dependencies are reachable. 200 when
//!   every configured dependency responds, 503 when any required one
//!   does not. Used for readiness probes so traffic is withheld until
//!   the upstream surface is actually serving.
//!
//! These endpoints are deliberately unauthenticated: a probe that
//! can't call them is useless.
//!
//! Probes that are not configured at startup (Redis or CKB without a
//! reachable URL passed in) are reported with `state: "skipped"` and
//! do **not** fail the overall readiness — that decision belongs to
//! deployment, not to the application. CKB's initial-block-download
//! state surfaces as `is_synced: false` in a 200 response rather than
//! a 503, so the API can come online during catch-up.

use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::Serialize;
use tokio::time::timeout;
use utoipa::ToSchema;

use crate::state::AppState;

/// Version of the `cellora-api` binary as recorded at build time.
const API_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Per-dependency probe timeout. Each probe runs concurrently and is
/// cancelled at this deadline so a hung dependency cannot wedge the
/// readiness response.
const PROBE_TIMEOUT: Duration = Duration::from_secs(1);

/// Response body for the liveness endpoint.
#[derive(Debug, Serialize, ToSchema)]
pub struct HealthResponse {
    /// Always the literal string `"ok"`.
    #[schema(value_type = String)]
    pub status: &'static str,
    /// `cellora-api` crate version.
    #[schema(value_type = String)]
    pub version: &'static str,
}

/// Response body for the readiness endpoint. Each dependency is
/// reported separately so an operator can see which one is failing
/// without parsing log lines.
#[derive(Debug, Serialize, ToSchema)]
pub struct ReadyResponse {
    /// `"ready"` when every required dependency answered, `"not_ready"`
    /// otherwise.
    #[schema(value_type = String)]
    pub status: &'static str,
    /// State of the Postgres pool — `"ok"` or `"error"`.
    pub db: String,
    /// State of the Redis connection — `"ok"`, `"error"`, or
    /// `"skipped"` when no Redis client was attached at startup.
    pub redis: String,
    /// State of the CKB upstream node and its sync progress.
    pub ckb_node: CkbNodeStatus,
}

/// CKB node probe result. `state == "skipped"` when no client was
/// attached; `state == "error"` when the node was unreachable; `"ok"`
/// when `get_blockchain_info` returned. `is_synced` is `false` when the
/// node is in initial block download — operators are expected to alert
/// on `indexer_lag_blocks` separately.
#[derive(Debug, Serialize, ToSchema)]
pub struct CkbNodeStatus {
    /// `"ok"`, `"error"`, or `"skipped"`.
    pub state: String,
    /// Tip height the node reported, when reachable.
    pub tip: Option<u64>,
    /// `false` when the node is in initial block download.
    pub is_synced: bool,
}

/// Handler for `GET /v1/health`. Always returns 200; used for liveness.
#[utoipa::path(
    get,
    path = "/v1/health",
    tag = "health",
    responses((status = 200, description = "Service is up", body = HealthResponse)),
)]
pub async fn liveness() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        version: API_VERSION,
    })
}

/// Handler for `GET /v1/health/ready`. Probes each configured
/// dependency concurrently with a 1-second timeout per probe. Returns
/// 200 when every required dependency answered, 503 when any did not.
/// CKB IBD does **not** flip the response to 503 — `is_synced: false`
/// is reported in a 200.
#[utoipa::path(
    get,
    path = "/v1/health/ready",
    tag = "health",
    responses(
        (status = 200, description = "All required dependencies reachable", body = ReadyResponse),
        (status = 503, description = "One or more required dependencies unreachable", body = ReadyResponse),
    ),
)]
pub async fn readiness(State(state): State<AppState>) -> impl IntoResponse {
    let (db, redis, ckb_node) =
        tokio::join!(probe_db(&state), probe_redis(&state), probe_ckb(&state));

    // Each probe returns ("ok"/"error"/"skipped"). A required dep
    // failing flips the response to 503. "skipped" is treated as
    // "not relevant to this deployment" and does not fail the probe.
    let any_error = db == "error" || redis == "error" || ckb_node.state == "error";
    let status = if any_error { "not_ready" } else { "ready" };
    let http = if any_error {
        StatusCode::SERVICE_UNAVAILABLE
    } else {
        StatusCode::OK
    };

    (
        http,
        Json(ReadyResponse {
            status,
            db,
            redis,
            ckb_node,
        }),
    )
}

async fn probe_db(state: &AppState) -> String {
    let result = timeout(
        PROBE_TIMEOUT,
        sqlx::query_scalar::<_, i32>("SELECT 1").fetch_one(&state.db),
    )
    .await;
    match result {
        Ok(Ok(_)) => "ok".to_owned(),
        Ok(Err(err)) => {
            tracing::warn!(error = %err, "readiness probe: db check failed");
            "error".to_owned()
        }
        Err(_) => {
            tracing::warn!("readiness probe: db check timed out");
            "error".to_owned()
        }
    }
}

async fn probe_redis(state: &AppState) -> String {
    let Some(manager) = state.redis.as_ref() else {
        return "skipped".to_owned();
    };
    let mut conn = manager.clone();
    let probe = async { redis::cmd("PING").query_async::<String>(&mut conn).await };
    match timeout(PROBE_TIMEOUT, probe).await {
        Ok(Ok(_)) => "ok".to_owned(),
        Ok(Err(err)) => {
            tracing::warn!(error = %err, "readiness probe: redis ping failed");
            "error".to_owned()
        }
        Err(_) => {
            tracing::warn!("readiness probe: redis ping timed out");
            "error".to_owned()
        }
    }
}

async fn probe_ckb(state: &AppState) -> CkbNodeStatus {
    let Some(client) = state.ckb.as_ref() else {
        return CkbNodeStatus {
            state: "skipped".to_owned(),
            tip: None,
            is_synced: true,
        };
    };
    let probe = async {
        let chain = client.chain_info().await?;
        let tip = client.tip_block_number().await?;
        Ok::<_, cellora_common::error::Error>((chain.is_initial_block_download, tip))
    };
    match timeout(PROBE_TIMEOUT, probe).await {
        Ok(Ok((is_ibd, tip))) => CkbNodeStatus {
            state: "ok".to_owned(),
            tip: Some(tip),
            is_synced: !is_ibd,
        },
        Ok(Err(err)) => {
            tracing::warn!(error = %err, "readiness probe: ckb check failed");
            CkbNodeStatus {
                state: "error".to_owned(),
                tip: None,
                is_synced: false,
            }
        }
        Err(_) => {
            tracing::warn!("readiness probe: ckb check timed out");
            CkbNodeStatus {
                state: "error".to_owned(),
                tip: None,
                is_synced: false,
            }
        }
    }
}
