# Cellora — Architecture Overview

Audience: CKB protocol and ecosystem engineers. This document describes the
shape of the system, the design choices specific to CKB's cell model, and the
current state of the implementation versus what is still on the roadmap.

## Problem

Anyone building a non-trivial CKB application needs indexed access to the
chain. Running a full node is only the first step — most query patterns
(live cells by lock, historical cells by type, transaction history for an
address) require an indexer and a database behind it. Every team currently
pays that cost on their own.

The landscape today is the node's built-in `ckb-indexer`, Mercury, and various
bespoke indexers that teams run internally. The built-in indexer is excellent
for local wallet use but is not designed as a multi-tenant data layer.
Mercury is more ambitious in scope and we are leaning on the lessons from its
design rather than attempting to compete with it feature-for-feature.

Cellora is scoped deliberately narrower: a production data layer for cells,
transactions and blocks, with a REST and GraphQL surface, multi-tenant auth,
and operational qualities (reorg safety, observability, SLOs) that let a DApp
team treat it as infrastructure.

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

The ingestion plane is the only component with write access to PostgreSQL.
The query plane is stateless and reads only. This separation is the load-
bearing decision in the design — everything else follows from it.

## Ingestion plane

The indexer is a single Rust process that polls the CKB node, parses blocks
into normalized records and writes them to PostgreSQL. It is the only writer
to the database.

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

**RPC surface used.** The indexer relies on three JSON-RPC methods today:
`get_tip_block_number`, `get_block_by_number`, and `get_blockchain_info`. The
polling default is 2000 ms and is configurable via `CELLORA_POLL_INTERVAL_MS`.
We are not currently using the subscription RPCs; polling is simpler to
reason about and the 2 s cadence is well inside CKB's block time.

**Parsing.** Blocks are parsed using `ckb-jsonrpc-types` directly — scripts,
outpoints and capacity are carried through with their native types, not
re-invented. Hashes are stored raw (`BYTEA`, 32 bytes) rather than as hex so
equality lookups read from narrow fixed-width columns.

**Per-block transaction.** Everything a block contributes — new block row,
its transactions, its outputs (new cells), updates to any inputs (marking
previously-live cells as consumed) and the advancement of `indexer_state` —
is committed in a single PostgreSQL transaction. Either a block is fully
indexed or it is not indexed at all.

**Graceful shutdown.** A `CancellationToken` wired to `SIGINT`/`SIGTERM`
lets the poller finish the in-flight block before exiting. No block is ever
left half-written.

## Data model

PostgreSQL is the source of truth for query-serving but not for the chain —
every record is reconstructable from the CKB node. The database is a cache,
not a ledger.

**Schema (current).**

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

- `cells_lock_hash_idx` on `lock_hash` — the dominant query pattern (cells
  for an address / a known lock).
- `cells_type_hash_idx` on `type_hash` as a **partial index** where
  `type_hash IS NOT NULL`, so cells without a type script (the common case)
  don't bloat the index.
- `cells_consumed_idx` on `consumed_by_tx_hash` is also partial
  (`WHERE consumed_by_tx_hash IS NOT NULL`), so live cells — the majority
  of the table — stay out of that index.
- Outpoint lookup is free via the `(tx_hash, output_index)` primary key.

**Script representation.** Each cell stores the three script components
(`code_hash`, `hash_type`, `args`) separately *and* the precomputed script
hash. This lets callers filter by the hash for O(1) lookups but also by
prefix on `code_hash` + `hash_type` + `args` when they want pattern matching
(for example, xUDT cells with a particular owner lock prefix). We do not yet
normalize or tag well-known scripts (Sighash, MultiSig, xUDT, Spore) — that
is a future enrichment on top of the raw data.

**Live/dead accounting.** A cell is live when `consumed_by_tx_hash IS NULL`.
When an input references a previously-indexed output, the consuming
transaction's block write updates those three `consumed_*` columns in the
same per-block transaction. A single query on `lock_hash` filtered by
`consumed_by_tx_hash IS NULL` returns the live cell set for a lock.

**Partitioning.** Not yet in place. The plan is range partitioning on
`cells.block_number` so historical ranges can be detached / archived without
touching the live partition. This is scheduled for the production-readiness
milestone rather than retrofitted later.

## Query plane

Stateless Axum services read from PostgreSQL (with read replicas in
production) and a Redis cache.

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

Every response carries the indexer's tip height and the node's tip height so
clients can compute their own freshness. Cache entries in Redis are keyed by
query signature and invalidated on reorg via pub/sub from the ingestion
plane (see below). Pagination is cursor-based with opaque, base64-encoded
cursors so the on-the-wire format can evolve without breaking clients.

## Edge and control plane

Cloudflare terminates TLS and absorbs abuse. Authentication is API-key based;
keys are hashed with Argon2 and stored per-organization with a tier
(free / starter / pro). Rate limiting is a per-key token bucket in Redis,
with separate buckets for REST and GraphQL since the two surfaces have
different cost profiles. Exceeded limits return `429` with `Retry-After`.

## Reorg handling

Reorgs are treated as a first-class case, not an edge case. The indexer
validates the parent-hash chain on every block and, on a mismatch, walks
back to the divergence point, rolls back affected blocks in a single
database transaction, and re-indexes forward.

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

**Status.** This is the design. Reorg detection, rollback, and the
`reorg_log` audit table are scheduled for the observability/correctness
milestone, not yet in the current build. Until then the indexer assumes the
happy path, which is acceptable for the dev-node / staging environments the
service currently runs against but is the main item blocking production
traffic against mainnet.

## Consistency and correctness

- **Single-writer model** eliminates write-side concurrency. There is no
  scenario in which two processes race on the cell table.
- **One transaction per block** means the database never observes a partial
  block. Readers either see block N in full or they don't see it.
- **`indexer_state` is updated inside that same transaction**, so the
  recorded tip can never be ahead of the data.
- **Reorgs are transactional** — the rollback and the advancement of
  `indexer_state` to the common ancestor happen together.
- **Every record is reconstructable** from the node, so the recovery story
  for any corruption is "reindex."

## Tech stack

Rust throughout: Axum for HTTP, async-graphql for GraphQL, SQLx with
compile-time query checking for the database layer, `ckb-jsonrpc-types` +
`reqwest` for the RPC client. PostgreSQL is the store. Redis handles cache
and rate limiting. Cloudflare sits at the edge. Docker Compose for local
development, Kubernetes for production. Observability via OpenTelemetry
tracing and Prometheus metrics.

## Current state vs. roadmap

| Area                       | Status                               |
|----------------------------|--------------------------------------|
| Block/tx/cell ingestion    | Shipped                              |
| Live/dead cell accounting  | Shipped (via `consumed_*` columns)   |
| Indexer state + tip        | Shipped                              |
| Graceful shutdown          | Shipped                              |
| REST API                   | Next milestone                       |
| GraphQL + auth + rate limit| Milestone after                      |
| Reorg handling             | Designed; not yet implemented        |
| Partitioning on `cells`    | Deferred to production hardening     |
| Webhooks / subscriptions   | Planned                              |
| Grafana + OpenTelemetry    | Planned                              |

## Non-goals

- **Not a wallet backend.** No key management, no transaction construction.
  Read-only data layer.
- **Not a replacement for the node's built-in indexer** for local, single-
  user workloads. The value proposition is multi-tenant, SLO-backed, with
  a higher-level query surface — not raw speed on a laptop.
- **Not consensus-aware.** We trust the node we poll; we don't validate
  proofs ourselves.

## Open questions we'd value input on

1. Are there CKB RPC stability or compatibility quirks we should be planning
   around when a node is upgraded?
2. Well-known script tagging (Sighash, MultiSig, xUDT, Spore, RGB++ etc.) —
   is there a canonical source the ecosystem uses, or should we maintain our
   own registry?
3. Reorg depth in practice on mainnet — what window should we assume when
   sizing the rollback path?
