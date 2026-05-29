use std::time::Duration;
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;

#[derive(serde::Deserialize, Debug)]
struct ApiRequestLogPayload {
    api_key_id: uuid::Uuid,
    method: String,
    path: String,
    status_code: u16,
    latency_ms: u32,
}

/// Spawns the background worker to pop metric payloads from Redis and insert them into Postgres.
pub fn spawn(
    pool: PgPool,
    mut redis: redis::aio::ConnectionManager,
    cancel: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    break;
                }
                _ = interval.tick() => {
                    // Try popping up to 100 items at a time
                    match redis::cmd("LPOP").arg("api_request_logs").arg(100).query_async::<Vec<String>>(&mut redis).await {
                        Ok(items) => {
                            if items.is_empty() {
                                continue;
                            }
                            
                            // Insert in a batch or one by one. For MVP, one by one is fine
                            // or we can build a bulk insert query
                            for json_str in items {
                                if let Ok(payload) = serde_json::from_str::<ApiRequestLogPayload>(&json_str) {
                                    let res = sqlx::query!(
                                        "INSERT INTO api_request_logs (api_key_id, method, path, status_code, latency_ms) VALUES ($1, $2, $3, $4, $5)",
                                        payload.api_key_id,
                                        payload.method,
                                        payload.path,
                                        payload.status_code as i16,
                                        payload.latency_ms as i32
                                    ).execute(&pool).await;

                                    if let Err(e) = res {
                                        tracing::warn!("failed to insert metric: {}", e);
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!("redis LPOP failed: {}", e);
                        }
                    }
                }
            }
        }
    })
}
