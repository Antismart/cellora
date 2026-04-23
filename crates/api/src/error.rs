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
            Self::UpstreamUnavailable(_) => "upstream_unavailable",
            Self::Internal(_) => "internal",
        }
    }

    /// HTTP status returned for this variant.
    fn status(&self) -> StatusCode {
        match self {
            Self::NotFound(_) => StatusCode::NOT_FOUND,
            Self::BadRequest(_) | Self::InvalidCursor(_) => StatusCode::BAD_REQUEST,
            Self::UpstreamUnavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// Public-facing message placed in the envelope. Internal errors are
    /// deliberately opaque — the original is still logged.
    fn public_message(&self) -> String {
        match self {
            Self::NotFound(msg) | Self::InvalidCursor(msg) | Self::UpstreamUnavailable(msg) => {
                (*msg).to_owned()
            }
            Self::BadRequest(msg) => msg.clone(),
            Self::Internal(_) => "internal error".to_owned(),
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
#[derive(Debug, Serialize)]
struct ErrorEnvelope<'a> {
    error: ErrorBody<'a>,
}

#[derive(Debug, Serialize)]
struct ErrorBody<'a> {
    code: &'a str,
    message: String,
    details: Option<serde_json::Value>,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        // Log internal errors at ERROR with the original source attached.
        // Everything else is logged at INFO — these are client errors, not
        // service faults, and should not trip alerts.
        if let Self::Internal(err) = &self {
            tracing::error!(error = %err, "internal api error");
        } else {
            tracing::info!(code = self.code(), error = %self, "client error");
        }

        let body = ErrorEnvelope {
            error: ErrorBody {
                code: self.code(),
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
