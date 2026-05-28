# ADR 0006 — Week 4 reorg handling, observability, and verifiable-data foundations

- **Status:** Accepted
- **Date:** 2026-04-28
- **Context:** Week 4 of the roadmap. Two main themes: making the
  indexer correct under chain reorgs, and adding production-grade
  observability (Prometheus, OpenTelemetry, expanded readiness probes,
  Grafana dashboards). Plus two scope additions from
  [ADR 0004](./0004-trust-model-and-verifiable-responses.md) and
  [ADR 0005](./0005-well-known-script-registry.md): a proof passthrough
  endpoint and a well-known script registry.

## Decisions

### Reorg detection and rollback

On every new block `N` from `get_block_by_number`, the poller verifies
`parent_hash(N)` against the stored hash for `N-1`. On mismatch, it
walks back fetching previous heights from the node, comparing each
candidate's hash against the stored hash for that height, until the
common ancestor `A` is found.

The rollback runs inside one PostgreSQL transaction. It writes a
`reorg_log` row in `in_progress`, deletes blocks `(A, tip]` (`ON DELETE
CASCADE` removes transactions and cells), resets `consumed_*` columns
on cells consumed in rolled-back blocks, advances `indexer_state` to
`A`, and marks the `reorg_log` row `completed`. A reorg event is
published on Redis pub/sub channel `cellora:reorg` so the query plane
can invalidate caches and (later) webhook subscribers can react.

### Reorg sizing

Default rollback target: 12 blocks. Upper plumbing bound: env
configurable, default 100. A reorg deeper than the upper bound logs at
ERROR, increments `reorg_oversized_total`, and still completes — the
alternative (failing closed) leaves the database in a permanently
wrong state, which is worse than a noisy recovery.

### `reorg_log` table

Single table with a row per detected reorg: `detected_at`,
`divergence_block_number`, the canonical and indexed hashes at that
height, `depth`, `completed_at`, `status` enum, optional `error`. No
retention policy in this milestone — the table is bounded by chain
age in practice, and a truncation cron can land alongside partitioning
in Week 7.

### Metrics

The `prometheus` crate, one `Registry` per binary. Indexer and API each
expose `/metrics` text-format. Indexer metrics cover tip, lag, indexing
duration, and reorg counters/histograms. API metrics cover per-endpoint
request counts and durations, rate-limit decisions, and DB pool stats.
The endpoint is **public** (Prometheus convention); operators are
expected to IP-restrict it at the edge.

### OpenTelemetry tracing

Optional, env-gated. Setting `CELLORA_OTEL_OTLP_ENDPOINT` enables an
OTLP exporter; when unset, the existing `tracing-subscriber` formatter
runs alone. Sampler is parent-based with a configurable ratio (default
0.1). Errors and slow spans are sampled at 1.0 regardless of the
ratio. Service name auto-detected from the binary.

### Health checks

`/v1/health/ready` expands to probe Redis and the CKB node alongside
the database. Each probe has a 1-second timeout. Response body shape
preserves backward compatibility — existing fields stay; new fields
are additive.

CKB node IBD state surfaces as `is_synced: false` in a 200 response,
not a 503. Refusing to be ready during IBD would prevent the service
from coming online during catch-up, which is the wrong default. The
operator's runbook will alert on `indexer_lag_blocks` regardless.

### `/v1/proofs/:tx_hash`

Authenticated REST endpoint that passes through the CKB node's
`get_transaction_proof`. Returns the Merkle branch and block header so
clients can verify the transaction-to-header path themselves. Opt-in;
not surfaced on cell or transaction responses by default.

### Well-known script registry

Static map from `(code_hash, hash_type)` to a label, sourced from
[explorer.nervos.org/scripts](https://explorer.nervos.org/scripts) and
[nervosnetwork/ckb-system-scripts](https://github.com/nervosnetwork/ckb-system-scripts).
Cell responses gain optional `lock_kind` / `type_kind` fields,
populated only when the registry has a match. Network-aware
(mainnet/testnet split) via `CELLORA_NETWORK`.

## Consequences

- The reorg log is the audit surface for the trust model that ADR 0004
  describes — operators can see exactly what was rolled back and when.
- `/metrics` becomes the contract Grafana dashboards depend on. Adding
  or renaming a metric in a later week is a breaking change for
  operators; we will track that as we would an API change.
- Tracing exports are off by default, so dev workflows are unchanged.
  Production deployments opt in via a single env var.
- The well-known script registry is the only place a label like
  `"sighash"` is defined. It becomes part of the API surface — clients
  can't depend on a label appearing for a script that isn't in the
  registry.
- `/v1/proofs/:tx_hash` is the first concrete step in the
  verifiable-responses path. Future MMR / Flyclient bundles build on
  the same theme without changing this endpoint.

## Implementation slices

1. `reorg_log` migration + reorg detection + rollback + Redis pub/sub
   + tests (largest, ~2 days).
2. Prometheus registry + `/metrics` + indexer / API instrumentation +
   tests.
3. Expanded `/v1/health/ready` (Redis + CKB probes) + tests.
4. OpenTelemetry tracing initialisation + tests.
5. Well-known script registry + cell-response enrichment + OpenAPI
   regen + tests.
6. `/v1/proofs/:tx_hash` passthrough + OpenAPI regen + tests.
7. Grafana dashboard JSON + `docs/observability.md` + README pass.
