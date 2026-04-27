//! Per-key token-bucket rate limiting backed by Redis.
//!
//! The limiter runs on the authenticated sub-router after [`crate::auth`]
//! has resolved the [`crate::auth::AuthenticatedKey`]. For each request
//! it executes a single Lua script in Redis that atomically reads the
//! bucket state, refills based on elapsed time, decrements one token,
//! and writes the result back. The script returns
//! `{ allowed, remaining_tokens, retry_after_ms }`.
//!
//! On allow:
//! * `X-RateLimit-Limit` — bucket capacity for this tier+surface.
//! * `X-RateLimit-Remaining` — tokens left after this request.
//! * `X-RateLimit-Reset` — seconds until the bucket would refill to full.
//!
//! On deny:
//! * 429 [`crate::error::ApiError::RateLimited`] with `Retry-After` set
//!   to the bucket's recovery time in seconds (rounded up).
//!
//! Redis outage handling is configurable. Defaulting to **fail open** —
//! a 5xx storm because the limiter cannot reach Redis is worse than a
//! brief period of unmetered traffic. Operators who need fail-closed
//! flip `CELLORA_API_RATE_LIMIT_FAIL_OPEN=false`.

use std::sync::Arc;

use cellora_common::config::Config;
use cellora_db::models::ApiKeyTier;
use redis::aio::ConnectionManager;
use redis::Script;
use thiserror::Error;

/// Atomic token-bucket Lua script executed once per request.
///
/// `KEYS[1]`: bucket key, `rl:rest:<prefix>` or `rl:graphql:<prefix>`.
/// `ARGV`: `capacity`, `refill_per_sec`, `now_ms`, `cost`.
/// Returns `{ allowed, remaining, retry_after_ms }`.
const TOKEN_BUCKET_LUA: &str = r#"
local key = KEYS[1]
local capacity = tonumber(ARGV[1])
local refill_per_sec = tonumber(ARGV[2])
local now_ms = tonumber(ARGV[3])
local cost = tonumber(ARGV[4])

local data = redis.call('HMGET', key, 'tokens', 'last')
local tokens = tonumber(data[1])
local last = tonumber(data[2])
if tokens == nil then
    tokens = capacity
    last = now_ms
end

local elapsed_ms = math.max(0, now_ms - last)
local refill = elapsed_ms * refill_per_sec / 1000
tokens = math.min(capacity, tokens + refill)

local allowed = 0
local retry_after_ms = 0
if tokens >= cost then
    tokens = tokens - cost
    allowed = 1
else
    if refill_per_sec > 0 then
        retry_after_ms = math.ceil((cost - tokens) * 1000 / refill_per_sec)
    else
        retry_after_ms = 1000
    end
end

redis.call('HSET', key, 'tokens', tokens, 'last', now_ms)
local ttl_ms
if refill_per_sec > 0 then
    ttl_ms = math.ceil(capacity * 1000 / refill_per_sec * 2)
else
    ttl_ms = 60000
end
redis.call('PEXPIRE', key, ttl_ms)

return { allowed, math.floor(tokens), retry_after_ms }
"#;

/// Identifies which surface (REST or GraphQL) is being limited so
/// separate buckets are used per key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    /// REST endpoints under `/v1/`.
    Rest,
    /// GraphQL endpoint at `/graphql` (lands in slice 4).
    #[allow(dead_code)]
    Graphql,
}

impl Surface {
    fn slug(self) -> &'static str {
        match self {
            Self::Rest => "rest",
            Self::Graphql => "graphql",
        }
    }
}

/// Rate-limit parameters resolved from `Config` for a given tier and
/// surface. Capacity is the burst (max tokens); refill rate is tokens per
/// second.
#[derive(Debug, Clone, Copy)]
pub struct LimitParams {
    /// Bucket capacity in tokens.
    pub burst: u32,
    /// Refill rate, tokens per second.
    pub refill_per_sec: f64,
}

impl LimitParams {
    /// Resolve the limit for the given tier on the given surface from
    /// the global [`Config`]. GraphQL constants land in slice 4; until
    /// then the GraphQL surface mirrors the REST limits.
    pub fn from_config(config: &Config, tier: ApiKeyTier, surface: Surface) -> Self {
        let _ = surface; // GraphQL-specific limits land with the GraphQL surface itself.
        match tier {
            ApiKeyTier::Free => Self {
                burst: config.api_rate_limit_free_rest_burst,
                refill_per_sec: config.api_rate_limit_free_rest_refill_per_sec,
            },
            ApiKeyTier::Starter => Self {
                burst: config.api_rate_limit_starter_rest_burst,
                refill_per_sec: config.api_rate_limit_starter_rest_refill_per_sec,
            },
            ApiKeyTier::Pro => Self {
                burst: config.api_rate_limit_pro_rest_burst,
                refill_per_sec: config.api_rate_limit_pro_rest_refill_per_sec,
            },
        }
    }
}

/// Outcome of a single bucket check.
#[derive(Debug, Clone, Copy)]
pub struct Decision {
    /// `true` when the request is permitted.
    pub allowed: bool,
    /// Tokens remaining in the bucket after the decrement (0 when
    /// denied).
    pub remaining: u32,
    /// Seconds the client should wait before the bucket has the cost
    /// available again. `0` on allow.
    pub retry_after_seconds: u64,
    /// Bucket capacity, surfaced as `X-RateLimit-Limit`.
    pub limit: u32,
}

/// Errors raised by the limiter.
#[derive(Debug, Error)]
pub enum LimitError {
    /// The Redis call failed. The middleware translates this into either
    /// a fail-open allow or a 503 depending on configuration.
    #[error("redis error: {0}")]
    Redis(#[from] redis::RedisError),
}

/// Cheap-to-clone handle the middleware reads on every request.
#[derive(Clone)]
pub struct RateLimiter {
    inner: Arc<RateLimiterInner>,
}

impl std::fmt::Debug for RateLimiter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RateLimiter")
            .field("fail_open", &self.inner.fail_open)
            .finish_non_exhaustive()
    }
}

struct RateLimiterInner {
    redis: ConnectionManager,
    script: Script,
    fail_open: bool,
}

impl RateLimiter {
    /// Construct a limiter from an existing connection manager. The Lua
    /// script's hash is computed once and reused on subsequent calls,
    /// so EVALSHA hits the script cache.
    pub fn new(redis: ConnectionManager, fail_open: bool) -> Self {
        Self {
            inner: Arc::new(RateLimiterInner {
                redis,
                script: Script::new(TOKEN_BUCKET_LUA),
                fail_open,
            }),
        }
    }

    /// Whether the limiter is configured to fail open. Used by the
    /// middleware to decide what to do on a [`LimitError`].
    pub fn fails_open(&self) -> bool {
        self.inner.fail_open
    }

    /// Run one bucket check. Builds the Redis key from `(surface, prefix)`
    /// so different surfaces and different keys can never share a bucket.
    pub async fn check(
        &self,
        prefix: &str,
        surface: Surface,
        params: LimitParams,
    ) -> Result<Decision, LimitError> {
        let key = format!("rl:{}:{}", surface.slug(), prefix);
        let now_ms = u64::try_from(now_millis()).unwrap_or(0);
        let cost = 1u32;

        // The script returns three integers; we map them onto Decision.
        let mut conn = self.inner.redis.clone();
        let raw: Vec<i64> = self
            .inner
            .script
            .key(&key)
            .arg(params.burst)
            .arg(params.refill_per_sec)
            .arg(now_ms)
            .arg(cost)
            .invoke_async(&mut conn)
            .await?;

        let allowed = raw.first().copied().unwrap_or(0) == 1;
        let remaining = u32::try_from(raw.get(1).copied().unwrap_or(0).max(0)).unwrap_or(0);
        let retry_after_ms = u64::try_from(raw.get(2).copied().unwrap_or(0).max(0)).unwrap_or(0);
        let retry_after_seconds = retry_after_ms.div_ceil(1000);

        Ok(Decision {
            allowed,
            remaining,
            retry_after_seconds,
            limit: params.burst,
        })
    }
}

/// Wall-clock milliseconds since the Unix epoch. Used by the Lua script
/// for refill calculations. A clock skew on the API node would cause
/// over- or under-refill on the next request after the skew; we accept
/// the bounded error rather than synchronising via Redis's TIME.
fn now_millis() -> u128 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}
