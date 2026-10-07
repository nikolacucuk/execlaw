//! MCP connection manager (Phase 8c).
//!
//! Owns one tokio task per row in `config_mcp_servers` (enabled =
//! true, transport = stdio). Each task:
//!   1. Spawns the stdio child via `execlaw_mcp_client::McpClient::stdio`.
//!   2. On successful initialise, fetches `tools/list` and reflects
//!      every tool into `config_tool_access` with the canonical
//!      `mcp:<server_id>:<remote_name>` prefix.
//!   3. Subscribes to `tools/list_changed` notifications and
//!      re-syncs whenever the server signals a change.
//!   4. On EOF / error, marks the row `error`, sleeps an
//!      exponentially-growing backoff (capped at 60s), and reconnects.
//!
//! Public surface:
//!   * `McpHost::start(db)` — spawns the supervisor; returns a
//!     handle the dispatch chain (Phase 8d) can call.
//!   * `McpHost::call_tool(prefixed_name, args)` — routes to the
//!     right server actor, awaits the JSON result, returns it as
//!     a single text-content array (the runner expects `Value`).
//!   * `McpHost::reconcile()` — re-reads `config_mcp_servers` and
//!     starts/stops actors as the operator adds, edits, or
//!     disables servers.

use crate::mcp_http_client::HttpMcpClient;
use crate::tool_sync::default_mcp_classes_for;
use dashmap::DashMap;
use execlaw_core::Database;
use execlaw_core::mcp_servers::{McpServerRow, McpServerStatus, McpServerStore, McpTransport};
use execlaw_core::tool::{compile_tool_schema, tool_schema_hash};
use execlaw_core::tool_access::{ToolAccessSeed, ToolAccessStore, ToolSource};
use execlaw_core::vault_row::VaultRowStore;
use execlaw_mcp_client::{McpClient, McpError, McpNotification, McpResult, McpTool, StdioSpec};
use serde_json::Value;
use sha2::Digest;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, Notify};
use tracing::{debug, info, warn};

/// Connected MCP client — branches by transport. Both arms expose
/// the same `list_tools` / `call_tool` shape so `mcp_host` can
/// dispatch uniformly.
#[derive(Clone)]
enum ConnectedClient {
    Stdio(Arc<McpClient>),
    Http(HttpMcpClient),
}

impl ConnectedClient {
    async fn list_tools(&self) -> McpResult<Vec<McpTool>> {
        match self {
            Self::Stdio(c) => c.list_tools().await,
            Self::Http(c) => c.list_tools().await,
        }
    }
    async fn call_tool(
        &self,
        name: &str,
        args: Value,
    ) -> McpResult<execlaw_mcp_client::CallToolResult> {
        match self {
            Self::Stdio(c) => c.call_tool(name, args).await,
            Self::Http(c) => c.call_tool(name, args).await,
        }
    }

    fn redact_secret(&self, value: &str) -> String {
        match self {
            Self::Http(client) => client.redact_secret(value),
            Self::Stdio(_) => value.to_owned(),
        }
    }
}

fn redact_json_strings(client: &ConnectedClient, value: &mut Value) {
    match value {
        Value::String(text) => *text = client.redact_secret(text),
        Value::Array(values) => {
            for value in values {
                redact_json_strings(client, value);
            }
        }
        Value::Object(values) => {
            for value in values.values_mut() {
                redact_json_strings(client, value);
            }
        }
        _ => {}
    }
}

/// Tool-name prefix all MCP-sourced tools use in the registry. Lets
/// the dispatch chain route by string-prefix instead of looking up
/// the source out-of-band.
pub const MCP_TOOL_PREFIX: &str = "mcp:";

/// Reconnect backoff bounds.
const RECONNECT_INITIAL: Duration = Duration::from_secs(1);
const RECONNECT_MAX: Duration = Duration::from_secs(60);

fn load_mcp_bearer(db: &Database, row: &McpServerRow) -> Result<Option<String>, &'static str> {
    let Some(reference) = row
        .auth_secret_ref
        .as_deref()
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };
    let scope = execlaw_core::mcp_servers::auth_vault_scope(&row.id);
    let bytes = VaultRowStore::new(db)
        .get(Some(&scope), reference)
        .map_err(|_| "vault_read_failed")?
        .ok_or("credential_ref_missing")?;
    let value = String::from_utf8(bytes).map_err(|_| "credential_not_utf8")?;
    if value.is_empty() {
        return Err("credential_empty");
    }
    Ok(Some(value))
}

fn mcp_config_fingerprint(db: &Database, row: &McpServerRow) -> Result<String, String> {
    let env = row.env.iter().collect::<std::collections::BTreeMap<_, _>>();
    let credential_updated_at = match row.auth_secret_ref.as_deref() {
        Some(reference) => {
            let scope = execlaw_core::mcp_servers::auth_vault_scope(&row.id);
            VaultRowStore::new(db)
                .updated_at(Some(&scope), reference)
                .map_err(|_| "MCP credential version lookup failed".to_owned())?
        }
        None => None,
    };
    let canonical = serde_json::json!({
        "id": &row.id,
        "display_name": &row.display_name,
        "transport": row.transport.as_str(),
        "command": &row.command,
        "args": &row.args,
        "env": env,
        "cwd": &row.cwd,
        "url": &row.url,
        "auth_secret_ref": &row.auth_secret_ref,
        "credential_updated_at": credential_updated_at,
        "enabled": row.enabled,
        "default_allowed_classes": &row.default_allowed_classes,
    });
    let bytes = serde_json::to_vec(&canonical)
        .expect("canonical MCP configuration JSON serialization is infallible");
    Ok(hex::encode(sha2::Sha256::digest(bytes)))
}

/// One running MCP server actor's handle.
struct ServerHandle {
    client: Mutex<Option<ConnectedClient>>,
    tool_schemas: Mutex<HashMap<String, RegisteredMcpSchema>>,
    shutdown: Arc<Notify>,
    config_fingerprint: String,
}

struct RegisteredMcpSchema {
    validator: Arc<jsonschema::Validator>,
    hash: String,
}

#[derive(Clone)]
pub struct McpHost {
    inner: Arc<Inner>,
}

struct Inner {
    db: Database,
    /// server_id → handle.
    servers: DashMap<String, Arc<ServerHandle>>,
    /// Global stop signal — flipping this drains every actor.
    global_stop: Arc<Notify>,
}

impl McpHost {
    /// Build an empty host. Call `reconcile()` after construction to
    /// spin up the configured servers.
    pub fn new(db: Database) -> Self {
        Self {
            inner: Arc::new(Inner {
                db,
                servers: DashMap::new(),
                global_stop: Arc::new(Notify::new()),
            }),
        }
    }

    /// Snapshot each connected MCP tool's schema and operator-declared server identity.
    pub async fn implementation_pins(
        &self,
    ) -> Result<Vec<execlaw_core::runs::RunToolImplementationPin>, String> {
        let rows = McpServerStore::new(&self.inner.db)
            .list_all()
            .map_err(|error| format!("list MCP server identities: {error}"))?;
        let rows = rows
            .into_iter()
            .filter(|row| row.enabled)
            .map(|row| (row.id.clone(), row))
            .collect::<HashMap<_, _>>();
        let handles = self
            .inner
            .servers
            .iter()
            .map(|entry| (entry.key().clone(), entry.value().clone()))
            .collect::<Vec<_>>();
        let mut pins = Vec::new();
        for (server_id, handle) in handles {
            let Some(row) = rows.get(&server_id) else {
                continue;
            };
            let current_identity = mcp_config_fingerprint(&self.inner.db, row)?;
            if current_identity != handle.config_fingerprint {
                return Err(format!(
                    "MCP server '{server_id}' configuration changed before its tools were pinned"
                ));
            }
            let schemas = handle.tool_schemas.lock().await;
            for (remote_name, schema) in schemas.iter() {
                pins.push(execlaw_core::runs::RunToolImplementationPin {
                    runtime_kind: "mcp".into(),
                    plugin_id: server_id.clone(),
                    plugin_version: "remote-unpinned".into(),
                    tool_name: format!("mcp:{server_id}:{remote_name}"),
                    artifact_sha256: current_identity.clone(),
                    input_schema_sha256: Some(schema.hash.clone()),
                    result_schema_sha256: None,
                    server_identity: Some(current_identity.clone()),
                });
            }
        }
        pins.sort_by(|left, right| left.tool_name.cmp(&right.tool_name));
        Ok(pins)
    }

    /// Reject an MCP call if server configuration or advertised schema changed after planning.
    pub async fn verify_run_tool_pin(&self, run_id: &str, tool_name: &str) -> Result<(), String> {
        let expected = execlaw_core::runs::RunStore::new(&self.inner.db)
            .implementation_pins(run_id)
            .map_err(|error| format!("load run MCP pins: {error}"))?
            .into_iter()
            .find(|pin| pin.runtime_kind == "mcp" && pin.tool_name == tool_name)
            .ok_or_else(|| {
                format!("run '{run_id}' has no MCP implementation pin for '{tool_name}'")
            })?;
        let current = self
            .implementation_pins()
            .await?
            .into_iter()
            .find(|pin| pin.tool_name == tool_name)
            .ok_or_else(|| format!("MCP tool '{tool_name}' is no longer advertised"))?;
        if current != expected {
            return Err(format!(
                "MCP tool '{tool_name}' server identity or schema changed after run '{run_id}' was planned"
            ));
        }
        Ok(())
    }

    /// Re-read `config_mcp_servers` and start / stop actors so the
    /// running set matches the persisted set. Idempotent.
    pub async fn reconcile(&self) {
        let store = McpServerStore::new(&self.inner.db);
        let rows = match store.list_all() {
            Ok(r) => r,
            Err(e) => {
                warn!(error = %e, "mcp reconcile: failed to list servers");
                return;
            }
        };

        // Start / restart actors for every enabled row. Both
        // stdio and streamable_http are now wired.
        for row in &rows {
            if !row.enabled {
                self.stop_one(&row.id).await;
                continue;
            }
            let desired = match mcp_config_fingerprint(&self.inner.db, row) {
                Ok(fingerprint) => fingerprint,
                Err(error) => {
                    warn!(server = %row.id, %error, "MCP configuration authority could not be checked");
                    continue;
                }
            };
            let current = self
                .inner
                .servers
                .get(&row.id)
                .map(|handle| handle.config_fingerprint.clone());
            if current.as_deref() != Some(desired.as_str()) {
                if current.is_some() {
                    self.stop_one(&row.id).await;
                }
                self.start_one(row.clone(), desired);
            }
        }

        // Stop actors whose row was deleted.
        let live_ids: std::collections::HashSet<String> =
            rows.iter().map(|r| r.id.clone()).collect();
        let stale: Vec<String> = self
            .inner
            .servers
            .iter()
            .filter_map(|kv| {
                if live_ids.contains(kv.key()) {
                    None
                } else {
                    Some(kv.key().clone())
                }
            })
            .collect();
        for id in stale {
            self.stop_one(&id).await;
        }
    }

    /// Stop the supervisor for a single server. Best-effort — if the
    /// actor is mid-reconnect it'll observe the shutdown on its next
    /// loop iteration.
    pub async fn stop_one(&self, id: &str) {
        if let Some((_, handle)) = self.inner.servers.remove(id) {
            handle.shutdown.notify_waiters();
            handle.shutdown.notify_one();
            // Drop our client clone so the actor's only owner goes away.
            *handle.client.lock().await = None;
        }
    }

    /// Stop every actor. Called from the server's tokio shutdown path.
    pub async fn shutdown_all(&self) {
        self.inner.global_stop.notify_waiters();
        for kv in self.inner.servers.iter() {
            kv.value().shutdown.notify_waiters();
            kv.value().shutdown.notify_one();
        }
        self.inner.servers.clear();
    }

    /// Dispatch a prefixed tool name (`mcp:<id>:<remote_name>`) to
    /// the right server actor. Returns the raw JSON value of the
    /// `tools/call` result so the dispatch chain can hand it back as
    /// a tool_result payload.
    pub async fn call_tool(&self, prefixed: &str, args: Value) -> Result<Value, String> {
        let (server_id, remote_name) = parse_prefixed_tool_name(prefixed)
            .ok_or_else(|| format!("not an MCP tool name: '{prefixed}'"))?;
        let handle = self
            .inner
            .servers
            .get(server_id)
            .map(|kv| kv.value().clone())
            .ok_or_else(|| format!("MCP server '{server_id}' not connected"))?;
        let current = McpServerStore::new(&self.inner.db)
            .get(server_id)
            .map_err(|_| "MCP server authority lookup failed".to_owned())?
            .filter(|row| row.enabled)
            .ok_or_else(|| format!("MCP server '{server_id}' is disabled or missing"))?;
        let fingerprint = mcp_config_fingerprint(&self.inner.db, &current)?;
        if fingerprint != handle.config_fingerprint {
            return Err(format!(
                "MCP server '{server_id}' configuration changed; reconnect is required"
            ));
        }
        let client =
            handle.client.lock().await.clone().ok_or_else(|| {
                format!("MCP server '{server_id}' has no live connection right now")
            })?;
        let schemas = handle.tool_schemas.lock().await;
        let schema = schemas
            .get(remote_name)
            .ok_or_else(|| format!("MCP tool '{prefixed}' has no validated input schema"))?;
        schema.validator.validate(&args).map_err(|error| {
            format!("MCP tool '{prefixed}' arguments do not match its JSON Schema: {error}")
        })?;
        check_mcp_outbound_args(&args)?;
        debug!(tool = prefixed, schema_hash = %schema.hash, "dispatching validated MCP tool");
        drop(schemas);
        let r = client.call_tool(remote_name, args).await.map_err(|error| {
            format!(
                "MCP call failed: {}",
                client.redact_secret(&error.to_string())
            )
        })?;
        let mut result = serde_json::json!({
            "content": r.content,
            "isError": r.is_error,
        });
        redact_json_strings(&client, &mut result);
        Ok(result)
    }

    fn start_one(&self, row: McpServerRow, config_fingerprint: String) {
        let shutdown = Arc::new(Notify::new());
        let handle = Arc::new(ServerHandle {
            client: Mutex::new(None),
            tool_schemas: Mutex::new(HashMap::new()),
            shutdown: shutdown.clone(),
            config_fingerprint,
        });
        self.inner.servers.insert(row.id.clone(), handle.clone());

        let db = self.inner.db.clone();
        let global_stop = self.inner.global_stop.clone();
        tokio::spawn(async move {
            match row.transport {
                McpTransport::Stdio => {
                    stdio_actor_loop(db, row, handle, shutdown, global_stop).await
                }
                McpTransport::StreamableHttp => {
                    http_actor_loop(db, row, handle, shutdown, global_stop).await
                }
            }
        });
    }
}

fn check_mcp_outbound_args(args: &Value) -> Result<(), String> {
    let encoded = serde_json::to_string(args)
        .map_err(|_| "MCP arguments could not be inspected for outbound secrets".to_owned())?;
    let findings = execlaw_policy::outbound::secret_indicators(&encoded);
    if findings.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "outbound data check blocked MCP tool arguments: {}",
            findings.join(",")
        ))
    }
}

/// Per-server stdio supervisor: spawns the child, runs the sync,
/// drives reconnects with exponential backoff. Exits when its
/// `shutdown` notify fires.
async fn stdio_actor_loop(
    db: Database,
    row: McpServerRow,
    handle: Arc<ServerHandle>,
    shutdown: Arc<Notify>,
    global_stop: Arc<Notify>,
) {
    let mut backoff = RECONNECT_INITIAL;
    loop {
        let spec = StdioSpec {
            command: row.command.clone().unwrap_or_default(),
            args: row.args.clone(),
            env: row.env.clone(),
            cwd: row.cwd.as_ref().map(std::path::PathBuf::from),
        };
        let connect_shutdown = Arc::new(Notify::new());
        let client_result = McpClient::stdio(&spec, connect_shutdown.clone()).await;
        match client_result {
            Ok(client) => {
                let client = Arc::new(client);
                *handle.client.lock().await = Some(ConnectedClient::Stdio(client.clone()));
                let now = chrono::Utc::now().timestamp();
                let _ = McpServerStore::new(&db).set_status(
                    &row.id,
                    McpServerStatus::Connected,
                    None,
                    now,
                );
                info!(server = %row.id, "MCP server connected");
                match sync_tools(&db, &row, &ConnectedClient::Stdio(client.clone())).await {
                    Ok(schemas) => *handle.tool_schemas.lock().await = schemas,
                    Err(e) => warn!(server = %row.id, error = %e, "initial tool sync failed"),
                }

                // Watch for notifications + shutdown.
                let mut notif_rx = client.subscribe_notifications();
                let stop_local = shutdown.clone();
                let stop_global = global_stop.clone();
                let connect_shutdown_inner = connect_shutdown.clone();
                let client_for_loop = client.clone();
                let row_for_loop = row.clone();
                let db_for_loop = db.clone();
                let handle_for_loop = handle.clone();
                let exit = tokio::spawn(async move {
                    loop {
                        tokio::select! {
                            biased;
                            _ = stop_local.notified() => {
                                debug!(server = %row_for_loop.id, "actor: local stop");
                                connect_shutdown_inner.notify_one();
                                return;
                            }
                            _ = stop_global.notified() => {
                                debug!(server = %row_for_loop.id, "actor: global stop");
                                connect_shutdown_inner.notify_one();
                                return;
                            }
                            recv = notif_rx.recv() => {
                                match recv {
                                    Ok(McpNotification::ToolsListChanged) => {
                                        let cc = ConnectedClient::Stdio(client_for_loop.clone());
                                        match sync_tools(&db_for_loop, &row_for_loop, &cc).await {
                                            Ok(schemas) => *handle_for_loop.tool_schemas.lock().await = schemas,
                                            Err(e) => warn!(server = %row_for_loop.id, error = %e, "list_changed re-sync failed"),
                                        }
                                    }
                                    Ok(McpNotification::ResourcesListChanged) => {
                                        debug!(server = %row_for_loop.id, "resources list changed (no-op for tool sync)");
                                    }
                                    Err(_) => {
                                        // sender dropped — connection actor is gone.
                                        return;
                                    }
                                }
                            }
                        }
                    }
                });
                let _ = exit.await;
                // Wake the inner client task so it shuts down.
                connect_shutdown.notify_one();
                *handle.client.lock().await = None;
                let now = chrono::Utc::now().timestamp();
                let _ = McpServerStore::new(&db).set_status(
                    &row.id,
                    McpServerStatus::Disconnected,
                    None,
                    now,
                );
                info!(server = %row.id, "MCP server disconnected (operator stop or EOF)");
                return;
            }
            Err(e) => {
                let msg = e.to_string();
                warn!(
                    server = %row.id,
                    error = %msg,
                    backoff_ms = backoff.as_millis(),
                    "MCP connect failed; will retry"
                );
                let now = chrono::Utc::now().timestamp();
                let _ = McpServerStore::new(&db).set_status(
                    &row.id,
                    McpServerStatus::Error,
                    Some(&msg),
                    now,
                );
            }
        }
        // Sleep with respect to shutdown / global stop.
        tokio::select! {
            _ = tokio::time::sleep(backoff) => {}
            _ = shutdown.notified() => {
                return;
            }
            _ = global_stop.notified() => {
                return;
            }
        }
        backoff = (backoff * 2).min(RECONNECT_MAX);
    }
}

/// Per-server HTTP supervisor. Connects via streamable HTTP, runs
/// the initial tool sync, then sits idle until shutdown. HTTP MCP
/// is connection-per-request under the hood (the reqwest client
/// handles pooling) so there's no long-lived socket to babysit —
/// failures surface on the next `call_tool` rather than out-of-band
/// disconnect events.
async fn http_actor_loop(
    db: Database,
    row: McpServerRow,
    handle: Arc<ServerHandle>,
    shutdown: Arc<Notify>,
    global_stop: Arc<Notify>,
) {
    let mut backoff = RECONNECT_INITIAL;
    loop {
        let url = match row.url.as_deref() {
            Some(u) if !u.is_empty() => u.to_owned(),
            _ => {
                let msg = "streamable_http server has no url configured";
                warn!(server = %row.id, "{msg}");
                let now = chrono::Utc::now().timestamp();
                let _ = McpServerStore::new(&db).set_status(
                    &row.id,
                    McpServerStatus::Error,
                    Some(msg),
                    now,
                );
                return;
            }
        };
        // Resolve credentials only inside the configured server's vault
        // namespace. A stale or cross-account ref must never downgrade the
        // connection to unauthenticated mode.
        let bearer = match load_mcp_bearer(&db, &row) {
            Ok(value) => value,
            Err(reason) => {
                warn!(server = %row.id, %reason, "MCP credential resolution denied");
                let now = chrono::Utc::now().timestamp();
                let _ = McpServerStore::new(&db).set_status(
                    &row.id,
                    McpServerStatus::Error,
                    Some("MCP credential reference is unavailable or invalid"),
                    now,
                );
                tokio::select! {
                    _ = tokio::time::sleep(backoff) => {}
                    _ = shutdown.notified() => return,
                    _ = global_stop.notified() => return,
                }
                backoff = (backoff * 2).min(RECONNECT_MAX);
                continue;
            }
        };

        let endpoint_key = format!("mcp:{}", row.id);
        let http = match crate::local_endpoint_policy::checked_client(
            &db,
            &endpoint_key,
            &url,
            |builder| {
                builder.timeout(Duration::from_secs(60)).user_agent(concat!(
                    "execlaw/",
                    env!("CARGO_PKG_VERSION"),
                    "/mcp-http"
                ))
            },
        ) {
            Ok((client, _)) => client,
            Err(error) => {
                warn!(server = %row.id, %error, "MCP HTTP endpoint denied by local-only policy");
                let now = chrono::Utc::now().timestamp();
                let _ = McpServerStore::new(&db).set_status(
                    &row.id,
                    McpServerStatus::Error,
                    Some(&error),
                    now,
                );
                // An operator may grant this endpoint later. Keep the actor
                // alive so the next bounded retry re-resolves and rechecks
                // the policy; a denied address still receives no request.
                tokio::select! {
                    _ = tokio::time::sleep(backoff) => {}
                    _ = shutdown.notified() => return,
                    _ = global_stop.notified() => return,
                }
                backoff = (backoff * 2).min(RECONNECT_MAX);
                continue;
            }
        };

        match HttpMcpClient::connect_with_client(&url, bearer.as_deref(), http).await {
            Ok(client) => {
                let cc = ConnectedClient::Http(client);
                *handle.client.lock().await = Some(cc.clone());
                let now = chrono::Utc::now().timestamp();
                let _ = McpServerStore::new(&db).set_status(
                    &row.id,
                    McpServerStatus::Connected,
                    None,
                    now,
                );
                info!(server = %row.id, url = %url, "MCP server connected (http)");
                match sync_tools(&db, &row, &cc).await {
                    Ok(schemas) => *handle.tool_schemas.lock().await = schemas,
                    Err(e) => warn!(server = %row.id, error = %e, "initial tool sync failed"),
                }
                // No notification stream in v1 — just wait for
                // shutdown. Re-syncs happen on `reconcile()`.
                tokio::select! {
                    _ = shutdown.notified() => {
                        debug!(server = %row.id, "http actor: local stop");
                    }
                    _ = global_stop.notified() => {
                        debug!(server = %row.id, "http actor: global stop");
                    }
                }
                *handle.client.lock().await = None;
                let now = chrono::Utc::now().timestamp();
                let _ = McpServerStore::new(&db).set_status(
                    &row.id,
                    McpServerStatus::Disconnected,
                    None,
                    now,
                );
                info!(server = %row.id, "MCP server disconnected (http)");
                return;
            }
            Err(e) => {
                let msg = e.to_string();
                warn!(
                    server = %row.id,
                    error = %msg,
                    backoff_ms = backoff.as_millis(),
                    "MCP http connect failed; will retry"
                );
                let now = chrono::Utc::now().timestamp();
                let _ = McpServerStore::new(&db).set_status(
                    &row.id,
                    McpServerStatus::Error,
                    Some(&msg),
                    now,
                );
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(backoff) => {}
            _ = shutdown.notified() => return,
            _ = global_stop.notified() => return,
        }
        backoff = (backoff * 2).min(RECONNECT_MAX);
    }
}

/// Reflect a server's `tools/list` response into `config_tool_access`.
/// Transport-agnostic — works for both stdio and streamable_http
/// because both arms of `ConnectedClient` expose the same surface.
async fn sync_tools(
    db: &Database,
    row: &McpServerRow,
    client: &ConnectedClient,
) -> McpResult<HashMap<String, RegisteredMcpSchema>> {
    let tools: Vec<McpTool> = client.list_tools().await?;
    let schemas = compile_mcp_tool_schemas(&tools)?;
    let store = ToolAccessStore::new(db);
    let now = chrono::Utc::now().timestamp();
    let mut n = 0;
    for t in &tools {
        let prefixed = format!("{MCP_TOOL_PREFIX}{}:{}", row.id, t.name);
        let input_schema_str = t
            .input_schema
            .as_ref()
            .and_then(|v| serde_json::to_string(v).ok());
        store
            .upsert_seen(
                &ToolAccessSeed {
                    tool_name: prefixed,
                    source: ToolSource::Mcp,
                    source_id: Some(row.id.clone()),
                    description: t.description.clone(),
                    input_schema: input_schema_str,
                    default_allowed_classes: default_mcp_classes_for(row),
                },
                now,
            )
            .map_err(|e| McpError::Protocol(format!("tool_access upsert: {e}")))?;
        n += 1;
    }
    // Mark any previously-seen MCP tool from this server that the
    // current tools/list omitted as `removed_at = now`. Operator
    // policy stays put so a brief disappearance + reappearance
    // restores the same allowlist.
    let live: std::collections::HashSet<String> = tools
        .iter()
        .map(|t| format!("{MCP_TOOL_PREFIX}{}:{}", row.id, t.name))
        .collect();
    let all = store
        .list_all()
        .map_err(|e| McpError::Protocol(format!("tool_access list: {e}")))?;
    for existing in all {
        if existing.source == ToolSource::Mcp
            && existing.source_id.as_deref() == Some(row.id.as_str())
            && existing.removed_at.is_none()
            && !live.contains(&existing.tool_name)
        {
            let _ = store.mark_removed(&existing.tool_name, now);
        }
    }
    info!(server = %row.id, tools = n, "MCP tool sync complete");
    Ok(schemas)
}

fn compile_mcp_tool_schemas(tools: &[McpTool]) -> McpResult<HashMap<String, RegisteredMcpSchema>> {
    let mut schemas = HashMap::with_capacity(tools.len());
    for tool in tools {
        let schema = tool.input_schema.as_ref().ok_or_else(|| {
            McpError::Protocol(format!("MCP tool '{}' omitted inputSchema", tool.name))
        })?;
        let validator =
            compile_tool_schema(schema, &format!("MCP tool '{}' input schema", tool.name))
                .map_err(McpError::Protocol)?;
        schemas.insert(
            tool.name.clone(),
            RegisteredMcpSchema {
                validator: Arc::new(validator),
                hash: tool_schema_hash(schema),
            },
        );
    }
    Ok(schemas)
}

/// Split a prefixed tool name into `(server_id, remote_name)`. Returns
/// `None` if the prefix is missing or either half is empty.
pub fn parse_prefixed_tool_name(name: &str) -> Option<(&str, &str)> {
    let rest = name.strip_prefix(MCP_TOOL_PREFIX)?;
    let (server, tool) = rest.split_once(':')?;
    if server.is_empty() || tool.is_empty() {
        return None;
    }
    Some((server, tool))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_credentials_are_resolved_only_from_the_server_account_scope() {
        use execlaw_core::db::DbConfig;
        use execlaw_core::migrations::MigrationRunner;
        use execlaw_core::vault_row::VaultRowStore;

        let db = Database::open(&DbConfig::in_memory_unencrypted()).unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        let scope_a = execlaw_core::mcp_servers::auth_vault_scope("server-a");
        VaultRowStore::new(&db)
            .put(Some(&scope_a), "credential-a", b"synthetic-bearer", 1)
            .unwrap();
        let mut row = McpServerRow {
            id: "server-a".into(),
            display_name: "Server A".into(),
            transport: McpTransport::StreamableHttp,
            command: None,
            args: Vec::new(),
            env: HashMap::new(),
            cwd: None,
            url: Some("http://127.0.0.1:8080/mcp".into()),
            auth_secret_ref: Some("credential-a".into()),
            enabled: true,
            default_allowed_classes: vec!["Controller".into()],
            status: McpServerStatus::Idle,
            last_error: None,
            created_at: 1,
            updated_at: 1,
        };
        assert_eq!(
            load_mcp_bearer(&db, &row).unwrap().as_deref(),
            Some("synthetic-bearer")
        );
        row.id = "server-b".into();
        assert_eq!(load_mcp_bearer(&db, &row), Err("credential_ref_missing"));
        row.auth_secret_ref = None;
        assert_eq!(load_mcp_bearer(&db, &row).unwrap(), None);
    }

    #[test]
    fn mcp_auth_reference_rotation_changes_the_live_connection_fingerprint() {
        use execlaw_core::db::DbConfig;
        use execlaw_core::migrations::MigrationRunner;
        use execlaw_core::vault_row::VaultRowStore;

        let db = Database::open(&DbConfig::in_memory_unencrypted()).unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        let mut row = McpServerRow {
            id: "server-a".into(),
            display_name: "Server A".into(),
            transport: McpTransport::StreamableHttp,
            command: None,
            args: Vec::new(),
            env: HashMap::new(),
            cwd: None,
            url: Some("http://127.0.0.1:8080/mcp".into()),
            auth_secret_ref: Some("credential-a".into()),
            enabled: true,
            default_allowed_classes: vec!["Controller".into()],
            status: McpServerStatus::Idle,
            last_error: None,
            created_at: 1,
            updated_at: 1,
        };
        let scope = execlaw_core::mcp_servers::auth_vault_scope(&row.id);
        VaultRowStore::new(&db)
            .put(Some(&scope), "credential-a", b"first-token", 1)
            .unwrap();
        let before = mcp_config_fingerprint(&db, &row).unwrap();
        VaultRowStore::new(&db)
            .put(Some(&scope), "credential-a", b"rotated-token", 2)
            .unwrap();
        assert_ne!(before, mcp_config_fingerprint(&db, &row).unwrap());
        row.auth_secret_ref = Some("credential-b".into());
        assert_ne!(before, mcp_config_fingerprint(&db, &row).unwrap());
    }

    #[tokio::test]
    async fn mcp_run_pins_include_declared_server_and_tool_schema_and_reject_config_drift() {
        use execlaw_core::db::DbConfig;
        use execlaw_core::ids::{ConversationId, EventSeq};
        use execlaw_core::mcp_servers::McpServerInsert;
        use execlaw_core::migrations::MigrationRunner;
        use execlaw_core::runs::{NewRun, RunInputManifest, RunStore};

        let db = Database::open(&DbConfig::in_memory_unencrypted()).unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        let row = McpServerInsert {
            id: "peer-mcp".into(),
            display_name: "Paired local MCP".into(),
            transport: McpTransport::StreamableHttp,
            command: None,
            args: Vec::new(),
            env: HashMap::new(),
            cwd: None,
            url: Some("https://127.0.0.1:7443/mcp".into()),
            auth_secret_ref: None,
            enabled: true,
            default_allowed_classes: vec!["Controller".into()],
        };
        execlaw_core::mcp_servers::McpServerStore::new(&db)
            .insert(&row, 100)
            .unwrap();
        let server_row = execlaw_core::mcp_servers::McpServerStore::new(&db)
            .list_all()
            .unwrap()
            .remove(0);
        let identity = mcp_config_fingerprint(&db, &server_row).unwrap();
        let schema = serde_json::json!({"type":"object","properties":{"query":{"type":"string"}}});
        let validator = compile_tool_schema(&schema, "peer-mcp.search").unwrap();
        let host = McpHost::new(db.clone());
        host.inner.servers.insert(
            "peer-mcp".into(),
            Arc::new(ServerHandle {
                client: Mutex::new(None),
                tool_schemas: Mutex::new(HashMap::from([(
                    "search".into(),
                    RegisteredMcpSchema {
                        validator: Arc::new(validator),
                        hash: tool_schema_hash(&schema),
                    },
                )])),
                shutdown: Arc::new(Notify::new()),
                config_fingerprint: identity.clone(),
            }),
        );
        let pins = host.implementation_pins().await.unwrap();
        assert_eq!(pins.len(), 1);
        assert_eq!(pins[0].runtime_kind, "mcp");
        assert_eq!(pins[0].server_identity.as_deref(), Some(identity.as_str()));
        let expected_schema_hash = tool_schema_hash(&schema);
        assert_eq!(
            pins[0].input_schema_sha256.as_deref(),
            Some(expected_schema_hash.as_str())
        );

        db.with_conn(|connection| {
            connection.execute(
                "INSERT INTO state_events(conversation_id,seq,kind,payload,committed_at)
                 VALUES('mcp-pin-conversation',1,'user_msg',x'00',100)",
                [],
            )?;
            Ok(())
        })
        .unwrap();
        let run_store = RunStore::new(&db);
        let run_id = run_store
            .create_run(&NewRun {
                conversation_id: ConversationId::from("mcp-pin-conversation"),
                parent_run_id: None,
                input_event_seq: EventSeq(1),
                started_at: 100,
                deadline_at: None,
            })
            .unwrap();
        let empty_catalog = serde_json::json!([]);
        run_store
            .record_input_manifest(
                &run_id,
                &RunInputManifest {
                    input_version: 1,
                    prompt_hash: "prompt".into(),
                    model_settings_hash: "model".into(),
                    tool_catalog_hash: execlaw_core::tool::tool_schema_hash(&empty_catalog),
                    tool_catalog_snapshot_json: Some(
                        serde_json::json!({
                            "tools":[],
                            "discoverable_tools":[],
                            "implementation_pins":pins,
                        })
                        .to_string(),
                    ),
                    recorded_at: 100,
                },
            )
            .unwrap();
        host.verify_run_tool_pin(&run_id, "mcp:peer-mcp:search")
            .await
            .unwrap();
        let changed_schema = serde_json::json!({"type":"string"});
        let handle = host.inner.servers.get("peer-mcp").unwrap().value().clone();
        {
            let mut schemas = handle.tool_schemas.lock().await;
            schemas.get_mut("search").unwrap().hash = tool_schema_hash(&changed_schema);
            schemas.get_mut("search").unwrap().validator =
                Arc::new(compile_tool_schema(&changed_schema, "peer-mcp.search").unwrap());
        }
        assert!(
            host.verify_run_tool_pin(&run_id, "mcp:peer-mcp:search")
                .await
                .unwrap_err()
                .contains("schema changed")
        );

        let mut changed = row;
        changed.display_name = "Changed declaration".into();
        execlaw_core::mcp_servers::McpServerStore::new(&db)
            .update("peer-mcp", &changed, 101)
            .unwrap();
        assert!(
            host.implementation_pins()
                .await
                .unwrap_err()
                .contains("configuration changed")
        );
    }

    #[test]
    fn mcp_http_tool_arguments_are_scanned_before_the_external_call() {
        let args = serde_json::json!({"document": "api_key=synthetic-outbound-secret"});
        let error = check_mcp_outbound_args(&args).unwrap_err();
        assert!(error.contains("credential_assignment"));
        assert!(!error.contains("synthetic-outbound-secret"));
        assert!(check_mcp_outbound_args(&serde_json::json!({"query":"weather today"})).is_ok());
    }

    #[tokio::test]
    async fn denied_private_http_endpoint_waits_for_a_later_policy_grant() {
        use execlaw_core::db::DbConfig;
        use execlaw_core::migrations::MigrationRunner;

        let db = Database::open(&DbConfig::in_memory_unencrypted()).unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        let row = McpServerRow {
            id: "qualification-mcp".into(),
            display_name: "Qualification MCP".into(),
            transport: McpTransport::StreamableHttp,
            command: None,
            args: Vec::new(),
            env: HashMap::new(),
            cwd: None,
            url: Some("http://10.1.2.3:30072/mcp".into()),
            auth_secret_ref: None,
            enabled: true,
            default_allowed_classes: vec!["Controller".into()],
            status: McpServerStatus::Idle,
            last_error: None,
            created_at: 1,
            updated_at: 1,
        };
        let shutdown = Arc::new(Notify::new());
        let handle = Arc::new(ServerHandle {
            client: Mutex::new(None),
            tool_schemas: Mutex::new(HashMap::new()),
            shutdown: shutdown.clone(),
            config_fingerprint: mcp_config_fingerprint(&db, &row).unwrap(),
        });
        let task = tokio::spawn(http_actor_loop(
            db,
            row,
            handle,
            shutdown.clone(),
            Arc::new(Notify::new()),
        ));
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            !task.is_finished(),
            "a denied endpoint must remain eligible after approval"
        );
        shutdown.notify_one();
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap();
    }

    #[test]
    fn parse_prefixed_tool_name_round_trips() {
        let (server, tool) = parse_prefixed_tool_name("mcp:github:create_pr").unwrap();
        assert_eq!(server, "github");
        assert_eq!(tool, "create_pr");
    }

    #[test]
    fn parse_prefixed_tool_name_rejects_non_mcp() {
        assert!(parse_prefixed_tool_name("set_thread_name").is_none());
        assert!(parse_prefixed_tool_name("mcp:").is_none());
        assert!(parse_prefixed_tool_name("mcp:srv:").is_none());
        assert!(parse_prefixed_tool_name("mcp::tool").is_none());
    }

    #[test]
    fn mcp_discovery_requires_valid_object_input_schema() {
        let missing = McpTool {
            name: "missing".into(),
            description: None,
            input_schema: None,
            output_schema: None,
        };
        assert!(compile_mcp_tool_schemas(&[missing]).is_err());

        let malformed = McpTool {
            name: "malformed".into(),
            description: None,
            input_schema: Some(serde_json::json!({"type": 7})),
            output_schema: None,
        };
        assert!(compile_mcp_tool_schemas(&[malformed]).is_err());

        let networked = McpTool {
            name: "networked".into(),
            description: None,
            input_schema: Some(serde_json::json!({"$ref": "https://example.invalid/schema"})),
            output_schema: None,
        };
        assert!(compile_mcp_tool_schemas(&[networked]).is_err());
    }

    #[test]
    fn mcp_discovery_compiles_and_hashes_valid_schema() {
        let tool = McpTool {
            name: "lookup".into(),
            description: None,
            input_schema: Some(serde_json::json!({
                "type": "object",
                "properties": {"q": {"type": "string"}},
                "required": ["q"]
            })),
            output_schema: None,
        };
        let schemas = compile_mcp_tool_schemas(&[tool]).unwrap();
        let schema = schemas.get("lookup").unwrap();
        assert!(schema.validator.is_valid(&serde_json::json!({"q": "x"})));
        assert!(!schema.validator.is_valid(&serde_json::json!({"q": 1})));
        assert_eq!(schema.hash.len(), 64);
    }
}
