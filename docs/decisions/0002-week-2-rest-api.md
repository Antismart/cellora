# ADR 0002 — Week 2 REST API shape and scope

- **Status:** Accepted
- **Date:** 2026-04-23
- **Context:** Week 2 of the roadmap in `CLAUDE.md` — expose the indexed
  data via a read-only REST API with pagination, consistent errors and an
  OpenAPI spec. No GraphQL, auth, rate limiting or Redis this week.

## Context

Week 1 shipped ingestion: blocks, transactions and cells land in Postgres
under one transaction per block. Week 2 exposes that data to clients. The
decisions that needed making before writing code were: where the crate
lives, what the response and error shapes look like, how pagination is
modelled, how we avoid drift between the code and a committed OpenAPI
spec, and how tip-freshness is surfaced to callers.

## Decisions

### Crate layout

A new `crates/api` crate, same shape as `crates/indexer` (lib + bin):
`main.rs` wires config, logging, pool and server; `lib.rs` exposes
`build_app(state) -> Router` so integration tests construct the app
without going through `main`. Handlers stay thin; SQL lives in `db`.

### Endpoint set

`/v1/health` (liveness), `/v1/health/ready` (DB readiness),
`/v1/blocks/latest`, `/v1/blocks/:number`, `/v1/cells` (one required
filter: `lock_hash` or `type_hash`, optional `is_live`, `limit`,
`cursor`, `include_data`), `/v1/stats`. `/docs/openapi.json` plus a
Swagger UI at `/docs/`.

### Response and error shapes

All collection responses carry `data`, optional `next_cursor`, and a
`meta` block with `indexer_tip` and `node_tip`. Every 2xx sets an
`X-Indexer-Tip` header so clients can compute freshness without
parsing the body. Errors use a single envelope:
`{"error":{"code","message","details"}}` with an enumerated set of codes
(`bad_request`, `not_found`, `invalid_cursor`, `upstream_unavailable`,
`internal`).

### Hash serialization

Hashes are `BYTEA(32)` in Postgres; on the wire they serialize as `0x`-
prefixed hex. Implemented via a `Hex32` newtype with custom serde and
`utoipa::ToSchema`. This matches CKB ecosystem convention and keeps the
database schema narrow.

### Pagination

Opaque base64url cursor. For `cells`, the cursor encodes
`(block_number, tx_hash, output_index)` ordered `(bn DESC, tx, oi)`. The
query uses strict tuple comparison so pages don't overlap or gap.
Tampered or malformed cursors return 400 `invalid_cursor`, not 500.

### Tip cache

A `tip::TipTracker` holds an `arc-swap`'d snapshot refreshed every 1 s
by a background task (DB query + CKB `get_tip_block_number`). Handlers
read the snapshot with zero async overhead. On refresh failure, the last
known value continues to be served and a warning is logged; after a
configurable staleness threshold (default 5 s) responses set an
`X-Indexer-Tip-Stale: true` header.

### OpenAPI drift check

`utoipa` generates the spec at build time. A test (`openapi_drift`)
re-serialises the spec and compares it byte-for-byte to
`docs/openapi.json`. If they disagree the test fails with instructions
to regenerate. This works today without CI infrastructure.

### `include_data` default

`/v1/cells` omits the `data` blob unless `?include_data=true`. Cell data
can be kilobytes-to-megabytes (scripts, RGB++ proofs); defaulting it off
keeps the common-case response small. Clients that need it opt in.

### Empty-chain behaviour

Before the indexer has committed its first block, `/v1/blocks/latest`
returns 404 and `/v1/stats` returns `indexer_tip: null`. Neither case
is an error.

### Middleware stack

Outermost to innermost: `TraceLayer` (structured span with method, path,
request_id, status, latency), `SetRequestIdLayer` + `PropagateRequestIdLayer`,
`TimeoutLayer` (10 s default, configurable), `CatchPanicLayer` mapping
panics into the standard error envelope. CORS is deferred until the
dashboard lands.

### Dependencies added

`axum`, `tower`, `tower-http` (trace, timeout, request-id, catch-panic),
`utoipa` + `utoipa-axum` + `utoipa-swagger-ui`, `base64` (URL-safe),
`arc-swap`. No other new crates this week.

## Consequences

- The `api` crate depends on `common` and `db` only, preserving the
  dependency graph set in ADR 0001.
- Response shape is consistent across endpoints, so clients and the
  eventual GraphQL layer (Week 3) can share types — GraphQL will wrap
  the same repositories rather than duplicating SQL.
- `X-Indexer-Tip` and the `meta.indexer_tip` field are set from day one,
  so callers don't have to change their code when staleness handling
  becomes important in Week 4.
- The OpenAPI drift check locks the committed spec to the code. A PR
  that changes an endpoint without regenerating the spec fails tests.
- The tip cache keeps stats and tip-in-meta cheap; later weeks can swap
  the refresh source from the CKB node to a Redis key without changing
  handler code.

## Implementation slices

1. `api` crate scaffold, `/v1/health` + `/v1/health/ready`, middleware
   stack, integration harness.
2. Blocks endpoints, `Hex32`, error enum + `IntoResponse`.
3. Cells endpoint, pagination, cursor tests (largest piece).
4. Tip cache + `/v1/stats`.
5. OpenAPI integration, `docs/openapi.json`, drift check.
6. `docs/api.md` with curl examples, README update.

Each slice is a single commit, tests green, conventional commit messages.
