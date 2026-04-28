//! Integration tests for the reorg rollback transaction.
//!
//! These tests exercise [`cellora_indexer::reorg::rollback_to`]
//! against a real Postgres but bypass the wiremock CKB fixture: they
//! seed the database directly with a small chain, then call the
//! rollback function and assert the resulting state. The full
//! end-to-end "wiremock simulates a chain reorg" test is out of scope
//! here because it would require constructing CKB block JSON by hand;
//! the rollback transaction is the load-bearing primitive and the most
//! valuable thing to cover thoroughly.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::time::Duration;

use bigdecimal::BigDecimal;
use cellora_db::models::{
    BlockRow, CellRow, ConsumedCellRef, HashType, ReorgStatus, TransactionRow,
};
use cellora_db::{blocks, cells, checkpoint, connect, migrate, reorg_log, transactions};
use cellora_indexer::reorg::{rollback_to, Ancestor};
use testcontainers_modules::{
    postgres::Postgres,
    testcontainers::{runners::AsyncRunner, ContainerAsync, ImageExt},
};

struct Harness {
    pool: sqlx::PgPool,
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
    Harness { pool, _pg: pg }
}

async fn connect_with_retry(url: &str, attempts: u8) -> sqlx::PgPool {
    for attempt in 1..=attempts {
        match connect(url).await {
            Ok(p) => return p,
            Err(err) if attempt == attempts => panic!("connect: {err}"),
            Err(_) => tokio::time::sleep(Duration::from_millis(300)).await,
        }
    }
    unreachable!()
}

fn block(number: i64, seed: u8) -> BlockRow {
    BlockRow {
        number,
        hash: vec![seed; 32],
        parent_hash: vec![seed.wrapping_sub(1); 32],
        timestamp_ms: 1_700_000_000_000 + number * 1_000,
        epoch: number,
        transactions_count: 1,
        proposals_count: 0,
        uncles_count: 0,
        nonce: BigDecimal::from(number as u64),
        dao: vec![0; 32],
    }
}

fn tx(seed: u8, block_number: i64) -> TransactionRow {
    TransactionRow {
        hash: vec![seed; 32],
        block_number,
        tx_index: 0,
        version: 0,
        cell_deps: serde_json::json!([]),
        header_deps: serde_json::json!([]),
        witnesses: serde_json::json!([]),
        inputs_count: 0,
        outputs_count: 1,
    }
}

fn cell(tx_seed: u8, block_number: i64, lock_seed: u8) -> CellRow {
    CellRow {
        tx_hash: vec![tx_seed; 32],
        output_index: 0,
        block_number,
        capacity_shannons: 100_000_000,
        lock_code_hash: vec![lock_seed; 32],
        lock_hash_type: HashType::Type,
        lock_args: vec![],
        lock_hash: vec![lock_seed.wrapping_add(1); 32],
        type_code_hash: None,
        type_hash_type: None,
        type_args: None,
        type_hash: None,
        data: vec![],
    }
}

#[tokio::test]
async fn rollback_truncates_blocks_restores_cells_and_writes_log() {
    let h = up().await;
    // Build a 3-block chain: 0, 1, 2. A cell created in block 0 is
    // consumed by a tx in block 2. Rolling back to ancestor 0 should:
    //   * delete blocks 1 and 2 (and their txs / cells)
    //   * restore the cell created in block 0 (its consumed_* fields
    //     come back to NULL)
    //   * advance the checkpoint to 0
    //   * write one reorg_log row in `completed`
    let mut db_tx = h.pool.begin().await.expect("begin");
    blocks::insert(&mut *db_tx, &block(0, 0x10))
        .await
        .expect("b0");
    blocks::insert(&mut *db_tx, &block(1, 0x11))
        .await
        .expect("b1");
    blocks::insert(&mut *db_tx, &block(2, 0x12))
        .await
        .expect("b2");
    transactions::insert_batch(&mut db_tx, &[tx(0xA0, 0), tx(0xA1, 1), tx(0xA2, 2)])
        .await
        .expect("txs");
    let cell_in_0 = cell(0xA0, 0, 0xC0);
    let cell_in_1 = cell(0xA1, 1, 0xC1);
    cells::insert_batch(&mut db_tx, &[cell_in_0.clone(), cell_in_1.clone()])
        .await
        .expect("cells");
    // The cell created in block 0 is consumed by the tx in block 2.
    cells::mark_consumed(
        &mut db_tx,
        &[ConsumedCellRef {
            tx_hash: cell_in_0.tx_hash.clone(),
            output_index: 0,
            consumed_by_tx_hash: vec![0xA2; 32],
            consumed_by_input_index: 0,
            consumed_at_block_number: 2,
        }],
    )
    .await
    .expect("consume");
    checkpoint::upsert(&mut db_tx, 2, &[0x12; 32])
        .await
        .expect("checkpoint");
    db_tx.commit().await.expect("commit");

    // Roll back to ancestor 0.
    let ancestor = Ancestor {
        block_number: 0,
        node_hash: vec![0x10; 32],
    };
    let outcome = rollback_to(&h.pool, &ancestor, 2, &[0x11; 32])
        .await
        .expect("rollback");

    assert_eq!(outcome.depth, 2);
    assert_eq!(outcome.deleted_blocks, 2);
    assert_eq!(
        outcome.restored_cells, 1,
        "the spent-in-2 cell came back to live"
    );
    assert_eq!(outcome.ancestor_height, 0);

    // Database state now matches a tip at block 0.
    let block_count = sqlx::query!("SELECT count(*) AS n FROM blocks")
        .fetch_one(&h.pool)
        .await
        .expect("count")
        .n
        .unwrap_or(0);
    assert_eq!(block_count, 1, "only block 0 remains");

    let cell_count = sqlx::query!("SELECT count(*) AS n FROM cells WHERE block_number = 0")
        .fetch_one(&h.pool)
        .await
        .expect("count")
        .n
        .unwrap_or(0);
    assert_eq!(cell_count, 1, "block 0's cell is still there");

    // Cell created in block 0 is no longer consumed.
    let cell_state = sqlx::query!(
        "SELECT consumed_by_tx_hash FROM cells WHERE tx_hash = $1 AND output_index = 0",
        &cell_in_0.tx_hash,
    )
    .fetch_one(&h.pool)
    .await
    .expect("fetch");
    assert!(
        cell_state.consumed_by_tx_hash.is_none(),
        "cell back to live"
    );

    // Checkpoint now points at the ancestor.
    let cp = checkpoint::read(&h.pool).await.expect("read").expect("row");
    assert_eq!(cp.last_indexed_block, 0);
    assert_eq!(cp.last_indexed_hash, vec![0x10; 32]);

    // reorg_log has the audit row.
    let log = reorg_log::list_recent(&h.pool, 10).await.expect("log");
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].id, outcome.log_id);
    assert_eq!(log[0].depth, 2);
    assert_eq!(log[0].divergence_block_number, 1);
    assert_eq!(log[0].divergence_node_hash, vec![0x10; 32]);
    assert_eq!(log[0].divergence_indexed_hash, vec![0x11; 32]);
    assert_eq!(log[0].status, ReorgStatus::Completed);
    assert!(log[0].completed_at.is_some());
}

#[tokio::test]
async fn rollback_to_same_height_is_a_no_op_for_blocks_but_writes_a_log() {
    // Edge case: ancestor is the current tip (depth = 0). The DB stays
    // unchanged but we still want a row in the log so an operator can
    // see "we evaluated a reorg here and decided there was nothing to
    // do" — useful for catching false-positive divergence detections.
    let h = up().await;
    let mut db_tx = h.pool.begin().await.expect("begin");
    blocks::insert(&mut *db_tx, &block(0, 0x10))
        .await
        .expect("b0");
    db_tx.commit().await.expect("commit");

    let ancestor = Ancestor {
        block_number: 0,
        node_hash: vec![0x10; 32],
    };
    let outcome = rollback_to(&h.pool, &ancestor, 0, &[0x10; 32])
        .await
        .expect("rollback");

    assert_eq!(outcome.depth, 0);
    assert_eq!(outcome.deleted_blocks, 0);
    assert_eq!(outcome.restored_cells, 0);

    let block_count = sqlx::query!("SELECT count(*) AS n FROM blocks")
        .fetch_one(&h.pool)
        .await
        .expect("count")
        .n
        .unwrap_or(0);
    assert_eq!(block_count, 1);

    let log = reorg_log::list_recent(&h.pool, 10).await.expect("log");
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].depth, 0);
    assert_eq!(log[0].status, ReorgStatus::Completed);
}
