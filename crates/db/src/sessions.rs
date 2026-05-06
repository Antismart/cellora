//! Queries against the `sessions` table.
//!
//! A session row is keyed on the SHA-256 hash of the cookie value, hex-encoded.
//! Hashing the token before storing it means a database compromise does not
//! yield a set of ready-to-use cookies; an attacker would still need the
//! plaintext value, which only ever exists in the user's browser and the
//! response that issued it.

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::error::DbResult;
use crate::models::Session;

/// Insert a freshly minted session row. The caller computes
/// `SHA-256(plaintext_token)` and supplies the hex digest as `token_hash`.
pub async fn insert(
    pool: &PgPool,
    token_hash: &str,
    user_id: Uuid,
    expires_at: DateTime<Utc>,
    user_agent: Option<&str>,
) -> DbResult<Session> {
    let row = sqlx::query_as!(
        Session,
        r#"
        INSERT INTO sessions (token_hash, user_id, expires_at, user_agent)
        VALUES ($1, $2, $3, $4)
        RETURNING
            token_hash,
            user_id,
            created_at,
            expires_at,
            last_seen_at,
            user_agent
        "#,
        token_hash,
        user_id,
        expires_at,
        user_agent,
    )
    .fetch_one(pool)
    .await?;
    Ok(row)
}

/// Resolve a session by its token hash. Returns `None` when the row is
/// missing or already past its expiry.
pub async fn find_active(pool: &PgPool, token_hash: &str) -> DbResult<Option<Session>> {
    let row = sqlx::query_as!(
        Session,
        r#"
        SELECT
            token_hash,
            user_id,
            created_at,
            expires_at,
            last_seen_at,
            user_agent
        FROM sessions
        WHERE token_hash = $1 AND expires_at > now()
        "#,
        token_hash,
    )
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// Update `last_seen_at` to wall-clock now. Called from a fire-and-forget
/// task on the auth path so it never blocks the response.
pub async fn touch_last_seen(pool: &PgPool, token_hash: &str) -> DbResult<()> {
    sqlx::query!(
        r#"
        UPDATE sessions SET last_seen_at = now() WHERE token_hash = $1
        "#,
        token_hash,
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Delete a session row. Used by sign-out and by the periodic cleanup
/// task when an expired row is reaped early. Returns `true` when a row
/// was deleted, `false` when the hash did not match anything.
pub async fn delete(pool: &PgPool, token_hash: &str) -> DbResult<bool> {
    let result = sqlx::query!(
        r#"
        DELETE FROM sessions WHERE token_hash = $1
        "#,
        token_hash,
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// Delete every session row whose `expires_at` is in the past. Returns
/// the number of rows removed. Run periodically to keep the table small;
/// the auth path already filters by expiry so stale rows are harmless
/// other than as table bloat.
pub async fn delete_expired(pool: &PgPool) -> DbResult<u64> {
    let result = sqlx::query!(
        r#"
        DELETE FROM sessions WHERE expires_at <= now()
        "#,
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}
