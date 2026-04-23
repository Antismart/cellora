# Cellora

Production-grade, multi-tenant SaaS indexer for the [Nervos CKB](https://www.nervos.org) blockchain. Cellora exposes indexed on-chain data (blocks, transactions, cells) via REST and GraphQL APIs, so DApp teams can query CKB without running their own indexing infrastructure.

> **Status:** Week 2 of a 7-week build-out. Week 1 shipped the Cargo workspace, the docker-compose dev stack, the initial schema, and a block-ingestion service that polls a CKB node and writes blocks, transactions, and cells to Postgres. Week 2 introduces the REST API crate; today it serves health and blocks endpoints, with cells, stats, and an OpenAPI spec landing across the remaining Week 2 slices. GraphQL, authentication, webhooks, billing, and the dashboard are in later weeks.

## Architecture at a glance

```
┌───────────┐   poll    ┌───────────────┐   tx commit   ┌────────────┐   read   ┌────────┐
│  CKB node │ ───────── │  indexer svc  │ ────────────▶ │ PostgreSQL │ ◀─────── │ api    │ ◀── HTTP
└───────────┘           └───────────────┘               └────────────┘          │ svc    │
                              │                                                 └────────┘
                              └── graceful shutdown on SIGINT / SIGTERM
```

- **`crates/common`** — configuration, structured logging, CKB JSON-RPC client.
- **`crates/db`** — SQLx-backed repositories for blocks, transactions, cells, and the indexer checkpoint.
- **`crates/indexer`** — the service binary that runs the poll loop and writes to Postgres.
- **`crates/api`** — the REST service binary. Reads from the same Postgres and serves clients over HTTP.

See [`docs/architecture.md`](./docs/architecture.md) for the Week 1 walkthrough and [`docs/architecture-overview.md`](./docs/architecture-overview.md) for the end-state design.

## Requirements

- Rust **stable** (pinned via `rust-toolchain.toml`).
- Docker + Docker Compose (v2).
- `sqlx-cli` — installed automatically by `scripts/dev-up.sh` if missing.

## Quickstart

```bash
# 1. configure
cp .env.example .env

# 2. start the stack (Postgres, Redis, CKB dev node, CKB miner)
#    Redis is reserved for week 3 — it is not used by the Week 1 indexer.
scripts/dev-up.sh

# 3. run the indexer
cargo run -p cellora-indexer

# 4. in a second terminal, run the API
cargo run -p cellora-api
```

The indexer emits structured logs as it pulls blocks from the dev node:

```
INFO cellora_indexer::poller: indexed block block=0 hash=… txs=2 cells=11 consumed=1 elapsed_ms=53
INFO cellora_indexer::poller: indexed block block=1 hash=… txs=1 cells=0 consumed=0 elapsed_ms=2
```

The API binds by default to `0.0.0.0:8080`. Once it is running:

```bash
curl -s http://localhost:8080/v1/health        | jq
curl -s http://localhost:8080/v1/health/ready  | jq
curl -s http://localhost:8080/v1/blocks/latest | jq
curl -s http://localhost:8080/v1/blocks/0      | jq
```

`Ctrl-C` triggers graceful shutdown on either binary. The indexer finishes any in-flight block, advances the checkpoint, and exits zero; the API drains in-flight requests before closing the listener.

### Verifying what landed

```bash
docker exec -i cellora-postgres psql -U cellora -d cellora -c \
    "SELECT (SELECT count(*) FROM blocks)       AS blocks,
            (SELECT count(*) FROM transactions) AS txs,
            (SELECT count(*) FROM cells)        AS cells,
            (SELECT last_indexed_block FROM indexer_state) AS checkpoint;"
```

## Configuration

Every setting is environment-driven (figment loads from `.env` in dev and real env vars in production). See [`.env.example`](./.env.example) for the full list. The important ones:

| Variable | Default | Meaning |
|---|---|---|
| `CELLORA_DATABASE_URL` | `postgres://cellora:cellora@localhost:5432/cellora` | Postgres connection string |
| `CELLORA_CKB_RPC_URL` | `http://localhost:8114` | CKB JSON-RPC endpoint |
| `CELLORA_POLL_INTERVAL_MS` | `2000` | Delay between polls when caught up |
| `CELLORA_INDEXER_START_BLOCK` | `0` | Block to start indexing from on a fresh DB |
| `CELLORA_LOG_LEVEL` | `info` | `tracing` `EnvFilter` expression |
| `CELLORA_LOG_FORMAT` | `json` | `json` (prod) or `pretty` (local) |
| `CELLORA_API_BIND_ADDR` | `0.0.0.0:8080` | Socket the API binary binds to |
| `CELLORA_API_DEFAULT_PAGE_SIZE` | `50` | Page size applied when a request omits `limit` |
| `CELLORA_API_MAX_PAGE_SIZE` | `500` | Upper bound on `limit` accepted from clients |
| `CELLORA_API_REQUEST_TIMEOUT_MS` | `10000` | Per-request timeout enforced by the middleware stack |
| `CELLORA_API_TIP_CACHE_REFRESH_MS` | `1000` | Refresh interval for the cached `(indexer_tip, node_tip)` snapshot |

## Running the tests

The test suite has four layers, each runnable on its own:

```bash
# 1. Pure parser unit tests — no containers, fast, CI-safe.
cargo test -p cellora-indexer --test parser_test

# 2. DB integration — spins up Postgres via testcontainers (requires docker).
cargo test -p cellora-db --test db_integration

# 3. Full-stack end-to-end for the indexer — wiremock stands in for the CKB
#    node while the real poller writes into a testcontainers Postgres.
cargo test -p cellora-indexer --test indexer_stack_test

# 4. API end-to-end — builds the full Axum router against a testcontainers
#    Postgres and drives it with tower::ServiceExt::oneshot (no socket).
cargo test -p cellora-api

# Or run everything at once:
cargo test --workspace
```

## Development workflow

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo sqlx prepare --workspace   # regenerate .sqlx/ after changing SQL
```

The committed `.sqlx/` offline cache lets CI build without a live Postgres (`SQLX_OFFLINE=true`). After any change to a `sqlx::query!` call site or a migration, run `cargo sqlx prepare --workspace` and commit the refreshed cache.

## Repository layout

```
cellora/
├── Cargo.toml                      # workspace root
├── rust-toolchain.toml
├── docker-compose.yml
├── migrations/                     # SQL migrations (sqlx)
├── ops/ckb/                        # CKB dev-node boot scripts
├── scripts/                        # dev-up, test-integration
├── crates/
│   ├── common/                     # config, logging, CKB client
│   ├── db/                         # schema-aware repositories
│   ├── indexer/                    # block poller binary
│   └── api/                        # REST API binary
└── docs/
    ├── architecture.md
    ├── architecture-overview.md
    └── decisions/
        └── 0001-crate-boundaries.md
```

## Roadmap

1. **Week 1** — workspace, docker-compose, ingestion pipeline.
2. **Week 2** — REST API + OpenAPI ← *current*.
3. **Week 3** — API-key auth, Redis rate limiting, GraphQL.
4. **Week 4** — reorg handling, Prometheus metrics, Grafana, OpenTelemetry.
5. **Week 5** — dashboard (React + Vite + Tailwind) with GitHub OAuth.
6. **Week 6** — webhooks and GraphQL subscriptions.
7. **Week 7** — Stripe billing, partitioning, Kubernetes deployment.

## License

Apache-2.0.
