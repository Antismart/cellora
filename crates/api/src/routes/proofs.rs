//! `GET /v1/proofs/:tx_hash` — transaction inclusion proof passthrough.
//!
//! Forwards `get_transaction_proof` and `get_header` to the configured
//! CKB node and returns both in one envelope, so a client receives
//! everything it needs to verify the transaction-to-header Merkle path
//! without an extra round-trip.
//!
//! This endpoint is the first concrete step on the verifiable-data
//! path described in ADR 0004. The trust surface drops from "trust
//! Cellora's index" to "did Cellora hand you the right header?", which
//! the client can answer by checking the header against any source
//! they trust.

use axum::extract::{Path, State};
use axum::Json;
use serde::Serialize;
use serde_json::{json, Value as JsonValue};
use utoipa::ToSchema;

use crate::error::{ApiError, ApiResult, ErrorEnvelope};
use crate::hex::{self as hex_helper, Hex32};
use crate::state::AppState;

/// Wire-format envelope for a transaction inclusion proof.
#[derive(Debug, Serialize, ToSchema)]
pub struct ProofResponse {
    /// The transaction hash the proof refers to. Echoed for clients
    /// pipelining multiple lookups.
    #[schema(value_type = String, example = "0x0000000000000000000000000000000000000000000000000000000000000000")]
    pub tx_hash: Hex32,
    /// Hash of the block containing the transaction, as reported by
    /// the node's proof generator.
    #[schema(value_type = String)]
    pub block_hash: Hex32,
    /// Block header for `block_hash`. Forwarded verbatim from the
    /// node's `get_header`.
    #[schema(value_type = serde_json::Value)]
    pub block_header: JsonValue,
    /// Merkle proof body forwarded verbatim from the node's
    /// `get_transaction_proof` (`witnesses_root` + `proof`).
    #[schema(value_type = serde_json::Value)]
    pub proof: JsonValue,
}

/// Handler for `GET /v1/proofs/:tx_hash`.
#[utoipa::path(
    get,
    path = "/v1/proofs/{tx_hash}",
    tag = "proofs",
    params(("tx_hash" = String, Path, description = "0x-prefixed 32-byte transaction hash.")),
    responses(
        (status = 200, description = "Inclusion proof and containing header", body = ProofResponse),
        (status = 400, description = "Path segment is not a valid 32-byte hash", body = ErrorEnvelope),
        (status = 404, description = "Node has no proof for this transaction", body = ErrorEnvelope),
        (status = 503, description = "CKB node unreachable", body = ErrorEnvelope),
    ),
)]
pub async fn passthrough(
    State(state): State<AppState>,
    Path(raw): Path<String>,
) -> ApiResult<Json<ProofResponse>> {
    let tx_hash_bytes = parse_tx_hash(&raw)?;
    let tx_hash_hex = format!("0x{}", ::hex::encode(&tx_hash_bytes));

    let ckb = state
        .ckb
        .as_ref()
        .ok_or(ApiError::UpstreamUnavailable("ckb node not configured"))?;

    // get_transaction_proof returns null when the node has no proof for
    // the requested transaction (typically: not on-chain, or pruned).
    let proof_raw: JsonValue = ckb
        .call("get_transaction_proof", json!([[tx_hash_hex]]))
        .await
        .map_err(|err| {
            tracing::warn!(error = %err, tx = %tx_hash_hex, "get_transaction_proof failed");
            ApiError::UpstreamUnavailable("ckb node unreachable")
        })?;

    if proof_raw.is_null() {
        return Err(ApiError::NotFound(
            "no proof available for this transaction",
        ));
    }

    let block_hash_str = proof_raw
        .get("block_hash")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            tracing::error!("get_transaction_proof returned no block_hash");
            ApiError::Internal(anyhow::anyhow!("node response missing block_hash"))
        })?
        .to_owned();

    let header_raw: JsonValue = ckb
        .call("get_header", json!([block_hash_str.clone()]))
        .await
        .map_err(|err| {
            tracing::warn!(error = %err, block_hash = %block_hash_str, "get_header failed");
            ApiError::UpstreamUnavailable("ckb node unreachable")
        })?;

    if header_raw.is_null() {
        // Race condition: the proof referenced a block the node since
        // pruned. Surface as 503 — the proof we returned would be
        // unverifiable without the header, and the operator should
        // know.
        return Err(ApiError::UpstreamUnavailable(
            "ckb node returned proof for a block it can no longer header",
        ));
    }

    let block_hash_bytes = hex_helper::decode_prefixed(&block_hash_str).ok_or_else(|| {
        tracing::error!(block_hash = %block_hash_str, "invalid block_hash from node");
        ApiError::Internal(anyhow::anyhow!("invalid block_hash from node"))
    })?;

    Ok(Json(ProofResponse {
        tx_hash: Hex32::try_from_slice(&tx_hash_bytes)?,
        block_hash: Hex32::try_from_slice(&block_hash_bytes)?,
        block_header: header_raw,
        proof: strip_block_hash(proof_raw),
    }))
}

/// Pull the `block_hash` out of the proof body since it is hoisted to
/// the top of the response. Keeps the wire format flat for clients.
fn strip_block_hash(mut proof: JsonValue) -> JsonValue {
    if let Some(obj) = proof.as_object_mut() {
        obj.remove("block_hash");
    }
    proof
}

/// Parse a path segment into a 32-byte transaction hash. Accepts only
/// `0x`-prefixed lowercase hex of length 66.
fn parse_tx_hash(raw: &str) -> Result<Vec<u8>, ApiError> {
    let bytes = hex_helper::decode_prefixed(raw)
        .ok_or_else(|| ApiError::BadRequest(format!("'{raw}' is not 0x-prefixed hex")))?;
    if bytes.len() != 32 {
        return Err(ApiError::BadRequest(format!(
            "tx_hash must be exactly 32 bytes (got {})",
            bytes.len()
        )));
    }
    Ok(bytes)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn parse_tx_hash_accepts_canonical_form() {
        let bytes = parse_tx_hash(&format!("0x{}", "ab".repeat(32))).expect("ok");
        assert_eq!(bytes, vec![0xAB; 32]);
    }

    #[test]
    fn parse_tx_hash_rejects_missing_prefix() {
        assert!(matches!(
            parse_tx_hash(&"ab".repeat(32)),
            Err(ApiError::BadRequest(_))
        ));
    }

    #[test]
    fn parse_tx_hash_rejects_wrong_length() {
        assert!(matches!(
            parse_tx_hash("0xabcd"),
            Err(ApiError::BadRequest(_))
        ));
    }

    #[test]
    fn strip_block_hash_removes_top_level_field() {
        let v = json!({"block_hash": "0xff", "witnesses_root": "0xaa", "proof": {}});
        let out = strip_block_hash(v);
        assert!(out.get("block_hash").is_none());
        assert_eq!(out.get("witnesses_root").unwrap(), "0xaa");
    }
}
