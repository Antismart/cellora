# ADR 0003 — Week 3 auth, rate limiting, and GraphQL

- **Status:** Accepted
- **Date:** 2026-04-27
- **Context:** Week 3 of the roadmap — multi-tenant access control on the
  REST API, per-key rate limiting backed by Redis, and a GraphQL surface
  that mirrors REST. The REST surface from Week 2 is currently public and
  unauthenticated; this ADR captures the shape we're locking in.

## Decisions

### Key format and storage

Keys are issued as `cell_<8 hex char prefix>_<32 hex char secret>`. The
prefix is stored plaintext and is the primary key of `api_keys`; the
secret is hashed with Argon2id (interactive parameters, ~10 ms verify)
and only the PHC string is stored. Verification looks up the row by
prefix in O(1), then `argon2::verify` against the provided secret. The
prefix is safe to display or log; the secret is never persisted.

This is the standard SaaS pattern — Stripe, Linear, Vercel — and it
trades nothing material for two operational properties: support staff
can identify a key without holding auth material, and revocation can
key on the prefix alone.

### Auth header

`Authorization: Bearer <key>`. No `X-API-Key` fallback; one path keeps
the middleware tight and matches the convention OpenAPI and SDKs expect.

### Public vs authenticated routes

`/v1/health`, `/v1/health/ready`, and `/v1/openapi.json` are public.
Everything else under `/v1/*` and the new `/graphql` requires a valid
key. Routes are split into two `axum::Router` instances with the auth
and rate-limit layers attached only to the authenticated half — no
in-middleware path branching, which is where security bugs live.

### Auth verification cache

A `moka` LRU cache keyed on `(prefix, secret)` short-circuits the
Argon2 verification on the hot path. TTL is 60 s, capacity 10k entries,
both env-tunable. Revocation invalidation is best-effort — within the
TTL window a revoked key may still pass. This is acceptable for Week 3;
a Redis pub/sub invalidation channel comes alongside reorg events in
Week 4.

### Rate limiting

Token bucket per key, separate buckets for REST (`rl:rest:<prefix>`)
and GraphQL (`rl:graphql:<prefix>`). Implemented as a single atomic Lua
script in Redis returning `(allowed, remaining, retry_after_ms)`.

Tier defaults are sized for sanity, not from market data — they will be
revisited once survey responses inform real user volumes:

| Tier    | REST burst | REST refill/s | GraphQL burst | GraphQL refill/s |
|---------|-----------:|--------------:|--------------:|-----------------:|
| free    |         30 |             1 |            10 |              0.5 |
| starter |        300 |            20 |           100 |               10 |
| pro     |       3000 |           200 |          1000 |              100 |

Every authenticated 2xx carries `X-RateLimit-Limit`,
`X-RateLimit-Remaining`, and `X-RateLimit-Reset`. 429 responses
additionally carry `Retry-After`.

### Failing open on Redis outage

When Redis is unreachable, the limiter fails open by default. A 5xx on
every authenticated request because the limiter is hard-down would be
worse than briefly serving over-budget traffic. Operators who want
fail-closed flip `CELLORA_API_RATE_LIMIT_FAIL_OPEN=false`.

### GraphQL surface

`async-graphql` 7 + `async-graphql-axum`, mounted at `POST /graphql`.
Resolvers wrap the same `cellora-db` repository functions as the REST
handlers — drift is impossible by construction. Schema shape mirrors
REST one-to-one; pagination is `data + nextCursor` rather than Relay's
edges/pageInfo, both for consistency with REST and to keep the surface
small.

`Hex32`, `Hex`, and `DateTime` are custom scalars. The same auth and
rate-limit middleware apply to `/graphql` — the limiter cannot be
bypassed by routing through GraphQL.

A GraphQL playground is exposed at `GET /graphql/playground` only when
`CELLORA_API_GRAPHQL_PLAYGROUND=true`. Off by default in production.

### Admin CLI

The `cellora-api` binary grows a clap subcommand parser. Default
behaviour (no subcommand) is unchanged: serve. Subcommands:

- `admin create-key --tier <free|starter|pro> [--label <text>]`
- `admin list-keys`
- `admin revoke-key <prefix>`

`create-key` prints the full key once. The DB only stores the prefix
and the Argon2 hash, so a second display is impossible — the operator
must record the key themselves at creation time.

## Consequences

- The auth surface is small enough that we can audit it line by line.
  No JWT, no OAuth, no organisations — that complexity arrives in
  Week 5+ when the dashboard does.
- Tier limits are config-driven, so tuning post-launch does not
  require a deploy with code changes — env update + restart.
- GraphQL and REST share the same db-layer code. Adding a new query
  pattern means one repo function, two thin handler wrappers.
- Every authenticated request takes one Postgres lookup (cached) plus
  one Redis round-trip. Both are sub-millisecond on warm caches; the
  observable cost on the hot path is the Argon2 verification, mitigated
  by the in-process cache.

## Implementation slices

1. Migration + ApiKey model + Argon2 helpers + admin CLI subcommand.
2. Auth middleware + public/auth router split + 401 envelope + tests.
3. Redis pool + token-bucket Lua + rate-limit middleware + 429 envelope
   + headers + tests.
4. GraphQL schema + handler + auth/rate-limit wiring + tests.
5. Load test + `docs/api.md` GraphQL section + README pass.
