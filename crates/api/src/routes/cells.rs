//! Cells endpoint.
//!
//! `GET /v1/cells?lock_hash=0x…` and `?type_hash=0x…` list cells matching
//! the given script hash, paginated with an opaque cursor. The caller
//! supplies exactly one of the two filters; supplying both or neither is a
//! 400. Optional query parameters:
//!
//! - `is_live=true|false` — restrict to live or consumed cells (default
//!   returns both).
//! - `limit` — page size, bounded by `CELLORA_API_DEFAULT_PAGE_SIZE` and
//!   `CELLORA_API_MAX_PAGE_SIZE`.
//! - `cursor` — opaque string returned as `next_cursor` on the previous
//!   page.
//! - `include_data=true` — include the `data` blob in each cell. Off by
//!   default because cell data can be large (script binaries, proofs).
//!
//! Results are ordered `(block_number DESC, tx_hash DESC, output_index DESC)`
//! so the newest cells appear first and cursor-based paging is consistent.

use axum::extract::{Query, State};
use axum::Json;
use cellora_db::cells::{self, CellCursor, LivenessFilter};
use cellora_db::models::Cell;
use serde::{Deserialize, Serialize};

use crate::error::{ApiError, ApiResult};
use crate::hex::{self as hex_helper, Hex, Hex32};
use crate::pagination::{decode_cells_cursor, encode_cells_cursor};
use crate::state::AppState;

/// Incoming query-string parameters for `GET /v1/cells`.
#[derive(Debug, Deserialize)]
pub struct CellsQuery {
    /// Script hash of the cell's lock. One of `lock_hash` or `type_hash`
    /// must be supplied.
    pub lock_hash: Option<String>,
    /// Script hash of the cell's type. One of `lock_hash` or `type_hash`
    /// must be supplied.
    pub type_hash: Option<String>,
    /// Filter on liveness. Omit to return both live and consumed cells.
    pub is_live: Option<bool>,
    /// Opaque cursor returned by the previous page.
    pub cursor: Option<String>,
    /// Requested page size. Capped by `CELLORA_API_MAX_PAGE_SIZE`.
    pub limit: Option<u32>,
    /// Include the raw `data` blob on each returned cell. Default false.
    #[serde(default)]
    pub include_data: bool,
}

/// Response envelope for a single page of cells.
#[derive(Debug, Serialize)]
pub struct CellsPage {
    /// Cells returned by this page, ordered newest first.
    pub data: Vec<CellResponse>,
    /// Opaque cursor to fetch the next page, or `None` when this is the
    /// final page.
    pub next_cursor: Option<String>,
    /// Page-level metadata.
    pub meta: PageMeta,
}

/// Page-level metadata attached to every list response.
#[derive(Debug, Serialize)]
pub struct PageMeta {
    /// Highest block number Cellora has indexed, or `None` on a fresh DB.
    pub indexer_tip: Option<i64>,
    /// Last tip reported by the upstream CKB node, or `None` when the
    /// tip-refresh task has not yet observed the node.
    pub node_tip: Option<u64>,
}

/// CKB script projected onto the wire format.
#[derive(Debug, Serialize)]
pub struct ScriptResponse {
    /// The 32-byte `code_hash` of the script.
    pub code_hash: Hex32,
    /// `hash_type` — one of `data`, `type`, `data1`, `data2`.
    pub hash_type: &'static str,
    /// Variable-length `args` buffer.
    pub args: Hex,
}

/// Where a cell was consumed. `None` for live cells.
#[derive(Debug, Serialize)]
pub struct ConsumedByResponse {
    /// Hash of the consuming transaction.
    pub tx_hash: Hex32,
    /// Index into that transaction's `inputs` array.
    pub input_index: i32,
    /// Block the consuming transaction landed in.
    pub block_number: i64,
}

/// Wire-format shape of a single cell.
#[derive(Debug, Serialize)]
pub struct CellResponse {
    /// Hash of the transaction that produced this cell.
    pub tx_hash: Hex32,
    /// Index of this output within the producing transaction's outputs.
    pub output_index: i32,
    /// Block in which the cell was created.
    pub block_number: i64,
    /// Hash of that block. Lets a client cross-check without a second
    /// round-trip.
    pub block_hash: Hex32,
    /// Cell capacity in shannons (1 CKB = 1e8 shannons).
    pub capacity_shannons: i64,
    /// Lock script.
    pub lock: ScriptResponse,
    /// Precomputed hash of the lock script.
    pub lock_hash: Hex32,
    /// Type script, or `None` when the cell has no type script.
    #[serde(rename = "type")]
    pub type_script: Option<ScriptResponse>,
    /// Precomputed hash of the type script, or `None`.
    pub type_hash: Option<Hex32>,
    /// Raw cell data. Present only when `include_data=true`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Hex>,
    /// `true` when the cell has not been consumed.
    pub is_live: bool,
    /// Details of the spend, `None` when the cell is live.
    pub consumed_by: Option<ConsumedByResponse>,
}

/// Handler for `GET /v1/cells`.
pub async fn list(
    State(state): State<AppState>,
    Query(params): Query<CellsQuery>,
) -> ApiResult<Json<CellsPage>> {
    let filter = parse_filter(&params)?;
    let liveness = parse_liveness(params.is_live);
    let limit = parse_limit(params.limit, &state)?;
    let cursor = params
        .cursor
        .as_deref()
        .map(decode_cells_cursor)
        .transpose()?;

    let fetch_limit = i64::from(limit) + 1;
    let cells = match filter {
        ScriptFilter::Lock(hash) => {
            cells::query_by_lock_hash(&state.db, &hash, liveness, cursor.as_ref(), fetch_limit)
                .await?
        }
        ScriptFilter::Type(hash) => {
            cells::query_by_type_hash(&state.db, &hash, liveness, cursor.as_ref(), fetch_limit)
                .await?
        }
    };

    let (page, next_cursor) = build_page(cells, limit, params.include_data)?;
    let snap = state.tip.get();

    Ok(Json(CellsPage {
        data: page,
        next_cursor,
        meta: PageMeta {
            indexer_tip: snap.indexer_tip,
            node_tip: snap.node_tip,
        },
    }))
}

/// Which script field the client asked to filter on. Exactly one is
/// required.
#[derive(Debug)]
enum ScriptFilter {
    Lock(Vec<u8>),
    Type(Vec<u8>),
}

fn parse_filter(params: &CellsQuery) -> Result<ScriptFilter, ApiError> {
    match (&params.lock_hash, &params.type_hash) {
        (Some(_), Some(_)) => Err(ApiError::BadRequest(
            "specify exactly one of 'lock_hash' or 'type_hash'".into(),
        )),
        (None, None) => Err(ApiError::BadRequest(
            "one of 'lock_hash' or 'type_hash' is required".into(),
        )),
        (Some(raw), None) => Ok(ScriptFilter::Lock(parse_script_hash("lock_hash", raw)?)),
        (None, Some(raw)) => Ok(ScriptFilter::Type(parse_script_hash("type_hash", raw)?)),
    }
}

fn parse_script_hash(field: &'static str, raw: &str) -> Result<Vec<u8>, ApiError> {
    let bytes = hex_helper::decode_prefixed(raw)
        .ok_or_else(|| ApiError::BadRequest(format!("'{field}' must be 0x-prefixed hex")))?;
    if bytes.len() != 32 {
        return Err(ApiError::BadRequest(format!(
            "'{field}' must be exactly 32 bytes (got {})",
            bytes.len()
        )));
    }
    Ok(bytes)
}

fn parse_liveness(raw: Option<bool>) -> LivenessFilter {
    match raw {
        Some(true) => LivenessFilter::OnlyLive,
        Some(false) => LivenessFilter::OnlyConsumed,
        None => LivenessFilter::Any,
    }
}

fn parse_limit(raw: Option<u32>, state: &AppState) -> Result<u32, ApiError> {
    let default = state.config.api_default_page_size;
    let max = state.config.api_max_page_size;
    match raw {
        None => Ok(default),
        Some(0) => Err(ApiError::BadRequest("'limit' must be at least 1".into())),
        Some(n) if n > max => Err(ApiError::BadRequest(format!(
            "'limit' must not exceed {max}"
        ))),
        Some(n) => Ok(n),
    }
}

/// Slice the page, detect whether there is another page, and render each
/// row as a [`CellResponse`].
fn build_page(
    mut rows: Vec<Cell>,
    limit: u32,
    include_data: bool,
) -> Result<(Vec<CellResponse>, Option<String>), ApiError> {
    let has_more = rows.len() > limit as usize;
    if has_more {
        rows.truncate(limit as usize);
    }

    let next_cursor = if has_more {
        rows.last().map(|last| {
            encode_cells_cursor(&CellCursor {
                block_number: last.block_number,
                tx_hash: last.tx_hash.clone(),
                output_index: last.output_index,
            })
        })
    } else {
        None
    };

    let data = rows
        .into_iter()
        .map(|row| render_cell(row, include_data))
        .collect::<Result<Vec<_>, _>>()?;
    Ok((data, next_cursor))
}

fn render_cell(row: Cell, include_data: bool) -> Result<CellResponse, ApiError> {
    let lock = ScriptResponse {
        code_hash: Hex32::try_from_slice(&row.lock_code_hash)?,
        hash_type: hash_type_label(row.lock_hash_type)?,
        args: Hex::new(row.lock_args),
    };

    let type_script = match (
        row.type_code_hash,
        row.type_hash_type,
        row.type_args,
        row.type_hash.as_deref(),
    ) {
        (Some(code_hash), Some(hash_type), Some(args), Some(_)) => Some(ScriptResponse {
            code_hash: Hex32::try_from_slice(&code_hash)?,
            hash_type: hash_type_label(hash_type)?,
            args: Hex::new(args),
        }),
        _ => None,
    };

    let type_hash = row
        .type_hash
        .as_deref()
        .map(Hex32::try_from_slice)
        .transpose()?;

    let consumed_by = match (
        row.consumed_by_tx_hash.as_deref(),
        row.consumed_by_input_index,
        row.consumed_at_block_number,
    ) {
        (Some(tx), Some(input_index), Some(block_number)) => Some(ConsumedByResponse {
            tx_hash: Hex32::try_from_slice(tx)?,
            input_index,
            block_number,
        }),
        _ => None,
    };

    Ok(CellResponse {
        tx_hash: Hex32::try_from_slice(&row.tx_hash)?,
        output_index: row.output_index,
        block_number: row.block_number,
        block_hash: Hex32::try_from_slice(&row.block_hash)?,
        capacity_shannons: row.capacity_shannons,
        lock,
        lock_hash: Hex32::try_from_slice(&row.lock_hash)?,
        type_script,
        type_hash,
        data: include_data.then(|| Hex::new(row.data)),
        is_live: consumed_by.is_none(),
        consumed_by,
    })
}

/// Translate the raw `hash_type` SMALLINT into the CKB JSON-RPC label.
/// Unknown values are a schema violation and therefore an internal error,
/// not a client-facing one.
fn hash_type_label(raw: i16) -> Result<&'static str, ApiError> {
    match raw {
        0 => Ok("data"),
        1 => Ok("type"),
        2 => Ok("data1"),
        3 => Ok("data2"),
        other => Err(ApiError::Internal(anyhow::anyhow!(
            "unknown hash_type value in database: {other}"
        ))),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    fn default_query() -> CellsQuery {
        CellsQuery {
            lock_hash: None,
            type_hash: None,
            is_live: None,
            cursor: None,
            limit: None,
            include_data: false,
        }
    }

    #[test]
    fn parse_filter_requires_exactly_one() {
        let mut q = default_query();
        assert!(matches!(
            parse_filter(&q).unwrap_err(),
            ApiError::BadRequest(_)
        ));

        q.lock_hash = Some(format!("0x{}", "aa".repeat(32)));
        q.type_hash = Some(format!("0x{}", "bb".repeat(32)));
        assert!(matches!(
            parse_filter(&q).unwrap_err(),
            ApiError::BadRequest(_)
        ));
    }

    #[test]
    fn parse_filter_rejects_invalid_hash() {
        let mut q = default_query();
        q.lock_hash = Some("not-hex".to_owned());
        assert!(matches!(
            parse_filter(&q).unwrap_err(),
            ApiError::BadRequest(_)
        ));

        q.lock_hash = Some("0xabcd".to_owned()); // too short
        assert!(matches!(
            parse_filter(&q).unwrap_err(),
            ApiError::BadRequest(_)
        ));
    }

    #[test]
    fn parse_liveness_maps_variants() {
        assert!(matches!(parse_liveness(None), LivenessFilter::Any));
        assert!(matches!(
            parse_liveness(Some(true)),
            LivenessFilter::OnlyLive
        ));
        assert!(matches!(
            parse_liveness(Some(false)),
            LivenessFilter::OnlyConsumed
        ));
    }

    #[test]
    fn hash_type_label_covers_known_variants() {
        assert_eq!(hash_type_label(0).unwrap(), "data");
        assert_eq!(hash_type_label(1).unwrap(), "type");
        assert_eq!(hash_type_label(2).unwrap(), "data1");
        assert_eq!(hash_type_label(3).unwrap(), "data2");
        assert!(hash_type_label(99).is_err());
    }
}
