//! Integration tests for the `cellora-db` crate.
//!
//! Each test spins up a throwaway Postgres container via testcontainers,
//! applies the migrations, and exercises the repository modules. Tests are
//! independent — they do not share state — so they can run in parallel.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::time::Duration;

use bigdecimal::BigDecimal;
use cellora_db::models::{
    BlockRow, CellRow, Checkpoint, ConsumedCellRef, HashType, TransactionRow,
};
use cellora_db::{blocks, cells, checkpoint, connect, migrate, transactions};
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
