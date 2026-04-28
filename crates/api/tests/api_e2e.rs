//! End-to-end integration tests for `cellora-api`.
//!
//! Each test spins up a throwaway Postgres via testcontainers, applies the
//! schema, constructs the full [`axum::Router`] via [`cellora_api::build_app`]
//! and drives it through `tower::ServiceExt::oneshot` so no socket is bound.
//!
//! This file will grow as later slices land; slice 1 only exercises the
//! health endpoints and the middleware stack.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::time::Duration;

use axum::body::Body;
use axum::http::{header::HeaderName, Request, StatusCode};
use std::time::{Instant, SystemTime};

use bigdecimal::BigDecimal;
use cellora_api::keys as api_keys_helper;
use cellora_api::ratelimit::RateLimiter;
use cellora_api::tip::{TipSnapshot, TipTracker};
use cellora_api::{build_app, AppState};
use cellora_common::config::{Config, LogFormat};
use cellora_db::models::{ApiKeyTier, BlockRow, CellRow, ConsumedCellRef, HashType};
use cellora_db::{api_keys, blocks, cells, connect, migrate};
use http_body_util::BodyExt;
use serde_json::Value;
use sqlx::PgPool;
use testcontainers_modules::{
    postgres::Postgres,
    redis::Redis,
    testcontainers::{runners::AsyncRunner, ContainerAsync, ImageExt},
};
use tower::ServiceExt;

const REQUEST_ID: HeaderName = HeaderName::from_static("x-request-id");

struct Harness {
    app: axum::Router,
    pool: PgPool,
    tip: TipTracker,
    /// Full bearer string of the test key the harness pre-issues. Tests
    /// that exercise authenticated endpoints should pass this through
    /// [`get_authed`].
    bearer: String,
    // Keep containers alive for the lifetime of the test.
    _pg: ContainerAsync<Postgres>,
    _redis: Option<ContainerAsync<Redis>>,
}

/// What infrastructure the harness should spin up. Most tests use
/// [`HarnessOpts::default`] which provisions both Postgres and Redis;
/// rate-limit-specific failure tests use [`HarnessOpts::no_redis`] to
/// exercise the fail-open / fail-closed code paths without a real
/// outage.
#[derive(Debug, Clone, Copy, Default)]
struct HarnessOpts {
    /// When `false`, no Redis container is started and the limiter is
    /// not attached to `AppState`. Defaults to `true`.
    with_redis: bool,
    /// When set, override the free-tier REST burst capacity in the
    /// generated config so a small fixed N triggers 429.
    free_rest_burst_override: Option<u32>,
    /// When set, override the free-tier REST refill rate.
    free_rest_refill_override: Option<f64>,
    /// When `false`, configure the limiter to fail closed on Redis
    /// errors instead of fail-open.
    fail_open: bool,
}

impl HarnessOpts {
    fn defaults() -> Self {
        Self {
            with_redis: true,
            free_rest_burst_override: None,
            free_rest_refill_override: None,
            fail_open: true,
        }
    }

    fn no_redis(self) -> Self {
        Self {
            with_redis: false,
            ..self
        }
    }

    fn small_free_burst(self, burst: u32) -> Self {
        Self {
            free_rest_burst_override: Some(burst),
            // Default to a very slow refill so the burst bound is what
            // a test observes, not the per-second top-up.
            free_rest_refill_override: Some(0.1),
            ..self
        }
    }
}

async fn up() -> Harness {
    up_with(HarnessOpts::defaults()).await
}

async fn up_with(opts: HarnessOpts) -> Harness {
    let pg = Postgres::default()
        .with_tag("16-alpine")
        .start()
        .await
        .expect("start postgres");
    let host = pg.get_host().await.expect("host");
    let port = pg.get_host_port_ipv4(5432).await.expect("port");
    let url = format!("postgres://postgres:postgres@{host}:{port}/postgres");

    let pool = connect_with_retry(&url, 10).await;
    migrate::run(&pool).await.expect("migrate");

    // Pre-issue a free-tier key so authenticated endpoints can be
    // exercised by the existing test bodies. Specific auth-failure
    // cases construct their own keys / headers.
    let issued = api_keys_helper::generate().expect("generate");
    api_keys::insert(
        &pool,
        &issued.prefix,
        &issued.secret_hash,
        ApiKeyTier::Free,
        Some("test"),
    )
    .await
    .expect("insert key");

    let mut config = test_config(&url);
    if let Some(burst) = opts.free_rest_burst_override {
        config.api_rate_limit_free_rest_burst = burst;
    }
    if let Some(refill) = opts.free_rest_refill_override {
        config.api_rate_limit_free_rest_refill_per_sec = refill;
    }
    config.api_rate_limit_fail_open = opts.fail_open;

    // Start Redis (or not). If not, the limiter is never attached and
    // the middleware's no-limiter fail-open path is exercised.
    let (redis_container, limiter) = if opts.with_redis {
        let redis = Redis
            .with_tag("7-alpine")
            .start()
            .await
            .expect("start redis");
        let host = redis.get_host().await.expect("redis host");
        let port = redis.get_host_port_ipv4(6379).await.expect("redis port");
        config.redis_url = format!("redis://{host}:{port}");

        let client = redis::Client::open(config.redis_url.as_str()).expect("redis client");
        let manager = redis::aio::ConnectionManager::new(client)
            .await
            .expect("redis manager");
        let limiter = RateLimiter::new(manager, config.api_rate_limit_fail_open);
        (Some(redis), Some(limiter))
    } else {
        (None, None)
    };

    let tip = TipTracker::new();
    let mut state = AppState::with_tip(pool.clone(), config, tip.clone());
    if let Some(l) = limiter {
        state = state.with_rate_limiter(l);
    }
    let app = build_app(state);
    Harness {
        app,
        pool,
        tip,
        bearer: issued.full,
        _pg: pg,
        _redis: redis_container,
    }
}

fn fresh_tip(indexer_tip: Option<i64>, node_tip: Option<u64>) -> TipSnapshot {
    TipSnapshot {
        indexer_tip,
        node_tip,
        observed_at: SystemTime::now(),
        observed_monotonic: Instant::now(),
    }
}

/// Build a deterministic test block with the given number. Hashes derive
/// from `seed` so distinct fixtures don't collide on the `hash` unique
/// constraint.
fn make_block(number: i64, seed: u8) -> BlockRow {
    BlockRow {
        number,
        hash: vec![seed; 32],
        parent_hash: vec![seed.wrapping_sub(1); 32],
        timestamp_ms: 1_700_000_000_000 + number * 1_000,
        epoch: number,
        transactions_count: 1,
        proposals_count: 0,
        uncles_count: 0,
        nonce: BigDecimal::from(12345 + number),
        dao: vec![0xaa; 32],
    }
}

async fn seed_block(pool: &PgPool, row: &BlockRow) {
    blocks::insert(pool, row).await.expect("insert block");
}

/// Build a single cell with a deterministic shape. `lock_hash` and
/// `type_hash` are controllable so tests can target specific scripts.
fn make_cell(
    block_number: i64,
    tx_seed: u8,
    output_index: i32,
    lock_hash: [u8; 32],
    type_hash: Option<[u8; 32]>,
) -> CellRow {
    CellRow {
        tx_hash: vec![tx_seed; 32],
        output_index,
        block_number,
        capacity_shannons: 100 * 100_000_000 + i64::from(output_index),
        lock_code_hash: vec![0x01; 32],
        lock_hash_type: HashType::Type,
        lock_args: vec![tx_seed, 0x01, 0x02],
        lock_hash: lock_hash.to_vec(),
        type_code_hash: type_hash.map(|_| vec![0x02; 32]),
        type_hash_type: type_hash.map(|_| HashType::Data1),
        type_args: type_hash.map(|_| vec![tx_seed, 0x03]),
        type_hash: type_hash.map(|t| t.to_vec()),
        data: vec![tx_seed; 8],
    }
}

async fn seed_cells(pool: &PgPool, rows: &[CellRow]) {
    let mut tx = pool.begin().await.expect("begin tx");
    cells::insert_batch(&mut tx, rows)
        .await
        .expect("insert cells");
    tx.commit().await.expect("commit tx");
}

async fn seed_consumed(pool: &PgPool, refs: &[ConsumedCellRef]) {
    let mut tx = pool.begin().await.expect("begin tx");
    cells::mark_consumed(&mut tx, refs)
        .await
        .expect("mark consumed");
    tx.commit().await.expect("commit tx");
}

async fn connect_with_retry(url: &str, attempts: u8) -> sqlx::PgPool {
    for attempt in 1..=attempts {
        match connect(url).await {
            Ok(pool) => return pool,
            Err(err) if attempt == attempts => panic!("connect after {attempts} attempts: {err}"),
            Err(_) => tokio::time::sleep(Duration::from_millis(300)).await,
        }
    }
    unreachable!()
}

fn test_config(database_url: &str) -> Config {
    Config {
        database_url: database_url.to_owned(),
        ckb_rpc_url: "http://localhost:0".to_owned(),
        poll_interval_ms: 2_000,
        indexer_start_block: 0,
        indexer_reorg_target_depth: 12,
        indexer_reorg_max_depth: 100,
        indexer_metrics_bind_addr: "0.0.0.0:0".to_owned(),
        log_level: "info".to_owned(),
        log_format: LogFormat::Pretty,
        api_bind_addr: "0.0.0.0:0".to_owned(),
        api_default_page_size: 50,
        api_max_page_size: 500,
        api_request_timeout_ms: 10_000,
        api_tip_cache_refresh_ms: 1_000,
        api_auth_cache_ttl_seconds: 60,
        api_auth_cache_capacity: 10_000,
        redis_url: "redis://localhost:6379".to_owned(),
        api_rate_limit_fail_open: true,
        api_rate_limit_free_rest_burst: 30,
        api_rate_limit_free_rest_refill_per_sec: 1.0,
        api_rate_limit_starter_rest_burst: 300,
        api_rate_limit_starter_rest_refill_per_sec: 20.0,
        api_rate_limit_pro_rest_burst: 3_000,
        api_rate_limit_pro_rest_refill_per_sec: 200.0,
        api_rate_limit_free_graphql_burst: 10,
        api_rate_limit_free_graphql_refill_per_sec: 0.5,
        api_rate_limit_starter_graphql_burst: 100,
        api_rate_limit_starter_graphql_refill_per_sec: 10.0,
        api_rate_limit_pro_graphql_burst: 1_000,
        api_rate_limit_pro_graphql_refill_per_sec: 100.0,
    }
}

fn get(path: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(path)
        .body(Body::empty())
        .expect("build request")
}

/// `GET path` with `Authorization: Bearer <bearer>` attached. Used by every
/// test that hits an authenticated endpoint via the harness's pre-issued
/// key.
fn get_authed(path: &str, bearer: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(path)
        .header("authorization", format!("Bearer {bearer}"))
        .body(Body::empty())
        .expect("build request")
}

async fn read_json(body: Body) -> Value {
    let bytes = body.collect().await.expect("collect body").to_bytes();
    serde_json::from_slice(&bytes).expect("parse json")
}

#[tokio::test(flavor = "multi_thread")]
async fn liveness_returns_ok_with_version() {
    let harness = up().await;

    let response = harness
        .app
        .clone()
        .oneshot(get("/v1/health"))
        .await
        .expect("serve request");

    assert_eq!(response.status(), StatusCode::OK);
    let body = read_json(response.into_body()).await;
    assert_eq!(body["status"], "ok");
    assert!(body["version"].is_string(), "version field present");
}

#[tokio::test(flavor = "multi_thread")]
async fn readiness_returns_ok_when_db_is_healthy() {
    let harness = up().await;

    let response = harness
        .app
        .clone()
        .oneshot(get("/v1/health/ready"))
        .await
        .expect("serve request");

    assert_eq!(response.status(), StatusCode::OK);
    let body = read_json(response.into_body()).await;
    assert_eq!(body["status"], "ready");
    assert_eq!(body["db"], "ok");
}

#[tokio::test(flavor = "multi_thread")]
async fn request_id_is_propagated_to_response() {
    let harness = up().await;

    let request = Request::builder()
        .method("GET")
        .uri("/v1/health")
        .header(&REQUEST_ID, "test-correlation-id")
        .body(Body::empty())
        .expect("build request");

    let response = harness
        .app
        .clone()
        .oneshot(request)
        .await
        .expect("serve request");

    assert_eq!(response.status(), StatusCode::OK);
    let echoed = response
        .headers()
        .get(&REQUEST_ID)
        .expect("x-request-id header present")
        .to_str()
        .expect("header is ascii");
    assert_eq!(echoed, "test-correlation-id");
}

#[tokio::test(flavor = "multi_thread")]
async fn request_id_is_generated_when_client_omits_it() {
    let harness = up().await;

    let response = harness
        .app
        .clone()
        .oneshot(get("/v1/health"))
        .await
        .expect("serve request");

    assert_eq!(response.status(), StatusCode::OK);
    let generated = response
        .headers()
        .get(&REQUEST_ID)
        .expect("x-request-id header present")
        .to_str()
        .expect("header is ascii");
    assert!(
        !generated.is_empty(),
        "generated request id should be non-empty"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_route_under_protected_namespace_returns_401_without_auth() {
    // Once auth lands, the authenticated sub-router is the catch-all for
    // anything not matched by a public route. Without a Bearer header
    // every miss returns 401 — we deliberately do not leak whether a
    // path corresponds to a real endpoint to unauthenticated clients.
    let harness = up().await;

    let response = harness
        .app
        .clone()
        .oneshot(get("/v1/does-not-exist"))
        .await
        .expect("serve request");

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_route_under_protected_namespace_returns_404_when_authed() {
    // With a valid Bearer the auth layer passes through and the inner
    // authenticated router returns 404 for unmatched paths — tooling
    // (Postman, OpenAPI clients) can distinguish "wrong path" from
    // "wrong key".
    let harness = up().await;

    let response = harness
        .app
        .clone()
        .oneshot(get_authed("/v1/does-not-exist", &harness.bearer))
        .await
        .expect("serve request");

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread")]
async fn blocks_latest_returns_404_on_empty_database() {
    let harness = up().await;

    let response = harness
        .app
        .clone()
        .oneshot(get_authed("/v1/blocks/latest", &harness.bearer))
        .await
        .expect("serve request");

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = read_json(response.into_body()).await;
    assert_eq!(body["error"]["code"], "not_found");
}

#[tokio::test(flavor = "multi_thread")]
async fn blocks_latest_returns_highest_numbered_block() {
    let harness = up().await;
    seed_block(&harness.pool, &make_block(0, 0x10)).await;
    seed_block(&harness.pool, &make_block(7, 0x20)).await;
    seed_block(&harness.pool, &make_block(3, 0x30)).await;

    let response = harness
        .app
        .clone()
        .oneshot(get_authed("/v1/blocks/latest", &harness.bearer))
        .await
        .expect("serve request");

    assert_eq!(response.status(), StatusCode::OK);
    let body = read_json(response.into_body()).await;
    assert_eq!(body["number"], 7);
    let hash = body["hash"].as_str().expect("hash is a string");
    assert!(hash.starts_with("0x"));
    assert_eq!(hash.len(), 66, "32 bytes -> 64 hex chars + 0x prefix");
}

#[tokio::test(flavor = "multi_thread")]
async fn blocks_by_number_returns_requested_block() {
    let harness = up().await;
    seed_block(&harness.pool, &make_block(42, 0xab)).await;

    let response = harness
        .app
        .clone()
        .oneshot(get_authed("/v1/blocks/42", &harness.bearer))
        .await
        .expect("serve request");

    assert_eq!(response.status(), StatusCode::OK);
    let body = read_json(response.into_body()).await;
    assert_eq!(body["number"], 42);
    assert_eq!(body["transactions_count"], 1);
    assert!(body["indexed_at"].is_string(), "indexed_at is RFC3339");
}

#[tokio::test(flavor = "multi_thread")]
async fn blocks_by_number_returns_404_on_unknown_number() {
    let harness = up().await;

    let response = harness
        .app
        .clone()
        .oneshot(get_authed("/v1/blocks/999999", &harness.bearer))
        .await
        .expect("serve request");

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body = read_json(response.into_body()).await;
    assert_eq!(body["error"]["code"], "not_found");
}

#[tokio::test(flavor = "multi_thread")]
async fn blocks_by_number_rejects_non_numeric_path() {
    let harness = up().await;

    let response = harness
        .app
        .clone()
        .oneshot(get_authed("/v1/blocks/abc", &harness.bearer))
        .await
        .expect("serve request");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = read_json(response.into_body()).await;
    assert_eq!(body["error"]["code"], "bad_request");
}

#[tokio::test(flavor = "multi_thread")]
async fn blocks_by_number_rejects_negative_path() {
    let harness = up().await;

    let response = harness
        .app
        .clone()
        .oneshot(get_authed("/v1/blocks/-1", &harness.bearer))
        .await
        .expect("serve request");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = read_json(response.into_body()).await;
    assert_eq!(body["error"]["code"], "bad_request");
}

// ---------------------------------------------------------------------------
// /v1/cells
// ---------------------------------------------------------------------------

const LOCK_A: [u8; 32] = [0xaa; 32];
const LOCK_B: [u8; 32] = [0xbb; 32];
const TYPE_A: [u8; 32] = [0xcc; 32];

fn hex_prefixed(bytes: &[u8]) -> String {
    let mut buf = String::with_capacity(2 + bytes.len() * 2);
    buf.push_str("0x");
    for byte in bytes {
        use std::fmt::Write;
        let _ = write!(&mut buf, "{byte:02x}");
    }
    buf
}

#[tokio::test(flavor = "multi_thread")]
async fn cells_requires_exactly_one_filter() {
    let harness = up().await;

    let response = harness
        .app
        .clone()
        .oneshot(get_authed("/v1/cells", &harness.bearer))
        .await
        .expect("serve request");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = read_json(response.into_body()).await;
    assert_eq!(body["error"]["code"], "bad_request");

    let uri = format!(
        "/v1/cells?lock_hash={}&type_hash={}",
        hex_prefixed(&LOCK_A),
        hex_prefixed(&TYPE_A)
    );
    let response = harness
        .app
        .clone()
        .oneshot(get_authed(&uri, &harness.bearer))
        .await
        .expect("serve request");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test(flavor = "multi_thread")]
async fn cells_rejects_invalid_lock_hash() {
    let harness = up().await;

    let response = harness
        .app
        .clone()
        .oneshot(get_authed("/v1/cells?lock_hash=not-hex", &harness.bearer))
        .await
        .expect("serve request");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = read_json(response.into_body()).await;
    assert_eq!(body["error"]["code"], "bad_request");
}

#[tokio::test(flavor = "multi_thread")]
async fn cells_returns_empty_page_on_unknown_lock() {
    let harness = up().await;
    let uri = format!("/v1/cells?lock_hash={}", hex_prefixed(&LOCK_A));

    let response = harness
        .app
        .clone()
        .oneshot(get_authed(&uri, &harness.bearer))
        .await
        .expect("serve request");
    assert_eq!(response.status(), StatusCode::OK);
    let body = read_json(response.into_body()).await;
    assert!(body["data"].as_array().unwrap().is_empty());
    assert!(body["next_cursor"].is_null());
}

#[tokio::test(flavor = "multi_thread")]
async fn cells_by_lock_hash_returns_matching_cells() {
    let harness = up().await;
    seed_block(&harness.pool, &make_block(10, 0x10)).await;

    let cells_fixture = vec![
        make_cell(10, 0x11, 0, LOCK_A, Some(TYPE_A)),
        make_cell(10, 0x22, 0, LOCK_B, None),
        make_cell(10, 0x11, 1, LOCK_A, None),
    ];
    seed_cells(&harness.pool, &cells_fixture).await;

    let uri = format!("/v1/cells?lock_hash={}", hex_prefixed(&LOCK_A));
    let response = harness
        .app
        .clone()
        .oneshot(get_authed(&uri, &harness.bearer))
        .await
        .expect("serve request");
    assert_eq!(response.status(), StatusCode::OK);
    let body = read_json(response.into_body()).await;

    let data = body["data"].as_array().expect("array");
    assert_eq!(data.len(), 2);
    for cell in data {
        assert_eq!(cell["lock_hash"], hex_prefixed(&LOCK_A));
        assert_eq!(cell["is_live"], true);
        assert_eq!(cell["block_number"], 10);
        assert_eq!(cell["block_hash"], hex_prefixed(&[0x10u8; 32]));
        assert_eq!(cell["lock"]["hash_type"], "type");
        assert!(cell["data"].is_null(), "data omitted by default");
    }
    assert!(body["meta"]["indexer_tip"].is_null());
}

#[tokio::test(flavor = "multi_thread")]
async fn cells_include_data_toggle() {
    let harness = up().await;
    seed_block(&harness.pool, &make_block(10, 0x10)).await;
    seed_cells(
        &harness.pool,
        &[make_cell(10, 0x11, 0, LOCK_A, Some(TYPE_A))],
    )
    .await;

    let uri = format!(
        "/v1/cells?lock_hash={}&include_data=true",
        hex_prefixed(&LOCK_A)
    );
    let response = harness
        .app
        .clone()
        .oneshot(get_authed(&uri, &harness.bearer))
        .await
        .expect("serve request");
    assert_eq!(response.status(), StatusCode::OK);
    let body = read_json(response.into_body()).await;
    let data = body["data"].as_array().expect("array");
    assert_eq!(data.len(), 1);
    let hex = data[0]["data"].as_str().expect("data present");
    assert!(hex.starts_with("0x"));
    assert_eq!(hex.len(), 2 + 8 * 2, "8 bytes -> 16 hex chars + prefix");
}

#[tokio::test(flavor = "multi_thread")]
async fn cells_is_live_filter() {
    let harness = up().await;
    seed_block(&harness.pool, &make_block(10, 0x10)).await;
    seed_block(&harness.pool, &make_block(11, 0x11)).await;

    let live = make_cell(10, 0x11, 0, LOCK_A, None);
    let dead = make_cell(10, 0x22, 0, LOCK_A, None);
    seed_cells(&harness.pool, &[live.clone(), dead.clone()]).await;
    seed_consumed(
        &harness.pool,
        &[ConsumedCellRef {
            tx_hash: dead.tx_hash.clone(),
            output_index: dead.output_index,
            consumed_by_tx_hash: vec![0x99; 32],
            consumed_by_input_index: 0,
            consumed_at_block_number: 11,
        }],
    )
    .await;

    let base = format!("/v1/cells?lock_hash={}", hex_prefixed(&LOCK_A));

    let only_live = read_json(
        harness
            .app
            .clone()
            .oneshot(get_authed(&format!("{base}&is_live=true"), &harness.bearer))
            .await
            .expect("serve")
            .into_body(),
    )
    .await;
    let live_ids: Vec<_> = only_live["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["tx_hash"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(live_ids, vec![hex_prefixed(&live.tx_hash)]);

    let only_dead = read_json(
        harness
            .app
            .clone()
            .oneshot(get_authed(
                &format!("{base}&is_live=false"),
                &harness.bearer,
            ))
            .await
            .expect("serve")
            .into_body(),
    )
    .await;
    let dead_ids: Vec<_> = only_dead["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["tx_hash"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(dead_ids, vec![hex_prefixed(&dead.tx_hash)]);
    assert_eq!(only_dead["data"][0]["is_live"], false);
    assert_eq!(
        only_dead["data"][0]["consumed_by"]["tx_hash"],
        hex_prefixed(&[0x99u8; 32])
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn cells_pagination_returns_every_row_exactly_once() {
    let harness = up().await;
    seed_block(&harness.pool, &make_block(10, 0x10)).await;
    seed_block(&harness.pool, &make_block(11, 0x11)).await;
    seed_block(&harness.pool, &make_block(12, 0x12)).await;

    // 7 cells matching LOCK_A spread across three blocks / seeds.
    let cells_fixture: Vec<CellRow> = [
        (10, 0x01, 0),
        (10, 0x01, 1),
        (10, 0x02, 0),
        (11, 0x03, 0),
        (11, 0x03, 1),
        (12, 0x04, 0),
        (12, 0x05, 0),
    ]
    .iter()
    .map(|(bn, seed, oi)| make_cell(*bn, *seed, *oi, LOCK_A, None))
    .collect();
    seed_cells(&harness.pool, &cells_fixture).await;

    let mut seen = Vec::new();
    let mut cursor: Option<String> = None;
    let mut iteration = 0;

    loop {
        iteration += 1;
        assert!(iteration <= 10, "pagination did not terminate");

        let mut uri = format!("/v1/cells?lock_hash={}&limit=3", hex_prefixed(&LOCK_A));
        if let Some(c) = cursor.as_deref() {
            uri.push_str(&format!("&cursor={c}"));
        }
        let body = read_json(
            harness
                .app
                .clone()
                .oneshot(get_authed(&uri, &harness.bearer))
                .await
                .expect("serve")
                .into_body(),
        )
        .await;

        let page = body["data"].as_array().expect("array").clone();
        for cell in &page {
            let key = (
                cell["block_number"].as_i64().unwrap(),
                cell["tx_hash"].as_str().unwrap().to_owned(),
                cell["output_index"].as_i64().unwrap() as i32,
            );
            seen.push(key);
        }

        match body["next_cursor"].as_str() {
            Some(c) if !c.is_empty() => {
                assert!(page.len() <= 3, "page exceeds requested limit");
                cursor = Some(c.to_owned());
            }
            _ => break,
        }
    }

    assert_eq!(seen.len(), 7, "every cell returned exactly once");
    // Results are newest-first; first row should be block 12 / seed 0x05.
    assert_eq!(seen.first().unwrap().0, 12);
    // Each key is unique.
    let mut sorted = seen.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), seen.len(), "duplicates across pages");
}

#[tokio::test(flavor = "multi_thread")]
async fn cells_rejects_invalid_cursor() {
    let harness = up().await;
    let uri = format!(
        "/v1/cells?lock_hash={}&cursor=not-a-real-cursor",
        hex_prefixed(&LOCK_A)
    );

    let response = harness
        .app
        .clone()
        .oneshot(get_authed(&uri, &harness.bearer))
        .await
        .expect("serve request");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = read_json(response.into_body()).await;
    assert_eq!(body["error"]["code"], "invalid_cursor");
}

#[tokio::test(flavor = "multi_thread")]
async fn cells_rejects_limit_above_max() {
    let harness = up().await;
    let uri = format!("/v1/cells?lock_hash={}&limit=99999", hex_prefixed(&LOCK_A));

    let response = harness
        .app
        .clone()
        .oneshot(get_authed(&uri, &harness.bearer))
        .await
        .expect("serve request");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = read_json(response.into_body()).await;
    assert_eq!(body["error"]["code"], "bad_request");
}

#[tokio::test(flavor = "multi_thread")]
async fn cells_rejects_limit_zero() {
    let harness = up().await;
    let uri = format!("/v1/cells?lock_hash={}&limit=0", hex_prefixed(&LOCK_A));

    let response = harness
        .app
        .clone()
        .oneshot(get_authed(&uri, &harness.bearer))
        .await
        .expect("serve request");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test(flavor = "multi_thread")]
async fn cells_by_type_hash_returns_matching_cells() {
    let harness = up().await;
    seed_block(&harness.pool, &make_block(10, 0x10)).await;
    seed_cells(
        &harness.pool,
        &[
            make_cell(10, 0x11, 0, LOCK_A, Some(TYPE_A)),
            make_cell(10, 0x22, 0, LOCK_B, None),
        ],
    )
    .await;

    let uri = format!("/v1/cells?type_hash={}", hex_prefixed(&TYPE_A));
    let response = harness
        .app
        .clone()
        .oneshot(get_authed(&uri, &harness.bearer))
        .await
        .expect("serve request");
    assert_eq!(response.status(), StatusCode::OK);
    let body = read_json(response.into_body()).await;
    let data = body["data"].as_array().expect("array");
    assert_eq!(data.len(), 1);
    assert_eq!(data[0]["type_hash"], hex_prefixed(&TYPE_A));
    assert_eq!(data[0]["type"]["hash_type"], "data1");
}

// ---------------------------------------------------------------------------
// /v1/stats + tip headers
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn stats_returns_cached_tip_snapshot() {
    let harness = up().await;
    harness.tip.set(fresh_tip(Some(99), Some(102)));

    let response = harness
        .app
        .clone()
        .oneshot(get_authed("/v1/stats", &harness.bearer))
        .await
        .expect("serve request");
    assert_eq!(response.status(), StatusCode::OK);
    let body = read_json(response.into_body()).await;
    assert_eq!(body["indexer_tip"], 99);
    assert_eq!(body["node_tip"], 102);
    assert_eq!(body["lag_blocks"], 3);
    assert_eq!(body["is_stale"], false);
}

#[tokio::test(flavor = "multi_thread")]
async fn stats_reports_stale_snapshot_on_empty_tracker() {
    let harness = up().await;
    // No tip set — the tracker still holds its empty placeholder.

    let response = harness
        .app
        .clone()
        .oneshot(get_authed("/v1/stats", &harness.bearer))
        .await
        .expect("serve request");
    assert_eq!(response.status(), StatusCode::OK);
    let body = read_json(response.into_body()).await;
    assert!(body["indexer_tip"].is_null());
    assert!(body["node_tip"].is_null());
    assert_eq!(body["is_stale"], true);
}

#[tokio::test(flavor = "multi_thread")]
async fn tip_header_is_set_on_success_when_tip_is_known() {
    let harness = up().await;
    harness.tip.set(fresh_tip(Some(17), Some(20)));

    let response = harness
        .app
        .clone()
        .oneshot(get("/v1/health"))
        .await
        .expect("serve request");
    assert_eq!(response.status(), StatusCode::OK);
    let tip = response
        .headers()
        .get("x-indexer-tip")
        .expect("tip header present")
        .to_str()
        .expect("ascii");
    assert_eq!(tip, "17");
    assert!(response.headers().get("x-indexer-tip-stale").is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn tip_stale_header_appears_when_snapshot_is_empty() {
    let harness = up().await;

    let response = harness
        .app
        .clone()
        .oneshot(get("/v1/health"))
        .await
        .expect("serve request");
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.headers().get("x-indexer-tip").is_none());
    let stale = response
        .headers()
        .get("x-indexer-tip-stale")
        .expect("stale header present")
        .to_str()
        .expect("ascii");
    assert_eq!(stale, "true");
}

#[tokio::test(flavor = "multi_thread")]
async fn tip_header_absent_on_error_responses() {
    let harness = up().await;
    harness.tip.set(fresh_tip(Some(17), Some(20)));

    let response = harness
        .app
        .clone()
        .oneshot(get_authed("/v1/blocks/abc", &harness.bearer))
        .await
        .expect("serve request");
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(
        response.headers().get("x-indexer-tip").is_none(),
        "tip header must not leak onto error responses"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn cells_meta_reads_tip_from_tracker() {
    let harness = up().await;
    harness.tip.set(fresh_tip(Some(50), Some(52)));

    let uri = format!("/v1/cells?lock_hash={}", hex_prefixed(&LOCK_A));
    let body = read_json(
        harness
            .app
            .clone()
            .oneshot(get_authed(&uri, &harness.bearer))
            .await
            .expect("serve request")
            .into_body(),
    )
    .await;
    assert_eq!(body["meta"]["indexer_tip"], 50);
    assert_eq!(body["meta"]["node_tip"], 52);
}

// ---------------------------------------------------------------------------
// auth (Bearer)
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn protected_route_rejects_missing_authorization_header() {
    let harness = up().await;

    let response = harness
        .app
        .clone()
        .oneshot(get("/v1/blocks/latest"))
        .await
        .expect("serve request");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body = read_json(response.into_body()).await;
    assert_eq!(body["error"]["code"], "unauthorized");
    assert_eq!(body["error"]["message"], "unauthorized");
}

#[tokio::test(flavor = "multi_thread")]
async fn protected_route_rejects_non_bearer_scheme() {
    let harness = up().await;

    let req = Request::builder()
        .method("GET")
        .uri("/v1/blocks/latest")
        .header("authorization", "Basic dXNlcjpwYXNz")
        .body(Body::empty())
        .expect("request");
    let response = harness.app.clone().oneshot(req).await.expect("serve");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test(flavor = "multi_thread")]
async fn protected_route_rejects_unknown_prefix() {
    let harness = up().await;

    let response = harness
        .app
        .clone()
        .oneshot(get_authed(
            "/v1/blocks/latest",
            "cell_deadbeef_0000000000000000000000000000000a",
        ))
        .await
        .expect("serve");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test(flavor = "multi_thread")]
async fn protected_route_rejects_wrong_secret_for_known_prefix() {
    let harness = up().await;
    let issued = api_keys_helper::generate().expect("generate");
    api_keys::insert(
        &harness.pool,
        &issued.prefix,
        &issued.secret_hash,
        ApiKeyTier::Free,
        None,
    )
    .await
    .expect("insert");

    // Same prefix, but a fresh (different) secret tail.
    let other = api_keys_helper::generate().expect("generate other");
    let bearer = format!("{}_{}", issued.prefix, other.secret);

    let response = harness
        .app
        .clone()
        .oneshot(get_authed("/v1/blocks/latest", &bearer))
        .await
        .expect("serve");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test(flavor = "multi_thread")]
async fn protected_route_rejects_revoked_key_after_cache_expiry() {
    // The harness pre-issues a key but does not exercise the cache-bypass
    // path. Issue a separate key, revoke it, and present it before the
    // verification cache has had a chance to populate.
    let harness = up().await;
    let issued = api_keys_helper::generate().expect("generate");
    api_keys::insert(
        &harness.pool,
        &issued.prefix,
        &issued.secret_hash,
        ApiKeyTier::Free,
        None,
    )
    .await
    .expect("insert");
    cellora_db::api_keys::revoke(&harness.pool, &issued.prefix)
        .await
        .expect("revoke");

    let response = harness
        .app
        .clone()
        .oneshot(get_authed("/v1/blocks/latest", &issued.full))
        .await
        .expect("serve");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test(flavor = "multi_thread")]
async fn public_routes_remain_accessible_without_auth() {
    let harness = up().await;

    for path in ["/v1/health", "/v1/health/ready", "/v1/openapi.json"] {
        let response = harness.app.clone().oneshot(get(path)).await.expect("serve");
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "public path {path} returned {:?}",
            response.status()
        );
    }
}

// ---------------------------------------------------------------------------
// GraphQL
// ---------------------------------------------------------------------------

fn graphql_post(query: &str, bearer: Option<&str>) -> Request<Body> {
    let body = serde_json::json!({ "query": query }).to_string();
    let mut builder = Request::builder()
        .method("POST")
        .uri("/graphql")
        .header("content-type", "application/json");
    if let Some(b) = bearer {
        builder = builder.header("authorization", format!("Bearer {b}"));
    }
    builder
        .body(Body::from(body))
        .expect("build graphql request")
}

#[tokio::test(flavor = "multi_thread")]
async fn graphql_blocks_latest_query() {
    let harness = up().await;
    seed_block(&harness.pool, &make_block(1, 0xab)).await;

    let response = harness
        .app
        .clone()
        .oneshot(graphql_post(
            "{ blocksLatest { number hash } }",
            Some(&harness.bearer),
        ))
        .await
        .expect("serve");
    assert_eq!(response.status(), StatusCode::OK);
    let body = read_json(response.into_body()).await;
    assert_eq!(body["data"]["blocksLatest"]["number"], 1);
    let hash = body["data"]["blocksLatest"]["hash"]
        .as_str()
        .expect("hash string");
    assert!(hash.starts_with("0x"));
    assert_eq!(hash.len(), 66);
}

#[tokio::test(flavor = "multi_thread")]
async fn graphql_cells_query_paginates_consistently_with_rest() {
    let harness = up().await;
    seed_block(&harness.pool, &make_block(10, 0x10)).await;
    seed_cells(
        &harness.pool,
        &[
            make_cell(10, 0x11, 0, LOCK_A, None),
            make_cell(10, 0x22, 0, LOCK_A, None),
        ],
    )
    .await;

    let query = format!(
        r#"{{ cells(input: {{ lockHash: "{}", limit: 10 }}) {{ data {{ txHash blockNumber blockHash isLive }} nextCursor meta {{ indexerTip nodeTip }} }} }}"#,
        hex_prefixed(&LOCK_A)
    );
    let response = harness
        .app
        .clone()
        .oneshot(graphql_post(&query, Some(&harness.bearer)))
        .await
        .expect("serve");
    assert_eq!(response.status(), StatusCode::OK);
    let body = read_json(response.into_body()).await;
    let data = body["data"]["cells"]["data"].as_array().expect("array");
    assert_eq!(data.len(), 2);
    for cell in data {
        assert_eq!(cell["isLive"], true);
        assert_eq!(cell["blockNumber"], 10);
        assert_eq!(cell["blockHash"], hex_prefixed(&[0x10u8; 32]));
    }
    assert!(body["data"]["cells"]["nextCursor"].is_null());
}

#[tokio::test(flavor = "multi_thread")]
async fn graphql_requires_bearer_authentication() {
    let harness = up().await;

    let response = harness
        .app
        .clone()
        .oneshot(graphql_post("{ stats { isStale } }", None))
        .await
        .expect("serve");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test(flavor = "multi_thread")]
async fn graphql_validates_input_filters() {
    let harness = up().await;

    // Neither lockHash nor typeHash supplied — resolver returns a
    // GraphQL error envelope, not the REST envelope.
    let response = harness
        .app
        .clone()
        .oneshot(graphql_post(
            "{ cells(input: {}) { nextCursor } }",
            Some(&harness.bearer),
        ))
        .await
        .expect("serve");
    assert_eq!(response.status(), StatusCode::OK);
    let body = read_json(response.into_body()).await;
    assert!(
        body["errors"].is_array(),
        "expected GraphQL errors array, got {body}"
    );
}

// ---------------------------------------------------------------------------
// /metrics
// ---------------------------------------------------------------------------

async fn read_text(body: Body) -> String {
    let bytes = body.collect().await.expect("collect").to_bytes();
    String::from_utf8(bytes.to_vec()).expect("utf-8")
}

#[tokio::test(flavor = "multi_thread")]
async fn metrics_endpoint_is_public_and_exposes_text_format() {
    let harness = up().await;

    // Drive at least one request through the middleware so the
    // counters have a sample to serialise. Prometheus' text format
    // omits metric families that have never been observed.
    let _ = harness
        .app
        .clone()
        .oneshot(get("/v1/health"))
        .await
        .expect("warmup");

    let response = harness
        .app
        .clone()
        .oneshot(get("/metrics"))
        .await
        .expect("serve");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .expect("content-type")
            .to_str()
            .unwrap(),
        "text/plain; version=0.0.4"
    );
    let body = read_text(response.into_body()).await;
    assert!(body.contains("api_requests_total"));
    assert!(body.contains("api_request_duration_seconds"));
    assert!(body.contains("db_pool_connections_active"));
}

#[tokio::test(flavor = "multi_thread")]
async fn metrics_count_authenticated_requests() {
    let harness = up().await;
    seed_block(&harness.pool, &make_block(0, 0xAB)).await;

    // Make a successful request, then scrape /metrics and assert the
    // counter shows up with status="200" and the matched route.
    let _ = harness
        .app
        .clone()
        .oneshot(get_authed("/v1/blocks/latest", &harness.bearer))
        .await
        .expect("serve");

    let response = harness
        .app
        .clone()
        .oneshot(get("/metrics"))
        .await
        .expect("serve metrics");
    assert_eq!(response.status(), StatusCode::OK);
    let body = read_text(response.into_body()).await;
    assert!(
        body.contains(r#"api_requests_total{method="GET",path="/v1/blocks/latest",status="200"}"#),
        "metric line missing in:\n{body}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn metrics_record_rate_limit_decisions() {
    let harness = up_with(HarnessOpts::defaults().small_free_burst(2)).await;
    seed_block(&harness.pool, &make_block(0, 0xAB)).await;

    // Drain the bucket — first two pass, third gets 429.
    for _ in 0..3 {
        let _ = harness
            .app
            .clone()
            .oneshot(get_authed("/v1/blocks/latest", &harness.bearer))
            .await
            .expect("serve");
    }

    let response = harness
        .app
        .clone()
        .oneshot(get("/metrics"))
        .await
        .expect("serve metrics");
    let body = read_text(response.into_body()).await;
    // Prometheus serialises labels alphabetically (outcome, surface,
    // tier), regardless of the declaration order on the metric.
    assert!(
        body.contains(
            r#"api_rate_limit_decisions_total{outcome="allowed",surface="rest",tier="free"}"#
        ),
        "allowed line missing in:\n{body}"
    );
    assert!(
        body.contains(
            r#"api_rate_limit_decisions_total{outcome="limited",surface="rest",tier="free"}"#
        ),
        "limited line missing in:\n{body}"
    );
}

// ---------------------------------------------------------------------------
// rate limiting
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn rate_limit_blocks_burst_then_recovers_via_retry_after() {
    // Free-tier burst forced to 3 so the third request still passes and
    // the fourth gets 429 with a `Retry-After` set by the bucket.
    let harness = up_with(HarnessOpts::defaults().small_free_burst(3)).await;
    seed_block(&harness.pool, &make_block(0, 0xab)).await;

    for n in 0..3 {
        let response = harness
            .app
            .clone()
            .oneshot(get_authed("/v1/blocks/latest", &harness.bearer))
            .await
            .expect("serve");
        assert_eq!(response.status(), StatusCode::OK, "request {n} should pass");
        assert!(
            response.headers().get("x-ratelimit-limit").is_some(),
            "x-ratelimit-limit must accompany every authenticated 2xx"
        );
    }

    let response = harness
        .app
        .clone()
        .oneshot(get_authed("/v1/blocks/latest", &harness.bearer))
        .await
        .expect("serve");
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(
        response.headers().get("retry-after").is_some(),
        "429 must carry Retry-After"
    );
    let body = read_json(response.into_body()).await;
    assert_eq!(body["error"]["code"], "rate_limited");
}

#[tokio::test(flavor = "multi_thread")]
async fn rate_limit_buckets_are_per_key() {
    let harness = up_with(HarnessOpts::defaults().small_free_burst(2)).await;
    seed_block(&harness.pool, &make_block(0, 0xab)).await;

    // Issue a second key — its bucket should be independent of the
    // harness's pre-issued bearer.
    let issued = api_keys_helper::generate().expect("generate");
    api_keys::insert(
        &harness.pool,
        &issued.prefix,
        &issued.secret_hash,
        ApiKeyTier::Free,
        None,
    )
    .await
    .expect("insert");

    // Drain the first key's bucket.
    for _ in 0..2 {
        let response = harness
            .app
            .clone()
            .oneshot(get_authed("/v1/blocks/latest", &harness.bearer))
            .await
            .expect("serve");
        assert_eq!(response.status(), StatusCode::OK);
    }
    let drained = harness
        .app
        .clone()
        .oneshot(get_authed("/v1/blocks/latest", &harness.bearer))
        .await
        .expect("serve");
    assert_eq!(drained.status(), StatusCode::TOO_MANY_REQUESTS);

    // The second key's bucket is still full.
    let other = harness
        .app
        .clone()
        .oneshot(get_authed("/v1/blocks/latest", &issued.full))
        .await
        .expect("serve");
    assert_eq!(other.status(), StatusCode::OK);
}

#[tokio::test(flavor = "multi_thread")]
async fn rate_limit_fails_open_when_no_limiter_configured() {
    // With no Redis container, the harness leaves `rate_limiter` unset.
    // The middleware should pass requests through.
    let harness = up_with(HarnessOpts::defaults().no_redis()).await;
    seed_block(&harness.pool, &make_block(0, 0xab)).await;

    for _ in 0..40 {
        let response = harness
            .app
            .clone()
            .oneshot(get_authed("/v1/blocks/latest", &harness.bearer))
            .await
            .expect("serve");
        assert_eq!(response.status(), StatusCode::OK);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn rate_limit_emits_remaining_count_header() {
    let harness = up_with(HarnessOpts::defaults().small_free_burst(3)).await;
    seed_block(&harness.pool, &make_block(0, 0xab)).await;

    let response = harness
        .app
        .clone()
        .oneshot(get_authed("/v1/blocks/latest", &harness.bearer))
        .await
        .expect("serve");
    assert_eq!(response.status(), StatusCode::OK);
    let remaining = response
        .headers()
        .get("x-ratelimit-remaining")
        .expect("remaining header")
        .to_str()
        .expect("ascii");
    let remaining: u32 = remaining.parse().expect("integer");
    assert!(remaining < 3, "remaining count must reflect the decrement");
}

// ---------------------------------------------------------------------------
// OpenAPI spec + drift check
// ---------------------------------------------------------------------------

/// Locate `docs/openapi.json` from the crate's `CARGO_MANIFEST_DIR`.
fn committed_spec_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("docs")
        .join("openapi.json")
}

/// Drift check for the committed OpenAPI spec.
///
/// `cargo test -p cellora-api openapi_spec_is_current` fails when the
/// spec in the code has drifted from the committed file. To regenerate,
/// run `UPDATE_OPENAPI=1 cargo test -p cellora-api openapi_spec_is_current`.
#[test]
fn openapi_spec_is_current() {
    let live = format!("{}\n", cellora_api::openapi::spec_json());
    let path = committed_spec_path();

    if std::env::var("UPDATE_OPENAPI").is_ok() {
        std::fs::create_dir_all(path.parent().expect("parent dir")).expect("mkdir docs");
        std::fs::write(&path, &live).expect("write openapi.json");
        return;
    }

    let committed = std::fs::read_to_string(&path).unwrap_or_else(|err| {
        panic!(
            "docs/openapi.json missing ({err}); regenerate with UPDATE_OPENAPI=1 cargo test \
             -p cellora-api openapi_spec_is_current"
        );
    });
    assert_eq!(
        committed, live,
        "docs/openapi.json is out of date; regenerate with UPDATE_OPENAPI=1 cargo test \
         -p cellora-api openapi_spec_is_current"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn openapi_endpoint_serves_the_spec() {
    let harness = up().await;

    let response = harness
        .app
        .clone()
        .oneshot(get("/v1/openapi.json"))
        .await
        .expect("serve request");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get("content-type").unwrap(),
        "application/json"
    );
    let body = read_json(response.into_body()).await;
    assert_eq!(body["info"]["title"], "Cellora REST API");
    assert!(body["paths"]["/v1/health"].is_object());
    assert!(body["paths"]["/v1/cells"].is_object());
    assert!(body["paths"]["/v1/stats"].is_object());
}
