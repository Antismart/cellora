//! Full-stack end-to-end test for the indexer.
//!
//! Spins up a disposable Postgres via testcontainers and a `wiremock` server
//! standing in for the CKB node. Serves a canned chain of one block, runs
//! the poller until that block has been ingested, cancels, and verifies the
//! database state.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::time::{Duration, Instant};

use cellora_common::{
    ckb::CkbClient,
    config::{Config, LogFormat},
};
use cellora_db::{connect, migrate};
use cellora_indexer::poller::Poller;
use testcontainers_modules::{
    postgres::Postgres,
    testcontainers::{runners::AsyncRunner, ContainerAsync, ImageExt},
};
use tokio_util::sync::CancellationToken;
use wiremock::{
    matchers::{method, path},
    Mock, MockServer, ResponseTemplate,
};

const FIXTURE_GENESIS: &str = include_str!("fixtures/block_genesis.json");

async fn spin_postgres() -> (sqlx::PgPool, ContainerAsync<Postgres>) {
    let pg = Postgres::default()
        .with_tag("16-alpine")
        .start()
        .await
        .expect("start postgres");
    let host = pg.get_host().await.expect("host");
    let port = pg.get_host_port_ipv4(5432).await.expect("port");
    let url = format!("postgres://postgres:postgres@{host}:{port}/postgres");

    let mut pool = None;
    for _ in 0..10 {
        match connect(&url).await {
            Ok(p) => {
                pool = Some(p);
                break;
            }
            Err(_) => tokio::time::sleep(Duration::from_millis(300)).await,
        }
    }
    let pool = pool.expect("postgres eventually accepts connections");
    migrate::run(&pool).await.expect("migrate");
    (pool, pg)
}

fn rpc_response<T: serde::Serialize>(result: T) -> serde_json::Value {
    serde_json::json!({ "jsonrpc": "2.0", "id": 1, "result": result })
}

#[tokio::test]
async fn poller_indexes_a_single_block_and_stops_on_cancel() {
    let (pool, _pg) = spin_postgres().await;
    let mock = MockServer::start().await;

    // Genesis block from the fixture — shipped as the response for block 0.
    let genesis: serde_json::Value =
        serde_json::from_str(FIXTURE_GENESIS).expect("fixture is valid json");

    // `get_block_by_number("0x0")` — return the fixture block.
    Mock::given(method("POST"))
        .and(path("/"))
        .and(wiremock::matchers::body_partial_json(serde_json::json!({
            "method": "get_block_by_number",
            "params": ["0x0"],
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(rpc_response(genesis)))
        .mount(&mock)
        .await;

    // Any other block number — return null so the poller idles.
    Mock::given(method("POST"))
        .and(path("/"))
        .and(wiremock::matchers::body_partial_json(serde_json::json!({
            "method": "get_block_by_number",
        })))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(rpc_response(serde_json::Value::Null)),
        )
        .mount(&mock)
        .await;

    let ckb = CkbClient::new(mock.uri()).expect("client");
    let config = Config {
        database_url: "unused".into(),
        ckb_rpc_url: mock.uri(),
        poll_interval_ms: 50,
        indexer_start_block: 0,
        indexer_reorg_target_depth: 12,
        indexer_reorg_max_depth: 100,
        log_level: "warn".into(),
        log_format: LogFormat::Pretty,
        api_bind_addr: "0.0.0.0:0".into(),
        api_default_page_size: 50,
        api_max_page_size: 500,
        api_request_timeout_ms: 10_000,
        api_tip_cache_refresh_ms: 1_000,
        api_auth_cache_ttl_seconds: 60,
        api_auth_cache_capacity: 10_000,
        redis_url: "redis://localhost:6379".into(),
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
    };
    let cancel = CancellationToken::new();
    let poller = Poller::new(pool.clone(), ckb, config);

    let cancel_for_task = cancel.clone();
    let handle = tokio::spawn(async move { poller.run(cancel_for_task).await });

    // Wait for block 0 to land, then cancel.
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let row = sqlx::query!("SELECT MAX(number) as max FROM blocks")
            .fetch_one(&pool)
            .await
            .expect("query");
        if row.max == Some(0) {
            break;
        }
        if Instant::now() > deadline {
            cancel.cancel();
            let _ = handle.await;
            panic!("block 0 was never indexed");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    cancel.cancel();
    let result = handle.await.expect("task join");
    assert!(result.is_ok(), "poller should exit cleanly: {result:?}");

    // Genesis has 2 txs and the dev-chain system cells (>= 2). Full parity
    // with the parser unit tests.
    let stats = sqlx::query!(
        "SELECT \
            (SELECT count(*) FROM blocks)       AS blocks, \
            (SELECT count(*) FROM transactions) AS txs, \
            (SELECT count(*) FROM cells)        AS cells, \
            (SELECT last_indexed_block FROM indexer_state) AS checkpoint"
    )
    .fetch_one(&pool)
    .await
    .expect("stats");

    assert_eq!(stats.blocks, Some(1));
    assert_eq!(stats.txs, Some(2));
    assert!(stats.cells.unwrap_or(0) >= 2);
    assert_eq!(stats.checkpoint, Some(0));
}
