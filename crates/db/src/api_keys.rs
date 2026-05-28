//! Queries against the `api_keys` table.
//!
//! Issuance, lookup, listing, rotation, and revocation. The plaintext
//! secret is never stored — only the prefix (used to look up the row on
//! the auth hot path) and the Argon2id PHC string (used to verify the
//! secret half).
//!
//! Functions split into three groups:
//!
//! - **Hot path** (`find_active_by_prefix`, `touch_last_used`) — called
//!   on every authenticated `/v1/*` request; must stay cheap.
//! - **Operator** (`insert`, `list_all`, `revoke`) — used by the
//!   `cellora-api admin` CLI; not user-scoped.
//! - **Dashboard** (`list_for_user`, `find_for_user`, `rotate_for_user`,
//!   `revoke_for_user`) — used by the `/admin/keys/*` handlers; every
//!   query filters by `user_id` so one user cannot touch another's keys.

use sqlx::PgPool;
use uuid::Uuid;

use crate::error::DbResult;
use crate::models::{ApiKey, ApiKeyTier};

/// Insert a freshly issued API key owned by `user_id`. The caller hashes
/// the secret half and supplies the resulting PHC string; this layer
/// never sees the plaintext.
pub async fn insert(
    pool: &PgPool,
    user_id: Uuid,
    prefix: &str,
    secret_hash: &str,
    tier: ApiKeyTier,
    label: Option<&str>,
) -> DbResult<ApiKey> {
    let row = sqlx::query_as!(
        ApiKey,
        r#"
        INSERT INTO api_keys (user_id, prefix, secret_hash, tier, label)
        VALUES ($1, $2, $3, $4, $5)
        RETURNING
            id,
            user_id,
            prefix,
            secret_hash,
            tier AS "tier: ApiKeyTier",
            label,
            created_at,
            revoked_at,
            last_used_at
        "#,
        user_id,
        prefix,
        secret_hash,
        tier as ApiKeyTier,
        label,
    )
    .fetch_one(pool)
    .await?;
    Ok(row)
}

/// Look up an active (non-revoked) key by its plaintext prefix. Used on
/// the auth hot path; the verification cache in front of this means a
/// hit only requires one Argon2 compare per cache-miss request.
pub async fn find_active_by_prefix(pool: &PgPool, prefix: &str) -> DbResult<Option<ApiKey>> {
    let row = sqlx::query_as!(
        ApiKey,
        r#"
        SELECT
            id,
            user_id,
            prefix,
            secret_hash,
            tier AS "tier: ApiKeyTier",
            label,
            created_at,
            revoked_at,
            last_used_at
        FROM api_keys
        WHERE prefix = $1 AND revoked_at IS NULL
        "#,
        prefix,
    )
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// All keys ordered by creation time, newest first. Used by the
/// `admin list-keys` CLI subcommand. Not exposed via HTTP — the
/// dashboard surface uses `list_for_user` instead.
pub async fn list_all(pool: &PgPool) -> DbResult<Vec<ApiKey>> {
    let rows = sqlx::query_as!(
        ApiKey,
        r#"
        SELECT
            id,
            user_id,
            prefix,
            secret_hash,
            tier AS "tier: ApiKeyTier",
            label,
            created_at,
            revoked_at,
            last_used_at
        FROM api_keys
        ORDER BY created_at DESC
        "#,
    )
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Mark a key as revoked by prefix. Used by the operator CLI; the
/// dashboard surface uses `revoke_for_user` so it cannot touch another
/// user's keys. Returns `true` when a row was updated, `false` when no
/// matching active key was found.
pub async fn revoke(pool: &PgPool, prefix: &str) -> DbResult<bool> {
    let result = sqlx::query!(
        r#"
        UPDATE api_keys
        SET revoked_at = now()
        WHERE prefix = $1 AND revoked_at IS NULL
        "#,
        prefix,
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// Update `last_used_at` to the current wall-clock time. Called from a
/// fire-and-forget tokio task on the auth path so it never blocks the
/// response.
pub async fn touch_last_used(pool: &PgPool, prefix: &str) -> DbResult<()> {
    sqlx::query!(
        r#"
        UPDATE api_keys
        SET last_used_at = now()
        WHERE prefix = $1
        "#,
        prefix,
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Every key (active and revoked) owned by `user_id`, newest first.
/// Backs `GET /admin/keys`. Includes revoked rows so the dashboard can
/// render their status; the auth hot path filters them out separately.
pub async fn list_for_user(pool: &PgPool, user_id: Uuid) -> DbResult<Vec<ApiKey>> {
    let rows = sqlx::query_as!(
        ApiKey,
        r#"
        SELECT
            id,
            user_id,
            prefix,
            secret_hash,
            tier AS "tier: ApiKeyTier",
            label,
            created_at,
            revoked_at,
            last_used_at
        FROM api_keys
        WHERE user_id = $1
        ORDER BY created_at DESC
        "#,
        user_id,
    )
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

/// Look up a key by its surrogate `id`, scoped to a specific owning
/// user. Returns `None` both when the id is unknown and when the row
/// belongs to a different user — handlers map either case to 404 so
/// existence of one user's keys does not leak to another.
pub async fn find_for_user(pool: &PgPool, id: Uuid, user_id: Uuid) -> DbResult<Option<ApiKey>> {
    let row = sqlx::query_as!(
        ApiKey,
        r#"
        SELECT
            id,
            user_id,
            prefix,
            secret_hash,
            tier AS "tier: ApiKeyTier",
            label,
            created_at,
            revoked_at,
            last_used_at
        FROM api_keys
        WHERE id = $1 AND user_id = $2
        "#,
        id,
        user_id,
    )
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// Replace an active key's prefix and secret hash atomically. Used by
/// `POST /admin/keys/:id/rotate`. Returns the updated row, or `None`
/// when no active key matches `(id, user_id)` — handlers should respond
/// 404 in that case.
pub async fn rotate_for_user(
    pool: &PgPool,
    id: Uuid,
    user_id: Uuid,
    new_prefix: &str,
    new_secret_hash: &str,
) -> DbResult<Option<ApiKey>> {
    let row = sqlx::query_as!(
        ApiKey,
        r#"
        UPDATE api_keys
        SET prefix = $3,
            secret_hash = $4,
            last_used_at = NULL
        WHERE id = $1 AND user_id = $2 AND revoked_at IS NULL
        RETURNING
            id,
            user_id,
            prefix,
            secret_hash,
            tier AS "tier: ApiKeyTier",
            label,
            created_at,
            revoked_at,
            last_used_at
        "#,
        id,
        user_id,
        new_prefix,
        new_secret_hash,
    )
    .fetch_optional(pool)
    .await?;
    Ok(row)
}

/// Mark a key revoked, scoped to its owning user. Returns `true` when a
/// row was updated; `false` covers both "id unknown", "wrong user", and
/// "already revoked" — handlers should respond 404 in all three cases.
pub async fn revoke_for_user(pool: &PgPool, id: Uuid, user_id: Uuid) -> DbResult<bool> {
    let result = sqlx::query!(
        r#"
        UPDATE api_keys
        SET revoked_at = now()
        WHERE id = $1 AND user_id = $2 AND revoked_at IS NULL
        "#,
        id,
        user_id,
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected() > 0)
}
