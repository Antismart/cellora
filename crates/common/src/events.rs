use serde::{Deserialize, Serialize};

pub const BLOCKS_CHANNEL: &str = "cellora:blocks";
pub const CELLS_CHANNEL: &str = "cellora:cells";

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct BlockMinedEvent {
    pub number: i64,
    pub hash: String,
    pub parent_hash: String,
    pub timestamp: u64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CellCreatedEvent {
    pub tx_hash: String,
    pub output_index: i32,
    pub block_number: i64,
    pub lock_code_hash: String,
    pub lock_hash_type: i16,
    pub lock_args: String,
    pub lock_hash: String,
    pub type_code_hash: Option<String>,
    pub type_hash_type: Option<i16>,
    pub type_args: Option<String>,
    pub type_hash: Option<String>,
    pub capacity_shannons: i64,
}
