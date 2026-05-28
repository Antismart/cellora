# CKB Developer Community — Survey

A discovery survey for developers building on CKB. The goal is to validate
pain, query patterns, and willingness to pay for a managed indexing and
query service. Target completion time: ~8 minutes.

---

## 1. What are you building on CKB?

1. **Role.** Developer · Tech lead · Founder · Researcher · Other
2. **Project type.** Wallet · DEX / AMM · NFT or asset issuance (Spore etc.) ·
   RGB++ / Bitcoin-bridged · Analytics / explorer · Infra & tooling ·
   DAO / governance · Other
3. **Stage.** Exploring · Prototype · Testnet · Mainnet beta · Production
4. **Networks used.** Mainnet · Testnet · Dev chain only

## 2. How you access CKB data today

5. **Current setup.** *(select all)* Run a full node · Use the node's
   built-in indexer · Run Mercury · Run our own custom indexer · Public RPC
   endpoints · Third-party provider · Don't access chain data directly
6. **Request volume per day.** <1k · 1k–10k · 10k–100k · 100k–1M · >1M ·
   Don't know
7. **Share of engineering time spent on node / indexer operations.**
   <5% · 5–20% · 20–50% · >50%
8. **How do you handle reorgs today?** We don't · We wait N confirmations ·
   Our indexer handles it · Not sure

## 3. Pain points

9. **What is your single biggest pain with CKB data access today?**
   *(open text, ~2 sentences)*
10. **Which of these have cost you meaningful engineering time in the last
    6 months?** *(multi-select)* Node syncing · Indexer falling behind tip ·
    Reorg-related data inconsistency · Missing query patterns ·
    Public-endpoint rate limits · Cost of running infra · No historical
    data beyond X months · No webhooks, forced to poll · Schema/API
    instability · Other
11. **Have reorgs ever caused incorrect behaviour in your app or a
    user-visible bug?** Yes · No · Not sure

## 4. What would you want

12. **Preferred interface.** *(rank top 2)* REST · GraphQL · gRPC ·
    Typed SDK (TS / Rust / Go) · WebSocket subscriptions · Direct SQL
    read-replica
13. **Query patterns that matter most.** *(rank top 3)* Live cells by lock ·
    Live cells by type · Historical cells · Transaction by hash ·
    Transaction history by address · Transaction graph traversal · Balance
    aggregation · Custom script decoding · Block metadata
14. **Scripts you need first-class support for.** *(multi-select)*
    Sighash / default lock · MultiSig · Omnilock · xUDT · Spore ·
    Nervos DAO · RGB++ · Nostr binding · Our own custom scripts · Other
15. **How important are these capabilities? (1 = not, 5 = critical)**
    - Indexing lag under 5 seconds
    - 99.95%+ uptime SLA
    - Reorg-safe guarantees with audit log
    - Webhook delivery on matching events
    - GraphQL subscriptions for live updates
    - Historical queries over the full chain history
    - Multi-region / low-latency read edges

## 5. Commercial fit

16. **Would you pay for a managed CKB indexer that removes node + indexer
    ops from your stack?** Yes · Maybe · No
17. **Which pricing model fits how you buy infra?** Flat monthly tier ·
    Per-request metered · Hybrid (included quota + overage) ·
    Enterprise contract
18. **Rough budget ceiling per month for production-grade service at your
    current volume.** $0 · <$50 · $50–200 · $200–500 · $500–2k · $2k–10k ·
    >$10k
19. **What would make you *not* adopt a hosted indexer, even if the product
    were good?** *(open text)*

## 6. Open

20. **What would your ideal CKB data layer do that nothing currently does
    well?** *(open text)*
21. **Optional: email if you're open to a 20-min follow-up call.**
    *(free text)*
