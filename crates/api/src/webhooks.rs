use std::net::IpAddr;
use std::time::Duration;

use chrono::Utc;
use hmac::{Hmac, Mac};
use reqwest::Client;
use serde_json::json;
use sha2::Sha256;
use sqlx::{PgPool, Row};
use tokio::sync::broadcast;
use tracing::{error, info, warn};

use crate::events::ApiEvent;

type HmacSha256 = Hmac<Sha256>;

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
pub async fn run_webhook_dispatcher(pool: PgPool, mut rx: broadcast::Receiver<ApiEvent>) {
    let client = Client::builder()
        .timeout(Duration::from_secs(5))
        // Never follow redirects: a 30x from a user-controlled endpoint could
        // otherwise bounce an outbound delivery into internal address space
        // (SSRF), sidestepping the URL validation done at registration time.
        .redirect(reqwest::redirect::Policy::none())
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
        "#,
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
    // Re-validate at delivery time, not just at registration. DNS for the
    // stored host can be re-pointed after the webhook is created (rebinding),
    // so the guard must run against the address we are about to hit.
    if let Err(reason) = validate_webhook_url(&webhook.url).await {
        warn!(url = %webhook.url, reason, "skipping webhook delivery to disallowed url");
        return;
    }

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

    // Serialise once so the signed bytes are exactly the bytes we send.
    let body = match serde_json::to_vec(&payload) {
        Ok(b) => b,
        Err(e) => {
            warn!(url = %webhook.url, error = %e, "failed to serialise webhook payload");
            return;
        }
    };

    let mut req = client
        .post(&webhook.url)
        .header(reqwest::header::CONTENT_TYPE, "application/json");

    // Sign HMAC-SHA256 over "{timestamp}.{body}" keyed by the webhook secret
    // (Stripe-style). The receiver recomputes the MAC and compares in constant
    // time; the timestamp lets them reject replays. The raw secret is never
    // transmitted — the previous code sent it verbatim in the header.
    if let Some(secret) = &webhook.secret {
        let timestamp = Utc::now().timestamp();
        if let Ok(mut mac) = HmacSha256::new_from_slice(secret.as_bytes()) {
            mac.update(timestamp.to_string().as_bytes());
            mac.update(b".");
            mac.update(&body);
            let signature = hex::encode(mac.finalize().into_bytes());
            req = req
                .header("x-webhook-timestamp", timestamp.to_string())
                .header("x-webhook-signature", format!("sha256={signature}"));
        }
    }

    let req = req.body(body);
    let url = webhook.url.clone();

    // Spawn the actual request so we don't block the dispatcher loop
    // if there are multiple webhooks to fire.
    tokio::spawn(async move {
        match req.send().await {
            Ok(res) => {
                if !res.status().is_success() {
                    warn!("webhook {} returned error status: {}", url, res.status());
                }
            }
            Err(e) => {
                warn!("failed to send webhook to {}: {}", url, e);
            }
        }
    });
}

/// Reject webhook URLs that are not plain HTTP(S) or that resolve to a
/// non-public address. Without this guard any authenticated user could point a
/// webhook at cloud-metadata (`169.254.169.254`) or an internal service and
/// have the dispatcher POST event payloads to it (SSRF).
pub async fn validate_webhook_url(raw: &str) -> Result<(), &'static str> {
    let url = url::Url::parse(raw).map_err(|_| "invalid url")?;
    match url.scheme() {
        "http" | "https" => {}
        _ => return Err("scheme must be http or https"),
    }
    let host = url.host_str().ok_or("url has no host")?;
    let port = url.port_or_known_default().unwrap_or(443);

    let mut resolved = false;
    let addrs = tokio::net::lookup_host((host, port))
        .await
        .map_err(|_| "could not resolve host")?;
    for addr in addrs {
        resolved = true;
        if is_blocked_ip(addr.ip()) {
            return Err("url resolves to a non-public address");
        }
    }
    if !resolved {
        return Err("url did not resolve to any address");
    }
    Ok(())
}

/// True for loopback, private, link-local, unique-local, and other
/// non-publicly-routable addresses a webhook must never target.
fn is_blocked_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_documentation()
                // 100.64.0.0/10 carrier-grade NAT
                || (v4.octets()[0] == 100 && (v4.octets()[1] & 0xc0) == 0x40)
        }
        IpAddr::V6(v6) => {
            // An IPv4-mapped address (::ffff:a.b.c.d) must be judged by its
            // embedded IPv4 rules, not the IPv6 ones.
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return is_blocked_ip(IpAddr::V4(mapped));
            }
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                // fc00::/7 unique local
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                // fe80::/10 link local
                || (v6.segments()[0] & 0xffc0) == 0xfe80
        }
    }
}
