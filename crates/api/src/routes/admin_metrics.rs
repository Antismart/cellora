use crate::error::ApiError;
use crate::session::AuthenticatedSession;
use crate::state::AppState;
use axum::extract::{Extension, State};
use axum::Json;
use chrono::{DateTime, Utc};
use serde::Serialize;

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

#[derive(serde::Deserialize)]
pub struct StatusQuery {
    pub network: Option<String>,
}

pub async fn status(
    State(state): State<AppState>,
    axum::extract::Query(query): axum::extract::Query<StatusQuery>,
    Extension(_auth): Extension<AuthenticatedSession>,
) -> Result<Json<StatusResponse>, ApiError> {
    let network = query.network.unwrap_or_else(|| "mainnet".to_string());
    let tip = state.tip.get();

    let nodes = if network == "testnet" {
        vec![NodeStatus {
            name: "ckb-pizza-01 (primary)".to_string(),
            tip: tip.node_tip.unwrap_or(0).try_into().unwrap_or(0),
            sync_status: if tip.is_stale() {
                "syncing".to_string()
            } else {
                "synced".to_string()
            },
            latency: 0,
        }]
    } else {
        vec![NodeStatus {
            name: "ckb-node-01 (primary)".to_string(),
            tip: tip.node_tip.unwrap_or(0).try_into().unwrap_or(0),
            sync_status: if tip.is_stale() {
                "syncing".to_string()
            } else {
                "synced".to_string()
            },
            latency: 0,
        }]
    };

    Ok(Json(StatusResponse {
        network,
        nodes,
        snapshot_age_seconds: tip.observed_monotonic.elapsed().as_secs(),
    }))
}

#[derive(Serialize)]
pub struct MetricsSummary {
    pub rest_p95_ms: f64,
    pub graphql_p95_ms: f64,
    pub error_rate: f64,
    pub rate_limit_count: i64,
    pub rate_limit_rate: f64,
    pub vs_yesterday_percent: f64,
}

pub async fn summary(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedSession>,
) -> Result<Json<MetricsSummary>, ApiError> {
    let row = sqlx::query!(
        r#"
        WITH stats AS (
            SELECT
                COUNT(*) FILTER (WHERE timestamp >= now() - interval '24 hours') as recent_total,
                COUNT(*) FILTER (WHERE timestamp >= now() - interval '48 hours' AND timestamp < now() - interval '24 hours') as old_total,
                COUNT(*) FILTER (WHERE timestamp >= now() - interval '24 hours' AND status_code >= 400 AND status_code != 429) as errors_count,
                COUNT(*) FILTER (WHERE timestamp >= now() - interval '24 hours' AND status_code = 429) as rl_count,
                COALESCE(PERCENTILE_CONT(0.95) WITHIN GROUP (ORDER BY latency_ms) FILTER (WHERE timestamp >= now() - interval '24 hours' AND path NOT LIKE '/graphql%'), 0.0) as rest_p95,
                COALESCE(PERCENTILE_CONT(0.95) WITHIN GROUP (ORDER BY latency_ms) FILTER (WHERE timestamp >= now() - interval '24 hours' AND path LIKE '/graphql%'), 0.0) as gql_p95
            FROM api_request_logs
            WHERE api_key_id IN (SELECT id FROM api_keys WHERE user_id = $1)
            AND timestamp >= now() - interval '48 hours'
        )
        SELECT
            recent_total, old_total, errors_count, rl_count,
            rest_p95 as "rest_p95!", gql_p95 as "gql_p95!"
        FROM stats
        "#,
        auth.user.id
    )
    .fetch_one(&state.db)
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))?;

    let recent = row.recent_total.unwrap_or(0);
    let old = row.old_total.unwrap_or(0);
    let errors = row.errors_count.unwrap_or(0);
    let rl = row.rl_count.unwrap_or(0);

    let error_rate = if recent > 0 {
        (errors as f64 / recent as f64) * 100.0
    } else {
        0.0
    };
    let rate_limit_rate = if recent > 0 {
        (rl as f64 / recent as f64) * 100.0
    } else {
        0.0
    };

    let vs_yesterday_percent = if old > 0 {
        ((recent as f64 - old as f64) / old as f64) * 100.0
    } else if recent > 0 {
        100.0
    } else {
        0.0
    };

    Ok(Json(MetricsSummary {
        rest_p95_ms: row.rest_p95,
        graphql_p95_ms: row.gql_p95,
        error_rate,
        rate_limit_count: rl,
        rate_limit_rate,
        vs_yesterday_percent,
    }))
}

#[derive(Serialize)]
pub struct EndpointRow {
    pub endpoint: String,
    pub requests: i64,
    pub p95_ms: i32,
}

pub async fn endpoints(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedSession>,
) -> Result<Json<Vec<EndpointRow>>, ApiError> {
    let rows = sqlx::query!(
        r#"
        SELECT
            path,
            COUNT(*) as "requests!",
            COALESCE(PERCENTILE_CONT(0.95) WITHIN GROUP (ORDER BY latency_ms), 0.0) as "p95_ms!"
        FROM api_request_logs
        WHERE timestamp >= now() - interval '24 hours'
        AND api_key_id IN (SELECT id FROM api_keys WHERE user_id = $1)
        GROUP BY path
        ORDER BY COUNT(*) DESC
        LIMIT 10
        "#,
        auth.user.id
    )
    .fetch_all(&state.db)
    .await
    .map_err(|e| ApiError::Internal(anyhow::anyhow!(e)))?;

    let out = rows
        .into_iter()
        .map(|r| EndpointRow {
            endpoint: r.path,
            requests: r.requests,
            p95_ms: r.p95_ms as i32,
        })
        .collect();

    Ok(Json(out))
}

#[derive(Serialize)]
pub struct RateLimitRow {
    pub endpoint: String,
    pub key: String,
    pub surface: String,
    pub ts: DateTime<Utc>,
    pub retry_after: i32,
}

pub async fn rate_limits(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedSession>,
) -> Result<Json<Vec<RateLimitRow>>, ApiError> {
    let rows = sqlx::query!(
        r#"
        SELECT
            l.path,
            k.prefix as key_prefix,
            l.timestamp,
            (CASE WHEN l.path LIKE '/graphql%' THEN 'GraphQL' ELSE 'REST' END) as "surface!"
        FROM api_request_logs l
        JOIN api_keys k ON l.api_key_id = k.id
        WHERE k.user_id = $1
        AND l.status_code = 429
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
        .map(|r| RateLimitRow {
            endpoint: r.path,
            key: r.key_prefix,
            surface: r.surface,
            ts: r.timestamp,
            retry_after: 60,
        })
        .collect();

    Ok(Json(out))
}
