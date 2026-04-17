//! Minimal CKB JSON-RPC client built on `reqwest` + `ckb-jsonrpc-types`.
//!
//! The client speaks the subset of the CKB JSON-RPC surface the indexer
//! actually needs (tip number, block fetch, chain info). Adding more methods
//! is a matter of adding another thin wrapper on [`CkbClient::call`].

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use ckb_jsonrpc_types::{BlockNumber, BlockView, ChainInfo};
use reqwest::Client;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use url::Url;

use crate::error::{Error, Result};

/// CKB JSON-RPC client.
#[derive(Debug, Clone)]
pub struct CkbClient {
    http: Client,
    endpoint: Url,
    next_id: std::sync::Arc<AtomicU64>,
}

#[derive(Debug, Serialize)]
struct JsonRpcRequest<'a, P: Serialize> {
    jsonrpc: &'static str,
    id: u64,
    method: &'a str,
    params: P,
}

#[derive(Debug, Deserialize)]
struct JsonRpcResponse {
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<JsonRpcError>,
}

#[derive(Debug, Deserialize)]
struct JsonRpcError {
    code: i64,
    message: String,
}

impl CkbClient {
    /// Build a client pointing at the given CKB JSON-RPC endpoint.
    pub fn new(endpoint: impl AsRef<str>) -> Result<Self> {
        let endpoint = Url::parse(endpoint.as_ref())
            .map_err(|err| Error::InvalidUrl(format!("{}: {err}", endpoint.as_ref())))?;
        let http = Client::builder()
            .timeout(Duration::from_secs(15))
            .user_agent(concat!("cellora-indexer/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self {
            http,
            endpoint,
            next_id: std::sync::Arc::new(AtomicU64::new(1)),
        })
    }

    /// The current tip block number reported by the node.
    pub async fn tip_block_number(&self) -> Result<u64> {
        let raw: BlockNumber = self
            .call("get_tip_block_number", Value::Array(vec![]))
            .await?;
        Ok(u64::from(raw))
    }

    /// Fetch the block at `number`, or `None` if the node has not seen it yet.
    pub async fn get_block_by_number(&self, number: u64) -> Result<Option<BlockView>> {
        let hex = format!("0x{number:x}");
        // Single-arg form: node returns a bare `BlockView` (or null). Passing
        // `with_cycles = true` would wrap the result in a `{block, cycles}`
        // envelope which we do not need in week 1.
        let params = json!([hex]);
        let raw: Option<BlockView> = self.call("get_block_by_number", params).await?;
        Ok(raw)
    }

    /// High-level info about the chain the node is following.
    pub async fn chain_info(&self) -> Result<ChainInfo> {
        self.call("get_blockchain_info", Value::Array(vec![])).await
    }

    /// Low-level JSON-RPC call. Exposed so tests and future callers can issue
    /// methods without adding a wrapper to this type for every one.
    pub async fn call<P, R>(&self, method: &str, params: P) -> Result<R>
    where
        P: Serialize,
        R: DeserializeOwned,
    {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let request = JsonRpcRequest {
            jsonrpc: "2.0",
            id,
            method,
            params,
        };

        let response = self
            .http
            .post(self.endpoint.clone())
            .json(&request)
            .send()
            .await?
            .error_for_status()?;
        let envelope: JsonRpcResponse = response.json().await?;

        if let Some(err) = envelope.error {
            return Err(Error::CkbRpc {
                code: err.code,
                message: err.message,
            });
        }
        let raw = envelope.result.ok_or_else(|| Error::CkbRpc {
            code: 0,
            message: format!("missing result for method {method}"),
        })?;
        serde_json::from_value(raw).map_err(Error::from)
    }
}
