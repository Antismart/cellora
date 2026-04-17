//! Thin helpers around `ckb-jsonrpc-types` and `ckb-types`.

use ckb_jsonrpc_types::Script as RpcScript;
use ckb_types::{packed::Script as PackedScript, prelude::*, H256};

/// Compute the canonical 32-byte blake2b hash of a script as defined by the
/// CKB VM specification.
pub fn script_hash(script: &RpcScript) -> [u8; 32] {
    let packed: PackedScript = script.clone().into();
    let hash: H256 = packed.calc_script_hash().unpack();
    hash.0
}
