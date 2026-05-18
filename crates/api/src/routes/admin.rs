//! Dashboard admin routes — session-cookie authenticated.
//!
//! These endpoints back the `dashboard/` frontend. They share the
//! [`crate::session::middleware`] for auth and never accept bearer tokens —
//! API keys are for programmatic clients on `/v1/*`, sessions are for
//! humans on `/admin/*`.
//!
//! Slice 2 ships `GET /admin/me`. Key management, usage charts, and the
//! API explorer arrive in later slices of Week 5.

use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::{Extension, Json};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::{Duration as ChronoDuration, Utc};
use rand::RngCore;
use redis::AsyncCommands;
use serde::Serialize;
use serde::Deserialize;

use cellora_db::{sessions, users};

use crate::session::AuthenticatedSession;
use crate::{error::ApiError, session};
use crate::state::AppState;

/// `GET /admin/me` — current dashboard user.
///
/// Returns the public profile fields of the authenticated user. The
/// internal `github_user_id` is deliberately not surfaced; the
/// dashboard never needs it.
pub async fn me(
    Extension(auth): Extension<AuthenticatedSession>,
) -> Json<MeResponse> {
    Json(MeResponse {
        user: UserView::from(auth.user),
    })
}

/// `GET /admin/oauth/github/start` — redirect to GitHub OAuth.
pub async fn github_start(State(state): State<AppState>) -> Result<Redirect, ApiError> {
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
    Ok(Redirect::to(&authorize_url))
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
    let dashboard_redirect = config
        .dashboard_redirect_url
        .as_deref()
        .unwrap_or("/");

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
    let email = fetch_github_email(&client, &token)
        .await
        .unwrap_or(None);

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

    let cookie = build_session_cookie(&issued.plaintext, config.dashboard_cookie_secure, expires_at);
    let mut response = Redirect::to(dashboard_redirect).into_response();
    response.headers_mut().insert(header::SET_COOKIE, cookie);
    Ok(response)
}

/// `POST /admin/sign-out` — clear the session cookie.
pub async fn sign_out(State(state): State<AppState>, headers: HeaderMap) -> Result<Response, ApiError> {
    if let Some(token) = session::extract_session_cookie(&headers) {
        let hash = session::hash_token(&token);
        let _ = sessions::delete(&state.db, &hash).await;
    }
    let cookie = clear_session_cookie(state.config.dashboard_cookie_secure);
    let mut response = Response::builder()
        .status(StatusCode::NO_CONTENT)
        .body(axum::body::Body::empty())
        .unwrap_or_else(|_| StatusCode::NO_CONTENT.into_response());
    response.headers_mut().insert(header::SET_COOKIE, cookie);
    Ok(response)
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
        .or_else(|| payload.iter().find(|e| e.visibility.as_deref() == Some("public")))
        .map(|e| e.email.clone())
}

fn build_session_cookie(token: &str, secure: bool, expires_at: chrono::DateTime<Utc>) -> HeaderValue {
    let max_age = expires_at.timestamp().saturating_sub(Utc::now().timestamp());
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
