use std::time::Duration;
use reqwest::Client;
use serde_json::json;
use sqlx::{PgPool, Row};
use tokio::sync::broadcast;
use tracing::{error, info, warn};

use crate::events::ApiEvent;

#[derive(Clone, Debug)]
pub struct Webhook {
    pub id: uuid::Uuid,
    pub user_id: uuid::Uuid,
    pub url: String,
    pub events: Vec<String>,
    pub secret: Option<String>,
}

/// Run the webhook dispatcher task.
/// Listens to `rx` for incoming indexer events, loads configured webhooks,
/// and dispatches HTTP POST requests.
pub async fn run_webhook_dispatcher(
    pool: PgPool,
    mut rx: broadcast::Receiver<ApiEvent>,
) {
    let client = Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap_or_else(|e| {
            error!("failed to build webhook reqwest client: {}", e);
            Client::new()
        });

    info!("webhook dispatcher started");

    loop {
        match rx.recv().await {
            Ok(event) => {
                let event_type = match &event {
                    ApiEvent::BlockMined(_) => "block_mined",
                    ApiEvent::CellCreated(_) => "cell_created",
                };

                // In a production environment with high volume, webhooks should be cached in memory
                // rather than queried per-event.
                let webhooks = match load_webhooks(&pool, event_type).await {
                    Ok(w) => w,
                    Err(e) => {
                        error!("failed to load webhooks for event {}: {}", event_type, e);
                        continue;
                    }
                };

                for webhook in webhooks {
                    dispatch_webhook(&client, &webhook, &event).await;
                }
            }
            Err(broadcast::error::RecvError::Closed) => {
                warn!("event channel closed, stopping webhook dispatcher");
                break;
            }
            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                warn!("webhook dispatcher lagged, skipped {} events", skipped);
            }
        }
    }
}

async fn load_webhooks(pool: &PgPool, event_type: &str) -> Result<Vec<Webhook>, sqlx::Error> {
    let records = sqlx::query(
        r#"
        SELECT id, user_id, url, events, secret
        FROM webhooks
        WHERE $1 = ANY(events)
        "#
    )
    .bind(event_type)
    .fetch_all(pool)
    .await?;

    let mut webhooks = Vec::with_capacity(records.len());
    for rec in records {
        webhooks.push(Webhook {
            id: rec.get("id"),
            user_id: rec.get("user_id"),
            url: rec.get("url"),
            events: rec.get("events"),
            secret: rec.get("secret"),
        });
    }

    Ok(webhooks)
}

async fn dispatch_webhook(client: &Client, webhook: &Webhook, event: &ApiEvent) {
    let payload = match event {
        ApiEvent::BlockMined(b) => json!({
            "event": "block_mined",
            "data": b
        }),
        ApiEvent::CellCreated(c) => json!({
            "event": "cell_created",
            "data": c
        }),
    };

    let mut req = client.post(&webhook.url).json(&payload);
    
    // In a real app we'd sign the payload with `webhook.secret`.
    if let Some(secret) = &webhook.secret {
        req = req.header("x-webhook-signature", secret); // Simplified
    }

    let url = webhook.url.clone();

    // Spawn the actual request so we don't block the dispatcher loop
    // if there are multiple webhooks to fire.
    tokio::spawn(async move {
        match req.send().await {
            Ok(res) => {
                if !res.status().is_success() {
                    warn!(
                        "webhook {} returned error status: {}",
                        url,
                        res.status()
                    );
                }
            }
            Err(e) => {
                warn!("failed to send webhook to {}: {}", url, e);
            }
        }
    });
}
