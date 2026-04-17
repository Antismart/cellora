# CLAUDE.md — CKB Indexer Service

This file gives Claude Code the context and weekly plan for building this project. **Do not attempt to build everything at once.** Work only on the current week's scope. Each week has a clear, shippable deliverable.

---

## Project context

We are building a production-grade, multi-tenant SaaS platform for indexing and querying CKB (Nervos) on-chain data. The goal is a commercial product that other CKB developers and DApp teams will pay to use.

**This is not a toy project.** The codebase must be production-quality from day one — tests, error handling, observability, documentation. No shortcuts, no TODO comments left in the code.

---

## Tech stack (locked in, do not change)

| Layer | Technology |
|-------|-----------|
| Language | Rust (stable) |
| HTTP framework | Axum |
| GraphQL | async-graphql |
| Database | PostgreSQL |
| Database driver | SQLx (with compile-time query checking) |
| Cache / rate limiting | Redis |
| CKB integration | `ckb-jsonrpc-types` + `reqwest` |
| Tracing | `tracing` + `tracing-subscriber` |
| Metrics | `prometheus` crate |
| Config | `figment` or `config` crate |
| Deployment | Docker + docker-compose (local), Kubernetes (production) |

---

## Project structure (the target)

Follow this structure as the codebase grows. Do not create files that aren't needed yet — add them as the weekly scope requires.

```
ckb-indexer/
├── Cargo.toml                    # Workspace root
├── crates/
│   ├── indexer/                  # Indexer service (block polling, parsing)
│   ├── api/                      # API gateway (REST + GraphQL)
│   ├── db/                       # Shared database models and queries
│   └── common/                   # Shared types, errors, config
├── migrations/                   # SQL migrations (managed by sqlx-cli)
├── docker-compose.yml            # Local dev stack
├── Dockerfile                    # Multi-stage production build
├── .env.example                  # Documented env vars
├── README.md
└── docs/
    ├── architecture.md
    ├── api.md
    └── deployment.md
```

---

## Coding standards (enforced every week)

1. **No `unwrap()` or `expect()` in non-test code.** Always return `Result` and handle errors with `thiserror`-derived error types.
2. **Every public function has a rustdoc comment** explaining what it does, its parameters, and what errors it can return.
3. **Every module has integration tests.** Tests run against a real PostgreSQL and CKB dev node via docker-compose.
4. **No hardcoded secrets or URLs.** Everything configurable goes in environment variables loaded via `figment`.
5. **Structured logging only.** Use `tracing` macros with structured fields (`tracing::info!(block = %n, "indexed block")`). Never `println!`.
6. **SQL queries use `sqlx::query!` or `sqlx::query_as!`** for compile-time checking. No raw string concatenation.
7. **Commits follow conventional commits** (`feat:`, `fix:`, `chore:`, `docs:`, `test:`, `refactor:`).
8. **Every PR has a summary of what changed, why, and how to test it.**

---

## Weekly roadmap

Work ONLY on the current week. Do not skip ahead. Each week ends with a working, tested, deployable artifact.

---

### Week 1 — Foundation and block ingestion

**Goal:** Get a Rust service polling a CKB node and writing block and cell data into PostgreSQL.

**Scope:**
- Set up Cargo workspace with `indexer`, `db`, and `common` crates
- Set up `docker-compose.yml` with PostgreSQL, Redis, and a CKB dev node
- Create initial migrations for `blocks`, `cells`, `transactions` tables (simple schema first — partitioning comes later)
- Implement CKB JSON-RPC client wrapper in `common` that connects to the CKB node
- Implement block poller in `indexer`:
    - Polls the CKB node for new blocks every 2 seconds
    - Tracks the last indexed block number
    - Parses block data into cells and transactions
    - Writes to PostgreSQL in a single transaction per block
- Handle graceful shutdown with `tokio::signal`
- Structured logging with `tracing`

**Explicitly out of scope this week:**
- No API layer yet
- No reorg handling (assume happy path)
- No partitioning
- No Redis
- No authentication
- No GraphQL

**Deliverables:**
- `cargo run -p indexer` successfully indexes blocks from the dev node into PostgreSQL
- Integration test that spins up the stack via docker-compose and verifies blocks are indexed
- README with setup instructions
- `docs/architecture.md` with Week 1 scope documented

---

### Week 2 — REST API

**Goal:** Expose the indexed data via a REST API with proper error handling and pagination.

**Scope:**
- Create the `api` crate with Axum
- Implement endpoints:
    - `GET /v1/health` — liveness and readiness probes
    - `GET /v1/blocks/latest` — latest indexed block
    - `GET /v1/blocks/:number` — block by number
    - `GET /v1/cells?lock_hash=...` — query cells by lock hash (paginated)
    - `GET /v1/cells?type_hash=...` — query cells by type hash (paginated)
    - `GET /v1/stats` — indexer stats (indexed height, lag from node tip)
- Cursor-based pagination (opaque cursors, base64-encoded)
- Consistent JSON error response format
- Request tracing with `tower-http::trace`
- OpenAPI spec generation

**Explicitly out of scope this week:**
- No GraphQL yet
- No authentication
- No rate limiting
- No Redis caching

**Deliverables:**
- All endpoints working with integration tests
- OpenAPI spec generated and committed
- `docs/api.md` with curl examples for each endpoint
- README updated with API usage

---

### Week 3 — Authentication, rate limiting, and GraphQL

**Goal:** Add API key authentication, rate limiting via Redis, and a GraphQL endpoint.

**Scope:**
- Add `api_keys` table with Argon2-hashed keys, tier (free/starter/pro), and rate limits
- Middleware for API key validation on all `/v1/*` routes
- Redis-backed token bucket rate limiter per API key
- Return `429 Too Many Requests` with `Retry-After` header when exceeded
- Add GraphQL endpoint at `/graphql` using `async-graphql`
- GraphQL schema exposes `Block`, `Transaction`, `Cell`, `Script` types
- GraphQL queries mirror the REST endpoints
- Admin CLI subcommand to generate new API keys (`cargo run -p api -- admin create-key --tier free`)

**Explicitly out of scope this week:**
- No GraphQL subscriptions yet
- No billing / usage tracking yet
- No webhooks

**Deliverables:**
- REST and GraphQL both functional and authenticated
- Integration tests covering auth and rate limiting
- `docs/api.md` expanded with GraphQL examples
- Load test script in `tests/load/` showing rate limiter behaviour

---

### Week 4 — Reorg handling and observability

**Goal:** Make the indexer correct under chain reorgs and fully observable in production.

**Scope:**
- Implement reorg detection by walking parent hashes on each new block
- On reorg: roll back affected blocks, cells, and transactions in a single database transaction
- Add `reorg_log` table recording reorg events for audit
- Prometheus metrics endpoint at `/metrics`:
    - `indexer_latest_block`
    - `indexer_lag_blocks`
    - `api_requests_total` (by endpoint, status)
    - `api_request_duration_seconds` (histogram)
    - `db_pool_connections_active`
- OpenTelemetry tracing exported to Jaeger (configurable)
- Health checks that verify:
    - Database connectivity
    - Redis connectivity
    - CKB node reachable and synced
- Dashboard JSON for Grafana committed to `ops/dashboards/`

**Explicitly out of scope this week:**
- No billing yet
- No webhooks yet

**Deliverables:**
- Reorg test suite that simulates a chain reorg and verifies correct rollback
- Grafana dashboard importable from JSON
- All metrics documented in `docs/observability.md`

---

### Week 5 — Dashboard and developer experience

**Goal:** Build a simple web dashboard where users can create API keys, see usage and test queries.

**Scope:**
- Separate `dashboard/` directory with a React + Vite frontend (Tailwind for styling)
- Auth via OAuth (GitHub login for MVP)
- Pages:
    - Sign in / sign up
    - API key management (create, rotate, revoke)
    - Usage charts (requests over time, per endpoint)
    - API explorer (embedded Swagger UI for REST, GraphiQL for GraphQL)
    - Current indexer status (latest block, lag)
- API endpoints to support dashboard (`/admin/*`, authenticated via session cookie)

**Explicitly out of scope this week:**
- No Stripe integration yet
- No webhooks yet

**Deliverables:**
- Dashboard deployable via docker-compose
- Integration test that logs in, creates a key, makes a request, sees usage
- `docs/dashboard.md` with screenshots and setup

---

### Week 6 — Webhooks and subscriptions

**Goal:** Let users subscribe to on-chain events and receive webhook deliveries.

**Scope:**
- `webhooks` table with URL, secret, filters (lock hash, type hash), status
- Webhook delivery worker with exponential backoff and retry queue
- HMAC signature on every webhook payload
- Delivery log visible in dashboard
- GraphQL subscriptions over WebSocket for live cell updates

**Deliverables:**
- End-to-end test: create webhook, index a matching cell, verify delivery
- `docs/webhooks.md` with payload examples and signature verification guide

---

### Week 7 — Billing and production readiness

**Goal:** Stripe integration for paid plans, production deployment hardening.

**Scope:**
- Stripe Checkout integration for Starter and Pro plans
- Usage-based billing based on `usage_events` aggregation
- Webhook from Stripe to update organisation plan tier
- Database partitioning on `cells` table by block range
- Read replica support in the query service
- Terraform or Helm charts for Kubernetes deployment
- Runbook for common production incidents

**Deliverables:**
- Staging environment deployed on Kubernetes
- `docs/deployment.md` with full production deploy guide
- `ops/runbook.md` with incident response procedures

---

## Rules for working week-to-week

1. **Never work ahead.** If a feature is listed in a later week, do not add it now, even if it seems easy.
2. **Every week ends green.** Tests passing, CI green, docker-compose working, README up to date. No half-finished features carried over.
3. **If scope changes mid-week, update this file first.** Commit the change to CLAUDE.md before writing code.
4. **Document decisions.** If you deviate from the plan or choose between options, write it in `docs/decisions/NNN-title.md` as an ADR (Architecture Decision Record).
5. **Ask before adding dependencies.** New crates must be justified in the PR description. Prefer the standard library and existing dependencies.

---

## Current week

**Week 1 — Foundation and block ingestion**

Start here. Do not proceed to Week 2 until Week 1 is fully shipped, tested, and documented.