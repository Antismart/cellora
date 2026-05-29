use axum::extract::{Extension, State};
use axum::Json;
use chrono::{DateTime, Utc};
use serde::Serialize;
use crate::error::ApiError;
use crate::session::AuthenticatedSession;
use crate::state::AppState;

#[derive(Serialize)]
pub struct UsageDataPoint {
    pub timestamp: DateTime<Utc>,
    pub rest: i64,
    pub graphql: i64,
}

pub async fn usage(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedSession>,
) -> Result<Json<Vec<UsageDataPoint>>, ApiError> {
    let rows = sqlx::query!(
        r#"
        SELECT 
            date_trunc('hour', timestamp) as "ts!",
            COUNT(*) FILTER (WHERE path NOT LIKE '/graphql%') as rest,
            COUNT(*) FILTER (WHERE path LIKE '/graphql%') as graphql
        FROM api_request_logs
        WHERE timestamp >= now() - interval '24 hours'
        AND api_key_id IN (SELECT id FROM api_keys WHERE user_id = $1)
        GROUP BY 1
        ORDER BY 1
        "#,
        auth.user.id
    )
    .fetch_all(&state.db)
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))?;

    let out = rows
        .into_iter()
        .map(|r| UsageDataPoint {
            timestamp: r.ts,
            rest: r.rest.unwrap_or(0),
            graphql: r.graphql.unwrap_or(0),
        })
        .collect();

    Ok(Json(out))
}

#[derive(Serialize)]
pub struct ActivityRow {
    pub method: String,
    pub path: String,
    pub key: String,
    pub status: i16,
    pub ms: i32,
    pub timestamp: DateTime<Utc>,
}

pub async fn activity(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedSession>,
) -> Result<Json<Vec<ActivityRow>>, ApiError> {
    let rows = sqlx::query!(
        r#"
        SELECT 
            l.method,
            l.path,
            k.prefix as key_prefix,
            l.status_code as status,
            l.latency_ms as ms,
            l.timestamp
        FROM api_request_logs l
        JOIN api_keys k ON l.api_key_id = k.id
        WHERE k.user_id = $1
        ORDER BY l.timestamp DESC
        LIMIT 10
        "#,
        auth.user.id
    )
    .fetch_all(&state.db)
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))?;

    let out = rows
        .into_iter()
        .map(|r| ActivityRow {
            method: r.method,
            path: r.path,
            key: r.key_prefix,
            status: r.status,
            ms: r.ms,
            timestamp: r.timestamp,
        })
        .collect();

    Ok(Json(out))
}

#[derive(Serialize)]
pub struct NodeStatus {
    pub name: String,
    pub tip: i64,
    pub sync_status: String,
    pub latency: i32,
}

#[derive(Serialize)]
pub struct StatusResponse {
    pub network: String,
    pub nodes: Vec<NodeStatus>,
    pub snapshot_age_seconds: u64,
}

pub async fn status(
    State(state): State<AppState>,
    Extension(_auth): Extension<AuthenticatedSession>,
) -> Result<Json<StatusResponse>, ApiError> {
    let tip = state.tip.get();
    
    // We only have one CKB node configured right now.
    let nodes = vec![NodeStatus {
        name: "ckb-node-01 (primary)".to_string(),
        tip: tip.node_tip.unwrap_or(0).try_into().unwrap_or(0),
        sync_status: if tip.is_stale() { "syncing".to_string() } else { "synced".to_string() },
        latency: 0,
    }];

    Ok(Json(StatusResponse {
        network: "mainnet".to_string(), // hardcoded for MVP
        nodes,
        snapshot_age_seconds: tip.observed_monotonic.elapsed().as_secs(),
    }))
}
