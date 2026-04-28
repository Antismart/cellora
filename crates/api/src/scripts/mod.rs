//! Well-known CKB script registry.
//!
//! Looks up `(code_hash, hash_type)` pairs against a curated list of
//! standard CKB scripts (Sighash, MultiSig, Nervos DAO, …) and returns
//! the canonical short label. Cell responses surface the label as the
//! optional `lock_kind` / `type_kind` field; clients still receive the
//! raw script in every case, so the tag is purely additive.
//!
//! New entries land via PR per ADR 0005 — no auto-scrape, no implicit
//! drift between deployment and the upstream lists.

pub mod registry;
