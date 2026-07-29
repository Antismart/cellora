use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use chrono::Utc;
use hmac::{Hmac, Mac};
use reqwest::Client;
use serde_json::json;
use sha2::Sha256;
use sqlx::{PgPool, Row};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use tracing::{error, info, warn};

use crate::events::ApiEvent;

/// Maximum time to wait for in-flight webbook deliveries to drain when the
/// dispatcher is cancelled. Bounds shutdown so a slow or hung endpoint cannot
/// block the process from exiting indefinitely.
const SHUTDOWN_DRAIN_TIMEOUT: Duration = Duration::from_secs(10);

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
///
/// Listens to `rx` for incoming indexer events, loads configured webhooks, and
/// dispatches HTTP POST requests. Each delivery is spawned onto a
/// [`TaskTracker`] so that, when `cancel` fires, the loop stops promptly and
/// any in-flight POSTs are awaited (up to [`SHUTDOWN_DRAIN_TIMEOUT`]) instead of
/// being dropped mid-flight.
///
/// # Parameters
/// - `pool`: Postgres pool used to load webhook configuration per event.
/// - `rx`: broadcast receiver of indexer [`ApiEvent`]s to fan out.
/// - `cancel`: shutdown token; when cancelled the dispatcher drains and exits.
pub async fn run_webhook_dispatcher(
    pool: PgPool,
    mut rx: broadcast::Receiver<ApiEvent>,
    cancel: CancellationToken,
) {
    info!("webhook dispatcher started");

    // Tracks every spawned delivery so we can await them on shutdown.
    let tracker = TaskTracker::new();

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                info!("webhook dispatcher received shutdown signal");
                break;
            }
            recv = rx.recv() => {
                match recv {
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
                            dispatch_webhook(&tracker, &webhook, &event);
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
    }

    // No more deliveries will be spawned; wait for outstanding ones to finish so
    // in-flight POSTs complete instead of being abandoned on shutdown. Bound the
    // wait so a hung endpoint cannot block process exit.
    tracker.close();
    match tokio::time::timeout(SHUTDOWN_DRAIN_TIMEOUT, tracker.wait()).await {
        Ok(()) => info!("webhook dispatcher drained in-flight deliveries"),
        Err(_) => warn!(
            timeout_secs = SHUTDOWN_DRAIN_TIMEOUT.as_secs(),
            "webhook dispatcher drain timed out; abandoning remaining deliveries"
        ),
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

fn dispatch_webhook(tracker: &TaskTracker, webhook: &Webhook, event: &ApiEvent) {
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

    let url = webhook.url.clone();
    let secret = webhook.secret.clone();

    // Spawn the delivery so we don't block the dispatcher loop when there are
    // multiple webhooks to fire. The task is tracked so it can be awaited on
    // shutdown rather than abandoned.
    tracker.spawn(async move {
        // Re-resolve and vet the destination at delivery time, then PIN the
        // connection to the address we just validated. DNS for the stored host
        // can be re-pointed after the webhook is created (rebinding), and a
        // plain validate-then-connect pattern is a TOCTOU: an attacker serving
        // a TTL=0 record could answer our validation lookup with a public IP
        // and reqwest's connect-time lookup with an internal one. Pinning
        // collapses the two lookups into the single, vetted address.
        let (client, pinned) = match build_pinned_client(&url).await {
            Ok(pair) => pair,
            Err(reason) => {
                warn!(url = %url, reason, "skipping webhook delivery to disallowed url");
                return;
            }
        };

        let mut req = client
            .post(&url)
            .header(reqwest::header::CONTENT_TYPE, "application/json");

        // Sign HMAC-SHA256 over "{timestamp}.{body}" keyed by the webhook
        // secret (Stripe-style). The receiver recomputes the MAC and compares
        // in constant time; the timestamp lets them reject replays. The raw
        // secret is never transmitted — the previous code sent it verbatim in
        // the header.
        if let Some(secret) = &secret {
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

        // Surface the vetted address in delivery logs.
        info!(url = %url, addr = %pinned, "delivering webhook");
        match req.body(body).send().await {
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

/// Resolve `raw`'s host once, pick an allowed [`SocketAddr`], and build a
/// reqwest [`Client`] whose connect-time DNS is pinned to that exact address
/// via `resolve_to_addrs`. This defeats DNS-rebinding TOCTOU: reqwest cannot
/// re-resolve the hostname to a different (internal) address at connect time
/// because the pinned address short-circuits the lookup. SNI and the `Host`
/// header still derive from the original hostname, so TLS remains correct.
///
/// # Errors
/// Returns a static reason string when the URL is malformed, uses a
/// non-HTTP(S) scheme, has no host, fails to resolve, resolves only to blocked
/// (non-public) addresses, or when the client cannot be constructed.
async fn build_pinned_client(raw: &str) -> Result<(Client, SocketAddr), &'static str> {
    let url = url::Url::parse(raw).map_err(|_| "invalid url")?;
    match url.scheme() {
        "http" | "https" => {}
        _ => return Err("scheme must be http or https"),
    }
    let host = url.host_str().ok_or("url has no host")?;
    let port = url.port_or_known_default().unwrap_or(443);

    // Resolve asynchronously; the vetted address is then pinned so the
    // connect-time lookup cannot diverge from what we checked here.
    let addrs = tokio::net::lookup_host((host, port))
        .await
        .map_err(|_| "could not resolve host")?;

    let mut pinned: Option<SocketAddr> = None;
    for addr in addrs {
        if is_blocked_ip(addr.ip()) {
            return Err("url resolves to a non-public address");
        }
        // Take the first allowed address; every resolved address was vetted
        // above, so pinning any one of them is safe.
        if pinned.is_none() {
            pinned = Some(addr);
        }
    }
    let pinned = pinned.ok_or("url did not resolve to any address")?;

    let client = Client::builder()
        .timeout(Duration::from_secs(5))
        // Never follow redirects: a 30x from a user-controlled endpoint could
        // otherwise bounce an outbound delivery into internal address space
        // (SSRF), sidestepping the URL validation done here.
        .redirect(reqwest::redirect::Policy::none())
        // Pin every lookup for this host to the vetted address.
        .resolve_to_addrs(host, &[pinned])
        .build()
        .map_err(|_| "failed to build http client")?;

    Ok((client, pinned))
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
                // 0.0.0.0/8 "this host" — is_unspecified() only matches the
                // single 0.0.0.0, but the whole /8 (e.g. 0.0.0.1) routes to
                // localhost on Linux and must be blocked.
                || v4.octets()[0] == 0
                // 240.0.0.0/4 reserved (Ipv4Addr::is_reserved() is unstable on
                // stable Rust, so match the leading octet directly).
                || v4.octets()[0] >= 240
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn blocks_this_host_slash_eight() {
        // 0.0.0.0/8: is_unspecified() alone misses 0.0.0.1, which routes to
        // localhost on Linux and must be rejected.
        assert!(is_blocked_ip(IpAddr::V4(Ipv4Addr::new(0, 0, 0, 1))));
        assert!(is_blocked_ip(IpAddr::V4(Ipv4Addr::new(0, 1, 2, 3))));
        assert!(is_blocked_ip(IpAddr::V4(Ipv4Addr::UNSPECIFIED)));
    }

    #[test]
    fn blocks_reserved_slash_four() {
        // 240.0.0.0/4 reserved space.
        assert!(is_blocked_ip(IpAddr::V4(Ipv4Addr::new(240, 0, 0, 1))));
        assert!(is_blocked_ip(IpAddr::V4(Ipv4Addr::new(250, 10, 20, 30))));
        assert!(is_blocked_ip(IpAddr::V4(Ipv4Addr::new(255, 255, 255, 255))));
    }

    #[test]
    fn allows_normal_public_ip() {
        // example.com — a routable public address must pass.
        assert!(!is_blocked_ip(IpAddr::V4(Ipv4Addr::new(93, 184, 216, 34))));
        // A routable public address just below the multicast/reserved ranges.
        assert!(!is_blocked_ip(IpAddr::V4(Ipv4Addr::new(
            223, 255, 255, 254
        ))));
    }

    #[tokio::test]
    async fn build_pinned_client_rejects_blocked_and_bad_schemes() {
        // Loopback literal resolves to a blocked address.
        assert!(build_pinned_client("http://127.0.0.1/hook").await.is_err());
        // 0.0.0.0/8 literal is now rejected.
        assert!(build_pinned_client("http://0.0.0.1/hook").await.is_err());
        // Non-HTTP schemes are rejected before any resolution.
        assert!(build_pinned_client("ftp://example.com/hook").await.is_err());
        assert!(build_pinned_client("not a url").await.is_err());
    }
}
