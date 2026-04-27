//! GraphQL surface, mounted at `POST /graphql`.
//!
//! Resolvers wrap the same `cellora-db` repository functions as the REST
//! handlers. There is no SQL duplicated between the two surfaces — drift
//! between them is impossible by construction. Hashes are exposed as
//! `0x`-prefixed strings to match REST and the CKB ecosystem convention.
//!
//! Auth and rate limiting live on the route in [`crate::lib::build_app`];
//! resolvers run only after they pass.

use async_graphql::{Context, EmptyMutation, EmptySubscription, Object, Schema, SimpleObject};
use cellora_db::cells::{self as db_cells, CellCursor, LivenessFilter};
use cellora_db::models::Cell;

use crate::error::ApiError;
use crate::pagination::{decode_cells_cursor, encode_cells_cursor};
use crate::state::AppState;

/// The full schema. Constructed once in `build_app` and shared across
/// requests via the axum extractor.
pub type ApiSchema = Schema<QueryRoot, EmptyMutation, EmptySubscription>;

/// Build the schema with the [`AppState`] available to every resolver.
pub fn build_schema(state: AppState) -> ApiSchema {
    Schema::build(QueryRoot, EmptyMutation, EmptySubscription)
        .data(state)
        .finish()
}

/// Top-level query type.
#[derive(Default, Clone, Copy)]
pub struct QueryRoot;

#[Object]
impl QueryRoot {
    /// Highest-numbered indexed block, or `null` when the chain has not
    /// been sampled yet.
    async fn blocks_latest(&self, ctx: &Context<'_>) -> async_graphql::Result<Option<Block>> {
        let state = ctx.data::<AppState>()?;
        let row = cellora_db::blocks::latest(&state.db)
            .await
            .map_err(map_db_err)?;
        Ok(row.map(Block::from))
    }

    /// Block by number, or `null` when not indexed.
    async fn block(&self, ctx: &Context<'_>, number: i64) -> async_graphql::Result<Option<Block>> {
        if number < 0 {
            return Err(async_graphql::Error::new(
                "block number must be non-negative",
            ));
        }
        let state = ctx.data::<AppState>()?;
        let row = cellora_db::blocks::get_by_number(&state.db, number)
            .await
            .map_err(map_db_err)?;
        Ok(row.map(Block::from))
    }

    /// Paginated cells filtered by lock or type hash. Mirrors the REST
    /// `/v1/cells` endpoint exactly — same ordering, same cursor format.
    async fn cells(
        &self,
        ctx: &Context<'_>,
        input: CellsInput,
    ) -> async_graphql::Result<CellsConnection> {
        let state = ctx.data::<AppState>()?;
        let filter = parse_filter(&input)?;
        let liveness = match input.is_live {
            Some(true) => LivenessFilter::OnlyLive,
            Some(false) => LivenessFilter::OnlyConsumed,
            None => LivenessFilter::Any,
        };
        let limit = parse_limit(input.limit, state)?;
        let cursor = match input.cursor.as_deref() {
            Some(raw) => Some(decode_cells_cursor(raw).map_err(api_to_gql)?),
            None => None,
        };

        let fetch_limit = i64::from(limit) + 1;
        let rows = match filter {
            ScriptFilter::Lock(hash) => db_cells::query_by_lock_hash(
                &state.db,
                &hash,
                liveness,
                cursor.as_ref(),
                fetch_limit,
            )
            .await
            .map_err(map_db_err)?,
            ScriptFilter::Type(hash) => db_cells::query_by_type_hash(
                &state.db,
                &hash,
                liveness,
                cursor.as_ref(),
                fetch_limit,
            )
            .await
            .map_err(map_db_err)?,
        };

        let include_data = input.include_data.unwrap_or(false);
        let (data, next_cursor) = build_page(rows, limit, include_data)?;
        let snap = state.tip.get();
        Ok(CellsConnection {
            data,
            next_cursor,
            meta: Meta {
                indexer_tip: snap.indexer_tip,
                node_tip: snap.node_tip.map(|n| n as i64),
            },
        })
    }

    /// Indexer / node tip snapshot. Same shape as `/v1/stats`.
    async fn stats(&self, ctx: &Context<'_>) -> async_graphql::Result<Stats> {
        let state = ctx.data::<AppState>()?;
        let snap = state.tip.get();
        Ok(Stats {
            indexer_tip: snap.indexer_tip,
            node_tip: snap.node_tip.map(|n| n as i64),
            lag_blocks: snap.lag_blocks(),
            snapshot_age_seconds: snap.observed_monotonic.elapsed().as_secs() as i64,
            is_stale: snap.is_stale(),
        })
    }
}

/// GraphQL projection of `cellora_db::models::Block`.
#[derive(SimpleObject, Debug)]
pub struct Block {
    /// Block number.
    pub number: i64,
    /// Block hash, 0x-prefixed hex.
    pub hash: String,
    /// Parent block hash, 0x-prefixed hex.
    pub parent_hash: String,
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
    /// Nervos DAO field, 0x-prefixed hex.
    pub dao: String,
    /// When Cellora first observed this block (RFC3339).
    pub indexed_at: String,
}

impl From<cellora_db::models::Block> for Block {
    fn from(b: cellora_db::models::Block) -> Self {
        Self {
            number: b.number,
            hash: hex_prefixed(&b.hash),
            parent_hash: hex_prefixed(&b.parent_hash),
            timestamp_ms: b.timestamp_ms,
            epoch: b.epoch,
            transactions_count: b.transactions_count,
            proposals_count: b.proposals_count,
            uncles_count: b.uncles_count,
            nonce: b.nonce.to_string(),
            dao: hex_prefixed(&b.dao),
            indexed_at: b.indexed_at.to_rfc3339(),
        }
    }
}

/// Input shape for `cells(...)`. One of `lockHash` or `typeHash` must be
/// supplied — the resolver returns an error if both or neither are.
#[derive(async_graphql::InputObject, Debug)]
pub struct CellsInput {
    /// Lock script hash, 0x-prefixed 32-byte hex.
    pub lock_hash: Option<String>,
    /// Type script hash, 0x-prefixed 32-byte hex.
    pub type_hash: Option<String>,
    /// Restrict to live (`true`) or consumed (`false`) cells. Omit for
    /// both.
    pub is_live: Option<bool>,
    /// Page size; defaults from config, capped by config maximum.
    pub limit: Option<u32>,
    /// Opaque cursor returned by the previous page.
    pub cursor: Option<String>,
    /// Include the raw cell `data` blob in the response.
    pub include_data: Option<bool>,
}

/// CKB script in GraphQL form.
#[derive(SimpleObject, Debug)]
pub struct Script {
    /// Script `code_hash`, 0x-prefixed hex.
    pub code_hash: String,
    /// `hash_type` — `data`, `type`, `data1`, `data2`.
    pub hash_type: String,
    /// `args`, 0x-prefixed hex (variable length).
    pub args: String,
}

/// Where a cell was consumed; `null` for live cells.
#[derive(SimpleObject, Debug)]
pub struct ConsumedBy {
    /// Hash of the consuming transaction.
    pub tx_hash: String,
    /// Index into that transaction's `inputs` array.
    pub input_index: i32,
    /// Block the consuming transaction landed in.
    pub block_number: i64,
}

/// Cell projected onto GraphQL.
#[derive(SimpleObject, Debug)]
pub struct GraphCell {
    /// Hash of the producing transaction.
    pub tx_hash: String,
    /// Index of this output within its transaction.
    pub output_index: i32,
    /// Block in which the cell was created.
    pub block_number: i64,
    /// Hash of that block. Lets a client cross-check without a second
    /// round-trip.
    pub block_hash: String,
    /// Cell capacity in shannons.
    pub capacity_shannons: i64,
    /// Lock script.
    pub lock: Script,
    /// Precomputed lock-script hash.
    pub lock_hash: String,
    /// Type script, or `null` when absent.
    #[graphql(name = "type")]
    pub type_script: Option<Script>,
    /// Precomputed type-script hash, or `null`.
    pub type_hash: Option<String>,
    /// Raw cell data, present only when `includeData=true`.
    pub data: Option<String>,
    /// Whether the cell is live.
    pub is_live: bool,
    /// Spend details, `null` for live cells.
    pub consumed_by: Option<ConsumedBy>,
}

/// A page of cells.
#[derive(SimpleObject, Debug)]
pub struct CellsConnection {
    /// Cells in this page.
    pub data: Vec<GraphCell>,
    /// Cursor to fetch the next page, or `null` when this is the last
    /// page.
    pub next_cursor: Option<String>,
    /// Tip metadata.
    pub meta: Meta,
}

/// Page metadata.
#[derive(SimpleObject, Debug)]
pub struct Meta {
    /// Highest block Cellora has indexed.
    pub indexer_tip: Option<i64>,
    /// Last tip the upstream node reported.
    pub node_tip: Option<i64>,
}

/// Indexer status.
#[derive(SimpleObject, Debug)]
pub struct Stats {
    /// Highest block Cellora has indexed.
    pub indexer_tip: Option<i64>,
    /// Last tip the upstream node reported.
    pub node_tip: Option<i64>,
    /// `node_tip - indexer_tip` when both are known.
    pub lag_blocks: Option<i64>,
    /// Age of the cached snapshot in seconds.
    pub snapshot_age_seconds: i64,
    /// Whether the cached snapshot is stale.
    pub is_stale: bool,
}

/// Resolver-level helpers below. Kept private — handler code talks to the
/// schema, not these.

#[derive(Debug)]
enum ScriptFilter {
    Lock(Vec<u8>),
    Type(Vec<u8>),
}

fn parse_filter(input: &CellsInput) -> async_graphql::Result<ScriptFilter> {
    match (input.lock_hash.as_deref(), input.type_hash.as_deref()) {
        (Some(_), Some(_)) => Err(async_graphql::Error::new(
            "specify exactly one of 'lockHash' or 'typeHash'",
        )),
        (None, None) => Err(async_graphql::Error::new(
            "one of 'lockHash' or 'typeHash' is required",
        )),
        (Some(raw), None) => Ok(ScriptFilter::Lock(parse_script_hash("lockHash", raw)?)),
        (None, Some(raw)) => Ok(ScriptFilter::Type(parse_script_hash("typeHash", raw)?)),
    }
}

fn parse_script_hash(field: &'static str, raw: &str) -> async_graphql::Result<Vec<u8>> {
    let bytes = crate::hex::decode_prefixed(raw)
        .ok_or_else(|| async_graphql::Error::new(format!("'{field}' must be 0x-prefixed hex")))?;
    if bytes.len() != 32 {
        return Err(async_graphql::Error::new(format!(
            "'{field}' must be exactly 32 bytes (got {})",
            bytes.len()
        )));
    }
    Ok(bytes)
}

fn parse_limit(raw: Option<u32>, state: &AppState) -> async_graphql::Result<u32> {
    let default = state.config.api_default_page_size;
    let max = state.config.api_max_page_size;
    match raw {
        None => Ok(default),
        Some(0) => Err(async_graphql::Error::new("'limit' must be at least 1")),
        Some(n) if n > max => Err(async_graphql::Error::new(format!(
            "'limit' must not exceed {max}"
        ))),
        Some(n) => Ok(n),
    }
}

fn build_page(
    mut rows: Vec<Cell>,
    limit: u32,
    include_data: bool,
) -> async_graphql::Result<(Vec<GraphCell>, Option<String>)> {
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
        .collect::<async_graphql::Result<Vec<_>>>()?;
    Ok((data, next_cursor))
}

fn render_cell(row: Cell, include_data: bool) -> async_graphql::Result<GraphCell> {
    let lock = Script {
        code_hash: hex_prefixed(&row.lock_code_hash),
        hash_type: hash_type_label(row.lock_hash_type)?.to_owned(),
        args: hex_prefixed(&row.lock_args),
    };
    let type_script = match (
        row.type_code_hash.as_deref(),
        row.type_hash_type,
        row.type_args.as_deref(),
        row.type_hash.as_deref(),
    ) {
        (Some(code_hash), Some(hash_type), Some(args), Some(_)) => Some(Script {
            code_hash: hex_prefixed(code_hash),
            hash_type: hash_type_label(hash_type)?.to_owned(),
            args: hex_prefixed(args),
        }),
        _ => None,
    };
    let consumed_by = match (
        row.consumed_by_tx_hash.as_deref(),
        row.consumed_by_input_index,
        row.consumed_at_block_number,
    ) {
        (Some(tx), Some(input_index), Some(block_number)) => Some(ConsumedBy {
            tx_hash: hex_prefixed(tx),
            input_index,
            block_number,
        }),
        _ => None,
    };
    Ok(GraphCell {
        tx_hash: hex_prefixed(&row.tx_hash),
        output_index: row.output_index,
        block_number: row.block_number,
        block_hash: hex_prefixed(&row.block_hash),
        capacity_shannons: row.capacity_shannons,
        lock,
        lock_hash: hex_prefixed(&row.lock_hash),
        type_script,
        type_hash: row.type_hash.as_deref().map(hex_prefixed),
        data: include_data.then(|| hex_prefixed(&row.data)),
        is_live: consumed_by.is_none(),
        consumed_by,
    })
}

fn hash_type_label(raw: i16) -> async_graphql::Result<&'static str> {
    match raw {
        0 => Ok("data"),
        1 => Ok("type"),
        2 => Ok("data1"),
        3 => Ok("data2"),
        other => Err(async_graphql::Error::new(format!(
            "unknown hash_type value in database: {other}"
        ))),
    }
}

fn hex_prefixed(bytes: &[u8]) -> String {
    let mut buf = String::with_capacity(2 + bytes.len() * 2);
    buf.push_str("0x");
    for byte in bytes {
        use std::fmt::Write;
        let _ = write!(&mut buf, "{byte:02x}");
    }
    buf
}

fn map_db_err(err: cellora_db::DbError) -> async_graphql::Error {
    async_graphql::Error::new(format!("database error: {err}"))
}

fn api_to_gql(err: ApiError) -> async_graphql::Error {
    async_graphql::Error::new(err.to_string())
}
