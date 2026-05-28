# CKBuilders issue — draft

Draft of the issue to open at
https://github.com/Nervos-Community-Catalyst/CKBuilder-projects/issues/new

---

**Title:** Cellora: a multi-tenant indexing and query service for CKB

**Project Summary:**
Cellora is a managed indexing and query layer for CKB, designed as a
production data backbone for DApps that would rather not run their own full
node, indexer and database stack. It normalizes blocks, transactions and
cells into a PostgreSQL store with indexes tuned for the dominant access
patterns (by lock hash, type hash, outpoint), exposes REST and GraphQL
surfaces behind API-key auth and per-key rate limiting, and treats reorgs
as a first-class case rather than an edge case.

**Tools used:** Rust (Axum, async-graphql, SQLx), PostgreSQL, Redis,
`ckb-jsonrpc-types`, `reqwest`, Docker / Kubernetes, OpenTelemetry +
Prometheus.

**Current features:**
- Block polling loop against a CKB JSON-RPC endpoint
  (`get_tip_block_number`, `get_block_by_number`, `get_blockchain_info`)
- Parser and writer that normalize blocks → transactions → cells
- Live/dead cell accounting via `consumed_*` columns
- One-transaction-per-block write path — the database never observes a
  partial block
- Indexer-state tracking for tip recovery across restarts
- Graceful shutdown on `SIGINT` / `SIGTERM`

**Planned features:**
- REST API (blocks, cells, stats, health, cursor pagination)
- GraphQL endpoint via `async-graphql`
- API-key authentication (Argon2-hashed) with tiered rate limits backed by
  a Redis token bucket
- Reorg detection and transactional rollback with an audit log
- Range partitioning on the `cells` table by block number
- Webhook delivery with HMAC signatures and retry queue
- GraphQL subscriptions for live cell updates
- Grafana dashboards and OpenTelemetry tracing
- Dashboard for API-key management, usage charts, and a query explorer

**Deployed on:** Dev chain today. Testnet is the next milestone, mainnet
after reorg handling lands.

**Link to repository:** https://github.com/Antismart/cellora

**Link to hosted version of project:** Not yet hosted publicly — happy to
stand up a testnet-backed preview for reviewers who want to kick the tires.

**Screenshots:** Not applicable at this stage — Cellora is backend
infrastructure with no UI yet. Architecture overview with diagrams is at
[`docs/architecture-overview.md`](https://github.com/Antismart/cellora/blob/main/docs/architecture-overview.md)
in the repo.

**Request for feedback:**

I'd value both design-review input and product-market input from this
community.

*On the design (review):*

- The data model stores each cell's script components (`code_hash`,
  `hash_type`, `args`) both raw **and** as a precomputed `lock_hash` /
  `type_hash`. Is this enough for the query patterns DApps actually need,
  or should patterns like partial `args` matching or script-class tagging
  be first-class in the schema?
- The reorg algorithm walks parent hashes back to the common ancestor and
  rolls back in a single DB transaction. What reorg depth should I size
  the rollback path for in practice on mainnet?
- The indexer uses polling (2 s default) rather than RPC subscriptions.
  Are there known stability or compatibility concerns with the CKB
  subscription endpoints I should be aware of?
- Well-known script tagging (Sighash, MultiSig, Omnilock, xUDT, Spore,
  RGB++, Nostr binding) is currently out of scope for the base schema.
  Is there a canonical ecosystem registry for these, or should I maintain
  my own?

*On product fit (guidance):*

- If you're shipping a CKB app today, how do you currently access chain
  data — your own full node, the node's built-in indexer, Mercury,
  public RPC, a third-party provider? What breaks most often?
- Which query patterns matter most for your app: live cells by lock,
  cells by type, historical ranges, transaction graph traversal, balance
  aggregation?
- Which interface would you reach for first: REST, GraphQL, a typed SDK
  (TS / Rust / Go), or WebSocket subscriptions?
- How much engineering time does node + indexer operations cost you
  today, and would you pay to remove that from your stack?

**Problems:** No blockers right now — this issue is specifically to
collect community input before the API and reorg-handling work lands.

Happy to jump on a 20-minute call with anyone whose app has non-trivial
indexing needs. The architecture document in the repo has more detail for
reviewers who want to dig in.
