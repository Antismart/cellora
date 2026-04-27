//! Library surface for the Cellora REST API.
//!
//! The binary at `main.rs` wires configuration, logging, database pool and
//! graceful shutdown on top of [`build_app`]. Integration tests construct
//! the same [`axum::Router`] directly against a test pool without going
//! through `main`.
//!
//! The middleware stack applied by [`build_app`], outermost to innermost:
//!
//! 1. Panic catcher — turns handler panics into the standard error envelope
//!    instead of a torn connection.
//! 2. Request-id propagation — every request carries `x-request-id` through
//!    to the response, generating a fresh UUID when the client did not set
//!    one.
//! 3. Trace layer — structured spans for every request, keyed on method,
//!    matched path, request id, status and latency.
//! 4. Timeout — fails slow requests with HTTP 408 rather than holding a
//!    database connection open indefinitely.

pub mod admin;
pub mod auth;
pub mod error;
pub mod hex;
pub mod keys;
pub mod openapi;
pub mod pagination;
pub mod ratelimit;
pub mod routes;
pub mod state;
pub mod tip;

use std::time::Duration;

use axum::body::Body;
use axum::extract::State;
use axum::http::{header::HeaderName, HeaderValue, Request, Response, StatusCode};
use axum::middleware::{from_fn_with_state, Next};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use serde_json::json;
use tower::ServiceBuilder;
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::TraceLayer;
use tracing::Span;

// Timeout responses don't carry the standard JSON envelope — the layer
// short-circuits before any handler runs. Using 408 REQUEST_TIMEOUT keeps
// the signal unambiguous for clients.
const TIMEOUT_STATUS: StatusCode = StatusCode::REQUEST_TIMEOUT;

pub use state::AppState;

/// HTTP header used to correlate requests and responses.
const REQUEST_ID_HEADER: HeaderName = HeaderName::from_static("x-request-id");
/// HTTP header naming the indexer's tip height at response time.
const TIP_HEADER: HeaderName = HeaderName::from_static("x-indexer-tip");
/// HTTP header set when the cached tip snapshot is older than the
/// internal staleness threshold.
const TIP_STALE_HEADER: HeaderName = HeaderName::from_static("x-indexer-tip-stale");

/// Build the top-level [`axum::Router`] with every route and middleware
/// attached. Handed to the test harness and to `main.rs` alike.
pub fn build_app(state: AppState) -> Router {
    let request_timeout = Duration::from_millis(state.config.api_request_timeout_ms);

    let middleware = ServiceBuilder::new()
        .layer(CatchPanicLayer::custom(handle_panic))
        .layer(SetRequestIdLayer::new(
            REQUEST_ID_HEADER.clone(),
            MakeRequestUuid,
        ))
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(make_request_span)
                .on_response(on_response),
        )
        .layer(PropagateRequestIdLayer::new(REQUEST_ID_HEADER.clone()))
        .layer(TimeoutLayer::with_status_code(
            TIMEOUT_STATUS,
            request_timeout,
        ));

    let public = Router::new()
        .route("/v1/health", get(routes::health::liveness))
        .route("/v1/health/ready", get(routes::health::readiness))
        .route("/v1/openapi.json", get(openapi_handler));

    // The auth and rate-limit layers sit only on this sub-router. Public
    // routes live in a separate `Router` that never composes with them,
    // so there is no path branch inside the middleware to get wrong.
    //
    // Layer order: requests flow through the layer added LAST first, so
    // listing the rate limiter first and auth second means
    // `auth → rate_limit → handler`. Auth must run first because the
    // rate-limit middleware reads the `AuthenticatedKey` it inserts.
    let authenticated = Router::new()
        .route("/v1/blocks/latest", get(routes::blocks::latest))
        .route("/v1/blocks/:number", get(routes::blocks::by_number))
        .route("/v1/cells", get(routes::cells::list))
        .route("/v1/stats", get(routes::stats::stats))
        .layer(from_fn_with_state(state.clone(), rate_limit_middleware))
        .layer(from_fn_with_state(state.clone(), auth::middleware));

    Router::new()
        .merge(public)
        .merge(authenticated)
        .layer(from_fn_with_state(state.clone(), tip_headers))
        .layer(middleware)
        .with_state(state)
}

/// Serve the OpenAPI specification. Kept outside the normal `routes`
/// module because the spec is both code-derived and self-referential — it
/// describes the handlers that sit next to it.
async fn openapi_handler() -> impl IntoResponse {
    ([("content-type", "application/json")], openapi::spec_json())
}

/// HTTP header carrying the rate-limit bucket capacity for the current
/// surface and tier.
const RATELIMIT_LIMIT_HEADER: HeaderName = HeaderName::from_static("x-ratelimit-limit");
/// HTTP header reporting tokens remaining after the current request.
const RATELIMIT_REMAINING_HEADER: HeaderName = HeaderName::from_static("x-ratelimit-remaining");
/// HTTP header carrying the seconds until the bucket would refill to
/// full from the post-request state.
const RATELIMIT_RESET_HEADER: HeaderName = HeaderName::from_static("x-ratelimit-reset");
/// Standard `Retry-After` header set on 429 responses.
const RETRY_AFTER_HEADER: HeaderName = HeaderName::from_static("retry-after");

/// Rate-limit middleware. Reads the [`auth::AuthenticatedKey`] that
/// `auth::middleware` placed in request extensions, asks the limiter for
/// a decision, and either annotates the response or returns 429.
///
/// The bearer-side bookkeeping (auth resolution, cache) is on the inner
/// layer; this middleware does no DB work.
async fn rate_limit_middleware(
    State(state): State<AppState>,
    request: Request<Body>,
    next: Next,
) -> Response<Body> {
    let key = match request
        .extensions()
        .get::<auth::AuthenticatedKey>()
        .cloned()
    {
        Some(k) => k,
        None => {
            // Auth ran but did not attach a key — programming error in
            // layer ordering. Refuse the request rather than fail open.
            tracing::error!("rate limit middleware reached without an authenticated key");
            return error::ApiError::Internal(anyhow::anyhow!("missing authenticated key"))
                .into_response();
        }
    };

    let Some(limiter) = state.rate_limiter.as_ref() else {
        // No limiter configured — fail open. Logged once per request at
        // DEBUG so operators can spot prolonged misconfiguration.
        tracing::debug!(prefix = %key.prefix, "rate limiter unavailable, allowing");
        return next.run(request).await;
    };

    let params =
        ratelimit::LimitParams::from_config(&state.config, key.tier, ratelimit::Surface::Rest);

    let decision = match limiter
        .check(&key.prefix, ratelimit::Surface::Rest, params)
        .await
    {
        Ok(d) => d,
        Err(err) => {
            if limiter.fails_open() {
                tracing::warn!(error = %err, "rate limiter unreachable, failing open");
                return next.run(request).await;
            }
            tracing::error!(error = %err, "rate limiter unreachable, failing closed");
            return error::ApiError::UpstreamUnavailable("rate limiter unreachable")
                .into_response();
        }
    };

    if !decision.allowed {
        let mut response = error::ApiError::RateLimited {
            retry_after_seconds: decision.retry_after_seconds,
        }
        .into_response();
        if let Ok(value) = HeaderValue::try_from(decision.retry_after_seconds.to_string()) {
            response
                .headers_mut()
                .insert(RETRY_AFTER_HEADER.clone(), value);
        }
        attach_rate_limit_headers(&mut response, &decision);
        return response;
    }

    let mut response = next.run(request).await;
    attach_rate_limit_headers(&mut response, &decision);
    response
}

/// Helper: write `X-RateLimit-*` triplet onto a response. Used both on
/// allow and deny paths so clients can drive backoff from any response.
fn attach_rate_limit_headers(response: &mut Response<Body>, decision: &ratelimit::Decision) {
    let headers = response.headers_mut();
    if let Ok(v) = HeaderValue::try_from(decision.limit.to_string()) {
        headers.insert(RATELIMIT_LIMIT_HEADER.clone(), v);
    }
    if let Ok(v) = HeaderValue::try_from(decision.remaining.to_string()) {
        headers.insert(RATELIMIT_REMAINING_HEADER.clone(), v);
    }
    // Reset is the time-to-refill-to-full from the current state. For an
    // allow we approximate as seconds-to-fill-the-cost (1 token); for a
    // deny we use retry_after.
    if let Ok(v) = HeaderValue::try_from(decision.retry_after_seconds.to_string()) {
        headers.insert(RATELIMIT_RESET_HEADER.clone(), v);
    }
}

/// Annotate every 2xx response with the indexer's tip height. Responses
/// served on a stale snapshot additionally carry `X-Indexer-Tip-Stale`.
/// Non-2xx responses are untouched so error envelopes stay minimal.
async fn tip_headers(
    State(state): State<AppState>,
    request: Request<Body>,
    next: Next,
) -> Response<Body> {
    let mut response = next.run(request).await;
    if response.status().is_success() {
        let snap = state.tip.get();
        if let Some(tip) = snap.indexer_tip {
            if let Ok(value) = HeaderValue::try_from(tip.to_string()) {
                response.headers_mut().insert(TIP_HEADER.clone(), value);
            }
        }
        if snap.is_stale() {
            response
                .headers_mut()
                .insert(TIP_STALE_HEADER.clone(), HeaderValue::from_static("true"));
        }
    }
    response
}

/// Span factory used by [`TraceLayer`]. Captures the request method, matched
/// route (falling back to the raw URI when no route matched) and the
/// correlation id. Status and latency are added by [`on_response`].
fn make_request_span(request: &Request<Body>) -> Span {
    let request_id = request
        .headers()
        .get(&REQUEST_ID_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("-");

    let matched_path = request
        .extensions()
        .get::<axum::extract::MatchedPath>()
        .map(axum::extract::MatchedPath::as_str)
        .unwrap_or_else(|| request.uri().path());

    tracing::info_span!(
        "http_request",
        method = %request.method(),
        path = %matched_path,
        request_id = %request_id,
        status = tracing::field::Empty,
        latency_ms = tracing::field::Empty,
    )
}

/// Response hook used by [`TraceLayer`]. Records `status` and `latency_ms`
/// onto the span created by [`make_request_span`].
fn on_response(response: &Response<Body>, latency: Duration, span: &Span) {
    span.record("status", response.status().as_u16());
    span.record(
        "latency_ms",
        u64::try_from(latency.as_millis()).unwrap_or(u64::MAX),
    );
}

/// Convert a handler panic into a 500 response with the standard error
/// envelope. Without this layer the connection would be torn down and the
/// client would see an opaque transport error.
fn handle_panic(_payload: Box<dyn std::any::Any + Send + 'static>) -> Response<Body> {
    // The `CatchPanicLayer` has already logged the panic message at ERROR
    // through the default panic hook — we emit a stable envelope and move on.
    tracing::error!("api handler panicked");
    let body = Json(json!({
        "error": {
            "code": "internal",
            "message": "internal error",
            "details": null,
        }
    }));
    (StatusCode::INTERNAL_SERVER_ERROR, body).into_response()
}
