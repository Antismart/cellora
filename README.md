# Cellora

Production-grade, multi-tenant SaaS indexer for the [Nervos CKB](https://www.nervos.org) blockchain. Cellora exposes indexed on-chain data (blocks, transactions, cells) via REST and GraphQL APIs, so DApp teams can query CKB without running their own indexing infrastructure.

> **Status:** Week 3 of a 7-week build-out. Week 1 shipped block ingestion. Week 2 added the read-only REST API (health, blocks, cells, stats, OpenAPI spec) with cursor-based pagination and a tip cache. Week 3 lands API-key authentication (Argon2id-hashed bearer tokens), per-key Redis token-bucket rate limiting with separate REST and GraphQL surfaces, the GraphQL endpoint at `/graphql`, and an admin CLI for issuing keys. Reorg handling, observability, the dashboard, webhooks, and billing are in later weeks.

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

The API binds by default to `0.0.0.0:8080`. Health and the OpenAPI spec are public; everything else needs a Bearer token. Issue one via the admin CLI:

```bash
cargo run -p cellora-api -- admin create-key --tier free --label "local-dev"
# Record the printed `full` value — it is shown only once.
export CELLORA_API_KEY=cell_...
```

Then:

```bash
# Public — no auth needed.
curl -s http://localhost:8080/v1/health        | jq
curl -s http://localhost:8080/v1/health/ready  | jq

# Authenticated REST.
curl -s -H "authorization: Bearer $CELLORA_API_KEY" \
  http://localhost:8080/v1/blocks/latest | jq
curl -s -H "authorization: Bearer $CELLORA_API_KEY" \
  http://localhost:8080/v1/blocks/0 | jq
curl -s -H "authorization: Bearer $CELLORA_API_KEY" \
  "http://localhost:8080/v1/cells?lock_hash=0x$(printf 'aa%.0s' {1..32})" | jq
curl -s -H "authorization: Bearer $CELLORA_API_KEY" \
  http://localhost:8080/v1/stats | jq

# Authenticated GraphQL.
curl -s -X POST http://localhost:8080/graphql \
  -H "authorization: Bearer $CELLORA_API_KEY" \
  -H "content-type: application/json" \
  -d '{"query":"{ blocksLatest { number hash } stats { lagBlocks } }"}' | jq
```

See [`docs/api.md`](./docs/api.md) for every REST endpoint, the full GraphQL schema, auth and rate-limit semantics, and curl examples. The OpenAPI specification lives at [`docs/openapi.json`](./docs/openapi.json) and is also served at `/v1/openapi.json`.

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
| `CELLORA_API_AUTH_CACHE_TTL_SECONDS` | `60` | TTL of the in-process auth verification cache |
| `CELLORA_API_AUTH_CACHE_CAPACITY` | `10000` | Max entries in the auth verification cache |
| `CELLORA_REDIS_URL` | `redis://localhost:6379` | Redis used for the per-key rate limiter |
| `CELLORA_API_RATE_LIMIT_FAIL_OPEN` | `true` | Fail-open on Redis outage; set `false` to fail closed |
| `CELLORA_API_RATE_LIMIT_FREE_REST_BURST` | `30` | Free-tier REST bucket capacity |
| `CELLORA_API_RATE_LIMIT_FREE_REST_REFILL_PER_SEC` | `1` | Free-tier REST refill rate |
| `CELLORA_API_RATE_LIMIT_STARTER_REST_BURST` | `300` | Starter-tier REST bucket capacity |
| `CELLORA_API_RATE_LIMIT_STARTER_REST_REFILL_PER_SEC` | `20` | Starter-tier REST refill rate |
| `CELLORA_API_RATE_LIMIT_PRO_REST_BURST` | `3000` | Pro-tier REST bucket capacity |
| `CELLORA_API_RATE_LIMIT_PRO_REST_REFILL_PER_SEC` | `200` | Pro-tier REST refill rate |
| `CELLORA_API_RATE_LIMIT_FREE_GRAPHQL_BURST` | `10` | Free-tier GraphQL bucket capacity |
| `CELLORA_API_RATE_LIMIT_FREE_GRAPHQL_REFILL_PER_SEC` | `0.5` | Free-tier GraphQL refill rate |
| `CELLORA_API_RATE_LIMIT_STARTER_GRAPHQL_BURST` | `100` | Starter-tier GraphQL bucket capacity |
| `CELLORA_API_RATE_LIMIT_STARTER_GRAPHQL_REFILL_PER_SEC` | `10` | Starter-tier GraphQL refill rate |
| `CELLORA_API_RATE_LIMIT_PRO_GRAPHQL_BURST` | `1000` | Pro-tier GraphQL bucket capacity |
| `CELLORA_API_RATE_LIMIT_PRO_GRAPHQL_REFILL_PER_SEC` | `100` | Pro-tier GraphQL refill rate |

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
#    Postgres + Redis stack and drives it with tower::ServiceExt::oneshot
#    (no socket). Covers REST, GraphQL, auth, and rate limiting.
cargo test -p cellora-api

# Or run everything at once:
cargo test --workspace
```

There is also a load test against a running stack — see
[`tests/load/rate_limit.js`](./tests/load/rate_limit.js) for the k6
script and how to issue a key for it.

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
├── tests/
│   └── load/                       # k6 load tests against a running stack
└── docs/
    ├── architecture.md
    ├── architecture-overview.md
    ├── api.md
    ├── openapi.json
    └── decisions/
        └── 0001-crate-boundaries.md
```

## Roadmap

1. **Week 1** — workspace, docker-compose, ingestion pipeline.
2. **Week 2** — REST API + OpenAPI.
3. **Week 3** — API-key auth, Redis rate limiting, GraphQL ← *current*.
4. **Week 4** — reorg handling, Prometheus metrics, Grafana, OpenTelemetry.
5. **Week 5** — dashboard (React + Vite + Tailwind) with GitHub OAuth.
6. **Week 6** — webhooks and GraphQL subscriptions.
7. **Week 7** — Stripe billing, partitioning, Kubernetes deployment.

## License

Source-available under the [Functional Source License, Version 1.1, with Apache 2.0 future grant](./LICENSE.md) (**FSL-1.1-ALv2**).

In plain language:

- **Read, modify, self-host** — permitted for internal use, non-commercial research, and professional services to third parties.
- **Compete by offering Cellora-as-a-service** — not permitted while the license is in effect.
- Each release automatically converts to Apache-2.0 two years after it ships.

See [`LICENSE.md`](./LICENSE.md) for the full terms.
