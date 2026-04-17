# Architecture — Week 1

This document covers the system as it stands at the end of Week 1. Later weeks
will grow it, but the shape established here is intentionally minimal: one
service, one database, one external dependency (the CKB node).

## Components

```
            ┌───────────────────────────────────────────┐
            │              cellora-indexer              │
            │                                           │
 ┌────────┐ │  ┌─────────┐   ┌────────┐   ┌──────────┐  │  ┌────────────┐
 │ CKB    │◀─┼──│ poller  │──▶│ parser │──▶│   db    │──┼─▶│ PostgreSQL │
 │ node   │ │  └─────────┘   └────────┘   │  repos  │  │  └────────────┘
 └────────┘ │       ▲                     └──────────┘  │
            │       │  cancel token                     │
            │  ┌─────────┐                              │
            │  │shutdown │◀── SIGINT / SIGTERM          │
            │  └─────────┘                              │
            └───────────────────────────────────────────┘
```

### `crates/common`

Shared foundations used by every service, including ones that haven't been
written yet. Keeping this crate independent of `db` means later services (the
API gateway, webhook worker, etc.) can pick up the same configuration,
logging, and CKB client with zero refactor.

- `Config` — loaded from environment variables prefixed `CELLORA_`. `figment`
  does the heavy lifting so tests can inject alternative providers.
- `logging::init` — structured `tracing` subscriber. JSON format in production,
  pretty-printed for local development. Chooses via `CELLORA_LOG_FORMAT`.
- `CkbClient` — a thin `reqwest`-based wrapper around the CKB JSON-RPC API.
  Exposes `tip_block_number`, `get_block_by_number`, `chain_info`, plus a
  generic `call<P, R>` so we can add methods without touching the type in
  future weeks.
- `Error` — a `thiserror` enum that every RPC / config / logging failure maps
  into.

### `crates/db`

Compile-time-checked SQL behind a set of focused repository modules.

| Module | Responsibility |
| --- | --- |
| `pool` | Build a `PgPool` with production-sane defaults (16 connections, 5 s acquire timeout, pre-acquire ping). |
| `migrate` | Thin wrapper around `sqlx::migrate!` pointing at `./migrations`. |
| `models` | Row structs mirroring the schema one-to-one. `HashType` is a typed enum mapped to `SMALLINT`. |
| `blocks`, `transactions`, `cells`, `checkpoint` | One module per table, each exposing the narrow set of queries the indexer actually needs. |
| `error` | `DbError` wrapping `sqlx::Error` / migration errors. |

Every SQL statement uses `sqlx::query!` or `sqlx::query_as!`. No raw string
concatenation (CLAUDE rule 6), and the committed `.sqlx/` offline cache lets
CI build without a running database.

### `crates/indexer`

The long-running service binary.

- `main.rs` — loads config, initialises logging, opens the pool, runs pending
  migrations, builds the RPC client, spawns the shutdown listener, and hands
  control to the `Service`.
- `app::Service` — holds the pool, the RPC client, and the configuration.
  Today it delegates directly to `Poller`; in future weeks the same object
  will own the API server, metrics reporter, etc.
- `poller::Poller` — the ingestion loop. Reads the checkpoint to decide the
  next block number, asks the node for it, parses, writes the whole block in
  one database transaction, and advances the checkpoint. On transient error
  it backs off 1 → 2 → 4 → … capped at 30 s. A `CancellationToken` is checked
  between iterations, so shutdown is always clean at a block boundary.
- `parser::parse_block` — pure function converting a `BlockView` into
  `(BlockRow, Vec<TransactionRow>, Vec<CellRow>, Vec<ConsumedCellRef>)`. No
  I/O, so it is trivially unit-testable from JSON fixtures.
- `shutdown` — races `SIGINT` and `SIGTERM` against the cancel token and
  flips the token the first time either fires.

## Data model

Four tables; see `migrations/20260417000001_init.up.sql` for the full DDL.

- `blocks` — one row per canonical block. Primary key is `number`.
- `transactions` — one row per transaction. FK → `blocks.number` with
  `ON DELETE CASCADE` so reorg rollback (a Week 4 concern) can be a single
  statement.
- `cells` — one row per output, primary key `(tx_hash, output_index)`. Scripts
  are decomposed into columns (`lock_code_hash`, `lock_hash_type`, `lock_args`,
  `lock_hash`) so the Week 2 query endpoints (`GET /cells?lock_hash=…`) can
  be served by a cheap index lookup. `consumed_by_*` columns are `UPDATE`d
  when a later block spends the cell.
- `indexer_state` — a single-row checkpoint (`CHECK (id = 1)`) holding the
  last indexed block number and hash.

No separate `transaction_inputs` table this week: Week 1 only needs to mark
cells as consumed, which is captured in place on `cells`. If later weeks need
per-input witness data we revisit with an ADR.

## Invariants

- Every block is written in a single `BEGIN / COMMIT`, together with its
  transactions, new cells, consumed-cell updates, and the new checkpoint row.
  A crash mid-block leaves the database at the prior checkpoint — no half-
  indexed blocks.
- The checkpoint is the single source of truth for progress. Restart-safety:
  on boot, the poller reads `indexer_state.last_indexed_block` and resumes at
  `last_indexed_block + 1`. `CELLORA_INDEXER_START_BLOCK` only applies when
  the row is absent.
- Signal handling: the poller never interrupts a transaction. `Ctrl-C`
  triggers a graceful exit after the current block commits.

## What is intentionally not here yet

Everything in `CLAUDE.md` Week 2+ is deliberately out of scope for this drop:

- No HTTP/REST/GraphQL service and no OpenAPI. (Week 2.)
- No authentication, no rate limiting. (Week 3.)
- No reorg detection. The poller trusts the node's canonical chain. (Week 4.)
- No Prometheus metrics endpoint, no OpenTelemetry export. (Week 4.)
- Redis is included in `docker-compose.yml` but not consumed. (Week 3.)
- No Dockerfile for the indexer itself yet. Compose starts the dependencies;
  the indexer runs via `cargo run` locally. A containerized build arrives with
  the API in Week 2.

## Operational notes

### Dev CKB node

The dev node is brought up by `ops/ckb/entrypoint.sh`, which on first boot
runs `ckb init --chain dev`, then patches two things the canonical template
leaves unsuitable for docker compose:

1. Adds a `block_assembler` section so the dummy miner can produce blocks.
2. Points `ckb-miner.toml`'s `rpc_url` at the `cellora-ckb` compose service
   name (default is `127.0.0.1`, which is unreachable from a sibling
   container).

The healthcheck uses a bare TCP connect via `perl` because the upstream
image ships without `curl` / `wget` / `nc`.

### Performance budget

Current per-block write path is a single transaction doing: 1 block insert,
N transaction inserts, M cell inserts, K consumed-cell updates, 1 checkpoint
upsert. On the dev chain this runs in ~2 ms/block. Mainnet traffic pattern
(~50 txs/block, ~150 cells/block) should keep it under 50 ms/block in a
simple benchmark — well within the 1 block/second block rate. Bulk inserts
via `UNNEST` are a deliberate Week-4 optimisation; we need measurements, not
guesses, before picking that complexity up.
