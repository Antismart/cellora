# ADR 0001 — Crate boundaries for the Week 1 workspace

- **Status:** Accepted
- **Date:** 2026-04-17
- **Context:** Week 1 scaffold (see `CLAUDE.md`).

## Context

We're standing up a Cargo workspace that will grow to cover an indexer, a
REST + GraphQL API, authentication, billing, and eventually a worker pool for
webhook delivery. The question at Week 1 is how many crates to create and
where the seams should sit, knowing that we're only implementing the indexer
this week but want later weeks to slot in without refactor.

Two extremes were considered:

1. **Single binary crate.** Put everything in `cellora-indexer`, split later.
   Fastest to ship Week 1; costly when we add the API crate in Week 2 because
   we'd have to disentangle config, logging, DB access, and CKB plumbing.

2. **One crate per module** (e.g. a separate `cellora-logging`,
   `cellora-config`, `cellora-ckb-client`, `cellora-blocks`,
   `cellora-transactions`, …). Over-engineered. Adds ceremony — a dozen
   `Cargo.toml` files, a dozen `pub use` shims — with no payoff until we have
   a reason to ship any of those pieces independently (we don't).

We picked a three-crate middle ground:

- `crates/common` — configuration, logging, errors, CKB JSON-RPC client.
- `crates/db` — schema-aware repositories and migration runner.
- `crates/indexer` — the block poller binary plus a small lib for tests.

## Decision

Adopt the three-crate split above for the duration of the project. When the
API lands in Week 2 it becomes a fourth crate, `crates/api`, depending on
`common` and `db`. No service-specific types belong in `common` or `db`; they
belong in the service crate that owns them.

## Consequences

- Week 1 shipped with a workspace that Week 2's `api` crate can adopt
  unchanged — `common::Config`, `common::CkbClient`, and every `db::*`
  repository work identically in an Axum handler as they do in the poller.
- `common` stays dependency-light: no `sqlx`, no async-graphql, no Axum. That
  keeps its compile time low and its API surface focused. Anything touching
  the database goes in `db`.
- The `indexer` crate has both a `[lib]` and a `[[bin]]` target. The lib
  exposes `parser`, `poller`, `app`, `shutdown` so integration tests can
  construct a `Poller` directly against a testcontainers Postgres and a
  `wiremock`-backed CKB endpoint. The bin's `main.rs` is a thin composition
  layer on top.

### Schema side-decision: decomposed script columns, not JSONB

The `cells` table stores lock / type scripts as a fixed set of columns
(`lock_code_hash`, `lock_hash_type`, `lock_args`, `lock_hash`, and the
matching `type_*` trio) rather than serialising each script into a JSONB
blob.

Reason: the Week 2 query endpoints (`GET /cells?lock_hash=…` and
`GET /cells?type_hash=…`) need to be served in milliseconds. A decomposed
layout gives us a narrow BTREE index on `lock_hash` / `type_hash` that maps
straight into the query. A JSONB layout would force either a GIN index per
field or an application-side recomputation of the script hash on read, both
more expensive than the ~80 bytes/cell of column overhead we pay instead.

Re-evaluation trigger: if a later week needs to persist a full Molecule
encoding of the output cell (e.g. to support re-broadcasting), we add a
single `packed` BYTEA column rather than migrating to JSONB.

## Alternatives considered and rejected

- **Single binary crate.** Noted above; rejected because the Week 2 API
  split would force a mid-project workspace refactor.
- **One crate per table.** Noted above; no independent deployability story
  justifies the ceremony.
- **`common` re-exports `sqlx`.** Tempting for ergonomics but drags every
  consumer into a DB dependency graph. Rejected.
- **JSONB-everything schema.** Simpler DDL but slower queries and harder to
  reason about at the SQL level. Rejected on performance grounds.
