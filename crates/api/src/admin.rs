//! Operator CLI for managing API keys.
//!
//! Invoked via the `cellora-api` binary with the `admin` subcommand:
//!
//! ```bash
//! cargo run -p cellora-api -- admin create-key --tier free --label "alice's testnet"
//! cargo run -p cellora-api -- admin list-keys
//! cargo run -p cellora-api -- admin revoke-key cell_a1b2c3d4
//! ```
//!
//! `create-key` prints the full secret **once**. The database stores only
//! the prefix and the Argon2 hash; the operator must record the key at
//! creation time. `list-keys` prints prefixes only — the secret is
//! unrecoverable by design.

// `println!` is the right tool for a CLI; the workspace-wide ban is for
// service code paths.
#![allow(clippy::print_stdout, clippy::print_literal)]

use anyhow::{Context, Result};
use cellora_db::api_keys;
use cellora_db::models::ApiKeyTier;
use clap::{Parser, Subcommand, ValueEnum};
use sqlx::PgPool;

use crate::keys;

/// Top-level CLI for the `cellora-api` binary. Default behaviour (no
/// command supplied) is `Server`, which keeps the binary's existing
/// "just serve" UX.
#[derive(Debug, Parser)]
#[command(name = "cellora-api", about, version)]
pub struct Cli {
    /// Optional subcommand. When omitted, the binary starts the HTTP server.
    #[command(subcommand)]
    pub command: Option<Command>,
}

/// Top-level subcommands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Operator commands. Requires `CELLORA_DATABASE_URL` to be set so the
    /// CLI can connect to the same database the API serves from.
    Admin {
        /// Specific admin action to dispatch.
        #[command(subcommand)]
        action: AdminAction,
    },
}

/// `admin <action>` subcommands.
#[derive(Debug, Subcommand)]
pub enum AdminAction {
    /// Issue a new API key.
    CreateKey {
        /// Subscription tier the key is associated with.
        #[arg(long)]
        tier: TierArg,
        /// Optional human-readable label. Stored alongside the row.
        #[arg(long)]
        label: Option<String>,
        /// Emit the result as JSON instead of human-readable text.
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    /// List every key — active and revoked. Secrets are never displayed.
    ListKeys {
        /// Emit the result as JSON instead of a text table.
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    /// Revoke an active key by its prefix. Subsequent requests bearing
    /// the key will be rejected once the auth cache TTL expires.
    RevokeKey {
        /// Prefix of the key to revoke (e.g. `cell_a1b2c3d4`).
        prefix: String,
    },
}

/// Subscription tier as supplied on the command line.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum TierArg {
    /// Free tier — lowest rate limits.
    Free,
    /// Starter tier — mid-range rate limits.
    Starter,
    /// Pro tier — highest rate limits short of enterprise.
    Pro,
}

impl From<TierArg> for ApiKeyTier {
    fn from(value: TierArg) -> Self {
        match value {
            TierArg::Free => Self::Free,
            TierArg::Starter => Self::Starter,
            TierArg::Pro => Self::Pro,
        }
    }
}

/// Dispatch an admin action against the supplied pool. Returns
/// `Ok(true)` when output has been written to stdout (the binary should
/// exit zero); `Ok(false)` is reserved for future actions that signal
/// no-op completion.
pub async fn run(pool: &PgPool, action: AdminAction) -> Result<()> {
    match action {
        AdminAction::CreateKey { tier, label, json } => {
            let issued = keys::generate().context("generate api key")?;
            let row = api_keys::insert(
                pool,
                &issued.prefix,
                &issued.secret_hash,
                tier.into(),
                label.as_deref(),
            )
            .await
            .context("persist api key")?;

            if json {
                let payload = serde_json::json!({
                    "prefix": row.prefix,
                    "secret": issued.secret,
                    "full_key": issued.full,
                    "tier": row.tier.as_str(),
                    "label": row.label,
                });
                println!("{}", serde_json::to_string_pretty(&payload)?);
            } else {
                println!("API key created");
                println!();
                println!("  prefix : {}", row.prefix);
                println!("  tier   : {}", row.tier.as_str());
                if let Some(label) = &row.label {
                    println!("  label  : {label}");
                }
                println!("  full   : {}", issued.full);
                println!();
                println!("Record the full key now — it will not be shown again.");
            }
        }
        AdminAction::ListKeys { json } => {
            let rows = api_keys::list_all(pool).await.context("list api keys")?;
            if json {
                let payload: Vec<_> = rows
                    .iter()
                    .map(|r| {
                        serde_json::json!({
                            "prefix": r.prefix,
                            "tier": r.tier.as_str(),
                            "label": r.label,
                            "created_at": r.created_at,
                            "revoked_at": r.revoked_at,
                            "last_used_at": r.last_used_at,
                        })
                    })
                    .collect();
                println!("{}", serde_json::to_string_pretty(&payload)?);
            } else if rows.is_empty() {
                println!("No API keys.");
            } else {
                println!("{:<24} {:<8} {:<10} {}", "PREFIX", "TIER", "STATE", "LABEL");
                for row in rows {
                    let state = if row.is_revoked() {
                        "revoked"
                    } else {
                        "active"
                    };
                    let label = row.label.unwrap_or_default();
                    println!(
                        "{:<24} {:<8} {:<10} {}",
                        row.prefix,
                        row.tier.as_str(),
                        state,
                        label
                    );
                }
            }
        }
        AdminAction::RevokeKey { prefix } => {
            let revoked = api_keys::revoke(pool, &prefix)
                .await
                .context("revoke api key")?;
            if revoked {
                println!("Revoked {prefix}");
            } else {
                println!("No active key with prefix '{prefix}' (already revoked or unknown).");
                std::process::exit(1);
            }
        }
    }
    Ok(())
}
