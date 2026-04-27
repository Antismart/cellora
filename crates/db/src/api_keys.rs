//! Queries against the `api_keys` table.
//!
//! Issuance, lookup, listing and revocation. The plaintext secret is never
//! stored — only the prefix (used to look up the row) and the Argon2id PHC
//! string (used to verify the secret half).

use sqlx::PgPool;

use crate::error::DbResult;
use crate::models::{ApiKey, ApiKeyTier};

/// Insert a freshly issued API key. The caller hashes the secret half and
/// supplies the resulting PHC string; the model never sees the plaintext.
pub async fn insert(
    pool: &PgPool,
    prefix: &str,
    secret_hash: &str,
    tier: ApiKeyTier,
    label: Option<&str>,
) -> DbResult<ApiKey> {
    let row = sqlx::query_as!(
        ApiKey,
        r#"
        INSERT INTO api_keys (prefix, secret_hash, tier, label)
        VALUES ($1, $2, $3, $4)
        RETURNING
            prefix,
            secret_hash,
            tier AS "tier: ApiKeyTier",
            label,
            created_at,
            revoked_at,
            last_used_at
        "#,
        prefix,
        secret_hash,
        tier as ApiKeyTier,
        label,
    )
    .fetch_one(pool)
    .await?;
    Ok(row)
}

/// Look up an active (non-revoked) key by its plaintext prefix.
pub async fn find_active_by_prefix(pool: &PgPool, prefix: &str) -> DbResult<Option<ApiKey>> {
    let row = sqlx::query_as!(
        ApiKey,
        r#"
        SELECT
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
/// `admin list-keys` CLI subcommand.
pub async fn list_all(pool: &PgPool) -> DbResult<Vec<ApiKey>> {
    let rows = sqlx::query_as!(
        ApiKey,
        r#"
        SELECT
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

/// Mark a key as revoked. Returns `true` when a row was updated, `false`
/// when no matching active key was found.
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
