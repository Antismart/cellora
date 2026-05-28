# ADR 0004 — Trust model and verifiable responses

- **Status:** Accepted
- **Date:** 2026-04-28
- **Context:** Feedback on the CKBuilders project review (Retric, CKB
  team) raised the indexer-as-trust-surface problem and pointed at
  CKB's MMR / Flyclient primitives as the path to verifiable responses.
  This ADR captures the current posture, the agreed evolution, and the
  bounds of what we are committing to in this period.

## Context

A query service that sits between signers and consensus is a trust
surface whether the operator names it or not. The recent ~$300m
Ethereum incident originating from a compromised RPC was the prompt:
clients sign transactions on the basis of what an indexer tells them.
If the indexer lies, the client lies to itself.

CKB's primitives make the verifiable path more tractable than on most
chains:

- `get_transaction_proof` already returns Merkle branches from a
  transaction to its block header.
- The chain-wide MMR commitment lives in the block header's extension
  field after the relevant hard fork. Any recent trusted header lets a
  client verify arbitrary historical inclusion.
- Flyclient parameterisation lets that verification be sublinear in
  proof size.

We need to be honest about today's posture, name a concrete near-term
step, and reserve the long horizon without over-committing.

## Decisions

### Posture, named explicitly

Cellora is a trusted oracle today. The architecture document carries a
"Trust model" section that says this in plain language and frames the
evolution toward verifiable responses as a deliberate path, not an
absence we are quietly maintaining.

### Step 1: every record carries its authoritative block (shipped)

Every cell response carries `block_hash` alongside `block_number`. This
is a free cross-check primitive: a client can verify the block hash
against their own node, or a second indexer, with no extra round-trip.
This shipped in Week 2 (slice 3) as a direct response to the feedback
and was almost zero cost while the cells query was being built.

Transaction responses, when they land, will carry the same.

### Step 2: proof passthrough endpoint (Week 4)

A `/v1/proofs/:tx_hash` endpoint passes through the CKB node's
`get_transaction_proof` alongside the relevant block header. Clients
verify the transaction-to-header Merkle path themselves. The trust
surface drops to "did Cellora hand you the right header?", which the
client can answer by checking the header against their own node or any
other source they trust.

The endpoint is opt-in — `/v1/proofs` does not appear by default in
cell or transaction responses, so the standard wire format stays
narrow.

### Step 3: MMR / Flyclient bundles (post-Week 7 horizon)

A bundle endpoint that returns the full chain of proofs:

```
cell → tx → header → MMR root in a recent header
```

With this, a client holding any recent trusted header can verify any
historical cell or transaction without trusting Cellora at all. This
is the version where the trust surface is fully eliminated.

We do not calendar this for a specific week because two things must be
clearer first:

1. What the CKB nodes themselves expose for MMR proof generation, and
   whether that surface is stable enough to depend on.
2. Whether a canonical client-side MMR / Flyclient verifier exists in
   Rust or TypeScript, or whether we would be the ones writing it.

When both are answered the work is well-shaped and a few weeks long.

### Additivity

Every step is additive to the existing wire format. The REST and
GraphQL response shapes from earlier weeks do not change; clients
opting out of proofs see exactly what they see today. This is a hard
constraint on the design — no breaking changes in the name of
verifiability.

## Consequences

- The "Trust model" section in `docs/architecture-overview.md` is the
  user-facing description of this stance. New customers and reviewers
  see it before they read the API docs.
- `block_hash` annotation on every cell response is locked in by the
  Week 2 work and is not negotiable in later weeks. Removing it would
  be a breaking change.
- Week 4 scope grows by one endpoint (`/v1/proofs/:tx_hash`). The
  implementation is a thin pass-through — the node does the proof
  work, we forward it.
- Post-Week 7 work has a placeholder rather than a date. The scope is
  agreed; the prerequisites are not yet satisfied.

## Open questions

- Canonical Rust / TypeScript reference for client-side CKB MMR proof
  verification — does one exist, or will we maintain it?
- Flyclient parameterisation in the CKB ecosystem — is there a
  community consensus on sample counts / variance, or is it still open?
- "Downstream consideration of what the client is signing" — the wallet
  side of this conversation is out of Cellora's scope but worth its own
  thread with the CCC team if a productive path opens up.
