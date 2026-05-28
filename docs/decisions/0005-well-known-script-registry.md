# ADR 0005 — Well-known script registry

- **Status:** Accepted
- **Date:** 2026-04-28
- **Context:** Eval-exec (CKB) replied on the CKBuilders project review
  with two authoritative sources for well-known CKB scripts:
  [explorer.nervos.org/scripts](https://explorer.nervos.org/scripts)
  and
  [nervosnetwork/ckb-system-scripts](https://github.com/nervosnetwork/ckb-system-scripts).
  This unblocks the script-tagging item we previously listed as out of
  scope for the base schema. This ADR records what we will do with that.

## Context

The base `cells` table stores the three script components (`code_hash`,
`hash_type`, `args`) raw and the precomputed script hash. That is
sufficient for any query a client needs but presents an awkward UX:
clients have to look up the meaning of a `code_hash` themselves, and
common scripts (Sighash, MultiSig, Omnilock, xUDT, Spore, Nervos DAO,
RGB++ binding cells, etc.) are universal enough that every consumer
ends up doing the same lookup.

Having canonical sources makes a curated registry tractable. We do not
need to derive this from chain analysis — we map known `code_hash`
values to human-readable labels and surface the label as an additional
field on script responses.

## Decisions

### Registry shape

A static, compile-time registry in `crates/api/src/scripts/registry.rs`.
Entries:

```rust
pub struct WellKnownScript {
    pub code_hash: [u8; 32],
    pub hash_type: HashType,
    pub label: &'static str,             // e.g. "sighash", "multisig", "xudt"
    pub network: Network,                // Mainnet | Testnet | Both
    pub kind: ScriptKind,                // Lock | Type
}
```

`label` is lowercase, snake-cased, and stable. Code in the API maps a
cell's `(code_hash, hash_type)` to a label via a hashmap built once at
startup.

### Source of truth

Two upstream lists, treated as authoritative:

1. **explorer.nervos.org/scripts** — the canonical curated list,
   including third-party scripts (xUDT, Spore, RGB++, Omnilock, Nostr
   binding).
2. **nervosnetwork/ckb-system-scripts** — system scripts shipped with
   CKB itself (Sighash, MultiSig, Nervos DAO, etc.).

The registry file copies the relevant entries verbatim with citation
comments pointing at each source. We do not auto-scrape on every build
— version drift in upstream sources should be a deliberate, reviewable
PR, not a transparent bump.

### API surface change

The cell response gains two optional fields:

```json
{
  "lock": { "code_hash": "0x...", "hash_type": "type", "args": "0x..." },
  "lock_kind": "sighash",
  "type": null,
  "type_kind": null
}
```

`lock_kind` and `type_kind` are present only when the
`(code_hash, hash_type)` matches a registry entry; otherwise they are
omitted (not `null`). This keeps the response narrow for cells with
custom or unknown scripts. Same change applies to the GraphQL
projection — `lockKind` and `typeKind` as optional `String` fields.

The raw script representation is unchanged. Clients who do not care
about the registry see exactly the responses they see today.

### Versioning and freshness

PR-based updates: when a new well-known script appears in either
upstream source, a maintainer opens a PR adding the entry and citing
where it came from. CI enforces nothing on freshness — drift is human
caught.

If `explorer.nervos.org/scripts` ever exposes a stable JSON API, a
build-time fetch + diff with a committed snapshot becomes attractive,
but that is a future enhancement and not part of the initial
implementation.

### What we do NOT do

- We do not parse or interpret script `args`. xUDT owners, Spore
  metadata, etc. are content-aware projections that belong in
  protocol-specific helpers, not in a generic indexer.
- We do not filter on `lock_kind` / `type_kind` in queries (yet). The
  base query primitive remains `lock_hash` / `type_hash`. A
  `?lock_kind=xudt` filter is plausible but is deliberately out of
  scope until there is a real workload that needs it.
- We do not version the registry separately from the API. A
  registry update is an API release like any other.

## Consequences

- Cell responses become marginally more useful by default; integrators
  see human-readable labels for the universal scripts without an
  external lookup.
- The registry file is the only place a name like `"sighash"` is
  defined. A typo or stale entry there propagates to every consumer —
  it is a small file and reviewable, but it is now load-bearing.
- Adding a new well-known script after the API is in production is a
  low-risk change: a PR adds a row, the static map gains one entry,
  shipped clients seamlessly start seeing the new label.
- The eventual MMR / Flyclient verification work (ADR 0004) is
  unrelated. Tagging is informational; verifiability is structural.

## Implementation slot

Week 4 alongside reorg handling and observability. The registry itself
is a few hours of work; integrating into the cell renderers (REST and
GraphQL) is small. Tests cover: a cell with a known sighash lock gets
`lock_kind: "sighash"`, a cell with a custom lock has the field
omitted, the registry contains no duplicate `(code_hash, hash_type)`
pairs.
