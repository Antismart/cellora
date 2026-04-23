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

pub mod error;
pub mod hex;
pub mod pagination;
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

    Router::new()
        .route("/v1/health", get(routes::health::liveness))
        .route("/v1/health/ready", get(routes::health::readiness))
        .route("/v1/blocks/latest", get(routes::blocks::latest))
        .route("/v1/blocks/:number", get(routes::blocks::by_number))
        .route("/v1/cells", get(routes::cells::list))
        .route("/v1/stats", get(routes::stats::stats))
        .layer(from_fn_with_state(state.clone(), tip_headers))
        .layer(middleware)
        .with_state(state)
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
