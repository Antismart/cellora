//! Pure conversion from CKB JSON-RPC `BlockView` into database row structs.
//!
//! The parser does no I/O; it is trivially unit-testable from fixture JSON.

use std::str::FromStr;

use bigdecimal::BigDecimal;
use cellora_db::models::{BlockRow, CellRow, ConsumedCellRef, HashType, TransactionRow};
use ckb_jsonrpc_types::BlockView;
use ckb_types::{packed::Script as PackedScript, prelude::*, H256};
use thiserror::Error;

/// Errors that can occur while converting a `BlockView` to DB rows.
#[allow(missing_docs)]
#[derive(Debug, Error)]
pub enum ParseError {
    #[error("capacity {0} exceeds i64 max")]
    CapacityOverflow(u64),
    #[error("nonce parse error")]
    NonceParse,
    #[error("hash_type {0:?} is not representable")]
    UnknownHashType(String),
    #[error("failed to serialize field {field}: {source}")]
    Serialize {
        field: &'static str,
        #[source]
        source: serde_json::Error,
    },
}

/// Rows extracted from a single block, ready to write in one DB transaction.
#[allow(missing_docs)]
#[derive(Debug, Default)]
pub struct ParsedBlock {
    pub block: BlockRow,
    pub transactions: Vec<TransactionRow>,
    pub cells: Vec<CellRow>,
    pub consumed: Vec<ConsumedCellRef>,
}

/// Parse a CKB `BlockView` into the row set that will be written to the
/// database.
pub fn parse_block(view: &BlockView) -> Result<ParsedBlock, ParseError> {
    let header = &view.header;
    let number: u64 = header.inner.number.into();
    let block_number = i64::try_from(number).map_err(|_| ParseError::CapacityOverflow(number))?;

    let timestamp_raw: u64 = header.inner.timestamp.into();
    let epoch_raw: u64 = header.inner.epoch.into();
    let nonce_raw: u128 = header.inner.nonce.into();
    let nonce = BigDecimal::from_str(&nonce_raw.to_string()).map_err(|_| ParseError::NonceParse)?;

    let block = BlockRow {
        number: block_number,
        hash: header.hash.0.to_vec(),
        parent_hash: header.inner.parent_hash.0.to_vec(),
        timestamp_ms: i64::try_from(timestamp_raw)
            .map_err(|_| ParseError::CapacityOverflow(timestamp_raw))?,
        epoch: i64::try_from(epoch_raw).map_err(|_| ParseError::CapacityOverflow(epoch_raw))?,
        transactions_count: i32::try_from(view.transactions.len()).unwrap_or(i32::MAX),
        proposals_count: i32::try_from(view.proposals.len()).unwrap_or(i32::MAX),
        uncles_count: i32::try_from(view.uncles.len()).unwrap_or(i32::MAX),
        nonce,
        dao: header.inner.dao.0.to_vec(),
    };

    let mut transactions = Vec::with_capacity(view.transactions.len());
    let mut cells = Vec::new();
    let mut consumed = Vec::new();

    for (tx_index, tx) in view.transactions.iter().enumerate() {
        let tx_hash = tx.hash.0.to_vec();
        let version: u32 = tx.inner.version.into();

        let cell_deps =
            serde_json::to_value(&tx.inner.cell_deps).map_err(|source| ParseError::Serialize {
                field: "cell_deps",
                source,
            })?;
        let header_deps = serde_json::to_value(&tx.inner.header_deps).map_err(|source| {
            ParseError::Serialize {
                field: "header_deps",
                source,
            }
        })?;
        let witnesses =
            serde_json::to_value(&tx.inner.witnesses).map_err(|source| ParseError::Serialize {
                field: "witnesses",
                source,
            })?;

        transactions.push(TransactionRow {
            hash: tx_hash.clone(),
            block_number,
            tx_index: i32::try_from(tx_index).unwrap_or(i32::MAX),
            version: i32::try_from(version).unwrap_or(i32::MAX),
            cell_deps,
            header_deps,
            witnesses,
            inputs_count: i32::try_from(tx.inner.inputs.len()).unwrap_or(i32::MAX),
            outputs_count: i32::try_from(tx.inner.outputs.len()).unwrap_or(i32::MAX),
        });

        // Outputs -> new cells
        for (output_index, (output, data)) in tx
            .inner
            .outputs
            .iter()
            .zip(tx.inner.outputs_data.iter())
            .enumerate()
        {
            let capacity: u64 = output.capacity.into();
            let capacity_shannons =
                i64::try_from(capacity).map_err(|_| ParseError::CapacityOverflow(capacity))?;

            let lock_packed: PackedScript = output.lock.clone().into();
            let lock_hash: H256 = lock_packed.calc_script_hash().unpack();

            let (type_code_hash, type_hash_type_opt, type_args, type_hash) = match &output.type_ {
                Some(script) => {
                    let packed: PackedScript = script.clone().into();
                    let script_hash: H256 = packed.calc_script_hash().unpack();
                    (
                        Some(script.code_hash.0.to_vec()),
                        Some(encode_hash_type(&script.hash_type.to_string())?),
                        Some(script.args.as_bytes().to_vec()),
                        Some(script_hash.0.to_vec()),
                    )
                }
                None => (None, None, None, None),
            };

            cells.push(CellRow {
                tx_hash: tx_hash.clone(),
                output_index: i32::try_from(output_index).unwrap_or(i32::MAX),
                block_number,
                capacity_shannons,
                lock_code_hash: output.lock.code_hash.0.to_vec(),
                lock_hash_type: encode_hash_type(&output.lock.hash_type.to_string())?,
                lock_args: output.lock.args.as_bytes().to_vec(),
                lock_hash: lock_hash.0.to_vec(),
                type_code_hash,
                type_hash_type: type_hash_type_opt,
                type_args,
                type_hash,
                data: data.as_bytes().to_vec(),
            });
        }

        // Inputs -> mark referenced cells consumed (skip the cellbase null ref)
        for (input_index, input) in tx.inner.inputs.iter().enumerate() {
            let prev_hash = input.previous_output.tx_hash.0.to_vec();
            if prev_hash.iter().all(|b| *b == 0) {
                continue;
            }
            let prev_output_index: u32 = input.previous_output.index.into();
            consumed.push(ConsumedCellRef {
                tx_hash: prev_hash,
                output_index: i32::try_from(prev_output_index).unwrap_or(i32::MAX),
                consumed_by_tx_hash: tx_hash.clone(),
                consumed_by_input_index: i32::try_from(input_index).unwrap_or(i32::MAX),
                consumed_at_block_number: block_number,
            });
        }
    }

    Ok(ParsedBlock {
        block,
        transactions,
        cells,
        consumed,
    })
}

fn encode_hash_type(s: &str) -> Result<HashType, ParseError> {
    match s {
        "data" => Ok(HashType::Data),
        "type" => Ok(HashType::Type),
        "data1" => Ok(HashType::Data1),
        "data2" => Ok(HashType::Data2),
        other => Err(ParseError::UnknownHashType(other.to_owned())),
    }
}
