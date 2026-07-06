//! Dashboard admin routes — session-cookie authenticated.
//!
//! These endpoints back the `dashboard/` frontend. They share the
//! [`crate::session::middleware`] for auth and never accept bearer tokens —
//! API keys are for programmatic clients on `/v1/*`, sessions are for
//! humans on `/admin/*`.
//!
//! Slice 2 shipped `GET /admin/me`. Slice 5 adds API-key CRUD
//! (`GET /admin/keys`, `POST /admin/keys`, `POST /admin/keys/:id/rotate`,
//! `DELETE /admin/keys/:id`) and `POST /admin/sign-out`. Usage charts and
//! the API explorer arrive in later slices of Week 5.

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::{Extension, Json};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use rand::RngCore;
use redis::AsyncCommands;
use serde::Deserialize;
use serde::Serialize;
use utoipa::ToSchema;
use uuid::Uuid;

use cellora_db::models::{ApiKey, ApiKeyTier};
use cellora_db::{api_keys, sessions, users};

use crate::error::ErrorEnvelope;
use crate::keys;
use crate::session::AuthenticatedSession;
use crate::state::AppState;
use crate::{error::ApiError, session};

/// `GET /admin/me` — current dashboard user.
///
/// Returns the public profile fields of the authenticated user. The
/// internal `github_user_id` is deliberately not surfaced; the
/// dashboard never needs it.
pub async fn me(Extension(auth): Extension<AuthenticatedSession>) -> Json<MeResponse> {
    Json(MeResponse {
        user: UserView::from(auth.user),
    })
}

/// Cookie carrying the OAuth `state` value. `github_start` sets it and
/// `github_callback` requires it to match the `state` query parameter, which
/// binds the callback to the browser that began the flow (login-CSRF defence).
const OAUTH_STATE_COOKIE: &str = "cellora_oauth_state";

/// `GET /admin/oauth/github/start` — redirect to GitHub OAuth.
pub async fn github_start(State(state): State<AppState>) -> Result<Response, ApiError> {
    let config = &state.config;
    let client_id = config
        .dashboard_oauth_github_client_id
        .as_deref()
        .ok_or_else(|| ApiError::UpstreamUnavailable("oauth not configured"))?;
    let redirect_url = config
        .dashboard_oauth_github_redirect_url
        .as_deref()
        .ok_or_else(|| ApiError::UpstreamUnavailable("oauth not configured"))?;

    let Some(manager) = state.redis.as_ref() else {
        return Err(ApiError::UpstreamUnavailable("redis unavailable"));
    };
    let mut conn = manager.clone();

    let state_token = generate_state_token();
    let key = format!("cellora:oauth:github:state:{state_token}");
    let inserted: bool = redis::cmd("SET")
        .arg(&key)
        .arg("1")
        .arg("EX")
        .arg(600)
        .arg("NX")
        .query_async(&mut conn)
        .await
        .map_err(|_| ApiError::UpstreamUnavailable("redis unavailable"))?;
    if !inserted {
        return Err(ApiError::UpstreamUnavailable("state collision"));
    }

    let authorize_url = format!(
        "https://github.com/login/oauth/authorize?client_id={client_id}&redirect_uri={redirect_url}&scope=read:user%20user:email&state={state_token}&allow_signup=true"
    );

    // Also stash the state in an HttpOnly cookie. The Redis entry alone is a
    // global bearer value — proving only that *some* browser started a flow.
    // The cookie proves the callback returned to *this* browser.
    let cookie = build_oauth_state_cookie(&state_token, config.dashboard_cookie_secure);
    let mut response = Redirect::to(&authorize_url).into_response();
    response.headers_mut().insert(header::SET_COOKIE, cookie);
    Ok(response)
}

/// `GET /admin/oauth/github/callback` — exchange code, mint session, redirect.
pub async fn github_callback(
    State(state): State<AppState>,
    Query(query): Query<GithubCallback>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    if let Some(error) = query.error.as_deref() {
        return Err(ApiError::BadRequest(format!("oauth error: {error}")));
    }
    let code = query
        .code
        .ok_or_else(|| ApiError::BadRequest("missing code".into()))?;
    let state_token = query
        .state
        .ok_or_else(|| ApiError::BadRequest("missing state".into()))?;

    // Bind the callback to the browser that began the flow: the `state` value
    // must also be present in the HttpOnly cookie set by `github_start`. A
    // valid Redis entry alone is a global bearer value and lets an attacker
    // deliver their own (code, state) to a victim (login CSRF / fixation).
    match read_cookie(&headers, OAUTH_STATE_COOKIE) {
        Some(ref cookie_state) if cookie_state == &state_token => {}
        _ => return Err(ApiError::Unauthorized("invalid oauth state")),
    }

    let config = &state.config;
    let client_id = config
        .dashboard_oauth_github_client_id
        .as_deref()
        .ok_or_else(|| ApiError::UpstreamUnavailable("oauth not configured"))?;
    let client_secret = config
        .dashboard_oauth_github_client_secret
        .as_deref()
        .ok_or_else(|| ApiError::UpstreamUnavailable("oauth not configured"))?;
    let redirect_url = config
        .dashboard_oauth_github_redirect_url
        .as_deref()
        .ok_or_else(|| ApiError::UpstreamUnavailable("oauth not configured"))?;
    let dashboard_redirect = config.dashboard_redirect_url.as_deref().unwrap_or("/");

    let Some(manager) = state.redis.as_ref() else {
        return Err(ApiError::UpstreamUnavailable("redis unavailable"));
    };
    let mut conn = manager.clone();
    let key = format!("cellora:oauth:github:state:{state_token}");
    let exists: Option<String> = conn
        .get(&key)
        .await
        .map_err(|_| ApiError::UpstreamUnavailable("redis unavailable"))?;
    if exists.is_none() {
        return Err(ApiError::Unauthorized("invalid oauth state"));
    }
    let _: () = conn
        .del(&key)
        .await
        .map_err(|_| ApiError::UpstreamUnavailable("redis unavailable"))?;

    let client = reqwest::Client::new();
    let token = exchange_github_code(&client, client_id, client_secret, redirect_url, &code)
        .await
        .map_err(|_| ApiError::UpstreamUnavailable("github oauth failed"))?;
    let profile = fetch_github_profile(&client, &token)
        .await
        .map_err(|_| ApiError::UpstreamUnavailable("github profile failed"))?;
    let email = fetch_github_email(&client, &token).await.unwrap_or(None);

    let user = users::upsert_from_github(
        &state.db,
        profile.id,
        &profile.login,
        email.as_deref(),
        profile.avatar_url.as_deref(),
    )
    .await
    .map_err(ApiError::from)?;

    let issued = session::generate_token();
    let expires_at = Utc::now() + ChronoDuration::days(config.dashboard_session_ttl_days);
    let user_agent = headers
        .get(header::USER_AGENT)
        .and_then(|h| h.to_str().ok());
    sessions::insert(&state.db, &issued.hash, user.id, expires_at, user_agent)
        .await
        .map_err(ApiError::from)?;

    let cookie = build_session_cookie(
        &issued.plaintext,
        config.dashboard_cookie_secure,
        expires_at,
    );
    let mut response = Redirect::to(dashboard_redirect).into_response();
    response.headers_mut().insert(header::SET_COOKIE, cookie);
    // Clear the now-consumed state cookie so it cannot be replayed.
    response.headers_mut().append(
        header::SET_COOKIE,
        clear_oauth_state_cookie(config.dashboard_cookie_secure),
    );
    Ok(response)
}

/// `POST /admin/sign-out` — delete the current session and clear the cookie.
///
/// Sits behind the session middleware, so a missing or invalid cookie is
/// rejected with 401 before this handler runs. On a valid session we
/// delete the matching row and return 204 with a `Set-Cookie` header that
/// instructs the browser to drop the cookie. Other sessions for the same
/// user are untouched — sign-out closes the current browser only.
pub async fn sign_out(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedSession>,
) -> Result<Response, ApiError> {
    sessions::delete(&state.db, &auth.session.token_hash)
        .await
        .map_err(ApiError::from)?;

    let cookie = clear_session_cookie(state.config.dashboard_cookie_secure);
    let mut response = Response::builder()
        .status(StatusCode::NO_CONTENT)
        .body(axum::body::Body::empty())
        .unwrap_or_else(|_| StatusCode::NO_CONTENT.into_response());
    response.headers_mut().insert(header::SET_COOKIE, cookie);
    Ok(response)
}

/// `GET /admin/keys` — list every API key owned by the current user.
///
/// Returns active and revoked keys, newest first. The response never
/// includes secret material; clients must record the plaintext at
/// creation time via `POST /admin/keys`.
pub async fn list_keys(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedSession>,
) -> Result<Json<ListKeysResponse>, ApiError> {
    let rows = api_keys::list_for_user(&state.db, auth.user.id)
        .await
        .map_err(ApiError::from)?;
    Ok(Json(ListKeysResponse {
        keys: rows.into_iter().map(KeyView::from).collect(),
    }))
}

/// `POST /admin/keys` — issue a new API key for the current user.
///
/// The request body may omit `tier`, in which case the key is created at
/// the free tier. Paid tiers (starter, pro) are accepted; the dashboard
/// is responsible for gating selection of paid tiers behind a billing
/// check. The response carries the full `cell_<prefix>_<secret>` string
/// in `secret` exactly once — the database stores only the prefix and
/// the Argon2id PHC hash, so the plaintext is unrecoverable later.
pub async fn create_key(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedSession>,
    Json(body): Json<CreateKeyRequest>,
) -> Result<(StatusCode, Json<CreateKeyResponse>), ApiError> {
    let tier = body.tier.unwrap_or(TierInput::Free).into();
    let label = body
        .label
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let issued =
        keys::generate().map_err(|err| ApiError::Internal(anyhow::anyhow!(err.to_string())))?;
    let row = api_keys::insert(
        &state.db,
        auth.user.id,
        &issued.prefix,
        &issued.secret_hash,
        tier,
        label,
    )
    .await
    .map_err(ApiError::from)?;

    Ok((
        StatusCode::CREATED,
        Json(CreateKeyResponse {
            key: KeyView::from(row),
            secret: issued.full,
        }),
    ))
}

/// `POST /admin/keys/:id/rotate` — issue a fresh prefix and secret for
/// an existing key while preserving its surrogate `id`, tier, and label.
///
/// Returns 404 when the id is malformed, unknown, owned by another user,
/// or already revoked — every case maps to the same response so callers
/// cannot enumerate other users' keys. The new `secret` is returned once
/// and never persisted in plaintext.
///
/// Note: when a key is rotated, the in-process verification cache
/// (`AppState::auth_cache`) may still hold the OLD `cell_<prefix>_<secret>`
/// string until its TTL expires (default 60s). The old key continues to
/// authenticate until then. Cache invalidation on rotation would require
/// a cross-instance fan-out (Redis pub/sub or a versioned cache key) that
/// is deferred until billing-grade key management lands in a later week.
pub async fn rotate_key(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedSession>,
    Path(raw_id): Path<String>,
) -> Result<Json<CreateKeyResponse>, ApiError> {
    let id = parse_uuid(&raw_id)?;

    let issued =
        keys::generate().map_err(|err| ApiError::Internal(anyhow::anyhow!(err.to_string())))?;
    let row = api_keys::rotate_for_user(
        &state.db,
        id,
        auth.user.id,
        &issued.prefix,
        &issued.secret_hash,
    )
    .await
    .map_err(ApiError::from)?
    .ok_or(ApiError::NotFound("api key not found"))?;

    Ok(Json(CreateKeyResponse {
        key: KeyView::from(row),
        secret: issued.full,
    }))
}

/// `DELETE /admin/keys/:id` — revoke an API key.
///
/// Idempotent only in the trivial sense: a second revoke on the same id
/// returns 404 because the row is already revoked. Unknown ids, ids
/// belonging to other users, and already-revoked ids all map to 404 for
/// the same reason as [`rotate_key`].
///
/// Note: the same auth-cache caveat as `rotate_key` applies. A revoked
/// key still authenticates for up to one cache TTL window after the
/// revoke lands. Operators relying on instant cutoff should treat the
/// cache TTL (`CELLORA_API_AUTH_CACHE_TTL_SECONDS`) as the worst-case
/// staleness budget.
pub async fn revoke_key(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthenticatedSession>,
    Path(raw_id): Path<String>,
) -> Result<Response, ApiError> {
    let id = parse_uuid(&raw_id)?;

    let revoked = api_keys::revoke_for_user(&state.db, id, auth.user.id)
        .await
        .map_err(ApiError::from)?;
    if !revoked {
        return Err(ApiError::NotFound("api key not found"));
    }

    let response = Response::builder()
        .status(StatusCode::NO_CONTENT)
        .body(axum::body::Body::empty())
        .unwrap_or_else(|_| StatusCode::NO_CONTENT.into_response());
    Ok(response)
}

/// Parse a path-segment UUID. Malformed input maps to 404 (not 400) so
/// callers cannot probe for valid id shapes — a request for a
/// nonsensical id is indistinguishable from a request for a real id
/// owned by another user.
fn parse_uuid(raw: &str) -> Result<Uuid, ApiError> {
    Uuid::parse_str(raw).map_err(|_| ApiError::NotFound("api key not found"))
}

/// Response body for `GET /admin/me`.
#[derive(Debug, Serialize)]
pub struct MeResponse {
    /// The signed-in user.
    pub user: UserView,
}

/// Public projection of a user record. Only the fields the dashboard
/// renders — no internal identifiers, no timestamps.
#[derive(Debug, Serialize)]
pub struct UserView {
    /// Stable opaque ID for the user. Used as a key in API requests
    /// from the dashboard.
    pub id: uuid::Uuid,
    /// GitHub login at last sign-in. Mutable upstream.
    pub github_login: String,
    /// Public email reported by GitHub, when available.
    pub email: Option<String>,
    /// Avatar URL reported by GitHub, when available.
    pub avatar_url: Option<String>,
}

impl From<cellora_db::models::User> for UserView {
    fn from(user: cellora_db::models::User) -> Self {
        Self {
            id: user.id,
            github_login: user.github_login,
            email: user.email,
            avatar_url: user.avatar_url,
        }
    }
}

/// Public projection of an `api_keys` row. Carries every field the
/// dashboard needs to render the key-management table; the `secret_hash`
/// column is deliberately omitted because it has no display value and
/// surfacing it would only invite confusion with the plaintext secret.
#[derive(Debug, Serialize, ToSchema)]
pub struct KeyView {
    /// Stable surrogate identifier. Used in `/admin/keys/:id/*` paths.
    #[schema(value_type = String, example = "00000000-0000-0000-0000-000000000000")]
    pub id: Uuid,
    /// Operator-supplied label, or `null`.
    pub label: Option<String>,
    /// Subscription tier — `"free"`, `"starter"`, or `"pro"`.
    #[schema(value_type = String, example = "free")]
    pub tier: &'static str,
    /// Public prefix of the key (e.g. `cell_a1b2c3d4`). Safe to log or
    /// display; uniquely identifies the row for support workflows.
    pub prefix: String,
    /// `"active"` when the key still authenticates, `"revoked"` once it
    /// has been removed. Derived from the `revoked_at` timestamp.
    #[schema(value_type = String, example = "active")]
    pub status: &'static str,
    /// When the key was first issued.
    pub created_at: DateTime<Utc>,
    /// When the key was revoked, or `null` for active keys.
    pub revoked_at: Option<DateTime<Utc>>,
    /// Most recent time the key was presented on the auth hot path.
    /// Updated lazily so the value can lag wall-clock by a small amount.
    pub last_used_at: Option<DateTime<Utc>>,
}

impl From<ApiKey> for KeyView {
    fn from(row: ApiKey) -> Self {
        let status = if row.revoked_at.is_some() {
            "revoked"
        } else {
            "active"
        };
        Self {
            id: row.id,
            label: row.label,
            tier: row.tier.as_str(),
            prefix: row.prefix,
            status,
            created_at: row.created_at,
            revoked_at: row.revoked_at,
            last_used_at: row.last_used_at,
        }
    }
}

/// Response body for `GET /admin/keys`.
#[derive(Debug, Serialize, ToSchema)]
pub struct ListKeysResponse {
    /// All keys belonging to the signed-in user, newest first.
    pub keys: Vec<KeyView>,
}

/// Request body for `POST /admin/keys`.
#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateKeyRequest {
    /// Optional human-readable label. Empty strings and whitespace-only
    /// values are coerced to `null`.
    #[serde(default)]
    pub label: Option<String>,
    /// Subscription tier. Defaults to `free` when omitted. Paid tiers
    /// are accepted on the wire; gating is the dashboard's job.
    #[serde(default)]
    pub tier: Option<TierInput>,
}

/// Tier value as supplied in a JSON request body. Lower-cased to match
/// the storage form and the existing CLI flag.
#[derive(Debug, Clone, Copy, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum TierInput {
    /// Free tier.
    Free,
    /// Starter tier.
    Starter,
    /// Pro tier.
    Pro,
}

impl From<TierInput> for ApiKeyTier {
    fn from(value: TierInput) -> Self {
        match value {
            TierInput::Free => Self::Free,
            TierInput::Starter => Self::Starter,
            TierInput::Pro => Self::Pro,
        }
    }
}

/// Response body for `POST /admin/keys` and `POST /admin/keys/:id/rotate`.
#[derive(Debug, Serialize, ToSchema)]
pub struct CreateKeyResponse {
    /// The freshly issued or rotated key row.
    pub key: KeyView,
    /// The full `cell_<prefix>_<secret>` string. Shown only on this
    /// response — the database stores only the prefix and Argon2id PHC
    /// hash, so the secret half is unrecoverable after this point.
    pub secret: String,
}

/// `GET /admin/keys` — schema-only stub for the OpenAPI generator.
///
/// The actual handler ([`list_keys`]) takes a session extension that
/// utoipa cannot derive parameters for, so we expose a thin wrapper as
/// the OpenAPI surface. The route is still wired to [`list_keys`].
#[utoipa::path(
    get,
    path = "/admin/keys",
    tag = "admin",
    responses(
        (status = 200, description = "Keys belonging to the signed-in user", body = ListKeysResponse),
        (status = 401, description = "Missing or invalid session", body = ErrorEnvelope),
    ),
)]
#[allow(dead_code)]
pub fn list_keys_doc() {}

/// `POST /admin/keys` — schema stub.
#[utoipa::path(
    post,
    path = "/admin/keys",
    tag = "admin",
    request_body = CreateKeyRequest,
    responses(
        (status = 201, description = "Key created; full secret returned once", body = CreateKeyResponse),
        (status = 401, description = "Missing or invalid session", body = ErrorEnvelope),
    ),
)]
#[allow(dead_code)]
pub fn create_key_doc() {}

/// `POST /admin/keys/:id/rotate` — schema stub.
#[utoipa::path(
    post,
    path = "/admin/keys/{id}/rotate",
    tag = "admin",
    params(("id" = String, Path, description = "Surrogate id of the key to rotate.")),
    responses(
        (status = 200, description = "Key rotated; fresh secret returned once", body = CreateKeyResponse),
        (status = 401, description = "Missing or invalid session", body = ErrorEnvelope),
        (status = 404, description = "Unknown id, wrong owner, or already revoked", body = ErrorEnvelope),
    ),
)]
#[allow(dead_code)]
pub fn rotate_key_doc() {}

/// `DELETE /admin/keys/:id` — schema stub.
#[utoipa::path(
    delete,
    path = "/admin/keys/{id}",
    tag = "admin",
    params(("id" = String, Path, description = "Surrogate id of the key to revoke.")),
    responses(
        (status = 204, description = "Key revoked"),
        (status = 401, description = "Missing or invalid session", body = ErrorEnvelope),
        (status = 404, description = "Unknown id, wrong owner, or already revoked", body = ErrorEnvelope),
    ),
)]
#[allow(dead_code)]
pub fn revoke_key_doc() {}

/// Query parameters sent by GitHub's OAuth callback redirect.
#[derive(Debug, Deserialize)]
pub struct GithubCallback {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GithubTokenResponse {
    access_token: String,
}

#[derive(Debug, Deserialize)]
struct GithubProfile {
    id: i64,
    login: String,
    avatar_url: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GithubEmail {
    email: String,
    verified: bool,
    primary: bool,
    visibility: Option<String>,
}

fn generate_state_token() -> String {
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

async fn exchange_github_code(
    client: &reqwest::Client,
    client_id: &str,
    client_secret: &str,
    redirect_url: &str,
    code: &str,
) -> Result<String, anyhow::Error> {
    let response = client
        .post("https://github.com/login/oauth/access_token")
        .header("accept", "application/json")
        .form(&[
            ("client_id", client_id),
            ("client_secret", client_secret),
            ("code", code),
            ("redirect_uri", redirect_url),
        ])
        .send()
        .await?
        .error_for_status()?;
    let payload: GithubTokenResponse = response.json().await?;
    Ok(payload.access_token)
}

async fn fetch_github_profile(
    client: &reqwest::Client,
    token: &str,
) -> Result<GithubProfile, anyhow::Error> {
    let response = client
        .get("https://api.github.com/user")
        .bearer_auth(token)
        .header("user-agent", "cellora-api")
        .send()
        .await?
        .error_for_status()?;
    let payload = response.json().await?;
    Ok(payload)
}

async fn fetch_github_email(
    client: &reqwest::Client,
    token: &str,
) -> Result<Option<String>, anyhow::Error> {
    let response = client
        .get("https://api.github.com/user/emails")
        .bearer_auth(token)
        .header("user-agent", "cellora-api")
        .send()
        .await?
        .error_for_status()?;
    let payload: Vec<GithubEmail> = response.json().await?;
    Ok(select_email(&payload))
}

fn select_email(payload: &[GithubEmail]) -> Option<String> {
    payload
        .iter()
        .find(|e| e.primary && e.verified)
        .or_else(|| payload.iter().find(|e| e.verified))
        .or_else(|| {
            payload
                .iter()
                .find(|e| e.visibility.as_deref() == Some("public"))
        })
        .map(|e| e.email.clone())
}

fn build_session_cookie(
    token: &str,
    secure: bool,
    expires_at: chrono::DateTime<Utc>,
) -> HeaderValue {
    let max_age = expires_at
        .timestamp()
        .saturating_sub(Utc::now().timestamp());
    let mut cookie = format!(
        "{}={}; Path=/; HttpOnly; SameSite=Lax; Max-Age={}",
        session::COOKIE_NAME,
        token,
        max_age,
    );
    if secure {
        cookie.push_str("; Secure");
    }
    HeaderValue::from_str(&cookie).unwrap_or_else(|_| HeaderValue::from_static(""))
}

fn clear_session_cookie(secure: bool) -> HeaderValue {
    let mut cookie = format!(
        "{}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0",
        session::COOKIE_NAME
    );
    if secure {
        cookie.push_str("; Secure");
    }
    HeaderValue::from_str(&cookie).unwrap_or_else(|_| HeaderValue::from_static(""))
}

/// Short-lived HttpOnly cookie holding the OAuth `state` for the duration of
/// the round-trip to GitHub (10 minutes, matching the Redis entry's TTL).
fn build_oauth_state_cookie(state_token: &str, secure: bool) -> HeaderValue {
    let mut cookie =
        format!("{OAUTH_STATE_COOKIE}={state_token}; Path=/; HttpOnly; SameSite=Lax; Max-Age=600");
    if secure {
        cookie.push_str("; Secure");
    }
    HeaderValue::from_str(&cookie).unwrap_or_else(|_| HeaderValue::from_static(""))
}

/// Expire the OAuth state cookie once the callback has consumed it.
fn clear_oauth_state_cookie(secure: bool) -> HeaderValue {
    let mut cookie = format!("{OAUTH_STATE_COOKIE}=; Path=/; HttpOnly; SameSite=Lax; Max-Age=0");
    if secure {
        cookie.push_str("; Secure");
    }
    HeaderValue::from_str(&cookie).unwrap_or_else(|_| HeaderValue::from_static(""))
}

/// Read a named cookie value out of the request headers, or `None` if the
/// header is absent, non-ASCII, or has no cookie of that name.
fn read_cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    let raw = headers.get(header::COOKIE)?.to_str().ok()?;
    for pair in raw.split(';') {
        if let Some((key, value)) = pair.trim().split_once('=') {
            if key == name && !value.is_empty() {
                return Some(value.to_owned());
            }
        }
    }
    None
}
