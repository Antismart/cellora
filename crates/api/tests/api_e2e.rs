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
use bigdecimal::BigDecimal;
use cellora_api::{build_app, AppState};
use cellora_common::config::{Config, LogFormat};
use cellora_db::models::BlockRow;
use cellora_db::{blocks, connect, migrate};
use http_body_util::BodyExt;
use serde_json::Value;
use sqlx::PgPool;
use testcontainers_modules::{
    postgres::Postgres,
    testcontainers::{runners::AsyncRunner, ContainerAsync, ImageExt},
};
use tower::ServiceExt;

const REQUEST_ID: HeaderName = HeaderName::from_static("x-request-id");

struct Harness {
    app: axum::Router,
    pool: PgPool,
    // Keep the container alive for the lifetime of the test.
    _pg: ContainerAsync<Postgres>,
}

async fn up() -> Harness {
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

    let config = test_config(&url);
    let state = AppState::new(pool.clone(), config);
    let app = build_app(state);
    Harness { app, pool, _pg: pg }
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
        log_level: "info".to_owned(),
        log_format: LogFormat::Pretty,
        api_bind_addr: "0.0.0.0:0".to_owned(),
        api_default_page_size: 50,
        api_max_page_size: 500,
        api_request_timeout_ms: 10_000,
        api_tip_cache_refresh_ms: 1_000,
    }
}

fn get(path: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(path)
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
async fn unknown_route_returns_404() {
    let harness = up().await;

    let response = harness
        .app
        .clone()
        .oneshot(get("/v1/does-not-exist"))
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
        .oneshot(get("/v1/blocks/latest"))
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
        .oneshot(get("/v1/blocks/latest"))
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
        .oneshot(get("/v1/blocks/42"))
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
        .oneshot(get("/v1/blocks/999999"))
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
        .oneshot(get("/v1/blocks/abc"))
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
        .oneshot(get("/v1/blocks/-1"))
        .await
        .expect("serve request");

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = read_json(response.into_body()).await;
    assert_eq!(body["error"]["code"], "bad_request");
}
