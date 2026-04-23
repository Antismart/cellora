//! Row structs that mirror the database schema one-to-one.

use bigdecimal::BigDecimal;
use chrono::{DateTime, Utc};

/// Raw encoding of a CKB script `hash_type` field.
///
/// Stored as a `SMALLINT` in Postgres and converted at the boundary. Using
/// a typed enum in Rust eliminates an entire class of invariant bugs while
/// leaving the on-disk representation cheap and indexable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i16)]
pub enum HashType {
    /// `hash_type = "data"`.
    Data = 0,
    /// `hash_type = "type"`.
    Type = 1,
    /// `hash_type = "data1"`.
    Data1 = 2,
    /// `hash_type = "data2"`.
    Data2 = 3,
}

impl HashType {
    /// Encode as the SMALLINT value used in Postgres.
    pub fn as_i16(self) -> i16 {
        self as i16
    }
}

/// Write-side shape of a `blocks` row. Ingestion code builds this and hands
/// it to [`crate::blocks::insert`]; `indexed_at` is populated by the column
/// default so it is deliberately absent here.
#[allow(missing_docs)]
#[derive(Debug, Clone, Default)]
pub struct BlockRow {
    pub number: i64,
    pub hash: Vec<u8>,
    pub parent_hash: Vec<u8>,
    pub timestamp_ms: i64,
    pub epoch: i64,
    pub transactions_count: i32,
    pub proposals_count: i32,
    pub uncles_count: i32,
    pub nonce: BigDecimal,
    pub dao: Vec<u8>,
}

/// Read-side shape of a `blocks` row, including the server-populated
/// `indexed_at` timestamp. Returned by [`crate::blocks::latest`] and
/// [`crate::blocks::get_by_number`].
#[allow(missing_docs)]
#[derive(Debug, Clone)]
pub struct Block {
    pub number: i64,
    pub hash: Vec<u8>,
    pub parent_hash: Vec<u8>,
    pub timestamp_ms: i64,
    pub epoch: i64,
    pub transactions_count: i32,
    pub proposals_count: i32,
    pub uncles_count: i32,
    pub nonce: BigDecimal,
    pub dao: Vec<u8>,
    pub indexed_at: DateTime<Utc>,
}

/// One row in the `transactions` table.
#[allow(missing_docs)]
#[derive(Debug, Clone)]
pub struct TransactionRow {
    pub hash: Vec<u8>,
    pub block_number: i64,
    pub tx_index: i32,
    pub version: i32,
    pub cell_deps: serde_json::Value,
    pub header_deps: serde_json::Value,
    pub witnesses: serde_json::Value,
    pub inputs_count: i32,
    pub outputs_count: i32,
}

/// Write-side shape of a `cells` row. Ingestion code builds this and hands
/// it to [`crate::cells::insert_batch`]; consumed / indexed metadata is
/// populated separately.
#[allow(missing_docs)]
#[derive(Debug, Clone)]
pub struct CellRow {
    pub tx_hash: Vec<u8>,
    pub output_index: i32,
    pub block_number: i64,
    pub capacity_shannons: i64,
    pub lock_code_hash: Vec<u8>,
    pub lock_hash_type: HashType,
    pub lock_args: Vec<u8>,
    pub lock_hash: Vec<u8>,
    pub type_code_hash: Option<Vec<u8>>,
    pub type_hash_type: Option<HashType>,
    pub type_args: Option<Vec<u8>>,
    pub type_hash: Option<Vec<u8>>,
    pub data: Vec<u8>,
}

/// Read-side shape of a `cells` row, joined with the hash of the block in
/// which the cell was created. `lock_hash_type` / `type_hash_type` are kept
/// as raw `i16`; callers map them to [`HashType`] at the API boundary.
#[allow(missing_docs)]
#[derive(Debug, Clone)]
pub struct Cell {
    pub tx_hash: Vec<u8>,
    pub output_index: i32,
    pub block_number: i64,
    pub block_hash: Vec<u8>,
    pub capacity_shannons: i64,
    pub lock_code_hash: Vec<u8>,
    pub lock_hash_type: i16,
    pub lock_args: Vec<u8>,
    pub lock_hash: Vec<u8>,
    pub type_code_hash: Option<Vec<u8>>,
    pub type_hash_type: Option<i16>,
    pub type_args: Option<Vec<u8>>,
    pub type_hash: Option<Vec<u8>>,
    pub data: Vec<u8>,
    pub consumed_by_tx_hash: Option<Vec<u8>>,
    pub consumed_by_input_index: Option<i32>,
    pub consumed_at_block_number: Option<i64>,
}

/// A pointer from a transaction input to a cell it consumes.
#[allow(missing_docs)]
#[derive(Debug, Clone)]
pub struct ConsumedCellRef {
    pub tx_hash: Vec<u8>,
    pub output_index: i32,
    pub consumed_by_tx_hash: Vec<u8>,
    pub consumed_by_input_index: i32,
    pub consumed_at_block_number: i64,
}

/// Current value of the singleton `indexer_state` row.
#[allow(missing_docs)]
#[derive(Debug, Clone)]
pub struct Checkpoint {
    pub last_indexed_block: i64,
    pub last_indexed_hash: Vec<u8>,
}
