//! Library surface for the Cellora block indexer.
//!
//! The binary (`main.rs`) wires configuration, logging, and signal handling
//! on top of these modules. Tests and future services can compose the same
//! building blocks without going through `main`.

pub mod app;
pub mod metrics;
pub mod metrics_server;
pub mod parser;
pub mod poller;
pub mod reorg;
pub mod shutdown;
