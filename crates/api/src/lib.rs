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
pub mod graphql;
pub mod hex;
pub mod keys;
pub mod metrics;
pub mod openapi;
pub mod pagination;
pub mod ratelimit;
pub mod routes;
pub mod scripts;
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
        .route("/v1/openapi.json", get(openapi_handler))
        .route("/metrics", get(metrics_handler));

    // The auth and rate-limit layers sit only on these sub-routers.
    // Public routes live in a separate `Router` that never composes with
    // them, so there is no path branch inside the middleware to get
    // wrong. REST and GraphQL each have their own rate-limit middleware
    // because the surface (and therefore bucket key + tier params) is
    // fixed per-router.
    //
    // Layer order: requests flow through the layer added LAST first, so
    // listing the rate limiter first and auth second means
    // `auth → rate_limit → handler`. Auth must run first because the
    // rate-limit middleware reads the `AuthenticatedKey` it inserts.
    let rest = Router::new()
        .route("/v1/blocks/latest", get(routes::blocks::latest))
        .route("/v1/blocks/:number", get(routes::blocks::by_number))
        .route("/v1/cells", get(routes::cells::list))
        .route("/v1/stats", get(routes::stats::stats))
        .route("/v1/proofs/:tx_hash", get(routes::proofs::passthrough))
        .layer(from_fn_with_state(state.clone(), rate_limit_rest))
        .layer(from_fn_with_state(state.clone(), auth::middleware));

    let graphql_schema = graphql::build_schema(state.clone());
    let graphql_router = Router::new()
        .route("/graphql", axum::routing::post(graphql_handler))
        .layer(axum::Extension(graphql_schema))
        .layer(from_fn_with_state(state.clone(), rate_limit_graphql))
        .layer(from_fn_with_state(state.clone(), auth::middleware));

    Router::new()
        .merge(public)
        .merge(rest)
        .merge(graphql_router)
        .layer(from_fn_with_state(state.clone(), tip_headers))
        .layer(from_fn_with_state(state.clone(), record_request_metrics))
        .layer(middleware)
        .with_state(state)
}

/// Serve the Prometheus text-format snapshot. Public route — operators
/// are expected to IP-restrict it at the edge (Cloudflare, ingress) in
/// production rather than rely on the application for access control.
async fn metrics_handler(State(state): State<AppState>) -> impl IntoResponse {
    // Snapshot pool stats once per scrape so the `db_pool_*` gauges
    // reflect the moment the scrape was taken, not request-driven.
    let active = state.db.size();
    let idle = u32::try_from(state.db.num_idle()).unwrap_or(u32::MAX);
    state.metrics.record_pool(active, idle);
    (
        [("content-type", "text/plain; version=0.0.4")],
        state.metrics.render(),
    )
}

/// Outermost-but-one middleware that records every request's outcome
/// against the Prometheus registry. Sits outside the auth/rate-limit
/// layers so requests that are rejected at those layers still show up
/// in `api_requests_total` — operators want to see the 401 / 429 rate.
async fn record_request_metrics(
    State(state): State<AppState>,
    request: Request<Body>,
    next: Next,
) -> Response<Body> {
    // The `/metrics` endpoint counts itself; the volume is one request
    // per scrape interval (typically 15s) so the noise is negligible
    // and excluding it would just complicate the middleware.
    let method = request.method().as_str().to_owned();
    let matched_path = request
        .extensions()
        .get::<axum::extract::MatchedPath>()
        .map(|m| m.as_str().to_owned())
        .unwrap_or_else(|| request.uri().path().to_owned());

    let started = std::time::Instant::now();
    let response = next.run(request).await;
    let elapsed = started.elapsed().as_secs_f64();
    let status = response.status().as_u16();

    state
        .metrics
        .observe_request(&method, &matched_path, status, elapsed);
    response
}

/// Wire-format shape of a GraphQL request body. Mirrors the standard
/// `application/json` shape used by every GraphQL client.
#[derive(serde::Deserialize)]
struct GraphQlRequestBody {
    query: String,
    #[serde(default)]
    variables: Option<serde_json::Value>,
    #[serde(default, rename = "operationName")]
    operation_name: Option<String>,
}

/// GraphQL POST handler. We avoid the optional `async-graphql-axum`
/// integration crate because it locks us to a specific axum version;
/// `async-graphql::Schema::execute` accepts a `Request` we can build
/// directly from the JSON body.
async fn graphql_handler(
    axum::Extension(schema): axum::Extension<graphql::ApiSchema>,
    Json(body): Json<GraphQlRequestBody>,
) -> Response<Body> {
    let mut req = async_graphql::Request::new(body.query);
    if let Some(variables) = body.variables {
        req = req.variables(async_graphql::Variables::from_json(variables));
    }
    if let Some(name) = body.operation_name {
        req = req.operation_name(name);
    }
    let response = schema.execute(req).await;
    let payload = serde_json::to_vec(&response).unwrap_or_else(|_| b"{}".to_vec());
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "application/json")
        .body(Body::from(payload))
        .unwrap_or_else(|_| (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response())
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

/// Rate-limit middleware for the REST surface.
async fn rate_limit_rest(
    state: State<AppState>,
    request: Request<Body>,
    next: Next,
) -> Response<Body> {
    rate_limit_for(state, request, next, ratelimit::Surface::Rest).await
}

/// Rate-limit middleware for the GraphQL surface.
async fn rate_limit_graphql(
    state: State<AppState>,
    request: Request<Body>,
    next: Next,
) -> Response<Body> {
    rate_limit_for(state, request, next, ratelimit::Surface::Graphql).await
}

/// Shared rate-limit logic. Reads the [`auth::AuthenticatedKey`] that
/// `auth::middleware` placed in request extensions, asks the limiter for
/// a decision against the supplied `surface`, and either annotates the
/// response or returns 429. Bearer resolution / verification cache live
/// on the inner auth layer; this middleware does no DB work.
async fn rate_limit_for(
    State(state): State<AppState>,
    request: Request<Body>,
    next: Next,
    surface: ratelimit::Surface,
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

    let surface_label = match surface {
        ratelimit::Surface::Rest => "rest",
        ratelimit::Surface::Graphql => "graphql",
    };
    let tier_label = key.tier.as_str();

    let Some(limiter) = state.rate_limiter.as_ref() else {
        // No limiter configured — fail open. Logged at DEBUG so
        // operators can spot prolonged misconfiguration.
        tracing::debug!(prefix = %key.prefix, "rate limiter unavailable, allowing");
        state.metrics.observe_rate_limit(
            surface_label,
            tier_label,
            metrics::RateLimitOutcome::FailOpen,
        );
        return next.run(request).await;
    };

    let params = ratelimit::LimitParams::from_config(&state.config, key.tier, surface);

    let decision = match limiter.check(&key.prefix, surface, params).await {
        Ok(d) => d,
        Err(err) => {
            if limiter.fails_open() {
                tracing::warn!(error = %err, "rate limiter unreachable, failing open");
                state.metrics.observe_rate_limit(
                    surface_label,
                    tier_label,
                    metrics::RateLimitOutcome::FailOpen,
                );
                return next.run(request).await;
            }
            tracing::error!(error = %err, "rate limiter unreachable, failing closed");
            state.metrics.observe_rate_limit(
                surface_label,
                tier_label,
                metrics::RateLimitOutcome::FailClosed,
            );
            return error::ApiError::UpstreamUnavailable("rate limiter unreachable")
                .into_response();
        }
    };

    if !decision.allowed {
        state.metrics.observe_rate_limit(
            surface_label,
            tier_label,
            metrics::RateLimitOutcome::Limited,
        );
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

    state.metrics.observe_rate_limit(
        surface_label,
        tier_label,
        metrics::RateLimitOutcome::Allowed,
    );
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
