//! Shared error type used by the common crate and re-exported for the indexer.

use thiserror::Error;

/// Convenience alias for operations that return a common [`Error`].
pub type Result<T> = std::result::Result<T, Error>;

/// Top-level error variants raised by configuration, logging, and the CKB
/// JSON-RPC client. Library layers above this crate wrap these inside their
/// own error types rather than returning [`Error`] directly.
#[allow(missing_docs)]
#[derive(Debug, Error)]
pub enum Error {
    /// Failed to load or validate configuration.
    #[error("configuration error: {0}")]
    Config(String),

    /// Failed to initialise the tracing subscriber.
    #[error("logging setup failed: {0}")]
    Logging(String),

    /// HTTP-level failure reaching the CKB node.
    #[error("ckb http error: {0}")]
    CkbHttp(#[from] reqwest::Error),

    /// The CKB node returned a JSON-RPC error envelope.
    #[error("ckb rpc error: code={code} message={message}")]
    CkbRpc {
        /// JSON-RPC error code returned by the node.
        code: i64,
        /// Human-readable message attached to the error.
        message: String,
    },

    /// Failed to deserialize a response from the CKB node.
    #[error("ckb response decode error: {0}")]
    CkbDecode(#[from] serde_json::Error),

    /// URL parsing failure (usually invalid configured endpoint).
    #[error("invalid url: {0}")]
    InvalidUrl(String),
}
