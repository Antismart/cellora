# ADR 0008 — Verifiable completeness and reproducible indexing

- **Status:** Accepted
- **Date:** 2026-07-29
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

Two additive primitives, both built on the fixed deterministic schema:

1. **State root / Proof-of-Indexing.** The indexer maintains its query
   indexes as an authenticated structure and commits a content-addressed
   root of the indexed set at each tip. The root is a deterministic
   function of consensus-anchored blocks, so it is reproducible by any
   independent operator.

2. **Coverage proofs.** For a filtered query (e.g. "live cells with
   `lock_hash = X` at tip `T`"), return a range/coverage proof over the
   authenticated index: proof that the returned set is *exactly* the set
   under that key in the committed root — no more, no fewer. This turns
   "trust that this is all of them" into "here is a proof this is all of
   them, relative to state root R."

Reproducibility closes the loop: coverage proofs give *complete with
respect to R*; independent operators reproducing the same R at tip T
(and anchoring R to consensus via the Level-1 MMR/Flyclient work) give
*R is the canonical view*. A light federation of independent operators,
with disagreement surfaced by a gateway, is the deployment shape. A
minimal on-chain bond in CKB (slash on *proven* divergence) is permitted
as a later option; it is not a token.

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
  indexer must produce byte-identical state across operators; any
  non-determinism (ordering, encoding, clock, node-response variance) is
  a Level-2 bug, not a detail. This constrains future schema changes.
- Depends on ADR 0004 step 3 (MMR/Flyclient) for anchoring R to
  consensus, and shares substrate with the separate
  `ckb-crosschain-verification` project (its `no_std` verification crate
  is the header-verification building block).
- Additive, like ADR 0004: state root and coverage proofs are opt-in;
  the default REST/GraphQL wire format is unchanged.
- The public reply to phroi can reference this ADR rather than floating
  an unroadmapped direction.

## Open questions

- **State-root commitment format:** what exactly is hashed, at what tip
  granularity, and which authenticated structure (sparse Merkle tree,
  Merkle B-tree, other) makes coverage proofs cheap for the real query
  shapes (`lock_hash`, `type_hash`, ranges, liveness filters)?
- **Coverage-proof feasibility per query shape:** filtered-by-key is
  clearly provable; are all supported filters expressible as authenticated
  range/membership proofs, or do some (e.g. data-content filters) fall
  outside?
- **Reproduction protocol:** how do independent operators publish and
  compare R — on-chain, a shared registry, or gateway-side attestation?
  Coordinate with phroi, who indicated work in this direction.
- **Cost:** authenticated indexes add write and storage overhead to the
  single-writer ingestion path; needs measurement before committing to a
  structure.
