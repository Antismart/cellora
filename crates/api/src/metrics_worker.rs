//! Background worker that drains buffered API request logs from Redis and
//! persists them to Postgres.
//!
//! Requests are enqueued with `LPUSH` (newest at the head) and capped with
//! `LTRIM` at the enqueue site (see `crate::lib`), so the list can never grow
//! unbounded. This worker `RPOP`s from the tail, giving first-in-first-out
//! processing so the oldest entries drain first under sustained load.
//!
//! Deferred optimization: each payload is inserted with its own `INSERT`
//! statement. A single multi-row bulk insert would reduce round-trips, but is
//! intentionally left for later so the compile-checked `sqlx::query!` offline
//! cache stays valid without a live database.

use sqlx::PgPool;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// Maximum number of payloads pulled from Redis in a single `RPOP` call.
const BATCH_SIZE: i64 = 100;

/// Maximum number of `RPOP` batches processed per timer tick.
///
/// Draining is bounded so a flooded queue cannot monopolise the worker task
/// and starve the `select!` that also handles graceful shutdown. With
/// [`BATCH_SIZE`] this drains up to 1,000 entries per second; any backlog is
/// carried over to subsequent ticks.
const MAX_BATCHES_PER_TICK: u32 = 10;

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
                    // Drain the queue in FIFO order. Enqueue uses LPUSH (newest at
                    // the head), so we RPOP from the tail to process the *oldest*
                    // entries first and avoid starving them under sustained load.
                    //
                    // We loop over multiple batches until the list is empty, but
                    // cap the number of batches per tick so a flooded queue can
                    // never monopolise this task and starve the `select!` (which
                    // also handles graceful shutdown via `cancel`).
                    let mut batches_remaining = MAX_BATCHES_PER_TICK;
                    while batches_remaining > 0 {
                        batches_remaining -= 1;

                        let items = match redis::cmd("RPOP")
                            .arg("api_request_logs")
                            .arg(BATCH_SIZE)
                            .query_async::<Vec<String>>(&mut redis)
                            .await
                        {
                            Ok(items) => items,
                            Err(e) => {
                                tracing::warn!(error = %e, "redis RPOP failed");
                                break;
                            }
                        };

                        if items.is_empty() {
                            // Queue drained; nothing more to do until the next tick.
                            break;
                        }

                        let drained = items.len();

                        // Insert one row per payload. A multi-row bulk insert would
                        // be more efficient, but is deferred (see module docs) to
                        // keep the compile-checked `sqlx::query!` offline cache valid.
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
                                    tracing::warn!(error = %e, "failed to insert metric");
                                }
                            }
                        }

                        // A short batch means the list is now empty, so stop early
                        // rather than issuing another RPOP that would return nothing.
                        if drained < BATCH_SIZE as usize {
                            break;
                        }
                    }
                }
            }
        }
    })
}
