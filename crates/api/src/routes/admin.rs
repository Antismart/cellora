//! Dashboard admin routes — session-cookie authenticated.
//!
//! These endpoints back the `dashboard/` frontend. They share the
//! [`crate::session::middleware`] for auth and never accept bearer tokens —
//! API keys are for programmatic clients on `/v1/*`, sessions are for
//! humans on `/admin/*`.
//!
//! Slice 2 ships `GET /admin/me`. Key management, usage charts, and the
//! API explorer arrive in later slices of Week 5.

use axum::Extension;
use axum::Json;
use serde::Serialize;

use crate::session::AuthenticatedSession;

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
