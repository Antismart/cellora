# ADR 0008 — Verifiable completeness and reproducible indexing

- **Status:** Accepted (revised 2026-07-30)
- **Date:** 2026-07-29
- **Revision (2026-07-30):** In a follow-up on the thread, phroi argued
  that a completeness/coverage proof only shows the returned rows are
  real, not that none were omitted, and that once a client must
  reconstruct the state root to trust it, the proof is pure overhead.
  That is correct. Level 2 is updated to drop authenticated-index
  coverage proofs in favour of **flat digests + independent
  reconstruction**, and adopts phroi's commitment format. A further
  follow-up settled the packaging (per-exact-script units) and the
  publication/reorg policy (depth over cadence, invalidate-and-replace),
  both folded into Level 2 below. phroi's reference design is
  `phroi/light-client-snapshots`. Credit to phroi for the design.
- **Context:** The design-review thread on Nervos Talk ("Cellora —
  designing a production indexing and query service for CKB") converged
  on trust as the central concern. [[0004]] captured the near-term
  answer for *positive results* — inclusion proofs and block anchoring
  so a client can verify a record the API returns. phroi then raised the
  harder half: even with a consensus-anchored header, how does a client
  prove that no matching cells or transactions were *omitted* from a
  filtered answer? That is a completeness (non-omission) guarantee,
  which ADR 0004 does not address. phroi also pointed at the better
  design frame: not "one provider proves everything" but "independent
  parties reproducing the same canonical view at the same tip." This ADR
  captures how far we commit to that direction.

## Context

ADR 0004 is honest that Cellora is a trusted oracle whose *returned*
records become verifiable over three steps (block annotation →
`/v1/proofs/:tx_hash` passthrough → MMR/Flyclient bundles). None of
that proves completeness: a client cannot tell, from a proof of the
rows it received, whether the operator silently dropped rows it should
have received.

Two things make a middle ground tractable on CKB specifically, and they
are why this is worth committing to rather than leaving as an
aspiration:

- The indexed schema is **fixed and canonical** (blocks, transactions,
  cells, precomputed script hashes — see [[0001]], [[0005]]). There is
  no per-application mapping logic that can diverge between operators,
  which is the thing The Graph had to invent Proof-of-Indexing to
  paper over.
- The cell model is **deterministic**. Two honest operators indexing the
  same chain to the same tip should derive byte-identical state, so
  agreement is checkable and disagreement is detectable.

## Decision

We commit to **Levels 1 and 2** below and **explicitly decline Level 3**
in this period. This is a product decision as much as a technical one:
Cellora stays a hosted product with a path to trust-minimisation, and
does not become a token/protocol network.

### Level 1 — Verifiable responses (in progress, per ADR 0004)

No change to ADR 0004. Finish its staged path: block annotation
(shipped), proofs passthrough (shipped, Week 4), and the MMR/Flyclient
bundle endpoint on the post-Week-7 horizon. This verifies *what the API
returns*, not completeness.

### Level 2 — Verifiable completeness + reproducible indexing (new)

Completeness is established by **independent reconstruction, not proofs**
(the correction from phroi's follow-up):

1. **State root — a content-addressed digest of the indexed set at a
   tip.** The indexer commits a flat digest `R` over the canonical
   logical rows of the indexed set. Per phroi's format: hash the
   canonical rows *including the full script and whether it is a lock or
   type view* (not precomputed hashes alone), and pin the tip by **both
   block number and block hash** (height alone does not identify a fork).
   `R` is a deterministic function of consensus-anchored blocks, so any
   independent operator indexing the same chain to the same tip
   reproduces the identical `R`. No authenticated data structure — it
   adds write/storage overhead without solving omission.

2. **Independent reconstruction.** A client (or peer operator) that wants
   completeness re-derives the indexed artifact at the pinned tip and
   compares its digest against the published `R`. Matching digests mean
   the two parties agree on the *entire* set at that tip — the
   completeness guarantee a per-query proof cannot give. A coverage proof
   only shows the returned rows match `R`, not that `R` itself omitted
   nothing, and once you reconstruct `R` to trust it the proof is
   redundant. This is the "light-client snapshot" shape: import indexed
   state at a pinned tip, skip the replay, verify by reconstructing the
   digest.

Reproducibility closes the loop: independent operators (and clients)
reproducing the same `R` at tip `T` — with `R` anchored to consensus via
the Level-1 MMR/Flyclient work — establish that `R` is the canonical
view, with no per-query proof machinery. A light federation of operators,
with disagreement on `R` surfaced by a gateway, is the deployment shape.
A minimal on-chain bond in CKB (slash on *proven* divergence of `R`) is
permitted as a later option; it is not a token.

#### Packaging and publication (settled with phroi)

phroi's `phroi/light-client-snapshots` is the reference design. Cellora
adopts its shape rather than inventing a parallel one, so exports stay
comparable across independent parties.

- **Unit.** One exact script plus its lock/type kind is the canonical
  unit, with shared chain state published once per release. A release may
  bundle several per-script payloads (each with its digest), but must
  never merge them into one aggregate payload — that would lose selective
  download and per-script digest comparison. Payloads are canonical
  sorted key/value streams; the digest is over the decoded bytes, not the
  compressed transport.
- **Publication policy — depth, not cadence.** Do not publish provisional
  snapshots. Choose a tip already buried by a fixed depth, compare the
  candidate against a baseline privately, then publish it once. Depth is
  the safety rule; cadence is an operational choice. The consumer still
  verifies the pinned tip is on the accepted chain after import.
- **Reorg handling.** On a deep reorg that removes the pinned tip,
  invalidate the release and publish a replacement at the new canonical
  tip; do not merge over orphaned state. This matches Cellora's existing
  rollback-to-common-ancestor model ([[0006]]).
- **Honesty.** Matching digests prove that exporters produced the same
  artifact, not that the chain is stable and not that history is
  complete. Given CKB's mining-pool concentration, a buried tip is
  *operationally stable, not final*: no depth defends against every
  majority-miner reorg, and snapshots cannot solve that consensus risk.

**The hard prerequisite (Cellora-specific).** Cellora indexes into
Postgres with its own schema, not the CKB light-client key layout. For a
Cellora export to be *comparable*, its decoded bytes must match what a
from-genesis light-client sync would produce for that script, byte for
byte — not merely what Cellora happens to store. If they do not, the
digest is just another single-provider artifact and the reproducibility
benefit is gone. Re-deriving the canonical LC rows from our indexes and
testing them against a cold rebuild is the real work, and it gates the
whole direction.

### Level 3 — Decentralised protocol (declined for now)

No staking token, delegation, curation, query-fee token, slashing
marketplace, or governance. The economics only work at query volumes and
token liquidity Cellora does not have, and the overhead would dwarf the
product. Revisit only if demand demonstrably justifies a protocol pivot.
Levels 1–2 keep that door open without walking through it.

### Honesty bound (hard constraint)

This is trust-*minimised*, not trustless. The irreducible residual: if
every operator runs the same derivation rule with the same bug, they
agree on a wrong R, and only a client re-deriving from its own light or
full node closes that gap. Docs and marketing state this plainly and
never claim completeness is proven end to end. Same rule as ADR 0004:
name the trust surface, do not quietly maintain it.

## Consequences

- The roadmap in `CLAUDE.md` gains a **Phase 2 (beyond Week 7)** track
  for Level 2. It is not calendared to specific weeks — like ADR 0004
  step 3, prerequisites must be clearer first (see open questions).
- **Determinism becomes a hard design constraint.** The schema and
  indexer must produce byte-identical state across operators, and the
  canonical row encoding and set ordering must be specified precisely so
  independent implementations compute the same `R`. Any non-determinism
  (ordering, encoding, clock, node-response variance) is a Level-2 bug,
  not a detail. This constrains future schema changes.
- Depends on ADR 0004 step 3 (MMR/Flyclient) for anchoring R to
  consensus, and shares substrate with the separate
  `ckb-crosschain-verification` project (its `no_std` verification crate
  is the header-verification building block).
- Additive, like ADR 0004: the published `R` digest and reconstruction
  tooling are opt-in; the default REST/GraphQL wire format is unchanged.
- The public reply to phroi can reference this ADR rather than floating
  an unroadmapped direction.

The commitment scope, packaging, and publication/reorg policy are settled
above (per phroi). What remains open:

- **Cold-rebuild parity (the gating problem):** producing exports whose
  decoded bytes are byte-identical to a from-genesis light-client sync,
  from Cellora's Postgres-shaped indexes. Until this holds, reproducibility
  is only aspirational (see "the hard prerequisite").
- **Reproduction in practice:** reproducibility only *means* something once
  a second independent exporter exists and digests are compared; Cellora
  alone is still single-provider trust. Where operators publish and compare
  `R` (registry, transparency log, gateway) is undecided.
- **Demand and priority:** most Cellora users want the query API, not to
  run a local light client. This is Phase-2 work whose priority tracks real
  demand for the own-your-client path, not a given.
