//! Queries against the `users` table.
//!
//! Users are minted at the end of the GitHub OAuth callback. We key on
//! `github_user_id` (a stable numeric identifier) rather than the login
//! handle, which can change. Updating the login on every sign-in keeps the
//! display name fresh without rotating the primary key.

use sqlx::PgPool;
use uuid::Uuid;

use crate::error::DbResult;
use crate::models::User;

/// Insert a user keyed on `github_user_id`, or update the displayable
/// fields when one already exists. Returns the canonical row after the
/// upsert.
pub async fn upsert_from_github(
    pool: &PgPool,
    github_user_id: i64,
    github_login: &str,
    email: Option<&str>,
    avatar_url: Option<&str>,
) -> DbResult<User> {
    let row = sqlx::query_as!(
        User,
        r#"
        INSERT INTO users (github_user_id, github_login, email, avatar_url)
        VALUES ($1, $2, $3, $4)
        ON CONFLICT (github_user_id) DO UPDATE SET
            github_login = EXCLUDED.github_login,
            email = EXCLUDED.email,
            avatar_url = EXCLUDED.avatar_url,
            updated_at = now()
        RETURNING
            id,
            github_user_id,
            github_login,
            email,
            avatar_url,
            created_at,
            updated_at
        "#,
        github_user_id,
        github_login,
        email,
        avatar_url,
    )
    .fetch_one(pool)
    .await?;
    Ok(row)
}

/// Look up a user by primary key. Returns `None` when the row no longer
/// exists — the auth path treats that as the session being orphaned and
/// clears the cookie.
pub async fn find_by_id(pool: &PgPool, id: Uuid) -> DbResult<Option<User>> {
    let row = sqlx::query_as!(
        User,
        r#"
        SELECT
            id,
            github_user_id,
            github_login,
            email,
            avatar_url,
            created_at,
            updated_at
        FROM users
        WHERE id = $1
        "#,
        id,
    )
    .fetch_optional(pool)
    .await?;
    Ok(row)
}
