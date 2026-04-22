# CKB Indexer — Architecture

## Problem
Running a CKB node and indexer is a hard prerequisite for any serious CKB application. Every team pays this cost independently. The existing alternatives (Mercury, public RPC providers) are either abandoned or too low-level to be useful as a data layer.

## Shape of the system
The system is split into three independently scaling components sitting behind a CKB full node:

- **Ingestion plane** — a single-writer indexer that tails the chain, parses blocks into normalized cell and transaction records, and writes them transactionally to PostgreSQL. It is the only component with write access to the database. It owns reorg handling: on a parent-hash mismatch it rolls back affected blocks and re-indexes forward from the divergence point.

- **Query plane** — stateless Rust services behind Axum exposing REST and GraphQL. They read from PostgreSQL replicas and a Redis cache. Every response carries the indexer's tip height so clients know their data freshness. The query plane scales horizontally on request volume and is fully decoupled from ingestion.

- **Edge and control plane** — Cloudflare terminates TLS and absorbs abuse. Authentication is API-key based with Argon2 hashes. Rate limiting is a per-key token bucket in Redis, separate buckets for REST and GraphQL.

## Data model
PostgreSQL is the source of truth. Cells, transactions and blocks are stored in tables partitioned by block-number range so historical data can be pruned or archived without touching the hot path. Indexes are tuned for the three dominant query patterns: by lock hash, by type hash and by outpoint. Every record is reconstructable from the chain — the database is a cache, not a ledger.

## Consistency and correctness
The indexer is the only writer, which eliminates a class of concurrency problems. Each block is committed in a single transaction along with the updates to the `is_live` flags on consumed cells. Reorgs are treated as a first-class case, not an edge case. The indexer tracks the canonical chain by walking parent hashes and emits a reorg event to Redis pub/sub so downstream consumers can invalidate caches and notify webhook subscribers.

## Tech stack
Rust throughout (Axum, async-graphql, SQLx), PostgreSQL, Redis, CKB full nodes, Cloudflare at the edge, Docker and Kubernetes for deployment. Observability via OpenTelemetry tracing and Prometheus metrics.

## Why this shape
Separating ingestion from query is the most important decision in the system. It lets the read path scale to thousands of requests per second on commodity infrastructure while keeping the write path simple, serial and easy to reason about. Every other design choice — partitioning, the single-writer model, the explicit reorg handling — follows from treating this as a durable data layer rather than a thin cache over the node.
