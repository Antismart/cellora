//! Shared foundations for the Cellora indexer: configuration, observability,
//! error types, and the CKB JSON-RPC client wrapper.
//!
//! This crate deliberately has no dependency on the database or the indexer
//! binary so that later services (the API gateway in week 2, worker pools in
//! later weeks) can reuse the same building blocks without refactor.

pub mod ckb;
pub mod config;
pub mod error;
pub mod logging;

pub use config::Config;
pub use error::{Error, Result};
