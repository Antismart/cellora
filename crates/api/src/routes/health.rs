//! Liveness and readiness endpoints.
//!
//! * `GET /v1/health` — process is up. Always 200; used by container
//!   orchestrators for liveness probes.
//! * `GET /v1/health/ready` — dependencies are reachable. 200 when the
//!   database answers a trivial query, 503 otherwise. Used for readiness
//!   probes so traffic is withheld until the pool is actually serving.
//!
//! These endpoints are deliberately unauthenticated: a probe that can't
//! call them is useless.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::Serialize;
use utoipa::ToSchema;

use crate::state::AppState;

/// Version of the `cellora-api` binary as recorded at build time.
const API_VERSION: &str = env!("CARGO_PKG_VERSION");

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

/// Response body for the readiness endpoint.
#[derive(Debug, Serialize, ToSchema)]
pub struct ReadyResponse {
    /// `"ready"` when all dependencies succeeded, `"not_ready"` otherwise.
    #[schema(value_type = String)]
    pub status: &'static str,
    /// `"ok"` or an error message, for the Postgres pool.
    pub db: String,
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

/// Handler for `GET /v1/health/ready`. Returns 200 when the database pool
/// answers, 503 otherwise. The response body names the failing dependency.
#[utoipa::path(
    get,
    path = "/v1/health/ready",
    tag = "health",
    responses(
        (status = 200, description = "All dependencies reachable", body = ReadyResponse),
        (status = 503, description = "One or more dependencies unreachable", body = ReadyResponse),
    ),
)]
pub async fn readiness(State(state): State<AppState>) -> impl IntoResponse {
    match sqlx::query_scalar::<_, i32>("SELECT 1")
        .fetch_one(&state.db)
        .await
    {
        Ok(_) => (
            StatusCode::OK,
            Json(ReadyResponse {
                status: "ready",
                db: "ok".to_owned(),
            }),
        ),
        Err(err) => {
            tracing::warn!(error = %err, "readiness probe: database check failed");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(ReadyResponse {
                    status: "not_ready",
                    db: "error".to_owned(),
                }),
            )
        }
    }
}
