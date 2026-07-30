# Cellora API

Read-only HTTP surface over indexed CKB data. The REST surface lives at
`/v1/*` and mirrors the OpenAPI specification at
[`docs/openapi.json`](./openapi.json). The GraphQL surface lives at
`POST /graphql` and exposes the same data through a single typed query
root.

All responses are JSON. Hashes are `0x`-prefixed lowercase hex.

## Authentication

Every data-serving endpoint requires an API key, presented as
`Authorization: Bearer cell_<prefix>_<secret>`. Issue keys with the
admin CLI:

```bash
cargo run -p cellora-api -- admin create-key --tier free --label "alice"
```

The full key is shown once at creation. Only the prefix and the
Argon2id hash of the secret are persisted; a lost key is unrecoverable
and must be reissued. Tier (`free`, `starter`, `pro`) drives rate-limit
parameters and is fixed for the lifetime of the key.

> **Testing phase:** tiers are currently **rate-limit presets only** —
> there is no billing or payment yet, and every tier is freely available
> while the service is in testing. Paid plans and usage-based billing are
> planned for a later milestone, once testing is complete. Pick the tier
> whose limits suit your workload; you will not be charged for any of them.

**Public paths (no auth required):**

- `GET /v1/health`
- `GET /v1/health/ready`
- `GET /v1/openapi.json`

Anything else returns 401 `unauthorized` without a valid Bearer token.
Requests under `/v1/*` to paths that do not exist return 401 to
unauthenticated clients (we do not leak the route surface) and 404 to
authenticated clients.

## Rate limiting

Per-key token-bucket limits, separate buckets for the REST and GraphQL
surfaces. Allowed requests carry `X-RateLimit-Limit`,
`X-RateLimit-Remaining`, and `X-RateLimit-Reset`. Exceeded limits
return 429 `rate_limited` with `Retry-After` set to the seconds until
the bucket has a token available.

Default tier limits (sized for sanity, tuneable via env vars):

| Tier    | REST burst | REST refill/s | GraphQL burst | GraphQL refill/s |
|---------|-----------:|--------------:|--------------:|-----------------:|
| free    |         30 |             1 |            10 |              0.5 |
| starter |        300 |            20 |           100 |               10 |
| pro     |       3000 |           200 |          1000 |              100 |

When Redis is unreachable the limiter fails open by default — set
`CELLORA_API_RATE_LIMIT_FAIL_OPEN=false` to fail closed instead.

## Conventions

### Error envelope

Every non-2xx response uses a single shape:

```json
{
  "error": {
    "code": "bad_request",
    "message": "'lock_hash' must be 0x-prefixed hex",
    "details": null
  }
}
```

`code` is one of `bad_request`, `not_found`, `invalid_cursor`,
`unauthorized`, `rate_limited`, `upstream_unavailable`, `internal`.

### Response headers

Every response carries `x-request-id` (echoed from the client or
generated server-side). 2xx responses additionally carry:

- `x-indexer-tip` — the tip block number the service observed at
  response time.
- `x-indexer-tip-stale: true` — set when the internal tip snapshot is
  older than the staleness threshold (default 5 seconds). Typically
  means the refresh task cannot reach Postgres or the CKB node.

### Pagination

List endpoints return `{ data, next_cursor, meta }`. Pass the string
from `next_cursor` back as `?cursor=…` to fetch the next page. Cursors
are opaque — do not parse them. A `next_cursor` of `null` means the
final page.

## Endpoints

### `GET /v1/health` — liveness

Always returns 200 if the process is running. Used by container
orchestrators for liveness probes.

```bash
curl -s http://localhost:8080/v1/health | jq
```

```json
{ "status": "ok", "version": "0.1.0" }
```

### `GET /v1/health/ready` — readiness

Returns 200 when the database, Redis, and the CKB node are all reachable;
503 if any dependency is failing. Used by container orchestrators for
readiness probes.

```bash
curl -s http://localhost:8080/v1/health/ready | jq
```

```json
{
  "status": "ready",
  "db": "ok",
  "redis": "ok",
  "ckb_node": { "state": "ok", "tip": 1910830, "is_synced": true }
}
```

### `GET /v1/blocks/latest`

The highest-numbered block Cellora has indexed.

```bash
curl -s http://localhost:8080/v1/blocks/latest | jq
```

```json
{
  "number": 12345,
  "hash": "0x0000000000000000000000000000000000000000000000000000000000000000",
  "parent_hash": "0x0000000000000000000000000000000000000000000000000000000000000000",
  "timestamp_ms": 1712345678901,
  "epoch": 8796117893191933,
  "transactions_count": 12,
  "proposals_count": 0,
  "uncles_count": 0,
  "nonce": "51297458091837492857483918273746501283",
  "dao": "0x0000000000000000000000000000000000000000000000000000000000000000",
  "indexed_at": "2026-04-23T10:45:12.123456Z"
}
```

Returns 404 `not_found` when the chain has not been sampled yet.

### `GET /v1/blocks/{number}`

A specific block by number.

```bash
curl -s http://localhost:8080/v1/blocks/1000 | jq
```

Path-segment errors:

- 400 `bad_request` — non-numeric, negative, or overflowing values
  (e.g., `/v1/blocks/abc`, `/v1/blocks/-1`).
- 404 `not_found` — the block has not been indexed.

### `GET /v1/cells`

Paginated list of cells matching a lock or type script hash. Exactly
one of `lock_hash` or `type_hash` must be supplied.

**Query parameters:**

| Name | Type | Default | Notes |
|---|---|---|---|
| `lock_hash` | `0x`-prefixed 32-byte hex | — | Required if `type_hash` is absent. |
| `type_hash` | `0x`-prefixed 32-byte hex | — | Required if `lock_hash` is absent. |
| `is_live` | `true` / `false` | returns both | Restrict to live or consumed cells. |
| `limit` | integer | `CELLORA_API_DEFAULT_PAGE_SIZE` (50) | Capped by `CELLORA_API_MAX_PAGE_SIZE` (500). |
| `cursor` | opaque string | — | Pass the `next_cursor` from the previous page. |
| `include_data` | `true` / `false` | `false` | Include the raw `data` blob on every cell. Off by default because cell data can be large. |

**Example — live cells for a lock hash, first page:**

```bash
curl -s "http://localhost:8080/v1/cells?lock_hash=0x$(printf 'aa%.0s' {1..32})&is_live=true&limit=2" | jq
```

```json
{
  "data": [
    {
      "tx_hash": "0x...",
      "output_index": 0,
      "block_number": 12345,
      "block_hash": "0x...",
      "capacity_shannons": 10000000000,
      "lock": {
        "code_hash": "0x...",
        "hash_type": "type",
        "args": "0xdeadbeef"
      },
      "lock_hash": "0x...",
      "type": null,
      "type_hash": null,
      "is_live": true,
      "consumed_by": null
    }
  ],
  "next_cursor": "eyJibiI6MTIzNDUsInR4IjoiMHguLi4iLCJvaSI6MH0",
  "meta": {
    "indexer_tip": 12345,
    "node_tip": 12348
  }
}
```

**Walking pages:**

```bash
cursor=""
while :; do
  resp=$(curl -s "http://localhost:8080/v1/cells?lock_hash=0x...&limit=100${cursor:+&cursor=$cursor}")
  echo "$resp" | jq '.data[]'
  cursor=$(echo "$resp" | jq -r '.next_cursor // empty')
  [ -z "$cursor" ] && break
done
```

Errors:

- 400 `bad_request` — both / neither of `lock_hash`/`type_hash`, invalid
  hex, hash wrong length, `limit` 0 or above `CELLORA_API_MAX_PAGE_SIZE`.
- 400 `invalid_cursor` — cursor is malformed, tampered, or references
  a tx hash of the wrong length.

### `GET /v1/stats`

Indexer progress and lag, read from an in-memory snapshot refreshed by a
background task. The endpoint does not touch the database, so it is
safe to poll.

```bash
curl -s http://localhost:8080/v1/stats | jq
```

```json
{
  "indexer_tip": 12345,
  "node_tip": 12348,
  "lag_blocks": 3,
  "snapshot_age_seconds": 0,
  "is_stale": false
}
```

`is_stale: true` indicates the refresh task has not published a fresh
snapshot within the staleness threshold (default 5 s). Either the CKB
node or Postgres is unreachable; check `/v1/health/ready` and server
logs.

### `GET /v1/openapi.json`

Serves the OpenAPI 3 specification for the API, matching the committed
`docs/openapi.json`. Feed this into Postman, Swagger UI, or any OpenAPI
code generator.

```bash
curl -s http://localhost:8080/v1/openapi.json | jq '.paths | keys'
```

## GraphQL

`POST /graphql` exposes the same data as the REST surface through a
single typed query root. Auth and rate limiting apply identically; the
GraphQL bucket is separate from REST.

### Schema

```graphql
type Query {
  blocksLatest: Block
  block(number: Int!): Block
  cells(input: CellsInput!): CellsConnection!
  stats: Stats!
}

input CellsInput {
  lockHash: String
  typeHash: String
  isLive: Boolean
  limit: Int
  cursor: String
  includeData: Boolean
}

type Block {
  number: Int!
  hash: String!
  parentHash: String!
  timestampMs: Int!
  epoch: Int!
  transactionsCount: Int!
  proposalsCount: Int!
  unclesCount: Int!
  nonce: String!
  dao: String!
  indexedAt: String!
}

type Cell {
  txHash: String!
  outputIndex: Int!
  blockNumber: Int!
  blockHash: String!
  capacityShannons: Int!
  lock: Script!
  lockHash: String!
  type: Script
  typeHash: String
  data: String
  isLive: Boolean!
  consumedBy: ConsumedBy
}

type Script {
  codeHash: String!
  hashType: String!
  args: String!
}

type ConsumedBy {
  txHash: String!
  inputIndex: Int!
  blockNumber: Int!
}

type CellsConnection {
  data: [Cell!]!
  nextCursor: String
  meta: Meta!
}

type Meta {
  indexerTip: Int
  nodeTip: Int
}

type Stats {
  indexerTip: Int
  nodeTip: Int
  lagBlocks: Int
  snapshotAgeSeconds: Int!
  isStale: Boolean!
}
```

### Examples

Latest block:

```bash
curl -s -X POST http://localhost:8080/graphql \
  -H "authorization: Bearer $CELLORA_API_KEY" \
  -H "content-type: application/json" \
  -d '{"query":"{ blocksLatest { number hash transactionsCount } }"}' | jq
```

Cells by lock hash with pagination:

```bash
curl -s -X POST http://localhost:8080/graphql \
  -H "authorization: Bearer $CELLORA_API_KEY" \
  -H "content-type: application/json" \
  -d '{"query":"query Q($lock: String!) { cells(input: { lockHash: $lock, limit: 50 }) { data { txHash outputIndex blockHash isLive } nextCursor meta { indexerTip nodeTip } } }","variables":{"lock":"0xaaaa...32 bytes"}}' \
  | jq
```

Indexer stats:

```bash
curl -s -X POST http://localhost:8080/graphql \
  -H "authorization: Bearer $CELLORA_API_KEY" \
  -H "content-type: application/json" \
  -d '{"query":"{ stats { indexerTip nodeTip lagBlocks isStale } }"}' | jq
```

### Error semantics

GraphQL errors use the standard GraphQL response shape — a top-level
`errors` array on a 200 response — rather than the REST envelope:

```json
{ "data": null, "errors": [{ "message": "..." }] }
```

This is convention per protocol; clients that wrap both surfaces should
branch on the response shape, not the HTTP status. Auth failures
(missing or invalid Bearer) and rate-limit refusals still return their
respective HTTP statuses (401 / 429) with the REST error envelope —
those checks happen before the GraphQL handler runs.

## Load testing

A k6 script at [`tests/load/rate_limit.js`](../tests/load/rate_limit.js)
exercises the rate limiter against a running stack. Issue a free-tier
key, export `CELLORA_API_KEY`, and run:

```bash
k6 run tests/load/rate_limit.js
```

The script asserts that no 5xx responses are returned, both 200 and 429
appear, and every 429 carries `Retry-After` and `X-RateLimit-Reset`.
