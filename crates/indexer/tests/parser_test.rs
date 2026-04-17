//! Parser unit tests driven by real `BlockView` fixtures captured from a
//! local CKB dev node. These tests do no I/O and are safe to run in CI
//! without any containers.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

use std::path::Path;

use cellora_indexer::parser::{parse_block, ParseError, ParsedBlock};
use ckb_jsonrpc_types::BlockView;

fn load(name: &str) -> BlockView {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name);
    let raw = std::fs::read_to_string(&path).unwrap_or_else(|err| {
        panic!("read fixture {}: {err}", path.display());
    });
    serde_json::from_str(&raw).unwrap_or_else(|err| {
        panic!("parse fixture {}: {err}", path.display());
    })
}

fn parse(name: &str) -> ParsedBlock {
    parse_block(&load(name)).expect("parse")
}

#[test]
fn genesis_block_has_expected_shape() {
    let parsed = parse("block_genesis.json");

    assert_eq!(parsed.block.number, 0);
    assert_eq!(parsed.block.hash.len(), 32);
    assert_eq!(parsed.block.parent_hash.len(), 32);
    assert!(parsed.block.parent_hash.iter().all(|b| *b == 0));
    assert_eq!(parsed.transactions.len(), 2);
    // Genesis ships the system cells — there must be more than one cell row.
    assert!(parsed.cells.len() >= 2);
    // Every output row must carry both a 32-byte lock hash and a lock code hash.
    for cell in &parsed.cells {
        assert_eq!(cell.tx_hash.len(), 32);
        assert_eq!(cell.lock_hash.len(), 32);
        assert_eq!(cell.lock_code_hash.len(), 32);
        if let Some(type_hash) = cell.type_hash.as_ref() {
            assert_eq!(type_hash.len(), 32);
            let type_code_hash = cell
                .type_code_hash
                .as_ref()
                .expect("type_code_hash present when type_hash is");
            assert_eq!(type_code_hash.len(), 32);
        }
    }
}

#[test]
fn cellbase_block_has_one_transaction_and_no_consumed_inputs() {
    let parsed = parse("block_12.json");
    assert_eq!(parsed.block.number, 12);
    assert_eq!(parsed.transactions.len(), 1);
    // Cellbase input references the null outpoint and must never be treated
    // as a consume operation.
    assert!(parsed.consumed.is_empty());
    // Cellbase produces exactly one output on the dev chain.
    assert_eq!(parsed.cells.len(), 1);
}

#[test]
fn parse_is_pure_and_deterministic() {
    let a = parse("block_genesis.json");
    let b = parse("block_genesis.json");
    assert_eq!(a.block.number, b.block.number);
    assert_eq!(a.block.hash, b.block.hash);
    assert_eq!(a.cells.len(), b.cells.len());
}

#[test]
fn invalid_hash_type_is_rejected_at_decode_boundary() {
    // Construct a deliberately invalid script hash_type by editing the JSON.
    // `BlockView` should refuse to deserialise it — we want such inputs to fail
    // loudly at the decode boundary, never leaving a half-parsed block for
    // the indexer to act on.
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("block_12.json");
    let mut value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(path).expect("read fixture"))
            .expect("valid json");
    value["transactions"][0]["outputs"][0]["lock"]["hash_type"] =
        serde_json::Value::String("not-a-real-hash-type".to_string());
    let err = serde_json::from_value::<BlockView>(value).err();
    assert!(err.is_some(), "BlockView must reject unknown hash_type");
    // Also assert the fallback path through the parser's own guard exists —
    // dead today (decoder catches it first) but a defence in depth we
    // intentionally want around.
    let _ = ParseError::UnknownHashType("synthetic".into());
}
