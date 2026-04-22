# Cellora — Architecture Overview

A design document for a multi-tenant indexing and query service over the CKB
(Nervos) chain. It describes the system's planes, the decisions behind each
one, and the tradeoffs those decisions accept.

## Context

An application reading CKB on-chain state needs more than a node. The
dominant query patterns — live cells by lock, cells by type, transaction
history for an address, outpoint lookups — require a normalized store with
indexes tuned for those access paths. The system described here is that
store, fronted by a multi-tenant REST and GraphQL surface with the
operational properties (reorg safety, observability, SLOs) expected of a
data layer that other services depend on.

The design constraints that shape every other decision:

- Reads dominate writes by orders of magnitude.
- The write path is inherently serial (a chain has one tip).
- The chain can reorg, and the store has to reflect reality, not a stale
  branch.
- Every record in the store is reconstructable from the chain. The database
  is a cache, not a ledger.

## System shape

Three planes, each scaling independently, sitting behind a CKB full node.

```
                      ┌────────────────┐
                      │  CKB full node │
                      └───────┬────────┘
                              │ JSON-RPC
                              │ (get_tip_block_number,
                              │  get_block_by_number,
                              │  get_blockchain_info)
                              ▼
       ┌────────────────────────────────────────────┐
       │            Ingestion plane                 │
       │  poller ─▶ parser ─▶ db writer (1 txn/blk) │
       │  single writer · owns reorg handling       │
       └──────────────┬─────────────────────────────┘
                      │  writes
                      ▼
                ┌──────────────┐        ┌──────────────┐
                │ PostgreSQL   │◀──────▶│    Redis     │
                │ (primary +   │        │  cache +     │
                │  replicas)   │        │  rate limit  │
                └───────┬──────┘        └──────┬───────┘
                        │ reads                │
                        ▼                      ▼
       ┌────────────────────────────────────────────┐
       │              Query plane                   │
       │   Axum ─ REST  ·  async-graphql ─ /graphql │
       │   stateless · scales horizontally          │
       └──────────────┬─────────────────────────────┘
                      │
                      ▼
       ┌────────────────────────────────────────────┐
       │         Edge & control plane               │
       │  Cloudflare TLS · API keys (Argon2)        │
       │  token-bucket rate limit per key in Redis  │
       └────────────────────────────────────────────┘
```

The load-bearing decision is that the ingestion plane is the only writer to
PostgreSQL and the query plane is stateless and read-only. Every other
property of the system — the simplicity of the write path, the horizontal
scalability of the read path, the cleanliness of the reorg algorithm — is
downstream of that separation.

## Ingestion plane

The indexer is a single process that polls the CKB node, parses blocks into
normalized records, and writes them to PostgreSQL. It is the sole writer to
the database.

```
   ┌────────────┐   poll 2s     ┌─────────┐   BlockView    ┌──────────┐
   │ CKB node   │◀──────────────│ poller  │◀───────────────│  parser  │
   └────────────┘               └────┬────┘                └────┬─────┘
                                     │                          │
                                     │  next-block-number       │  normalized
                                     │                          │  (blocks,
                                     │                          │   txs, cells)
                                     ▼                          ▼
                                ┌────────────────────────────────────┐
                                │  db writer — one txn per block     │
                                │  inserts blocks/txs/cells + marks  │
                                │  consumed cells + advances         │
                                │  indexer_state in the same txn     │
                                └──────────────────┬─────────────────┘
                                                   │
                                                   ▼
                                            ┌────────────┐
                                            │ PostgreSQL │
                                            └────────────┘
```

**Polling over subscription.** The indexer calls `get_tip_block_number`,
`get_block_by_number` and `get_blockchain_info` on a 2 s cadence (well
inside the block time). Polling is simpler to reason about than streaming
subscriptions, recovers from transient connection loss without special
cases, and is sufficient for the latency targets of a data layer. The
tradeoff is a few seconds of indexing lag, accepted in exchange for
simpler failure semantics.

**Parsing.** Blocks are parsed using `ckb-jsonrpc-types` directly — scripts,
outpoints, and capacities pass through with their native types rather than
being re-invented. Hashes are stored raw as `BYTEA` (32 bytes) rather than
hex strings so equality lookups and joins read from narrow fixed-width
columns.

**Per-block transaction.** A block's entire contribution — the block row,
its transactions, its output cells, the updates that mark previously-live
cells as consumed, and the advancement of the `indexer_state` tip pointer —
is committed in one PostgreSQL transaction. Either a block is fully indexed
or it is not indexed at all. Readers never observe half a block.

**Graceful shutdown.** A cancellation token wired to `SIGINT`/`SIGTERM` lets
the poller finish the in-flight block before exiting. No block is left
half-written on shutdown.

## Data model

PostgreSQL is the source of truth for query-serving. It is not the source
of truth for the chain — every record can be rebuilt from the node.

**Schema.**

```
blocks (number PK, hash, parent_hash, timestamp_ms, epoch,
        transactions_count, proposals_count, uncles_count,
        nonce, dao, indexed_at)

transactions (hash PK, block_number FK, tx_index,
              version, cell_deps JSONB, header_deps JSONB,
              witnesses JSONB, inputs_count, outputs_count,
              indexed_at)

cells (tx_hash, output_index) PK,
       block_number FK,
       capacity_shannons,
       lock_code_hash, lock_hash_type, lock_args, lock_hash,
       type_code_hash, type_hash_type, type_args, type_hash (nullable),
       data,
       consumed_by_tx_hash, consumed_by_input_index,
       consumed_at_block_number

indexer_state (singleton row: last_indexed_block, last_indexed_hash)
```

**Indexing decisions.**

- `cells_lock_hash_idx` on `lock_hash` — the dominant query pattern.
- `cells_type_hash_idx` on `type_hash` as a **partial index** where
  `type_hash IS NOT NULL`. Cells without a type script (the common case) do
  not bloat the index.
- `cells_consumed_idx` on `consumed_by_tx_hash`, also partial
  (`WHERE consumed_by_tx_hash IS NOT NULL`), so live cells — the majority
  of the table — stay out of the index.
- Outpoint lookup is covered by the `(tx_hash, output_index)` primary key.

**Script representation.** Each cell stores the three script components
(`code_hash`, `hash_type`, `args`) separately *and* the precomputed script
hash. Callers can filter by hash for O(1) lookups, or by prefix on the raw
components when they need pattern matching. Well-known script
classification (tagging common scripts) is an enrichment on top of the raw
data rather than a property of the base schema.

**Live/dead accounting.** A cell is live when `consumed_by_tx_hash IS NULL`.
When a new block's input references a previously-indexed output, the
consuming transaction's write updates the `consumed_*` columns in the same
per-block transaction. A single query on `lock_hash` filtered by
`consumed_by_tx_hash IS NULL` returns the live set for that lock.

**Partitioning.** Range partitioning on `cells.block_number` is on the
roadmap once volume justifies it. Partitioning by block range lets
historical ranges be detached or archived without touching the live
partition, and keeps the hot query path small.

## Query plane

Stateless Axum services read from PostgreSQL replicas and a Redis cache.

```
   client ──▶ Cloudflare ──▶ Axum ──▶ ┌── REST handlers  ──┐
                                      │                    │
                                      └── GraphQL (async-  │──▶ repo layer
                                          graphql)         │    (SQLx)
                                                           │
                                                           ▼
                                                  ┌──────────────┐
                                                  │  Redis cache │
                                                  │  (read-thru) │
                                                  └──────┬───────┘
                                                         │ miss
                                                         ▼
                                                  ┌──────────────┐
                                                  │  Postgres    │
                                                  │  read replica│
                                                  └──────────────┘
```

Every response carries the indexer's tip height and the node's tip height
so clients can compute their own freshness rather than having to trust the
service. Cache entries are keyed by query signature and invalidated by
reorg events published on Redis pub/sub by the ingestion plane. Pagination
is cursor-based with opaque, base64-encoded cursors so the wire format can
evolve without breaking clients.

## Edge and control plane

Cloudflare terminates TLS and absorbs abuse at the edge. Authentication is
API-key based; keys are Argon2-hashed and scoped to an organization with a
tier (free / starter / pro). Rate limiting is a per-key token bucket held
in Redis, with separate buckets for REST and GraphQL because the two
surfaces have different cost profiles. Exceeded limits return `429` with a
`Retry-After` header.

## Reorg handling

Reorgs are a first-class case, not an edge case. The indexer validates the
parent-hash chain on every new block; on a mismatch, it walks back to the
divergence point, rolls the database back to that point in a single
transaction, and resumes forward indexing.

```
  new block N arrives from CKB
          │
          ▼
  parent_hash(N) == hash of our N-1 ?
          │
      ┌───┴────┐
      │        │
    yes        no ──▶ walk back: fetch N-1, N-2, ... from CKB
      │                and compare hashes until we find the
      ▼                common ancestor A
  insert N in        │
  one txn            ▼
                within one PG txn:
                  delete blocks (A, our_tip] — ON DELETE CASCADE
                  removes associated txs and cells
                  re-create any cells that were consumed in
                  rolled-back blocks by resetting their
                  consumed_* columns to NULL
                  set indexer_state to A
                then continue the poll loop forward from A+1
                emit a reorg event on Redis pub/sub so the
                query plane can invalidate caches and downstream
                consumers (webhooks, subscriptions) can react
```

The rollback being transactional means no reader ever sees the system in a
split-brain state where the recorded tip is on the new chain but some of
the old chain's cells are still present.

## Consistency and correctness

- **Single-writer model** eliminates write-side concurrency. No scenario
  involves two processes racing on the cell table.
- **One transaction per block** means the database never observes a
  partial block. Readers see block N in full or not at all.
- **`indexer_state` advances inside that same transaction**, so the
  recorded tip can never be ahead of the data.
- **Reorgs are transactional** — rollback and the advancement of
  `indexer_state` to the common ancestor happen together.
- **Every record is reconstructable** from the node, so the recovery story
  for any corruption is "reindex."

## Tech stack

Rust throughout: Axum for HTTP, async-graphql for GraphQL, SQLx with
compile-time query checking for the database layer, `ckb-jsonrpc-types` +
`reqwest` for the RPC client. PostgreSQL is the store. Redis handles
caching and rate limiting. Cloudflare sits at the edge. Docker Compose for
local development, Kubernetes for production. Observability via
OpenTelemetry tracing and Prometheus metrics.
