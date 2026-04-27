//! API error type and JSON envelope.
//!
//! Every error reaching the HTTP boundary is serialised as:
//!
//! ```json
//! { "error": { "code": "...", "message": "...", "details": null } }
//! ```
//!
//! Handlers return `Result<T, ApiError>`; the [`axum::response::IntoResponse`]
//! implementation converts the variant into the right status code and
//! writes the envelope. Low-level errors (`sqlx::Error`, panics from the
//! middleware stack) are collapsed into [`ApiError::Internal`] and logged
//! at `ERROR` with the original error attached — clients only ever see
//! the stable envelope.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;
use thiserror::Error;
use utoipa::ToSchema;

/// Convenience alias for handler return types.
pub type ApiResult<T> = Result<T, ApiError>;

/// Top-level API error.
#[derive(Debug, Error)]
pub enum ApiError {
    /// Requested resource does not exist.
    #[error("not found: {0}")]
    NotFound(&'static str),

    /// Client-supplied input was malformed.
    #[error("bad request: {0}")]
    BadRequest(String),

    /// Opaque pagination cursor failed to decode or validate.
    #[error("invalid cursor: {0}")]
    InvalidCursor(&'static str),

    /// Authentication failed — missing, malformed, unknown, or revoked
    /// credentials. The variant carries an internal reason for logs;
    /// every external response uses the same opaque "unauthorized"
    /// message so callers cannot enumerate reasons.
    #[error("unauthorized: {0}")]
    Unauthorized(&'static str),

    /// Rate limit exceeded for the presented key. The `retry_after_seconds`
    /// is reflected in the `Retry-After` response header by the rate-limit
    /// middleware (the `IntoResponse` for [`ApiError`] only emits the
    /// JSON envelope).
    #[error("rate limited: retry after {retry_after_seconds}s")]
    RateLimited {
        /// Number of seconds the client should wait before retrying.
        retry_after_seconds: u64,
    },

    /// Dependency required to serve the request is unavailable.
    #[error("upstream unavailable: {0}")]
    UpstreamUnavailable(&'static str),

    /// Unexpected internal failure; original error is logged, not returned.
    #[error("internal error")]
    Internal(#[source] anyhow::Error),
}

impl ApiError {
    /// Stable machine-readable code used in the JSON envelope.
    fn code(&self) -> &'static str {
        match self {
            Self::NotFound(_) => "not_found",
            Self::BadRequest(_) => "bad_request",
            Self::InvalidCursor(_) => "invalid_cursor",
            Self::Unauthorized(_) => "unauthorized",
            Self::RateLimited { .. } => "rate_limited",
            Self::UpstreamUnavailable(_) => "upstream_unavailable",
            Self::Internal(_) => "internal",
        }
    }

    /// HTTP status returned for this variant.
    fn status(&self) -> StatusCode {
        match self {
            Self::NotFound(_) => StatusCode::NOT_FOUND,
            Self::BadRequest(_) | Self::InvalidCursor(_) => StatusCode::BAD_REQUEST,
            Self::Unauthorized(_) => StatusCode::UNAUTHORIZED,
            Self::RateLimited { .. } => StatusCode::TOO_MANY_REQUESTS,
            Self::UpstreamUnavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// Public-facing message placed in the envelope. Internal errors are
    /// deliberately opaque — the original is still logged. Authentication
    /// failures are also opaque so callers cannot enumerate reasons.
    fn public_message(&self) -> String {
        match self {
            Self::NotFound(msg) | Self::InvalidCursor(msg) | Self::UpstreamUnavailable(msg) => {
                (*msg).to_owned()
            }
            Self::BadRequest(msg) => msg.clone(),
            Self::Internal(_) => "internal error".to_owned(),
            Self::Unauthorized(_) => "unauthorized".to_owned(),
            Self::RateLimited { .. } => "rate limited".to_owned(),
        }
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(err: sqlx::Error) -> Self {
        Self::Internal(anyhow::Error::from(err))
    }
}

impl From<cellora_db::DbError> for ApiError {
    fn from(err: cellora_db::DbError) -> Self {
        Self::Internal(anyhow::Error::from(err))
    }
}

/// Wire-format body for an error response.
///
/// Every error reaching the HTTP boundary is rendered as this envelope.
/// The shape is deliberately narrow — `code` for programmatic handling,
/// `message` for humans, optional `details` reserved for structured extra
/// context when a future endpoint needs it.
#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorEnvelope {
    /// Single-field container holding the error description.
    pub error: ErrorBody,
}

/// Inner structure of an [`ErrorEnvelope`].
#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorBody {
    /// Stable machine-readable code — `bad_request`, `not_found`,
    /// `invalid_cursor`, `upstream_unavailable`, `internal`.
    pub code: String,
    /// Human-readable description of the failure.
    pub message: String,
    /// Optional structured extra context. Always `null` today.
    pub details: Option<serde_json::Value>,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        // Log internal errors at ERROR with the original source attached.
        // Everything else is logged at INFO — these are client errors, not
        // service faults, and should not trip alerts.
        match &self {
            Self::Internal(err) => {
                tracing::error!(error = %err, "internal api error");
            }
            Self::Unauthorized(reason) => {
                // Auth failures log the *internal* reason at INFO so
                // operators can debug, while clients always see the same
                // opaque public message.
                tracing::info!(reason = %reason, "unauthorized request");
            }
            other => {
                tracing::info!(code = self.code(), error = %other, "client error");
            }
        }

        let body = ErrorEnvelope {
            error: ErrorBody {
                code: self.code().to_owned(),
                message: self.public_message(),
                details: None,
            },
        };
        (self.status(), Json(body)).into_response()
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn status_and_code_mapping() {
        assert_eq!(ApiError::NotFound("x").status(), StatusCode::NOT_FOUND);
        assert_eq!(ApiError::NotFound("x").code(), "not_found");
        assert_eq!(
            ApiError::BadRequest("x".into()).status(),
            StatusCode::BAD_REQUEST,
        );
        assert_eq!(
            ApiError::InvalidCursor("x").status(),
            StatusCode::BAD_REQUEST,
        );
        assert_eq!(ApiError::InvalidCursor("x").code(), "invalid_cursor");
        assert_eq!(
            ApiError::UpstreamUnavailable("x").status(),
            StatusCode::SERVICE_UNAVAILABLE,
        );
        assert_eq!(
            ApiError::Internal(anyhow::anyhow!("boom")).status(),
            StatusCode::INTERNAL_SERVER_ERROR,
        );
    }

    #[test]
    fn internal_error_message_is_opaque() {
        let err = ApiError::Internal(anyhow::anyhow!("database connection refused"));
        assert_eq!(err.public_message(), "internal error");
    }
}
