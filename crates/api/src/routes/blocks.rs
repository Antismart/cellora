//! Blocks endpoints.
//!
//! * `GET /v1/blocks/latest` — returns the highest-numbered block Cellora
//!   has indexed. 404 when the chain has not been sampled yet.
//! * `GET /v1/blocks/:number` — returns a block by number. 400 on a
//!   non-numeric or negative path segment, 404 when the block has not
//!   been indexed.
//!
//! The response shape matches the columns in the `blocks` table, with
//! hashes and `dao` rendered as `0x`-prefixed hex and `nonce` as a decimal
//! string (it can be up to 128 bits wide, so representing it as a JSON
//! number would lose precision in many clients).

use axum::extract::{Path, State};
use axum::Json;
use cellora_db::models::Block;
use chrono::{DateTime, Utc};
use serde::Serialize;
use utoipa::ToSchema;

use crate::error::{ApiError, ApiResult, ErrorEnvelope};
use crate::hex::Hex32;
use crate::state::AppState;

/// Wire-format shape of a single block.
#[derive(Debug, Serialize, ToSchema)]
pub struct BlockResponse {
    /// Block number.
    pub number: i64,
    /// Block hash (0x-prefixed 32-byte hex string).
    #[schema(value_type = String, example = "0x0000000000000000000000000000000000000000000000000000000000000000")]
    pub hash: Hex32,
    /// Parent block hash.
    #[schema(value_type = String, example = "0x0000000000000000000000000000000000000000000000000000000000000000")]
    pub parent_hash: Hex32,
    /// Block timestamp in milliseconds since the Unix epoch.
    pub timestamp_ms: i64,
    /// Epoch encoded as CKB packs it (length/index/number bitfield).
    pub epoch: i64,
    /// Number of transactions in the block.
    pub transactions_count: i32,
    /// Number of proposals in the block.
    pub proposals_count: i32,
    /// Number of uncles attached to the block.
    pub uncles_count: i32,
    /// Raw nonce, rendered as a decimal string.
    pub nonce: String,
    /// Nervos DAO field.
    #[schema(value_type = String, example = "0x0000000000000000000000000000000000000000000000000000000000000000")]
    pub dao: Hex32,
    /// When Cellora first observed this block.
    pub indexed_at: DateTime<Utc>,
}

impl TryFrom<Block> for BlockResponse {
    type Error = ApiError;

    fn try_from(block: Block) -> Result<Self, ApiError> {
        Ok(Self {
            number: block.number,
            hash: Hex32::try_from_slice(&block.hash)?,
            parent_hash: Hex32::try_from_slice(&block.parent_hash)?,
            timestamp_ms: block.timestamp_ms,
            epoch: block.epoch,
            transactions_count: block.transactions_count,
            proposals_count: block.proposals_count,
            uncles_count: block.uncles_count,
            nonce: block.nonce.to_string(),
            dao: Hex32::try_from_slice(&block.dao)?,
            indexed_at: block.indexed_at,
        })
    }
}

/// Handler for `GET /v1/blocks/latest`.
#[utoipa::path(
    get,
    path = "/v1/blocks/latest",
    tag = "blocks",
    responses(
        (status = 200, description = "Highest indexed block", body = BlockResponse),
        (status = 404, description = "No blocks indexed yet", body = ErrorEnvelope),
    ),
)]
pub async fn latest(State(state): State<AppState>) -> ApiResult<Json<BlockResponse>> {
    let block = cellora_db::blocks::latest(&state.db)
        .await?
        .ok_or(ApiError::NotFound("no blocks indexed yet"))?;
    Ok(Json(block.try_into()?))
}

/// Handler for `GET /v1/blocks/:number`.
///
/// The path segment is parsed as an unsigned integer explicitly so that
/// malformed input returns the standard JSON error envelope rather than
/// Axum's default `Path` extractor rejection.
#[utoipa::path(
    get,
    path = "/v1/blocks/{number}",
    tag = "blocks",
    params(
        ("number" = i64, Path, description = "Block number (non-negative integer).")
    ),
    responses(
        (status = 200, description = "Block found", body = BlockResponse),
        (status = 400, description = "Path segment is not a valid block number", body = ErrorEnvelope),
        (status = 404, description = "Block not indexed", body = ErrorEnvelope),
    ),
)]
pub async fn by_number(
    State(state): State<AppState>,
    Path(raw): Path<String>,
) -> ApiResult<Json<BlockResponse>> {
    let number: i64 = parse_block_number(&raw)?;
    let block = cellora_db::blocks::get_by_number(&state.db, number)
        .await?
        .ok_or(ApiError::NotFound("block not indexed"))?;
    Ok(Json(block.try_into()?))
}

/// Parse a block number from a path segment. Accepts only non-negative
/// integers that fit in `i64` (the database column type).
fn parse_block_number(raw: &str) -> Result<i64, ApiError> {
    let parsed: u64 = raw
        .parse()
        .map_err(|_| ApiError::BadRequest(format!("invalid block number: '{raw}'")))?;
    i64::try_from(parsed)
        .map_err(|_| ApiError::BadRequest(format!("block number out of range: '{raw}'")))
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn parse_block_number_accepts_zero_and_large_values() {
        assert_eq!(parse_block_number("0").unwrap(), 0);
        assert_eq!(parse_block_number("1234567890").unwrap(), 1_234_567_890);
        assert_eq!(parse_block_number(&i64::MAX.to_string()).unwrap(), i64::MAX);
    }

    #[test]
    fn parse_block_number_rejects_non_numeric() {
        let err = parse_block_number("abc").unwrap_err();
        assert!(matches!(err, ApiError::BadRequest(_)));
    }

    #[test]
    fn parse_block_number_rejects_negative() {
        let err = parse_block_number("-1").unwrap_err();
        assert!(matches!(err, ApiError::BadRequest(_)));
    }

    #[test]
    fn parse_block_number_rejects_overflow() {
        let over = format!("{}", u64::MAX);
        let err = parse_block_number(&over).unwrap_err();
        assert!(matches!(err, ApiError::BadRequest(_)));
    }
}
