//! Session-cookie authentication for the dashboard surface.
//!
//! The dashboard is a separate audience from the programmatic API: humans
//! sign in through GitHub OAuth, the server issues an opaque session token,
//! and the browser carries it back on every request as an `HttpOnly` cookie.
//!
//! The flow on each request:
//!
//! 1. Pull the `cellora_session` cookie out of the request.
//! 2. Hash the value (SHA-256) and look the row up in the `sessions` table.
//! 3. Check the row's `expires_at` is still in the future.
//! 4. Resolve the owning `users` row.
//! 5. Attach an [`AuthenticatedSession`] to request extensions and continue.
//!
//! Failures are uniform — any reason to reject (no cookie, malformed cookie,
//! unknown hash, expired row, missing user) maps to a single 401 response.
//! The internal cause is logged at DEBUG so operators can debug without
//! leaking detail to the client.
//!
//! `last_seen_at` on the session row is updated lazily in a fire-and-forget
//! task; the response never waits on the write.

use axum::extract::{Request, State};
use axum::http::{header, HeaderMap};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use cellora_db::models::{Session, User};
use cellora_db::{sessions, users};
use rand::RngCore;
use sha2::{Digest, Sha256};

use crate::error::ApiError;
use crate::state::AppState;

/// Cookie name carrying the dashboard session token. Treated as an
/// opaque identifier; clients should never inspect or modify it.
pub const COOKIE_NAME: &str = "cellora_session";

/// Number of random bytes per session token. 32 bytes (256 bits) is far
/// beyond the threshold where brute-forcing a valid token is feasible
/// regardless of session table size.
pub const TOKEN_BYTES: usize = 32;

/// What gets attached to request extensions after a successful session
/// resolve. Handlers downstream of [`middleware`] can pull it out via
/// `axum::Extension<AuthenticatedSession>`.
#[derive(Debug, Clone)]
pub struct AuthenticatedSession {
    /// The persisted session row that authenticated this request.
    pub session: Session,
    /// The user the session belongs to.
    pub user: User,
}

/// A freshly minted session token, paired with its hash for storage.
/// Issued at the end of the OAuth callback; the plaintext is only ever
/// returned to the browser as a Set-Cookie value, never persisted.
#[derive(Debug, Clone)]
pub struct IssuedToken {
    /// Plaintext token. Set as the cookie value.
    pub plaintext: String,
    /// SHA-256(plaintext), hex-encoded. Stored in `sessions.token_hash`.
    pub hash: String,
}

/// Generate a new session token from the OS RNG.
pub fn generate_token() -> IssuedToken {
    let mut bytes = [0u8; TOKEN_BYTES];
    rand::thread_rng().fill_bytes(&mut bytes);
    let plaintext = URL_SAFE_NO_PAD.encode(bytes);
    let hash = hash_token(&plaintext);
    IssuedToken { plaintext, hash }
}

/// Hash a session token. SHA-256 is sufficient because tokens are
/// high-entropy random bytes — Argon2-style work factors are wasted here
/// and would slow every authenticated request.
pub fn hash_token(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    hex::encode(digest)
}

/// Middleware that resolves a session cookie on every request that
/// reaches it. Attach to a sub-router via `from_fn_with_state(...)`.
pub async fn middleware(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let token = match extract_session_cookie(request.headers()) {
        Some(t) => t,
        None => return ApiError::Unauthorized("missing session").into_response(),
    };

    let hash = hash_token(&token);
    let resolved = match resolve(&state, &hash).await {
        Ok(r) => r,
        Err(err) => return err.into_response(),
    };

    // Fire-and-forget last_seen_at update. Failures are logged but never
    // surface to the client — a stale timestamp is acceptable.
    let pool = state.db.clone();
    let hash_for_task = hash.clone();
    tokio::spawn(async move {
        if let Err(err) = sessions::touch_last_seen(&pool, &hash_for_task).await {
            tracing::warn!(error = %err, "touch session last_seen_at failed");
        }
    });

    request.extensions_mut().insert(resolved);
    next.run(request).await
}

/// Resolve a session hash against the database. Returns the
/// [`AuthenticatedSession`] on success or [`ApiError::Unauthorized`] for
/// every failure mode — the caller should not distinguish.
async fn resolve(state: &AppState, hash: &str) -> Result<AuthenticatedSession, ApiError> {
    let session = sessions::find_active(&state.db, hash)
        .await
        .map_err(|err| ApiError::Internal(anyhow::Error::from(err)))?
        .ok_or(ApiError::Unauthorized("unknown or expired session"))?;

    let user = users::find_by_id(&state.db, session.user_id)
        .await
        .map_err(|err| ApiError::Internal(anyhow::Error::from(err)))?
        .ok_or(ApiError::Unauthorized("orphaned session"))?;

    Ok(AuthenticatedSession { session, user })
}

/// Pull the `cellora_session` cookie value out of the headers. Returns
/// `None` for no Cookie header, a non-ASCII header, or no cookie of the
/// right name. Empty cookie values are also `None`.
fn extract_session_cookie(headers: &HeaderMap) -> Option<String> {
    let raw = headers.get(header::COOKIE)?.to_str().ok()?;
    for pair in raw.split(';') {
        let (key, value) = pair.trim().split_once('=')?;
        if key == COOKIE_NAME && !value.is_empty() {
            return Some(value.to_owned());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn extract_returns_none_when_cookie_header_is_absent() {
        let headers = HeaderMap::new();
        assert_eq!(extract_session_cookie(&headers), None);
    }

    #[test]
    fn extract_picks_the_session_cookie_among_others() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_static("foo=1; cellora_session=abc; bar=2"),
        );
        assert_eq!(
            extract_session_cookie(&headers),
            Some("abc".to_owned()),
        );
    }

    #[test]
    fn extract_returns_none_for_empty_cookie_value() {
        let mut headers = HeaderMap::new();
        headers.insert(header::COOKIE, HeaderValue::from_static("cellora_session="));
        assert_eq!(extract_session_cookie(&headers), None);
    }

    #[test]
    fn hash_is_deterministic_and_lowercase_hex() {
        let h1 = hash_token("token-value");
        let h2 = hash_token("token-value");
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 64);
        assert!(h1.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }

    #[test]
    fn generated_tokens_are_unique() {
        let a = generate_token();
        let b = generate_token();
        assert_ne!(a.plaintext, b.plaintext);
        assert_ne!(a.hash, b.hash);
        assert_eq!(a.hash, hash_token(&a.plaintext));
    }
}
