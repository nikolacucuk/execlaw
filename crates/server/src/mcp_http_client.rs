//! Streamable HTTP MCP client for execlaw.
//!
//! Implements the pinned protocol version, bounded JSON/SSE responses,
//! response-ID checks, session headers, and capability-gated discovery.
//!
//! Public surface mirrors `execlaw_mcp_client::McpClient`:
//!   * `connect(url, bearer).await -> HttpMcpClient` — runs the
//!     initialize + initialized handshake, returns a handle.
//!   * `list_tools()`, `call_tool(name, args)` — same shapes as
//!     stdio so the caller (`mcp_host`) can dispatch by transport
//!     and pretend they're the same.
//!
//! Auth: bearer token in `Authorization: Bearer <token>`. OAuth 2.1
//! dynamic client registration is a deferred follow-up; for v1 the
//! operator (or the agent on the operator's behalf) supplies a
//! pre-issued API token.

use execlaw_mcp_client::{
    CallToolResult, McpError, McpResource, McpResult, McpTool, ReadResourceResult,
    ServerCapabilities,
};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tracing::{debug, info};

/// MCP protocol version we negotiate with the server. Mirrors the
/// constant in execlaw-mcp-client.
const PROTOCOL_VERSION: &str = execlaw_mcp_client::protocol::PROTOCOL_VERSION;
const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const MAX_PAGE_COUNT: usize = 64;
const MAX_DISCOVERED_ITEMS: usize = 4096;

#[derive(Debug, Serialize)]
struct RpcEnvelope<'a> {
    jsonrpc: &'static str,
    id: u64,
    method: &'a str,
    params: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct RpcResponse {
    // Echoed by the peer for protocol-completeness; we don't validate
    // it (we already chose to speak JSON-RPC 2.0 by construction) but
    // keep the field so the deserializer accepts the full shape.
    #[allow(dead_code)]
    #[serde(default)]
    jsonrpc: String,
    #[serde(default)]
    id: Option<Value>,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<RpcError>,
}

#[derive(Debug, Deserialize)]
struct RpcError {
    code: i64,
    message: String,
    // Optional per JSON-RPC 2.0 — peers can attach structured detail
    // here. We surface `code` + `message` to the operator today and
    // leave `data` for a future inspector view.
    #[allow(dead_code)]
    #[serde(default)]
    data: Option<Value>,
}

/// One Streamable-HTTP MCP connection. Cheap to clone — the inner
/// reqwest client is reusable across calls.
#[derive(Clone)]
pub struct HttpMcpClient {
    http: reqwest::Client,
    url: String,
    bearer: Option<String>,
    next_id: std::sync::Arc<AtomicU64>,
    session_id: std::sync::Arc<tokio::sync::Mutex<Option<String>>>,
    negotiated_version: std::sync::Arc<tokio::sync::Mutex<Option<String>>>,
    capabilities: std::sync::Arc<tokio::sync::Mutex<ServerCapabilities>>,
}

impl HttpMcpClient {
    /// Open with a prevalidated, DNS-pinned client. The MCP host constructs
    /// this only after applying the private-integration endpoint policy.
    pub(crate) async fn connect_with_client(
        url: &str,
        bearer: Option<&str>,
        http: reqwest::Client,
    ) -> McpResult<Self> {
        let me = Self {
            http,
            url: url.to_owned(),
            bearer: bearer.map(|s| s.to_owned()),
            next_id: std::sync::Arc::new(AtomicU64::new(0)),
            session_id: std::sync::Arc::new(tokio::sync::Mutex::new(None)),
            negotiated_version: std::sync::Arc::new(tokio::sync::Mutex::new(None)),
            capabilities: std::sync::Arc::new(tokio::sync::Mutex::new(
                ServerCapabilities::default(),
            )),
        };

        // Initialize handshake.
        let init_params = serde_json::json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": {
                "name": "execlaw",
                "version": env!("CARGO_PKG_VERSION"),
            },
        });
        let init: Value = me.call("initialize", Some(init_params)).await?;
        let negotiated = init
            .get("protocolVersion")
            .and_then(Value::as_str)
            .ok_or_else(|| McpError::Protocol("initialize omitted protocolVersion".into()))?;
        if negotiated != PROTOCOL_VERSION {
            return Err(McpError::Protocol(format!(
                "server negotiated unsupported MCP protocol version '{negotiated}'"
            )));
        }
        *me.negotiated_version.lock().await = Some(negotiated.to_owned());
        *me.capabilities.lock().await = serde_json::from_value(
            init.get("capabilities")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({})),
        )
        .map_err(|e| McpError::Protocol(format!("decode initialize capabilities: {e}")))?;
        let server_name = init
            .get("serverInfo")
            .and_then(|v| v.get("name"))
            .and_then(|v| v.as_str())
            .unwrap_or("?");
        let protocol = init
            .get("protocolVersion")
            .and_then(|v| v.as_str())
            .unwrap_or("?");
        info!(server = %server_name, protocol = %protocol, url = %url, "MCP server initialized over HTTP");

        // Tell the server we're ready. Notifications have no id +
        // no response.
        me.notify("notifications/initialized", None).await?;

        Ok(me)
    }

    /// `tools/list` — same return shape as the stdio client.
    pub async fn list_tools(&self) -> McpResult<Vec<McpTool>> {
        if self.capabilities.lock().await.tools.is_none() {
            return Err(McpError::Protocol(
                "server did not advertise tools capability".into(),
            ));
        }
        let mut cursor: Option<String> = None;
        let mut tools = Vec::new();
        for _ in 0..MAX_PAGE_COUNT {
            let params = cursor.as_ref().map(|c| serde_json::json!({"cursor": c}));
            let result: Value = self.call("tools/list", params).await?;
            let page = result.get("tools").cloned().ok_or_else(|| {
                McpError::Protocol("tools/list response missing `tools` field".into())
            })?;
            let page: Vec<McpTool> = serde_json::from_value(page)
                .map_err(|e| McpError::Protocol(format!("decode tools/list: {e}")))?;
            if tools.len().saturating_add(page.len()) > MAX_DISCOVERED_ITEMS {
                return Err(McpError::Protocol("tools/list item limit exceeded".into()));
            }
            tools.extend(page);
            cursor = result
                .get("nextCursor")
                .and_then(Value::as_str)
                .map(str::to_owned);
            if cursor.is_none() {
                return Ok(tools);
            }
        }
        Err(McpError::Protocol("tools/list page limit exceeded".into()))
    }

    /// `tools/call` — same return shape as the stdio client.
    pub async fn call_tool(&self, name: &str, arguments: Value) -> McpResult<CallToolResult> {
        if self.capabilities.lock().await.tools.is_none() {
            return Err(McpError::Protocol(
                "server did not advertise tools capability".into(),
            ));
        }
        let params = serde_json::json!({ "name": name, "arguments": arguments });
        let result: Value = self.call("tools/call", Some(params)).await?;
        let parsed: CallToolResult = serde_json::from_value(result)
            .map_err(|e| McpError::Protocol(format!("decode tools/call: {e}")))?;
        Ok(parsed)
    }

    /// Call one tool with an explicit cancellation token. Cancelling sends
    /// MCP's `notifications/cancelled` request-id notification to the server.
    pub async fn call_tool_cancellable(
        &self,
        name: &str,
        arguments: Value,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> McpResult<CallToolResult> {
        if self.capabilities.lock().await.tools.is_none() {
            return Err(McpError::Protocol(
                "server did not advertise tools capability".into(),
            ));
        }
        let value = self
            .call_cancellable(
                "tools/call",
                Some(serde_json::json!({"name": name, "arguments": arguments})),
                Some(cancel),
            )
            .await?;
        serde_json::from_value(value)
            .map_err(|e| McpError::Protocol(format!("decode tools/call: {e}")))
    }

    /// Return the server capabilities from the negotiated initialize result.
    pub async fn server_capabilities(&self) -> ServerCapabilities {
        self.capabilities.lock().await.clone()
    }

    /// Read a server resource only when its initialize contract advertised
    /// the resources capability. Pagination and result size are bounded.
    pub async fn list_resources(&self) -> McpResult<Vec<McpResource>> {
        if self.capabilities.lock().await.resources.is_none() {
            return Err(McpError::Protocol(
                "server did not advertise resources capability".into(),
            ));
        }
        let mut cursor: Option<String> = None;
        let mut resources = Vec::new();
        for _ in 0..MAX_PAGE_COUNT {
            let params = cursor.as_ref().map(|c| serde_json::json!({"cursor": c}));
            let result: Value = self.call("resources/list", params).await?;
            let page: Vec<McpResource> =
                serde_json::from_value(result.get("resources").cloned().unwrap_or(Value::Null))
                    .map_err(|e| McpError::Protocol(format!("decode resources/list: {e}")))?;
            if resources.len().saturating_add(page.len()) > MAX_DISCOVERED_ITEMS {
                return Err(McpError::Protocol(
                    "resources/list item limit exceeded".into(),
                ));
            }
            resources.extend(page);
            cursor = result
                .get("nextCursor")
                .and_then(Value::as_str)
                .map(str::to_owned);
            if cursor.is_none() {
                return Ok(resources);
            }
        }
        Err(McpError::Protocol(
            "resources/list page limit exceeded".into(),
        ))
    }

    /// Read one bounded resource payload.
    pub async fn read_resource(&self, uri: &str) -> McpResult<ReadResourceResult> {
        if self.capabilities.lock().await.resources.is_none() {
            return Err(McpError::Protocol(
                "server did not advertise resources capability".into(),
            ));
        }
        let result: Value = self
            .call("resources/read", Some(serde_json::json!({"uri": uri})))
            .await?;
        serde_json::from_value(result)
            .map_err(|e| McpError::Protocol(format!("decode resources/read: {e}")))
    }

    /// Return the protocol version accepted during `initialize`.
    pub async fn negotiated_version(&self) -> Option<String> {
        self.negotiated_version.lock().await.clone()
    }

    /// End the negotiated HTTP session; stateless peers need no request.
    pub async fn close(&self) -> McpResult<()> {
        let Some(session) = self.session_id.lock().await.take() else {
            return Ok(());
        };
        let mut req = self
            .http
            .delete(&self.url)
            .header("MCP-Protocol-Version", PROTOCOL_VERSION)
            .header("MCP-Session-Id", session);
        if let Some(token) = &self.bearer {
            req = req.bearer_auth(token);
        }
        let response = req
            .send()
            .await
            .map_err(|e| McpError::Protocol(format!("http session close: {e}")))?;
        if response.status().is_success()
            || response.status() == reqwest::StatusCode::METHOD_NOT_ALLOWED
            || response.status() == reqwest::StatusCode::NOT_FOUND
            || response.status() == reqwest::StatusCode::GONE
        {
            return Ok(());
        }
        Err(McpError::Protocol(format!(
            "http session close returned {}",
            response.status().as_u16()
        )))
    }

    /// Generic JSON-RPC request. Returns the `result` field on
    /// success; surfaces RPC errors verbatim.
    async fn call(&self, method: &str, params: Option<Value>) -> McpResult<Value> {
        self.call_cancellable(method, params, None).await
    }

    async fn call_cancellable(
        &self,
        method: &str,
        params: Option<Value>,
        cancel: Option<&tokio_util::sync::CancellationToken>,
    ) -> McpResult<Value> {
        if cancel.is_some_and(tokio_util::sync::CancellationToken::is_cancelled) {
            return Err(McpError::Cancelled);
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let first = self
            .call_cancellable_once(method, params.clone(), id, cancel)
            .await;
        match first {
            Err(McpError::SessionExpired) => {
                self.initialize_session().await?;
                if method == "tools/call" {
                    return Err(McpError::SessionExpired);
                }
                let retry_id = self.next_id.fetch_add(1, Ordering::SeqCst);
                self.call_cancellable_once(method, params, retry_id, cancel)
                    .await
            }
            result => result,
        }
    }

    async fn initialize_session(&self) -> McpResult<()> {
        let params = serde_json::json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": {"name": "execlaw", "version": env!("CARGO_PKG_VERSION")},
        });
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let result = self.call_once("initialize", Some(params), id).await?;
        let version = result
            .get("protocolVersion")
            .and_then(Value::as_str)
            .ok_or_else(|| McpError::Protocol("initialize omitted protocolVersion".into()))?;
        if version != PROTOCOL_VERSION {
            return Err(McpError::Protocol(format!(
                "server negotiated unsupported MCP protocol version '{version}'"
            )));
        }
        *self.negotiated_version.lock().await = Some(version.to_owned());
        *self.capabilities.lock().await = serde_json::from_value(
            result
                .get("capabilities")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({})),
        )
        .map_err(|e| McpError::Protocol(format!("decode initialize capabilities: {e}")))?;
        self.notify("notifications/initialized", None).await
    }

    async fn call_cancellable_once(
        &self,
        method: &str,
        params: Option<Value>,
        id: u64,
        cancel: Option<&tokio_util::sync::CancellationToken>,
    ) -> McpResult<Value> {
        let request = self.call_once(method, params, id);
        if let Some(cancel) = cancel {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    let _ = tokio::time::timeout(
                        Duration::from_secs(1),
                        self.notify(
                            execlaw_mcp_client::protocol::notifications::CANCELLED,
                            Some(serde_json::json!({"requestId": id, "reason": "caller cancelled"})),
                        ),
                    ).await;
                    Err(McpError::Cancelled)
                }
                result = request => result,
            }
        } else {
            request.await
        }
    }

    async fn call_once(&self, method: &str, params: Option<Value>, id: u64) -> McpResult<Value> {
        let env = RpcEnvelope {
            jsonrpc: "2.0",
            id,
            method,
            params,
        };
        if serde_json::to_vec(&env)
            .map_err(|e| McpError::Protocol(format!("encode {method}: {e}")))?
            .len()
            > MAX_RESPONSE_BYTES
        {
            return Err(McpError::Protocol(format!(
                "MCP {method} request exceeds byte limit"
            )));
        }
        let mut req = self
            .http
            .post(&self.url)
            .header("Accept", "application/json, text/event-stream")
            .header("Content-Type", "application/json")
            .json(&env);
        if method != "initialize" {
            let version = self
                .negotiated_version
                .lock()
                .await
                .clone()
                .unwrap_or_else(|| PROTOCOL_VERSION.to_owned());
            req = req.header("MCP-Protocol-Version", version);
            if let Some(session) = self.session_id.lock().await.clone() {
                req = req.header("MCP-Session-Id", session);
            }
        }
        if let Some(tok) = &self.bearer {
            req = req.bearer_auth(tok);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| McpError::Protocol(format!("http {method}: {e}")))?;
        let status = resp.status();
        if !status.is_success() {
            if status == reqwest::StatusCode::NOT_FOUND || status == reqwest::StatusCode::GONE {
                let had_session = self.session_id.lock().await.take().is_some();
                if had_session {
                    return Err(McpError::SessionExpired);
                }
            }
            return Err(McpError::Protocol(format!(
                "http {method} returned {}",
                status.as_u16()
            )));
        }
        if let Some(session) = resp
            .headers()
            .get("MCP-Session-Id")
            .and_then(|v| v.to_str().ok())
        {
            let current = self.session_id.lock().await.clone();
            if method != "initialize"
                && current.as_deref().is_some_and(|current| current != session)
            {
                return Err(McpError::Protocol(
                    "MCP session id changed within session".into(),
                ));
            }
            *self.session_id.lock().await = Some(session.to_owned());
        }
        if let Some(version) = resp
            .headers()
            .get("MCP-Protocol-Version")
            .and_then(|v| v.to_str().ok())
        {
            if method == "initialize" {
                if version != PROTOCOL_VERSION {
                    return Err(McpError::Protocol(format!(
                        "server negotiated unsupported MCP protocol version '{version}'"
                    )));
                }
                *self.negotiated_version.lock().await = Some(version.to_owned());
            } else if version != PROTOCOL_VERSION {
                return Err(McpError::Protocol(
                    "MCP protocol version changed within session".into(),
                ));
            }
        }

        // Streamable HTTP may return either JSON or a bounded SSE stream.
        let ctype = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase();
        if ctype.contains("text/event-stream") {
            // Ignore notifications and unrelated frames until the matching response.
            let mut stream = resp.bytes_stream();
            let mut decoder = execlaw_inference_api::SseDecoder::default();
            let mut total = 0usize;
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|error| {
                    McpError::Protocol(format!("http {method} read sse: {error}"))
                })?;
                total = total.saturating_add(chunk.len());
                if total > MAX_RESPONSE_BYTES {
                    return Err(McpError::Protocol(
                        "MCP SSE response exceeds byte limit".into(),
                    ));
                }
                for event in decoder.push(&chunk, false).map_err(|error| {
                    McpError::Protocol(format!("http {method} decode sse: {error}"))
                })? {
                    let candidate: RpcResponse = match serde_json::from_str(&event.data) {
                        Ok(response) => response,
                        Err(_) => continue,
                    };
                    if candidate.id.as_ref() == Some(&serde_json::json!(id)) {
                        return rpc_to_result(method, id, candidate);
                    }
                }
            }
            for event in decoder
                .push(&[], true)
                .map_err(|error| McpError::Protocol(format!("http {method} decode sse: {error}")))?
            {
                let candidate: RpcResponse = match serde_json::from_str(&event.data) {
                    Ok(response) => response,
                    Err(_) => continue,
                };
                if candidate.id.as_ref() == Some(&serde_json::json!(id)) {
                    return rpc_to_result(method, id, candidate);
                }
            }
            return Err(McpError::Protocol(format!(
                "sse {method}: no matching response in stream"
            )));
        }
        if !ctype.contains("application/json") {
            return Err(McpError::Protocol(format!(
                "http {method}: unsupported content type"
            )));
        }

        if resp
            .content_length()
            .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
        {
            return Err(McpError::Protocol(
                "MCP JSON response exceeds byte limit".into(),
            ));
        }
        let mut stream = resp.bytes_stream();
        let mut bytes = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk =
                chunk.map_err(|e| McpError::Protocol(format!("http {method} read: {e}")))?;
            if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
                return Err(McpError::Protocol(
                    "MCP JSON response exceeds byte limit".into(),
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        let parsed: RpcResponse = serde_json::from_slice(&bytes)
            .map_err(|e| McpError::Protocol(format!("http {method} decode: {e}")))?;
        rpc_to_result(method, id, parsed)
    }

    /// Fire-and-forget JSON-RPC notification (no id, no response
    /// expected). Servers typically return 200 / 202 with empty body.
    async fn notify(&self, method: &str, params: Option<Value>) -> McpResult<()> {
        let env = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        if serde_json::to_vec(&env)
            .map_err(|e| McpError::Protocol(format!("encode {method}: {e}")))?
            .len()
            > MAX_RESPONSE_BYTES
        {
            return Err(McpError::Protocol(format!(
                "MCP {method} notification exceeds byte limit"
            )));
        }
        let mut req = self
            .http
            .post(&self.url)
            .header("Accept", "application/json, text/event-stream")
            .header("Content-Type", "application/json")
            .json(&env);
        req = req.header(
            "MCP-Protocol-Version",
            self.negotiated_version
                .lock()
                .await
                .as_deref()
                .unwrap_or(PROTOCOL_VERSION),
        );
        if let Some(session) = self.session_id.lock().await.as_deref() {
            req = req.header("MCP-Session-Id", session);
        }
        if let Some(tok) = &self.bearer {
            req = req.bearer_auth(tok);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| McpError::Protocol(format!("http {method}: {e}")))?;
        if !resp.status().is_success() {
            let s = resp.status().as_u16();
            return Err(McpError::Protocol(format!(
                "http notification {method} returned {s}"
            )));
            // Notifications: 4xx is informational only. Don't fail
            // the connection over a server that returns 405 on
            // notifications/initialized — the call() path is what
            // matters.
        }
        debug!(method, "MCP notification accepted");
        Ok(())
    }
}

fn rpc_to_result(method: &str, expected_id: u64, resp: RpcResponse) -> McpResult<Value> {
    if resp.jsonrpc != "2.0" || resp.id.as_ref() != Some(&serde_json::json!(expected_id)) {
        return Err(McpError::Protocol(format!(
            "rpc {method}: invalid JSON-RPC version or response id"
        )));
    }
    if resp.error.is_some() == resp.result.is_some() {
        return Err(McpError::Protocol(format!(
            "rpc {method}: response must contain exactly one of result or error"
        )));
    }
    if let Some(err) = resp.error {
        return Err(McpError::Protocol(format!(
            "rpc {method} -> code={} {}",
            err.code, err.message
        )));
    }
    resp.result.ok_or_else(|| {
        McpError::Protocol(format!(
            "rpc {method}: response had neither result nor error"
        ))
    })
}

#[cfg(test)]
fn truncate(s: &str, max: usize) -> String {
    let trimmed: String = s.chars().take(max).collect();
    if s.chars().count() > max {
        format!("{trimmed}…")
    } else {
        trimmed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn read_http_request(stream: &mut tokio::net::TcpStream) -> (String, Value) {
        let mut bytes = Vec::new();
        let header_end = loop {
            let mut chunk = [0u8; 1024];
            let count = stream.read(&mut chunk).await.unwrap();
            assert_ne!(count, 0, "fixture peer closed before request headers");
            bytes.extend_from_slice(&chunk[..count]);
            if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                break index + 4;
            }
        };
        let headers = String::from_utf8(bytes[..header_end].to_vec()).unwrap();
        let content_length = headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().unwrap())
            })
            .unwrap_or(0);
        while bytes.len() < header_end + content_length {
            let mut chunk = vec![0u8; header_end + content_length - bytes.len()];
            let count = stream.read(&mut chunk).await.unwrap();
            assert_ne!(count, 0, "fixture peer closed before request body");
            bytes.extend_from_slice(&chunk[..count]);
        }
        let body = serde_json::from_slice(&bytes[header_end..header_end + content_length]).unwrap();
        (headers, body)
    }

    async fn write_response(
        stream: &mut tokio::net::TcpStream,
        status: &str,
        headers: &str,
        body: &[u8],
    ) {
        let response = format!(
            "HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(response.as_bytes()).await.unwrap();
        stream.write_all(body).await.unwrap();
        stream.flush().await.unwrap();
    }

    async fn accept_request(
        listener: &tokio::net::TcpListener,
    ) -> (tokio::net::TcpStream, String, Value) {
        let (mut stream, _) = listener.accept().await.unwrap();
        let (headers, body) = read_http_request(&mut stream).await;
        (stream, headers, body)
    }

    #[test]
    fn rpc_to_result_returns_err_on_rpc_error() {
        let resp = RpcResponse {
            jsonrpc: "2.0".to_string(),
            id: Some(serde_json::json!(0)),
            result: None,
            error: Some(RpcError {
                code: -32603,
                message: "internal".to_string(),
                data: None,
            }),
        };
        let err = rpc_to_result("initialize", 0, resp).unwrap_err();
        assert!(format!("{err}").contains("-32603"));
    }

    #[test]
    fn rpc_to_result_returns_ok_on_result() {
        let resp = RpcResponse {
            jsonrpc: "2.0".to_string(),
            id: Some(serde_json::json!(0)),
            result: Some(serde_json::json!({"ok": true})),
            error: None,
        };
        let v = rpc_to_result("ping", 0, resp).unwrap();
        assert_eq!(v["ok"], true);
    }

    #[test]
    fn rpc_to_result_rejects_wrong_id_and_invalid_jsonrpc_version() {
        let wrong_id = RpcResponse {
            jsonrpc: "2.0".into(),
            id: Some(serde_json::json!(7)),
            result: Some(serde_json::json!({"ok": true})),
            error: None,
        };
        assert!(rpc_to_result("tools/list", 8, wrong_id).is_err());
        let wrong_version = RpcResponse {
            jsonrpc: "1.0".into(),
            id: Some(serde_json::json!(8)),
            result: Some(serde_json::json!({"ok": true})),
            error: None,
        };
        assert!(rpc_to_result("tools/list", 8, wrong_version).is_err());
    }

    #[tokio::test]
    async fn streamable_http_returns_matching_sse_response_before_stream_closes() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _, initialize) = accept_request(&listener).await;
            assert_eq!(initialize["method"], "initialize");
            let id = initialize["id"].as_u64().unwrap();
            let body = serde_json::to_vec(&serde_json::json!({
                "jsonrpc":"2.0", "id":id,
                "result":{"protocolVersion":PROTOCOL_VERSION,"capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"1"}}
            })).unwrap();
            write_response(
                &mut stream,
                "200 OK",
                &format!("Content-Type: application/json\r\nMCP-Session-Id: fixture-session\r\nMCP-Protocol-Version: {PROTOCOL_VERSION}\r\n"),
                &body,
            ).await;

            let (mut stream, _, initialized) = accept_request(&listener).await;
            assert_eq!(initialized["method"], "notifications/initialized");
            write_response(&mut stream, "202 Accepted", "", b"").await;

            let (mut stream, headers, list) = accept_request(&listener).await;
            assert_eq!(list["method"], "tools/list");
            assert!(
                headers
                    .to_ascii_lowercase()
                    .contains("mcp-session-id: fixture-session")
            );
            assert!(
                headers
                    .to_ascii_lowercase()
                    .contains("mcp-protocol-version: 2025-06-18")
            );
            let id = list["id"].as_u64().unwrap();
            let wrong = serde_json::json!({"jsonrpc":"2.0","id":id+100,"result":{"tools":[]}});
            let matching = serde_json::json!({"jsonrpc":"2.0","id":id,"result":{"tools":[]}});
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nMCP-Session-Id: fixture-session\r\nConnection: close\r\n\r\n").await.unwrap();
            stream
                .write_all(format!("data: {wrong}\n\n").as_bytes())
                .await
                .unwrap();
            stream.flush().await.unwrap();
            tokio::time::sleep(Duration::from_millis(75)).await;
            stream
                .write_all(format!("data: {matching}\n\n").as_bytes())
                .await
                .unwrap();
            stream.flush().await.unwrap();
            tokio::time::sleep(Duration::from_millis(250)).await;
        });

        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        let client =
            HttpMcpClient::connect_with_client(&format!("http://{address}/mcp"), None, http)
                .await
                .unwrap();
        assert!(client.list_tools().await.unwrap().is_empty());
        server.abort();
    }

    #[tokio::test]
    async fn expired_http_session_reinitializes_without_replaying_tool_call() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let mut tool_calls = 0;
            let mut initialize_calls = 0;
            for _ in 0..5 {
                let (mut stream, _, request) = accept_request(&listener).await;
                match request["method"].as_str().unwrap() {
                    "initialize" => {
                        initialize_calls += 1;
                        let id = request["id"].as_u64().unwrap();
                        let body = serde_json::to_vec(&serde_json::json!({
                            "jsonrpc":"2.0", "id":id,
                            "result":{"protocolVersion":PROTOCOL_VERSION,"capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"1"}}
                        })).unwrap();
                        write_response(&mut stream,"200 OK",&format!("Content-Type: application/json\r\nMCP-Session-Id: session-{initialize_calls}\r\nMCP-Protocol-Version: {PROTOCOL_VERSION}\r\n"),&body).await;
                    }
                    "notifications/initialized" => {
                        write_response(&mut stream, "202 Accepted", "", b"").await
                    }
                    "tools/call" => {
                        tool_calls += 1;
                        write_response(&mut stream, "404 Not Found", "", b"").await;
                    }
                    method => panic!("unexpected MCP method: {method}"),
                }
            }
            (initialize_calls, tool_calls)
        });

        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        let client =
            HttpMcpClient::connect_with_client(&format!("http://{address}/mcp"), None, http)
                .await
                .unwrap();
        let error = client
            .call_tool("write_file", serde_json::json!({"path":"x"}))
            .await
            .unwrap_err();
        assert!(matches!(error, McpError::SessionExpired));
        let (initialize_calls, tool_calls) = server.await.unwrap();
        assert_eq!(initialize_calls, 2);
        assert_eq!(
            tool_calls, 1,
            "tool effects must not replay after session expiry"
        );
    }

    #[tokio::test]
    async fn oversized_http_response_is_rejected_from_content_length() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _, initialize) = accept_request(&listener).await;
            let id = initialize["id"].as_u64().unwrap();
            let body = serde_json::to_vec(&serde_json::json!({
                "jsonrpc":"2.0", "id":id,
                "result":{"protocolVersion":PROTOCOL_VERSION,"capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"1"}}
            })).unwrap();
            write_response(
                &mut stream,
                "200 OK",
                &format!(
                    "Content-Type: application/json\r\nMCP-Protocol-Version: {PROTOCOL_VERSION}\r\n"
                ),
                &body,
            )
            .await;
            let (mut stream, _, _) = accept_request(&listener).await;
            write_response(&mut stream, "202 Accepted", "", b"").await;
            let (mut stream, _, list) = accept_request(&listener).await;
            assert_eq!(list["method"], "tools/list");
            stream.write_all(format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                MAX_RESPONSE_BYTES + 1,
            ).as_bytes()).await.unwrap();
            stream.flush().await.unwrap();
        });

        let http = reqwest::Client::builder().no_proxy().build().unwrap();
        let client =
            HttpMcpClient::connect_with_client(&format!("http://{address}/mcp"), None, http)
                .await
                .unwrap();
        let error = client.list_tools().await.unwrap_err();
        assert!(format!("{error}").contains("exceeds byte limit"));
        server.await.unwrap();
    }

    #[test]
    fn truncate_trims_long_strings() {
        let s = "a".repeat(300);
        let out = truncate(&s, 100);
        assert_eq!(out.chars().count(), 101);
        assert!(out.ends_with('…'));
    }
}
