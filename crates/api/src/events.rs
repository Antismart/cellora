use cellora_common::events::{BlockMinedEvent, CellCreatedEvent, BLOCKS_CHANNEL, CELLS_CHANNEL};
use futures_util::StreamExt;
use redis::aio::ConnectionManager;
use tokio::sync::broadcast;
use tracing::{error, info, warn};

#[derive(Clone, Debug)]
pub enum ApiEvent {
    BlockMined(BlockMinedEvent),
    CellCreated(CellCreatedEvent),
}

/// A background task that subscribes to `cellora:blocks` and `cellora:cells`
/// via a dedicated Redis pubsub connection, parses the JSON payloads, and
/// broadcasts them internally to any active WebSocket / Webhook subscribers.
pub async fn run_event_listener(
    client: redis::Client,
    tx: broadcast::Sender<ApiEvent>,
    mut shutdown: tokio_util::sync::CancellationToken,
) {
    let mut backoff = 1;
    loop {
        // Build an async connection.
        let conn = match client.get_async_pubsub().await {
            Ok(c) => c,
            Err(e) => {
                error!(error = %e, "failed to connect to redis pubsub, retrying...");
                tokio::select! {
                    _ = tokio::time::sleep(std::time::Duration::from_secs(backoff)) => {},
                    _ = shutdown.cancelled() => return,
                }
                backoff = std::cmp::min(backoff * 2, 30);
                continue;
            }
        };

        let mut pubsub = conn;
        if let Err(e) = pubsub.subscribe(BLOCKS_CHANNEL).await {
            error!(error = %e, "failed to subscribe to blocks channel");
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            continue;
        }
        if let Err(e) = pubsub.subscribe(CELLS_CHANNEL).await {
            error!(error = %e, "failed to subscribe to cells channel");
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            continue;
        }

        info!("redis pubsub connected and subscribed to events");
        backoff = 1;

        let mut stream = pubsub.on_message();

        loop {
            tokio::select! {
                _ = shutdown.cancelled() => {
                    info!("event listener shutting down");
                    return;
                }
                msg = stream.next() => {
                    match msg {
                        Some(msg) => {
                            let channel_name: String = msg.get_channel_name().to_string();
                            let payload: String = match msg.get_payload() {
                                Ok(p) => p,
                                Err(e) => {
                                    warn!(error = %e, "failed to get string payload");
                                    continue;
                                }
                            };

                            if channel_name == BLOCKS_CHANNEL {
                                match serde_json::from_str::<BlockMinedEvent>(&payload) {
                                    Ok(event) => {
                                        let _ = tx.send(ApiEvent::BlockMined(event));
                                    }
                                    Err(e) => warn!(error = %e, "failed to parse block event"),
                                }
                            } else if channel_name == CELLS_CHANNEL {
                                match serde_json::from_str::<CellCreatedEvent>(&payload) {
                                    Ok(event) => {
                                        let _ = tx.send(ApiEvent::CellCreated(event));
                                    }
                                    Err(e) => warn!(error = %e, "failed to parse cell event"),
                                }
                            }
                        }
                        None => {
                            error!("redis pubsub stream ended unexpectedly");
                            break; // breaks out of inner loop to reconnect
                        }
                    }
                }
            }
        }
    }
}
