# Cellora — designing a production indexing and query service for CKB (feedback welcome)

Hello Nervos community,

I'm building **Cellora**, a multi-tenant indexing and query layer for CKB, and I'd value input from this community before the API surface and reorg handling land. The project is in active development — ingestion is shipped, the query surface is next — and I'd rather bake your feedback into the design than retrofit it later.

Repo: https://github.com/Antismart/cellora

## The problem

Anyone shipping a serious CKB application runs into the same fork in the road: query patterns that a full node alone can't answer efficiently. Live cells by lock, cells by type, transaction history for an address, outpoint resolution, balance aggregation — these need a normalized store with indexes tuned for those access paths.

The options today are:

- **Run the built-in `ckb-indexer`.** Excellent for local, single-user workloads. Not designed as a multi-tenant data layer.
- **Run Mercury.** More ambitious in scope, but operating it in production is still every team's problem.
- **Roll your own indexer.** Every app ends up reimplementing a variant of the same indexer, database schema and ops story.
- **Lean on public RPC.** Rate-limited, no query-layer features, no SLA.

The result is that every CKB team pays the "indexer tax" independently. Cellora's bet is that a managed, multi-tenant indexing service with strong operational properties (reorg safety, observability, SLOs) can be the shared infrastructure that lets DApp teams focus on their product.

## What Cellora is

A managed data layer for CKB. It normalizes blocks, transactions and cells into PostgreSQL with indexes tuned for the common access patterns (by lock hash, type hash, outpoint), fronts them with REST and GraphQL behind API-key auth and per-key rate limiting, and treats reorgs as a first-class case rather than an edge case.

## The shape

Three planes that scale independently:

**Ingestion plane.** A single-writer indexer tails the chain, parses blocks into normalized records, and writes them transactionally to PostgreSQL. It is the only component with write access. It owns reorg handling: on a parent-hash mismatch it walks back to the common ancestor, rolls back the affected blocks, cells and transactions in one DB transaction, and resumes forward.

**Query plane.** Stateless Rust services behind Axum exposing REST and GraphQL. They read from PostgreSQL replicas and a Redis cache. Every response carries the indexer's tip height so clients can compute their own freshness.

**Edge and control plane.** TLS termination at the edge, Argon2-hashed API keys, and a per-key Redis token bucket for rate limiting with separate buckets for REST and GraphQL.

The load-bearing decision is separating ingestion from query. It keeps the write path serial and easy to reason about while letting the read path scale horizontally on request volume.

Full architecture, schema and reorg algorithm: https://github.com/Antismart/cellora/blob/main/docs/architecture-overview.md

## Why CKB shapes the design

A few places where CKB's primitives drove specific choices:

**1. Cells as first-class rows, not events.** CKB's cell model maps naturally onto a row-per-cell table. Live/dead state isn't derived — it's a column (`consumed_by_tx_hash IS NULL`). A single indexed query on `lock_hash` filtered by that predicate returns the live set for a lock. No event log replay, no materialized view rebuild.

**2. Script components stored raw *and* as precomputed hash.** A CKB script has three parts: `code_hash`, `hash_type`, `args`. Each cell stores all three raw alongside the precomputed `lock_hash` / `type_hash`. Exact-hash lookups are O(1); pattern matching on `args` prefixes (for xUDT owner filtering and similar) is possible without a script-specific schema.

**3. Reorgs as a first-class case.** The write path is built around transactional rollback from the start. On a parent-hash mismatch, the indexer walks back to the common ancestor and deletes blocks `(A, tip]` in a single DB transaction — `ON DELETE CASCADE` removes associated transactions and cells, and cells consumed in rolled-back blocks have their `consumed_*` columns reset to `NULL`. `indexer_state` advances to the ancestor in the same transaction.

**4. Single-writer ingestion matches chain-tip semantics.** Blocks arrive in canonical order; one tip means one writer. Single-writer eliminates cross-process locking on the cells table and makes the reorg algorithm trivially correct — there's no concurrent writer that might observe a half-rolled-back state.

## Current status

Block, transaction and cell ingestion is shipped. Live/dead cell accounting, one-transaction-per-block write semantics, and indexer-state tracking for tip recovery are working end-to-end against a dev chain.

What's working:

- Block polling loop (`get_tip_block_number`, `get_block_by_number`, `get_blockchain_info`) at a configurable 2 s cadence
- Parser and writer that normalize blocks → transactions → cells
- Live/dead cell accounting via `consumed_by_tx_hash`, `consumed_by_input_index`, `consumed_at_block_number`
- One PostgreSQL transaction per block (indexer state advances inside the same transaction)
- Partial indexes on `type_hash` (nullable) and `consumed_by_tx_hash` (live cells stay out)
- Graceful shutdown on `SIGINT` / `SIGTERM`
- Integration test harness that spins the full stack via docker-compose

What's not yet: REST and GraphQL surfaces, API-key auth, Redis-backed rate limiting, reorg handling, webhooks, partitioning, and the observability stack.

## Architecture

```
cellora/
├── Cargo.toml              # workspace root
├── crates/
│   ├── common/             # config, CKB RPC client, shared types
│   ├── db/                 # SQLx models, queries, migrations glue
│   └── indexer/            # poller, parser, writer, main
├── migrations/             # sqlx-cli managed SQL migrations
├── docker-compose.yml      # Postgres + Redis + CKB dev node
├── docs/
│   ├── architecture.md
│   ├── architecture-overview.md
│   └── decisions/          # ADRs
└── README.md
```

## Tech stack

| Layer | Technology |
|---|---|
| Language | Rust (stable) |
| CKB RPC client | `ckb-jsonrpc-types` + `reqwest` |
| Database | PostgreSQL + SQLx (compile-time query checking) |
| HTTP (planned) | Axum |
| GraphQL (planned) | async-graphql |
| Cache and rate limiting (planned) | Redis |
| Observability (planned) | OpenTelemetry + Prometheus |
| Deployment | Docker Compose (local), Kubernetes (production) |

## Key design decisions

A few non-obvious choices worth calling out, because this is where I most want pushback:

**Polling over subscriptions.** The indexer polls on a 2 s cadence rather than consuming the JSON-RPC subscription APIs. Polling recovers from transient connection loss without special cases and keeps the failure modes tiny — the cost is a few seconds of indexing lag, which is below the noise floor for a data layer. If CKB node subscriptions are stable enough in practice to be worth it, I'll revisit.

**One transaction per block, always.** The block row, its transactions, its output cells, the updates marking consumed inputs, and the advancement of `indexer_state` all commit together. Readers never see a partial block, and the recorded tip can never be ahead of the data. The tradeoff is that a very large block is one large transaction — acceptable for CKB's block sizes but something to watch.

**Database is a cache, not a ledger.** Every record in PostgreSQL is reconstructable from the node. Recovery from any corruption is "reindex from a known good height." This keeps schema evolution cheap — no migration needs to preserve data, because it can always be rebuilt. The tradeoff is initial backfill time on mainnet, which we'll mitigate with snapshots.

**Script representation stores components and hash.** Each cell row carries `lock_code_hash`, `lock_hash_type`, `lock_args`, `lock_hash` (and the same four for the type script). Exact lookups hit the hash; pattern matching works on the raw components. No script-specific schema needed in the base layer, and well-known script tagging can be layered on top as enrichment.

## What's next

The critical next milestone is the **REST and GraphQL query surface plus reorg handling**. Ingestion without a query surface is a tree falling in the forest, and ingestion without reorg safety isn't production-ready against mainnet. These two ship together because reorg events need somewhere to be published for the query layer to invalidate caches.

After that, in order:

- API-key auth (Argon2) + Redis-backed per-key rate limiting, separate buckets for REST and GraphQL
- Webhook delivery with HMAC signatures and exponential-backoff retries
- Range partitioning on the `cells` table by block number
- OpenTelemetry tracing + Prometheus metrics + Grafana dashboards
- Dashboard for API-key management, usage charts, and a query explorer
- GraphQL subscriptions over WebSocket for live cell updates

## Where I'd value input

The questions where I most want community input, design-leaning first, then product-leaning:

1. **Cell model fidelity.** Is storing script components raw + precomputed hash enough, or are there query patterns (partial `args` matching, script-class tagging) that should be first-class in the schema?

2. **Reorg depth in practice.** What rollback window should I size for on mainnet? Anecdotes of observed depths, even rough ones, would help.

3. **Polling vs subscriptions.** Are there stability or compatibility concerns with the CKB subscription endpoints the community has hit?

4. **Well-known script registry.** Tagging common scripts (Sighash, MultiSig, Omnilock, xUDT, Spore, RGB++, Nostr binding) is out of scope for the base schema. Is there a canonical ecosystem registry, or is everyone maintaining their own?

5. **Product fit.** For anyone shipping a CKB app today: how are you accessing chain data (own node, built-in indexer, Mercury, public RPC, third-party), and what breaks most often? Which query patterns and which interface (REST, GraphQL, typed SDK, subscriptions) do you reach for first?

If you'd rather leave structured feedback, there's also a short survey: https://forms.gle/efARGooCSnwwf2KPA

Happy to jump on a short call with anyone whose app has non-trivial indexing needs — concrete pain points are what I most want to hear. Direct and critical feedback is welcome; I want to get this right before mainnet.

Thanks for reading.

— Antismart
