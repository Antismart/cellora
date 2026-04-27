//! Integration tests for the `cellora-db` crate.
//!
//! Each test spins up a throwaway Postgres container via testcontainers,
//! applies the migrations, and exercises the repository modules. Tests are
//! independent — they do not share state — so they can run in parallel.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::time::Duration;

use bigdecimal::BigDecimal;
use cellora_db::models::{
    ApiKeyTier, BlockRow, CellRow, Checkpoint, ConsumedCellRef, HashType, TransactionRow,
};
use cellora_db::{api_keys, blocks, cells, checkpoint, connect, migrate, transactions};
use testcontainers_modules::{
    postgres::Postgres,
    testcontainers::{runners::AsyncRunner, ContainerAsync, ImageExt},
};

struct Harness {
    pool: sqlx::PgPool,
    // Keep the container alive for the duration of the test.
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
    // testcontainers-modules' Postgres image exposes `postgres` / `postgres`
    // credentials with a `postgres` database by default.
    let url = format!("postgres://postgres:postgres@{host}:{port}/postgres");

    let pool = connect_with_retry(&url, 10).await;
    migrate::run(&pool).await.expect("migrate");
    Harness { pool, _pg: pg }
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

fn block_row(number: i64, hash_seed: u8) -> BlockRow {
    BlockRow {
        number,
        hash: vec![hash_seed; 32],
        parent_hash: vec![hash_seed.wrapping_sub(1); 32],
        timestamp_ms: 1_700_000_000_000 + number * 1_000,
        epoch: 0,
        transactions_count: 1,
        proposals_count: 0,
        uncles_count: 0,
        nonce: BigDecimal::from(number as u64),
        dao: vec![0; 32],
    }
}

fn tx_row(tx_hash: u8, block_number: i64) -> TransactionRow {
    TransactionRow {
        hash: vec![tx_hash; 32],
        block_number,
        tx_index: 0,
        version: 0,
        cell_deps: serde_json::json!([]),
        header_deps: serde_json::json!([]),
        witnesses: serde_json::json!(["0x"]),
        inputs_count: 0,
        outputs_count: 1,
    }
}

fn cell_row(tx_hash: u8, block_number: i64, index: i32, lock_seed: u8) -> CellRow {
    CellRow {
        tx_hash: vec![tx_hash; 32],
        output_index: index,
        block_number,
        capacity_shannons: 100_000_000_000,
        lock_code_hash: vec![lock_seed; 32],
        lock_hash_type: HashType::Type,
        lock_args: vec![0xaa, 0xbb],
        lock_hash: vec![lock_seed.wrapping_add(1); 32],
        type_code_hash: None,
        type_hash_type: None,
        type_args: None,
        type_hash: None,
        data: vec![],
    }
}

#[tokio::test]
async fn inserts_and_reads_a_block() {
    let h = up().await;

    let block = block_row(0, 0x42);
    let mut tx = h.pool.begin().await.expect("begin");
    blocks::insert(&mut *tx, &block).await.expect("insert");
    tx.commit().await.expect("commit");

    let latest = blocks::latest_number(&h.pool).await.expect("latest");
    assert_eq!(latest, Some(0));
}

#[tokio::test]
async fn inserts_transactions_and_cells_and_marks_consumed() {
    let h = up().await;

    let block = block_row(0, 0x11);
    let tx_creator = tx_row(0x22, 0);
    let tx_spender = tx_row(0x33, 0);
    let created = cell_row(0x22, 0, 0, 0x44);
    let consumed_ref = ConsumedCellRef {
        tx_hash: created.tx_hash.clone(),
        output_index: 0,
        consumed_by_tx_hash: tx_spender.hash.clone(),
        consumed_by_input_index: 0,
        consumed_at_block_number: 0,
    };

    let mut db_tx = h.pool.begin().await.expect("begin");
    blocks::insert(&mut *db_tx, &block).await.expect("block");
    transactions::insert_batch(&mut db_tx, &[tx_creator.clone(), tx_spender.clone()])
        .await
        .expect("txs");
    cells::insert_batch(&mut db_tx, std::slice::from_ref(&created))
        .await
        .expect("cells");
    cells::mark_consumed(&mut db_tx, std::slice::from_ref(&consumed_ref))
        .await
        .expect("consume");
    db_tx.commit().await.expect("commit");

    let row = sqlx::query!(
        "SELECT consumed_by_tx_hash, consumed_by_input_index, consumed_at_block_number \
         FROM cells WHERE tx_hash = $1 AND output_index = 0",
        &created.tx_hash
    )
    .fetch_one(&h.pool)
    .await
    .expect("fetch");

    assert_eq!(row.consumed_by_tx_hash.unwrap(), tx_spender.hash);
    assert_eq!(row.consumed_by_input_index, Some(0));
    assert_eq!(row.consumed_at_block_number, Some(0));
}

#[tokio::test]
async fn checkpoint_upsert_is_idempotent() {
    let h = up().await;

    assert!(checkpoint::read(&h.pool).await.expect("read").is_none());

    let mut tx = h.pool.begin().await.expect("begin");
    checkpoint::upsert(&mut tx, 10, &[0x01; 32])
        .await
        .expect("first");
    checkpoint::upsert(&mut tx, 11, &[0x02; 32])
        .await
        .expect("second");
    tx.commit().await.expect("commit");

    let Checkpoint {
        last_indexed_block,
        last_indexed_hash,
    } = checkpoint::read(&h.pool)
        .await
        .expect("read")
        .expect("row present");
    assert_eq!(last_indexed_block, 11);
    assert_eq!(last_indexed_hash, vec![0x02; 32]);
}

// ---------------------------------------------------------------------------
// api_keys
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn api_key_insert_and_lookup() {
    let h = up().await;

    let inserted = api_keys::insert(
        &h.pool,
        "cell_aaaaaaaa",
        "$argon2id$v=19$m=19456,t=2,p=1$placeholder",
        ApiKeyTier::Free,
        Some("integration-test"),
    )
    .await
    .expect("insert");
    assert_eq!(inserted.tier, ApiKeyTier::Free);
    assert_eq!(inserted.label.as_deref(), Some("integration-test"));
    assert!(inserted.revoked_at.is_none());

    let found = api_keys::find_active_by_prefix(&h.pool, "cell_aaaaaaaa")
        .await
        .expect("lookup")
        .expect("row");
    assert_eq!(found.prefix, "cell_aaaaaaaa");
    assert_eq!(found.tier, ApiKeyTier::Free);
}

#[tokio::test(flavor = "multi_thread")]
async fn api_key_lookup_misses_when_unknown_prefix() {
    let h = up().await;

    let found = api_keys::find_active_by_prefix(&h.pool, "cell_doesnotexist")
        .await
        .expect("lookup");
    assert!(found.is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn api_key_revocation_hides_from_active_lookup() {
    let h = up().await;

    api_keys::insert(
        &h.pool,
        "cell_bbbbbbbb",
        "$argon2id$v=19$m=19456,t=2,p=1$placeholder",
        ApiKeyTier::Pro,
        None,
    )
    .await
    .expect("insert");

    let revoked = api_keys::revoke(&h.pool, "cell_bbbbbbbb")
        .await
        .expect("revoke");
    assert!(revoked, "first revocation should report a row updated");

    let found = api_keys::find_active_by_prefix(&h.pool, "cell_bbbbbbbb")
        .await
        .expect("lookup");
    assert!(
        found.is_none(),
        "revoked key must not appear in active lookup"
    );

    let revoked_again = api_keys::revoke(&h.pool, "cell_bbbbbbbb")
        .await
        .expect("revoke");
    assert!(
        !revoked_again,
        "second revocation should report no rows updated"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn api_key_list_orders_newest_first() {
    let h = up().await;

    api_keys::insert(
        &h.pool,
        "cell_11111111",
        "$argon2id$v=19$m=19456,t=2,p=1$placeholder",
        ApiKeyTier::Free,
        Some("first"),
    )
    .await
    .expect("insert first");
    // Tiny sleep so the timestamps differ; otherwise ORDER BY created_at
    // is non-deterministic for rows in the same millisecond.
    tokio::time::sleep(Duration::from_millis(10)).await;
    api_keys::insert(
        &h.pool,
        "cell_22222222",
        "$argon2id$v=19$m=19456,t=2,p=1$placeholder",
        ApiKeyTier::Starter,
        Some("second"),
    )
    .await
    .expect("insert second");

    let listed = api_keys::list_all(&h.pool).await.expect("list");
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].prefix, "cell_22222222");
    assert_eq!(listed[1].prefix, "cell_11111111");
}

#[tokio::test(flavor = "multi_thread")]
async fn api_key_touch_last_used_updates_timestamp() {
    let h = up().await;

    api_keys::insert(
        &h.pool,
        "cell_cccccccc",
        "$argon2id$v=19$m=19456,t=2,p=1$placeholder",
        ApiKeyTier::Free,
        None,
    )
    .await
    .expect("insert");

    assert!(api_keys::find_active_by_prefix(&h.pool, "cell_cccccccc")
        .await
        .expect("lookup")
        .expect("row")
        .last_used_at
        .is_none());

    api_keys::touch_last_used(&h.pool, "cell_cccccccc")
        .await
        .expect("touch");

    let after = api_keys::find_active_by_prefix(&h.pool, "cell_cccccccc")
        .await
        .expect("lookup")
        .expect("row");
    assert!(after.last_used_at.is_some());
}
