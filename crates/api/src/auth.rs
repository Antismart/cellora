//! Bearer-token authentication for protected routes.
//!
//! The flow:
//!
//! 1. Extract the bearer token from the `Authorization` header.
//! 2. Split it into `(prefix, secret)` per the [`crate::keys::split`]
//!    contract.
//! 3. Check the in-process [`AuthCache`]. On a hit, attach the cached
//!    [`AuthenticatedKey`] to request extensions and continue.
//! 4. On a miss, look the prefix up in Postgres and verify the secret
//!    against the stored Argon2id hash. On success, populate the cache
//!    and continue. On failure, return 401.
//!
//! Failures are deliberately uniform — every reason a request can fail
//! auth maps to the same opaque "unauthorized" response. The internal
//! variant is logged so operators can debug.
//!
//! `last_used_at` on the row is updated lazily in a fire-and-forget
//! task; the caller never waits on the write.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Request, State};
use axum::http::header;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use cellora_db::api_keys;
use cellora_db::models::ApiKeyTier;
use moka::future::Cache;

use crate::error::ApiError;
use crate::keys;
use crate::state::AppState;

/// What gets attached to request extensions after a successful auth.
/// Handlers can read this via `axum::Extension<AuthenticatedKey>` to
/// know which tier should drive rate-limit decisions.
#[derive(Debug, Clone)]
pub struct AuthenticatedKey {
    /// Public prefix of the key (e.g. `cell_a1b2c3d4`). Safe to log.
    pub prefix: String,
    /// Tier the key belongs to.
    pub tier: ApiKeyTier,
}

/// Lock-free cache that short-circuits Argon2 verification for
/// repeat-presented keys. Keyed on the full bearer token so a different
/// secret for the same prefix does not match.
#[derive(Debug, Clone)]
pub struct AuthCache {
    inner: Cache<String, Arc<AuthenticatedKey>>,
}

impl AuthCache {
    /// Build a cache with the supplied capacity and TTL.
    pub fn new(capacity: u64, ttl: Duration) -> Self {
        Self {
            inner: Cache::builder()
                .max_capacity(capacity)
                .time_to_live(ttl)
                .build(),
        }
    }

    async fn get(&self, key: &str) -> Option<Arc<AuthenticatedKey>> {
        self.inner.get(key).await
    }

    async fn insert(&self, key: String, value: AuthenticatedKey) {
        self.inner.insert(key, Arc::new(value)).await;
    }
}

/// Middleware that authenticates every request that reaches it. Attach
/// to the protected sub-router via `from_fn_with_state(...)`.
///
/// The bearer string is extracted from the request *before* any `await`
/// so we never hold `&Request` across a suspension point. `Request<Body>`
/// is not `Sync`, which means a borrow of it cannot be sent across
/// awaits — the resulting future would not be `Send`, and axum's layer
/// stack requires `Send` futures.
pub async fn middleware(
    State(state): State<AppState>,
    mut request: Request,
    next: Next,
) -> Response {
    let bearer = match extract_bearer(&request) {
        Ok(b) => b.to_owned(),
        Err(err) => return err.into_response(),
    };

    match resolve(&state, bearer).await {
        Ok(key) => {
            request.extensions_mut().insert((*key).clone());
            next.run(request).await
        }
        Err(err) => err.into_response(),
    }
}

/// Resolve the bearer string against the cache, then the database. On
/// cache miss this is the slow path: O(1) prefix lookup plus an Argon2
/// verification.
async fn resolve(state: &AppState, bearer: String) -> Result<Arc<AuthenticatedKey>, ApiError> {
    if let Some(cached) = state.auth_cache.get(&bearer).await {
        return Ok(cached);
    }

    let (prefix, secret) =
        keys::split(&bearer).map_err(|_| ApiError::Unauthorized("bad format"))?;

    let row = api_keys::find_active_by_prefix(&state.db, prefix)
        .await
        .map_err(|err| ApiError::Internal(anyhow::Error::from(err)))?
        .ok_or(ApiError::Unauthorized("unknown prefix"))?;

    keys::verify(secret, &row.secret_hash)
        .map_err(|_| ApiError::Unauthorized("secret mismatch"))?;

    let resolved = AuthenticatedKey {
        prefix: row.prefix.clone(),
        tier: row.tier,
    };
    state
        .auth_cache
        .insert(bearer.clone(), resolved.clone())
        .await;

    // Update last_used_at without blocking the response. Failures are
    // logged but never surface to the client — a stale timestamp is
    // acceptable.
    let pool = state.db.clone();
    let prefix_for_task = row.prefix.clone();
    tokio::spawn(async move {
        if let Err(err) = api_keys::touch_last_used(&pool, &prefix_for_task).await {
            tracing::warn!(error = %err, prefix = %prefix_for_task, "touch last_used_at failed");
        }
    });

    Ok(Arc::new(resolved))
}

/// Pull the bearer token out of the `Authorization` header. Returns
/// [`ApiError::Unauthorized`] for missing, non-ASCII, or non-Bearer
/// headers.
fn extract_bearer(request: &Request) -> Result<&str, ApiError> {
    let header_value = request
        .headers()
        .get(header::AUTHORIZATION)
        .ok_or(ApiError::Unauthorized("missing header"))?;
    let raw = header_value
        .to_str()
        .map_err(|_| ApiError::Unauthorized("non-ascii header"))?;
    let bearer = raw
        .strip_prefix("Bearer ")
        .ok_or(ApiError::Unauthorized("not bearer"))?;
    if bearer.is_empty() {
        return Err(ApiError::Unauthorized("empty token"));
    }
    Ok(bearer)
}
