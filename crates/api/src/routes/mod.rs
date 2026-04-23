//! HTTP route handlers.
//!
//! Each submodule owns a cluster of endpoints. Modules are kept small and
//! handler bodies thin — the heavy lifting lives in the `cellora-db` crate's
//! repository layer and in helpers on this crate.

pub mod blocks;
pub mod health;
