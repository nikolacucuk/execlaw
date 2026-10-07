//! Chains built-in tools + plugin tools into one
//! `execlaw_runner_local::turn::ToolDispatch` that TurnExecutor calls.
//!
//! Lookup order on every tool call:
//! 0. **Per-tool access policy** (Phase 8a): consult
//!    `config_tool_access` for the tool. If the row exists and the
//!    caller's trust class is not in `allowed_classes`, OR the tool
//!    is disabled, OR the source has marked it removed — return
//!    `Err("not authorized: ...")` immediately. This is the single
//!    enforcement point for the operator-driven trust-class allowlist
//!    that applies regardless of whether the tool came from a
//!    built-in, a plugin, or an MCP server.
//! 1. Built-in tools (via [`BuiltinTools`]) — e.g. the memory shim.
//! 2. Plugin-contributed tools (via [`PluginHost::call_tool`]) — with
//!    capability enforcement: the caller's `capability_set` must be a
//!    superset of the tool's `required_capabilities`, or the dispatch
//!    fails BEFORE the subprocess sees the args (§7.2 + §7.3).
//! 3. Anything else → `Err("no tool registered for '<name>'")`. That
//!    error is paired with a cancellation `tool_result` by
//!    `commit_turn`'s enforce_tool_pairing, so the log stays
//!    well-formed even when the model hallucinates a tool name.

use crate::host_caps_impl::builtin_artifacts_root_path as builtin_artifacts_root;
use crate::mcp_host::{MCP_TOOL_PREFIX, McpHost};
use crate::tool_apis_http::HttpWebFetchApi;
use crate::tool_apis_subagent::InferenceSubagentApi;
use async_trait::async_trait;
use execlaw_core::Database;
use execlaw_core::ids::ConversationId;
use execlaw_core::ids::PrincipalId;
use execlaw_core::principal::{PrincipalStore, TrustLevel as StoredTrustLevel};
use execlaw_core::tool::{Capability, Clock, SystemClock, ToolCtx, ToolImpl, ToolOutcome};
use execlaw_core::tool_access::ToolAccessStore;
use execlaw_core::tool_apis::{
    DbConversationApi, DbMemoryApi, DbNotifyApi, DbResearchApi, DbScheduleApi,
};
use execlaw_inference_api::InferenceClient;
use execlaw_plugin_host::{BuiltinTools, PluginHost};
use execlaw_policy::trust::TrustLevel;
use execlaw_runner_local::turn::ToolDispatch;
use sha2::Digest;
use std::sync::Arc;

#[derive(Debug, Clone)]
struct PolicyDecisionContext {
    run_id: String,
    conversation_id: String,
    input_event_seq: i64,
}

fn policy_reason_code(error: &str) -> &'static str {
    if error.contains("active safety profile") {
        "safety_profile_denied"
    } else if error.contains("trust class") || error.contains("trust >=") {
        "trust_floor_denied"
    } else if error.contains("disabled") || error.contains("no longer registered") {
        "tool_unavailable"
    } else if error.contains("caller principal") || error.contains("authority changed") {
        "live_authority_denied"
    } else if error.contains("capability") {
        "capability_denied"
    } else {
        "policy_denied"
    }
}

/// Concrete dispatcher built from a `PluginHost` + built-ins +
/// caller capability set + caller trust class (the access gate).
pub struct ChainedToolDispatch<B: BuiltinTools> {
    pub host: PluginHost,
    pub caller_caps: Vec<String>,
    pub caller_trust: TrustLevel,
    pub builtins: B,
    /// Database handle the dispatch consults for `config_tool_access`
    /// rows. Optional so test fixtures and pre-Phase-8a callers can
    /// keep working without seeding the gate; `None` means "skip the
    /// trust-class allowlist check," which is the legacy behaviour.
    pub access_db: Option<Database>,
    policy_revision_snapshot: Option<i64>,
    /// Task-scoped permissions frozen into the user event for this run.
    pub safety_profile: Option<execlaw_core::safety_profiles::SafetyProfileSnapshot>,
    policy_context: Option<PolicyDecisionContext>,
    /// Principal authority captured when the turn starts. The dispatcher
    /// compares the full persisted grant again at each tool boundary so a
    /// trust change, delegated-scope edit, or expiry cannot be hidden by a
    /// model request that was already in flight.
    live_principal: Option<(PrincipalId, Option<StoredTrustLevel>)>,
    live_authority_fingerprint: Option<String>,
    /// Phase-8d MCP dispatch tier. When present, tool names with the
    /// `mcp:<server>:<tool>` prefix route to the connection manager
    /// instead of the builtin/plugin layer.
    pub mcp_host: Option<McpHost>,
    /// 2026-04-29 — conversation context for the new
    /// `Arc<dyn ToolImpl>` built-in tier. When the dispatcher is
    /// constructed with a known conversation id, registry-resolved
    /// built-ins (`HookRegistry::builtin`) get invoked through their
    /// trait impl with a capability-scoped `ToolCtx`. When `None`,
    /// the new tier short-circuits and we fall back to the legacy
    /// `BuiltinTools::call` path so older fixtures keep working.
    pub conversation_id: Option<ConversationId>,
    /// Clock for `ToolCtx`. Defaults to `SystemClock`; tests can
    /// override via [`Self::with_clock`].
    pub clock: Arc<dyn Clock>,
    /// 2026-04-29 — inference client + model id used to construct
    /// `InferenceSubagentApi` when a tool's descriptor declares
    /// `Capability::SubagentSpawn`. `None` means "no subagent
    /// capability available this turn" — the dispatcher omits
    /// `ctx.subagent` and the tool falls into the standard
    /// "capability not granted" denial.
    pub inference: Option<(Arc<InferenceClient>, String)>,
    /// Stable durable parent run for child-agent work, when dispatching a
    /// tool inside the persistent runner path.
    pub parent_run_id: Option<String>,
    /// Root for content-addressed child-result handoff artifacts.
    pub artifact_root: Option<std::path::PathBuf>,
    pub cancel_flag: Option<Arc<std::sync::atomic::AtomicBool>>,
    /// Live event bus used by capabilities that emit broadcast
    /// events into the conversation (currently:
    /// `Capability::AttachmentSend`, which opens an Attachment
    /// card the SPA renders inline). `None` keeps the capability
    /// dormant — tools requesting it find `ctx.attachments == None`.
    pub events: Option<crate::events::EventBus>,
    /// Wake handle for the deep-research supervisor. Wired through
    /// to `DbResearchApi` so `research_start` can poke the
    /// supervisor immediately on insert instead of waiting up to 5 s
    /// for the next scheduled tick. `None` short-circuits to the
    /// tick-only path (fine for tests).
    pub research_supervisor_wake: Option<Arc<tokio::sync::Notify>>,
    /// Phase 3 — Signal transport endpoint resolver. Production
    /// wires the [`crate::sidecar_supervisor::SidecarSupervisor`]
    /// here; tests pass a `StaticEndpointResolver` that points at
    /// an in-process axum mock. `None` keeps `Capability::Transport`
    /// dormant — `signal.send_message` and `signal.reply` surface
    /// `Denied("transport capability not granted")`. Wraps in
    /// `Arc<dyn>` rather than the concrete supervisor so non-signal
    /// transports can land here later without churn.
    /// Phase B: vestigial. Kept as a `()` placeholder so existing
    /// builder methods + tests compile until they're cleaned up
    /// in a follow-up. The plugin tier reaches the sidecar via
    /// `sidecar_http_*` bindings directly — no host-side transport
    /// resolver needed anymore.
    pub signal_transport_resolver: Option<()>,
    /// Phase 3 — controller's registered Signal phone number,
    /// e.g. `+15551234567`. Read once at boot from
    /// `EXECLAW_SIGNAL_CONTROLLER_NUMBER`; flowed through the
    /// per-turn `SignalCliTransport` so `send` can populate
    /// signal-cli-rest-api's required `number` field.
    pub signal_self_number: Option<String>,
    /// Channel-keyed transport registry. The `send_attachment`
    /// built-in fans the file out across every channel the
    /// conversation is reachable on (Signal today; future plugins
    /// register at boot). Channel-agnostic — the dispatcher
    /// hands the registry to ServerAttachmentApi which walks
    /// bindings without naming Signal directly.
    pub host_transports: Option<crate::transport_registry::HostTransportRegistry>,
    /// Framework-owned tool-call ordinal for idempotent effects in the turn.
    pub transport_effect_ordinal: Arc<std::sync::atomic::AtomicU32>,
    /// Managed isolated workspace directory available to durable-run host tools.
    pub workspace_checkout_root: Option<std::path::PathBuf>,
    /// Test seam for the Docker-backed, effect-confined workspace executor.
    pub workspace_job_executor: Option<Arc<dyn execlaw_container_manager::WorkspaceJobExecutor>>,
}

impl<B: BuiltinTools> ChainedToolDispatch<B> {
    /// Legacy ctor — kept so existing call sites compile. `caller_trust`
    /// defaults to `Controller` and the access gate is disabled, which
    /// matches the pre-Phase-8a "no tool gate" semantic.
    pub fn new(host: PluginHost, caller_caps: Vec<String>, builtins: B) -> Self {
        Self {
            host,
            caller_caps,
            caller_trust: TrustLevel::Controller,
            builtins,
            access_db: None,
            policy_revision_snapshot: None,
            safety_profile: None,
            policy_context: None,
            live_principal: None,
            live_authority_fingerprint: None,
            mcp_host: None,
            conversation_id: None,
            clock: Arc::new(SystemClock),
            inference: None,
            parent_run_id: None,
            artifact_root: None,
            cancel_flag: None,
            events: None,
            research_supervisor_wake: None,
            signal_transport_resolver: None,
            signal_self_number: None,
            host_transports: None,
            transport_effect_ordinal: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            workspace_checkout_root: None,
            workspace_job_executor: None,
        }
    }

    /// Phase-8a ctor: wire the caller's trust class + the
    /// `config_tool_access` store. Production code paths use this so
    /// the per-tool allowlist is enforced on every dispatch.
    pub fn with_access_gate(
        host: PluginHost,
        caller_caps: Vec<String>,
        caller_trust: TrustLevel,
        builtins: B,
        access_db: Database,
    ) -> Self {
        let policy_revision_snapshot = ToolAccessStore::new(&access_db)
            .latest_policy_revision_id()
            .ok();
        Self {
            host,
            caller_caps,
            caller_trust,
            builtins,
            access_db: Some(access_db),
            policy_revision_snapshot,
            safety_profile: None,
            policy_context: None,
            live_principal: None,
            live_authority_fingerprint: None,
            mcp_host: None,
            conversation_id: None,
            clock: Arc::new(SystemClock),
            inference: None,
            parent_run_id: None,
            artifact_root: None,
            cancel_flag: None,
            events: None,
            research_supervisor_wake: None,
            signal_transport_resolver: None,
            signal_self_number: None,
            host_transports: None,
            transport_effect_ordinal: Arc::new(std::sync::atomic::AtomicU32::new(0)),
            workspace_checkout_root: None,
            workspace_job_executor: None,
        }
    }

    /// Attach the Phase-8d MCP dispatch tier. Builder-style so the
    /// existing test ctors don't have to grow another argument.
    pub fn with_mcp(mut self, mcp_host: McpHost) -> Self {
        self.mcp_host = Some(mcp_host);
        self
    }

    /// Bind this turn to the persisted principal that supplied its authority.
    /// The current trust record is captured now and rechecked immediately
    /// before every tool call. A missing principal fails closed at dispatch.
    pub fn with_live_principal(mut self, principal_id: &str) -> Self {
        let id = PrincipalId::from(principal_id.to_owned());
        let snapshot = self.access_db.as_ref().and_then(|db| {
            PrincipalStore::new(db)
                .get(&id)
                .ok()
                .flatten()
                .map(|principal| principal.trust_level)
        });
        self.live_authority_fingerprint = snapshot.as_ref().and_then(|trust| {
            serde_json::to_vec(trust)
                .ok()
                .map(|bytes| hex::encode(sha2::Sha256::digest(bytes)))
        });
        self.live_principal = Some((id, snapshot));
        self
    }

    fn live_authority_label_fields(&self) -> (Option<String>, Option<String>) {
        (
            self.live_principal
                .as_ref()
                .map(|(principal_id, _)| principal_id.as_str().to_owned()),
            self.live_authority_fingerprint.clone(),
        )
    }

    /// Bind a production turn when a participant id is available. System
    /// initiated turns have no principal and keep their existing host policy.
    pub fn with_live_principal_opt(mut self, principal_id: Option<&str>) -> Self {
        if let Some(principal_id) = principal_id {
            let persisted_id = if principal_id == "controller" {
                self.access_db.as_ref().and_then(|db| {
                    crate::routes::controller_principal_id(db)
                        .ok()
                        .map(|id| id.as_str().to_owned())
                })
            } else {
                Some(principal_id.to_owned())
            };
            if let Some(persisted_id) = persisted_id {
                self = self.with_live_principal(&persisted_id);
            } else {
                // Keep a failed Controller lookup in the live-principal
                // state so dispatch fails closed instead of silently
                // dropping the authority check.
                self = self.with_live_principal(principal_id);
            }
        }
        self
    }

    /// Apply the immutable task profile alongside caller trust and tool policy.
    pub fn with_safety_profile(
        mut self,
        profile: Option<execlaw_core::safety_profiles::SafetyProfileSnapshot>,
    ) -> Self {
        self.safety_profile = profile;
        self
    }

    /// Attach content-free run metadata used for policy decision simulation.
    pub fn with_policy_context(
        mut self,
        run_id: impl Into<String>,
        conversation_id: impl Into<String>,
        input_event_seq: i64,
    ) -> Self {
        self.policy_context = Some(PolicyDecisionContext {
            run_id: run_id.into(),
            conversation_id: conversation_id.into(),
            input_event_seq,
        });
        self
    }

    /// Attach a conversation id so registry-resolved `Arc<dyn
    /// ToolImpl>` built-ins receive a capability-scoped `ToolCtx`.
    /// Without this, the new built-in tier short-circuits and the
    /// dispatcher falls back to the legacy `BuiltinTools::call` path.
    pub fn with_conversation(mut self, conversation_id: ConversationId) -> Self {
        self.conversation_id = Some(conversation_id);
        self
    }

    /// Override the wall clock. Tests use this to drive deterministic
    /// memory `updated_at` values; production code uses the default
    /// `SystemClock`.
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// Attach the per-turn inference client + model so subagent-
    /// spawning tools (`delegate_task`) can fire child LLM calls
    /// against the parent's backend. Without this, the dispatcher
    /// omits `ctx.subagent` and any subagent tool returns a
    /// `Denied("subagent capability not granted")`.
    pub fn with_inference(
        mut self,
        client: Arc<InferenceClient>,
        model: impl Into<String>,
    ) -> Self {
        self.inference = Some((client, model.into()));
        self
    }

    /// Attach the durable parent run used to persist delegated child runs.
    pub fn with_parent_run(mut self, run_id: impl Into<String>) -> Self {
        self.parent_run_id = Some(run_id.into());
        self
    }

    async fn dispatch_manifest_host_tool(
        &self,
        contract: &execlaw_plugin_host::RegisteredTool,
        tool_name: &str,
        args: &serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let has_wildcard = self.caller_caps.iter().any(|cap| cap == "*");
        if !has_wildcard
            && contract
                .required_capabilities
                .iter()
                .any(|required| !self.caller_caps.iter().any(|cap| cap == required))
        {
            return Err(format!(
                "not authorized: host tool '{tool_name}' capability is not granted"
            ));
        }
        if let Some(floor) = contract.trust_floor.as_deref() {
            let required = TrustLevel::parse(floor)
                .ok_or_else(|| format!("host tool '{tool_name}' has an invalid trust floor"))?;
            if self.caller_trust.rank() < required.rank() {
                return Err(format!(
                    "not authorized: host tool '{tool_name}' requires trust >= {floor}"
                ));
            }
        }
        if let Some(validator) = &contract.schema_validator {
            validator.validate(args).map_err(|error| {
                format!("host tool '{tool_name}' arguments failed schema validation: {error}")
            })?;
        }
        if !tool_name.starts_with("workspace.") {
            return Err(format!(
                "no host implementation registered for '{tool_name}'"
            ));
        }
        let run_id = self
            .parent_run_id
            .as_deref()
            .ok_or_else(|| "workspace tools require a durable run".to_owned())?;
        let managed_root = self
            .workspace_checkout_root
            .as_deref()
            .ok_or_else(|| "workspace checkout service is unavailable".to_owned())?;
        let root = std::fs::canonicalize(managed_root)
            .map_err(|error| format!("workspace checkout root is unavailable: {error}"))?;
        let binding = execlaw_core::workspaces::WorkspaceStore::new(&self.host.db())
            .binding_for_run(run_id)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "durable run has no isolated workspace checkout".to_owned())?;
        let checkout = std::fs::canonicalize(&binding.checkout_path)
            .map_err(|error| format!("workspace checkout is unavailable: {error}"))?;
        if !checkout.is_dir() || !checkout.starts_with(&root) {
            return Err("workspace checkout is outside the managed checkout root".into());
        }
        match tool_name {
            "workspace.read_file" => {
                let path = args
                    .get("path")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| "workspace.read_file requires a path".to_owned())?;
                let target = crate::workspace_coding::safe_relative_file(&checkout, path)
                    .map_err(|error| error.message)?;
                let metadata = std::fs::metadata(&target).map_err(|error| error.to_string())?;
                if crate::workspace_coding::has_multiple_links(&target, &metadata) {
                    return Err("workspace file has multiple hard links".into());
                }
                if metadata.len() > 10 * 1024 * 1024 {
                    return Err("workspace file exceeds the 10 MiB read limit".into());
                }
                let bytes = std::fs::read(&target).map_err(|error| error.to_string())?;
                let content = String::from_utf8(bytes.clone())
                    .map_err(|_| "workspace file is not UTF-8".to_owned())?;
                Ok(
                    serde_json::json!({"path":path,"text":content,"sha256":hex::encode(sha2::Sha256::digest(bytes))}),
                )
            }
            "workspace.search" => {
                let query = args
                    .get("query")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| "workspace.search requires a query".to_owned())?;
                if query.is_empty() || query.len() > 1024 {
                    return Err("workspace search query must contain 1 to 1024 bytes".into());
                }
                let mut matches = Vec::new();
                for (path, bytes) in crate::workspace_coding::scan_workspace(&checkout)
                    .map_err(|error| error.message)?
                {
                    let Ok(text) = std::str::from_utf8(&bytes) else {
                        continue;
                    };
                    for (line, value) in text.lines().enumerate() {
                        if value.contains(query) {
                            matches.push(serde_json::json!({
                                "path":path,
                                "line":line + 1,
                                "text":value.chars().take(2048).collect::<String>()
                            }));
                            if matches.len() == 500 {
                                break;
                            }
                        }
                    }
                    if matches.len() == 500 {
                        break;
                    }
                }
                Ok(serde_json::json!({"matches":matches}))
            }
            "workspace.apply_patch" => {
                let request: crate::workspace_coding::WorkspacePatchRequest =
                    serde_json::from_value(args.clone())
                        .map_err(|_| "invalid workspace patch arguments".to_owned())?;
                if request.edits.is_empty() || request.edits.len() > 64 {
                    return Err("workspace patch must contain 1 to 64 file edits".into());
                }
                let bytes = serde_json::to_vec(&request).map_err(|error| error.to_string())?;
                let request_hash = hex::encode(sha2::Sha256::digest(&bytes));
                let ordinal = self
                    .transport_effect_ordinal
                    .load(std::sync::atomic::Ordering::SeqCst);
                let request_id = format!("tool-{ordinal}");
                let owner = format!(
                    "turn-{}",
                    hex::encode(sha2::Sha256::digest(
                        format!("{run_id}:{ordinal}").as_bytes()
                    ))
                );
                let store = execlaw_core::workspaces::WorkspaceStore::new(&self.host.db());
                match store
                    .begin_patch_job(
                        run_id,
                        &request_id,
                        &request_hash,
                        &owner,
                        chrono::Utc::now().timestamp(),
                    )
                    .map_err(|error| error.to_string())?
                {
                    execlaw_core::workspaces::WorkspacePatchClaim::Succeeded(value) => {
                        return serde_json::from_str(&value).map_err(|error| error.to_string());
                    }
                    execlaw_core::workspaces::WorkspacePatchClaim::Failed(code) => {
                        return Err(format!("workspace patch failed previously: {code}"));
                    }
                    execlaw_core::workspaces::WorkspacePatchClaim::Busy => {
                        return Err("workspace patch is already running".into());
                    }
                    execlaw_core::workspaces::WorkspacePatchClaim::Claimed => {}
                }
                let mut results = Vec::with_capacity(request.edits.len());
                let mut error_code = None;
                for edit in &request.edits {
                    match crate::workspace_coding::apply_checkout_edit(&checkout, edit) {
                        Ok(result) => results.push(result),
                        Err(error) => {
                            error_code = Some(error.code.to_owned());
                            break;
                        }
                    }
                }
                let result =
                    serde_json::json!({"run_id":run_id,"request_id":request_id,"edits":results});
                let result_json =
                    serde_json::to_string(&result).map_err(|error| error.to_string())?;
                store
                    .finish_patch_job(
                        run_id,
                        &request_id,
                        &owner,
                        error_code.is_none().then_some(result_json.as_str()),
                        error_code.as_deref(),
                        chrono::Utc::now().timestamp(),
                    )
                    .map_err(|error| error.to_string())?;
                if let Some(code) = error_code {
                    return Err(format!("workspace patch failed: {code}"));
                }
                Ok(result)
            }
            "workspace.run" | "workspace.diagnostics" => {
                self.dispatch_workspace_execution_job(run_id, &checkout, tool_name, args)
                    .await
            }
            _ => Err(format!(
                "no host implementation registered for '{tool_name}'"
            )),
        }
    }

    async fn dispatch_workspace_execution_job(
        &self,
        run_id: &str,
        checkout: &std::path::Path,
        tool_name: &str,
        args: &serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        use execlaw_container_manager::{
            BollardWorkspaceJobExecutor, WorkspaceDiagnosticsRequest, WorkspaceJobExecutor,
            WorkspaceRunRequest,
        };
        use execlaw_core::workspaces::{WorkspaceExecutionJobClaim, WorkspaceStore};

        let db = self.host.db();
        let store = WorkspaceStore::new(db);
        let config = store
            .execution_config()
            .map_err(|error| error.to_string())?;
        let image_reference = config.image_reference.ok_or_else(|| {
            "Controller has not configured a workspace toolchain image".to_owned()
        })?;
        let timeout_ms = args
            .get("timeout_ms")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(if tool_name == "workspace.run" {
                60_000
            } else {
                30_000
            });
        if !(1_000..=180_000).contains(&timeout_ms) {
            return Err("workspace job timeout must be between 1000 and 180000 ms".into());
        }
        let ordinal = self
            .transport_effect_ordinal
            .load(std::sync::atomic::Ordering::SeqCst);
        let job_id = format!("tool-{ordinal}");
        let job_name = format!(
            "{}-{ordinal}",
            &hex::encode(sha2::Sha256::digest(run_id.as_bytes()))[..20]
        );

        let (operation, request_hash, run_request, diagnostics_request) =
            if tool_name == "workspace.run" {
                let argv =
                    args.get("argv")
                        .and_then(serde_json::Value::as_array)
                        .ok_or_else(|| "workspace.run requires an argv array".to_owned())?
                        .iter()
                        .map(|argument| {
                            argument.as_str().map(str::to_owned).ok_or_else(|| {
                                "workspace.run argv entries must be strings".to_owned()
                            })
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                let request_hash = hex::encode(sha2::Sha256::digest(
                    serde_json::to_vec(&serde_json::json!({
                        "tool":tool_name,"args":args,"image":image_reference
                    }))
                    .map_err(|error| error.to_string())?,
                ));
                (
                    "terminal",
                    request_hash,
                    Some(WorkspaceRunRequest {
                        image_reference: image_reference.clone(),
                        checkout_path: std::path::PathBuf::new(),
                        job_name: job_name.clone(),
                        argv,
                        timeout_ms,
                    }),
                    None,
                )
            } else {
                let path = args
                    .get("path")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| "workspace.diagnostics requires a path".to_owned())?;
                let language_id = args
                    .get("language_id")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| "workspace.diagnostics requires a language_id".to_owned())?;
                let server_argv = config
                    .language_servers
                    .get(language_id)
                    .cloned()
                    .ok_or_else(|| {
                        format!("no Controller-configured language server for '{language_id}'")
                    })?;
                let target = crate::workspace_coding::safe_relative_file(checkout, path)
                    .map_err(|error| error.message)?;
                let metadata = std::fs::metadata(&target).map_err(|error| error.to_string())?;
                if crate::workspace_coding::has_multiple_links(&target, &metadata) {
                    return Err("workspace diagnostics file has multiple hard links".into());
                }
                if metadata.len() > 1024 * 1024 {
                    return Err("workspace diagnostics file exceeds the 1 MiB limit".into());
                }
                let text =
                    String::from_utf8(std::fs::read(&target).map_err(|error| error.to_string())?)
                        .map_err(|_| "workspace diagnostics file is not UTF-8".to_owned())?;
                let request_hash = hex::encode(sha2::Sha256::digest(
                    serde_json::to_vec(&serde_json::json!({
                        "tool":tool_name,"path":path,"language_id":language_id,
                        "text_sha256":hex::encode(sha2::Sha256::digest(text.as_bytes())),
                        "server_argv":server_argv,"image":image_reference
                    }))
                    .map_err(|error| error.to_string())?,
                ));
                (
                    "diagnostics",
                    request_hash,
                    None,
                    Some(WorkspaceDiagnosticsRequest {
                        image_reference: image_reference.clone(),
                        checkout_path: std::path::PathBuf::new(),
                        job_name: job_name.clone(),
                        server_argv,
                        path: path.to_owned(),
                        language_id: language_id.to_owned(),
                        text,
                        timeout_ms,
                    }),
                )
            };

        let owner = uuid::Uuid::new_v4().to_string();
        let lease_seconds = i64::try_from(timeout_ms / 1_000)
            .unwrap_or(180)
            .saturating_add(120);
        match store
            .begin_execution_job(
                run_id,
                &job_id,
                &request_hash,
                operation,
                &owner,
                lease_seconds,
                chrono::Utc::now().timestamp(),
            )
            .map_err(|error| error.to_string())?
        {
            WorkspaceExecutionJobClaim::Succeeded(result) => {
                return serde_json::from_str(&result).map_err(|error| error.to_string());
            }
            WorkspaceExecutionJobClaim::Failed(code) => {
                return Err(format!("workspace process job failed previously: {code}"));
            }
            WorkspaceExecutionJobClaim::Busy => {
                return Err("workspace process job is already running".into());
            }
            WorkspaceExecutionJobClaim::Claimed => {}
        }

        let snapshot = match crate::workspace_coding::create_tool_snapshot(checkout) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                store
                    .finish_execution_job(
                        run_id,
                        &job_id,
                        &owner,
                        None,
                        Some("workspace_snapshot_failed"),
                        chrono::Utc::now().timestamp(),
                    )
                    .map_err(|finish| finish.to_string())?;
                return Err(error.message);
            }
        };
        let executor: Arc<dyn WorkspaceJobExecutor> = match &self.workspace_job_executor {
            Some(executor) => executor.clone(),
            None => match BollardWorkspaceJobExecutor::connect(db.clone()) {
                Ok(executor) => Arc::new(executor),
                Err(error) => {
                    store
                        .finish_execution_job(
                            run_id,
                            &job_id,
                            &owner,
                            None,
                            Some("workspace_runtime_unavailable"),
                            chrono::Utc::now().timestamp(),
                        )
                        .map_err(|finish| finish.to_string())?;
                    return Err(error.to_string());
                }
            },
        };
        let job_result = if let Some(mut request) = run_request {
            request.checkout_path = snapshot.path().to_owned();
            executor.run(request).await.map(|result| {
                serde_json::json!({
                    "run_id":run_id,"job_id":job_id,"operation":operation,
                    "exit_code":result.exit_code,"timed_out":result.timed_out,
                    "output_truncated":result.output_truncated,"output":result.output,
                    "elapsed_ms":result.elapsed_ms
                })
            })
        } else if let Some(mut request) = diagnostics_request {
            request.checkout_path = snapshot.path().to_owned();
            executor.diagnostics(request).await.map(|result| {
                serde_json::json!({
                    "run_id":run_id,"job_id":job_id,"operation":operation,
                    "language_id":result.language_id,"path":result.path,
                    "diagnostics":result.diagnostics,"elapsed_ms":result.elapsed_ms
                })
            })
        } else {
            Err(execlaw_container_manager::WorkspaceExecutionError::Invalid(
                "workspace execution request is empty".into(),
            ))
        };
        match job_result {
            Ok(result) => {
                let result_json =
                    serde_json::to_string(&result).map_err(|error| error.to_string())?;
                store
                    .finish_execution_job(
                        run_id,
                        &job_id,
                        &owner,
                        Some(&result_json),
                        None,
                        chrono::Utc::now().timestamp(),
                    )
                    .map_err(|error| error.to_string())?;
                Ok(result)
            }
            Err(error) => {
                store
                    .finish_execution_job(
                        run_id,
                        &job_id,
                        &owner,
                        None,
                        Some("workspace_execution_failed"),
                        chrono::Utc::now().timestamp(),
                    )
                    .map_err(|finish| finish.to_string())?;
                Err(error.to_string())
            }
        }
    }

    /// Attach the managed isolated-checkout root for manifest-declared workspace host tools.
    pub fn with_workspace_checkout_root(mut self, root: impl Into<std::path::PathBuf>) -> Self {
        self.workspace_checkout_root = Some(root.into());
        self
    }

    /// Attach a workspace job executor; production falls back to Bollard when absent.
    pub fn with_workspace_job_executor(
        mut self,
        executor: Arc<dyn execlaw_container_manager::WorkspaceJobExecutor>,
    ) -> Self {
        self.workspace_job_executor = Some(executor);
        self
    }

    /// Attach the managed artifact directory used by durable child results.
    pub fn with_artifact_root(mut self, root: impl Into<std::path::PathBuf>) -> Self {
        self.artifact_root = Some(root.into());
        self
    }

    /// Attach the current turn's cancellation flag to delegated child work.
    pub fn with_cancel_flag(mut self, flag: Arc<std::sync::atomic::AtomicBool>) -> Self {
        self.cancel_flag = Some(flag);
        self
    }

    /// Attach the live event bus so attachment-send (and any
    /// future broadcast-emitting capability) can fire WS events.
    /// Production wiring sets this from `AppState::events`; tests
    /// that need to assert on emitted events pass a dedicated bus.
    pub fn with_events(mut self, events: crate::events::EventBus) -> Self {
        self.events = Some(events);
        self
    }

    /// Attach the deep-research supervisor's wake handle so
    /// `research_start` poke-wakes the supervisor on insert. Without
    /// this, the supervisor waits up to its 5 s tick interval before
    /// claiming the new Pending row — which is the dominant source
    /// of "the agent took a while to come back with the
    /// clarification question" wall-clock latency.
    pub fn with_research_supervisor_wake(mut self, wake: Arc<tokio::sync::Notify>) -> Self {
        self.research_supervisor_wake = Some(wake);
        self
    }

    /// Phase B: signal transport wiring is gone — the plugin tier
    /// reaches the sidecar through Rhai's `sidecar_http_*`
    /// bindings. These shims kept for API compat with call sites
    /// still passing `None` (e.g. tests, dispatch_routine_turn);
    /// remove in a follow-up cleanup.
    pub fn with_signal_transport<T>(self, _resolver: T, _self_number: Option<String>) -> Self {
        self
    }

    pub fn with_signal_transport_opt<T>(
        self,
        _resolver: Option<T>,
        _self_number: Option<String>,
    ) -> Self {
        self
    }

    /// Wire the host-transport registry. Production call sites
    /// pass `state.host_transports.clone()`; tests typically leave
    /// this unset and the `send_attachment` path skips the fan-out
    /// step (web-UI chip remains the only deliverable).
    pub fn with_host_transports(
        mut self,
        registry: crate::transport_registry::HostTransportRegistry,
    ) -> Self {
        self.host_transports = Some(registry);
        self
    }

    /// Convenience: chain through `Option<Arc<Notify>>` directly.
    /// Production callers read the handle from
    /// `state.research_supervisor.as_ref().map(|s| s.wake.clone())`,
    /// which is `Option<_>` because test fixtures construct an
    /// `AppState` without a supervisor. This avoids an `if-let`
    /// dance at every dispatch site.
    pub fn with_research_supervisor_wake_opt(
        mut self,
        wake: Option<Arc<tokio::sync::Notify>>,
    ) -> Self {
        self.research_supervisor_wake = wake;
        self
    }

    /// Build a `ToolCtx` populated with exactly the capability APIs
    /// the tool's descriptor declared. A tool that didn't request
    /// `MemoryRead`/`Write` gets `ctx.memory == None` and either
    /// returns `Denied` from its own body or simply never reaches a
    /// memory call.
    #[allow(clippy::too_many_lines)]
    fn build_ctx_for(&self, tool: &Arc<dyn ToolImpl>) -> Result<ToolCtx, String> {
        let conv_id = self
            .conversation_id
            .clone()
            .ok_or_else(|| "no conversation_id on dispatcher".to_string())?;
        let db = self.host.db().clone();
        let mut ctx = ToolCtx::empty(
            conv_id.clone(),
            self.caller_trust.as_str(),
            self.clock.clone(),
        );
        let caps = &tool.descriptor().capabilities;
        let needs_conv = caps.iter().any(|c| {
            matches!(
                c,
                Capability::ConversationRead | Capability::ConversationWrite
            )
        });
        let needs_mem = caps
            .iter()
            .any(|c| matches!(c, Capability::MemoryRead | Capability::MemoryWrite));
        let needs_notify = caps.iter().any(|c| matches!(c, Capability::Notify));
        let needs_schedule = caps
            .iter()
            .any(|c| matches!(c, Capability::ScheduleRead | Capability::ScheduleWrite));
        let needs_web_fetch = caps.iter().any(|c| matches!(c, Capability::WebFetch));
        let needs_search = caps.iter().any(|c| matches!(c, Capability::Search));
        let needs_subagent = caps.iter().any(|c| matches!(c, Capability::SubagentSpawn));
        let needs_research_spawn = caps.iter().any(|c| matches!(c, Capability::ResearchSpawn));
        let needs_research_read = caps.iter().any(|c| matches!(c, Capability::ResearchRead));
        if needs_conv {
            ctx.conversation = Some(Arc::new(DbConversationApi::new(
                db.clone(),
                conv_id.clone(),
            )));
        }
        let now = self.clock.now_unix();
        if needs_mem {
            let api = DbMemoryApi::new(db.clone(), self.caller_trust.as_str(), now);
            ctx.memory = Some(Arc::new(match &self.parent_run_id {
                Some(run_id) => api.with_run_id(run_id.clone()),
                None => api,
            }));
        }
        if needs_notify {
            ctx.notify = Some(Arc::new(DbNotifyApi::new(db.clone(), conv_id.clone(), now)));
        }
        if needs_schedule {
            ctx.schedule = Some(Arc::new(DbScheduleApi::new(
                db,
                self.caller_trust.as_str(),
                conv_id,
                now,
            )));
        }
        if needs_web_fetch {
            ctx.web_fetch = Some(Arc::new(HttpWebFetchApi::new()));
        }
        if needs_search {
            // 2026-05-04 (rev 9): the dispatcher used to hard-code
            // DuckDuckGo here. With config_search_providers + the
            // resolver landing, the active provider is resolved
            // from DB at every dispatch — the operator can swap
            // providers via Settings → Search without restart.
            // Resolver always returns SOMETHING (falls back to
            // DDG on error), so this can never produce None.
            ctx.search = Some(crate::search_resolver::resolve_active_provider(
                &self.host.db().clone(),
            ));
        }
        if needs_subagent {
            if let Some((client, model)) = self.inference.as_ref() {
                let mut api = InferenceSubagentApi::new_with_parent(
                    client.clone(),
                    model.clone(),
                    self.host.db().clone(),
                    ctx.conversation_id.clone(),
                    self.parent_run_id.clone(),
                );
                if let Some(root) = &self.artifact_root {
                    api = api.with_artifact_root(root.clone());
                }
                if let Some(flag) = &self.cancel_flag {
                    api = api.with_cancel_flag(flag.clone());
                }
                ctx.subagent = Some(Arc::new(api));
            }
            // When `inference` isn't wired (test fixture / no
            // backend resolved this turn), we leave `ctx.subagent
            // == None` and the tool falls into its own "capability
            // not granted" denial.
        }
        // Research API: declare `ResearchSpawn` to get a spawn-
        // enabled api; declaring only `ResearchRead` returns a
        // read-only impl whose `start` errors. A descriptor that
        // declares neither leaves `ctx.research == None`.
        if needs_research_spawn {
            let mut api = DbResearchApi::with_spawn(
                self.host.db().clone(),
                self.caller_trust.as_str(),
                ctx.conversation_id.clone(),
                now,
            );
            if let Some(wake) = self.research_supervisor_wake.as_ref() {
                api = api.with_supervisor_wake(wake.clone());
            }
            ctx.research = Some(Arc::new(api));
        } else if needs_research_read {
            ctx.research = Some(Arc::new(DbResearchApi::read_only(
                self.host.db().clone(),
                self.caller_trust.as_str(),
                ctx.conversation_id.clone(),
                now,
            )));
        }
        // Phase B: the host no longer injects a TransportApi for
        // Capability::Transport — channel plugins are script-tier
        // and reach their sidecar via `sidecar_http_*` Rhai
        // bindings directly. The capability is still recognised
        // here as a dormant gate; tool descriptors that declare it
        // surface "transport capability not granted" if any path
        // tries to reach `ctx.transport` (which is always None
        // post-Phase-B).
        let _needs_transport = caps.iter().any(|c| matches!(c, Capability::Transport));
        let needs_mcp_admin = caps.iter().any(|c| matches!(c, Capability::McpAdmin));
        let needs_attachment_send = caps.iter().any(|c| matches!(c, Capability::AttachmentSend));
        if needs_mcp_admin {
            // Belt-and-suspenders trust gate. The tool descriptor
            // already pins `default_allowed_classes = ["Controller"]`
            // and the dispatch policy check rejects below-Controller
            // callers BEFORE this point — but populating mcp_admin
            // for non-Controller trust would let a future policy
            // edit accidentally widen the surface. Hard-code the
            // gate here too.
            if self
                .caller_trust
                .as_str()
                .eq_ignore_ascii_case("Controller")
            {
                if let Some(host) = &self.mcp_host {
                    ctx.mcp_admin = Some(Arc::new(crate::tool_apis_mcp::DbMcpAdminApi::new(
                        self.host.db().clone(),
                        host.clone(),
                    )));
                }
                // No mcp_host wired (test fixture / boot order bug):
                // leave None and the tool surfaces a clean denial.
            }
        }
        if needs_attachment_send {
            if let Some(events) = self.events.as_ref() {
                // Hand the attachment API the host-transport
                // registry so `send` fans out across every channel
                // the conversation is reachable on. Channel-agnostic
                // — a Telegram/email/etc. plugin that registers a
                // factory at boot is auto-included with no edits to
                // the attachment API.
                //
                // 2026-05-15 — also wire `artifacts_root` so the
                // chart.render built-in can persist its rendered PNG
                // through `ctx.attachments.create_artifact`. Same
                // root the script-tier `host_caps.create_artifact_attachment`
                // path uses (`EXECLAW_PLUGIN_ARTIFACTS_DIR` env
                // override → `~/.execlaw/plugin_artifacts/` default
                // → `./.execlaw/plugin_artifacts/` last-ditch
                // fallback). Tests that don't opt in get an
                // explicit error from create_artifact instead of
                // silently writing under the developer's real home.
                let artifacts_root = builtin_artifacts_root();
                // 2026-05-16 — `with_plugin_host` was missing, which
                // silently disabled the auto-bridge path inside
                // `bridge_to_originating_transport` (it returns
                // early without logging when `plugin_host.is_none()`).
                // Symptom: chart.render fired cleanly, the chart
                // landed in web UI as a card, but no
                // `<channel>.send_with_attachments` call ever
                // dispatched — operators on Signal/WhatsApp/etc.
                // never received the PNG. The same gap silently
                // affected `send_attachment`'s transport fan-out.
                // Wire the plugin host so the bridge can actually
                // dispatch the channel-side delivery tool.
                let effect_turn_seq = self
                    .parent_run_id
                    .as_deref()
                    .and_then(|run_id| {
                        run_id
                            .strip_prefix(&format!("turn:{}:", ctx.conversation_id.as_str()))
                            .and_then(|seq| seq.parse::<i64>().ok())
                    })
                    .or_else(|| latest_user_event_seq(&self.host.db(), &ctx.conversation_id).ok());
                let (principal_id, fingerprint) = self.live_authority_label_fields();
                ctx.attachments = Some(Arc::new(
                    crate::attachment_api::ServerAttachmentApi::new(
                        self.host.db().clone(),
                        events.clone(),
                        ctx.conversation_id.clone(),
                    )
                    .with_transports(self.host_transports.clone())
                    .with_plugin_host(self.host.clone())
                    .with_effect_context(effect_turn_seq, self.transport_effect_ordinal.clone())
                    .with_outbound_authority(principal_id, fingerprint)
                    .with_artifacts_root(artifacts_root),
                ));
            }
            // No bus → capability stays dormant. The tool body's
            // own `ctx.attachments.is_none()` denial fires.
        }
        Ok(ctx)
    }

    /// Dispatch a tool name through the new `Arc<dyn ToolImpl>` tier
    /// if the registry has a built-in for it. Returns `Some(result)`
    /// when the registry owns the name (success or tool error);
    /// returns `None` when no built-in is registered, so the caller
    /// can fall through to the legacy `BuiltinTools::call` path or
    /// the plugin host.
    async fn try_registry_builtin(
        &self,
        tool_name: &str,
        args: &serde_json::Value,
    ) -> Option<Result<serde_json::Value, String>> {
        let tool = self.host.registry().builtin(tool_name)?;
        // The descriptor declared capabilities the new path needs
        // to populate, but we don't have a conversation id to scope
        // them to. Fall through (return None) so the legacy
        // `BuiltinTools` impl (if any) gets a chance.
        self.conversation_id.as_ref()?;
        if let Err(error) = self.host.registry().validate_builtin_input(tool_name, args) {
            return Some(Err(error));
        }
        let ctx = match self.build_ctx_for(&tool) {
            Ok(c) => c,
            Err(e) => return Some(Err(e)),
        };
        Some(match tool.invoke(ctx, args.clone()).await {
            ToolOutcome::Ok(v) => self
                .host
                .registry()
                .validate_builtin_result(tool_name, &v)
                .map(|()| v),
            ToolOutcome::Err { code, message } => Err(format!("{code}: {message}")),
            ToolOutcome::Denied { reason } => Err(format!("denied: {reason}")),
        })
    }

    pub fn into_arc(self) -> Arc<dyn ToolDispatch>
    where
        B: 'static,
    {
        Arc::new(self)
    }

    fn record_policy_decision(
        &self,
        tool_name: &str,
        outcome: execlaw_core::policy_simulation::PolicyDecisionOutcome,
        reason_code: &str,
    ) -> Result<(), String> {
        let (Some(context), Some(db)) = (&self.policy_context, &self.access_db) else {
            return Ok(());
        };
        let access = ToolAccessStore::new(db)
            .get(tool_name)
            .map_err(|error| format!("policy decision metadata lookup failed: {error}"))?;
        let globally_enabled = access
            .as_ref()
            .is_none_or(|row| row.enabled && row.removed_at.is_none());
        let allowed_classes = access
            .as_ref()
            .map(|row| row.allowed_classes.clone())
            .unwrap_or_default();
        let mut required_capabilities = Vec::new();
        let mut sensitive = false;
        let mut external_effect = tool_name.starts_with(MCP_TOOL_PREFIX);
        let mut trust_floor = None;

        if let Some(tool) = self.host.registry().builtin(tool_name) {
            let descriptor = tool.descriptor();
            sensitive = descriptor.sensitive;
            for capability in &descriptor.capabilities {
                required_capabilities.extend(
                    execlaw_policy::trust::required_policy_caps(*capability)
                        .iter()
                        .map(|capability| (*capability).to_owned()),
                );
                external_effect |= matches!(
                    capability,
                    Capability::WebFetch
                        | Capability::Search
                        | Capability::Notify
                        | Capability::SubagentSpawn
                        | Capability::ResearchSpawn
                        | Capability::AttachmentSend
                        | Capability::Transport
                );
                sensitive |= matches!(capability, Capability::MemoryRead | Capability::MemoryWrite);
            }
        } else if let Some(tool) = self.host.registry().tool(tool_name) {
            required_capabilities = tool.required_capabilities.clone();
            trust_floor = tool.trust_floor.clone();
            // Plugin manifests do not yet carry a complete effect/sensitivity
            // contract. Preserve a conservative simulation record until H065
            // can supply exact classifications.
            external_effect = true;
            sensitive = true;
            sensitive |= required_capabilities.iter().any(|capability| {
                let lower = capability.to_ascii_lowercase();
                lower.contains("secret") || lower.contains("sensitive")
            });
        }
        required_capabilities.sort();
        required_capabilities.dedup();
        let profile_id = self
            .safety_profile
            .as_ref()
            .map(|profile| profile.profile_id.as_str().to_owned());
        let profile_revision = self.safety_profile.as_ref().map(|profile| profile.revision);
        let decision = execlaw_core::policy_simulation::NewToolPolicyDecision {
            run_id: context.run_id.clone(),
            conversation_id: context.conversation_id.clone(),
            input_event_seq: context.input_event_seq,
            tool_name: tool_name.to_owned(),
            caller_trust: self.caller_trust.as_str().to_owned(),
            trust_floor,
            required_capabilities,
            globally_enabled,
            allowed_classes,
            profile_id,
            profile_revision,
            outcome,
            reason_code: reason_code.to_owned(),
            sensitive,
            external_effect,
            approval_required: false,
            decided_at: chrono::Utc::now().timestamp(),
        };
        execlaw_core::policy_simulation::ToolPolicyDecisionStore::new(db)
            .record(&decision)
            .map_err(|error| format!("persist policy decision metadata: {error}"))?;
        Ok(())
    }

    /// Per-tool access check. Returns `Ok(())` when the call should
    /// proceed, `Err(reason)` when it must be denied. Centralises the
    /// rules so the dispatch chain has exactly one enforcement point.
    fn check_access(&self, tool_name: &str) -> Result<(), String> {
        self.check_live_principal()?;
        if let Some(db) = &self.access_db {
            let current_revision = ToolAccessStore::new(db)
                .latest_policy_revision_id()
                .map_err(|error| format!("tool policy revision lookup failed: {error}"))?;
            if self.policy_revision_snapshot != Some(current_revision) {
                return Err("not authorized: tool policy changed during this turn".into());
            }
        }
        if let Some(db) = &self.access_db
            && let Some(row) = ToolAccessStore::new(db)
                .get(tool_name)
                .map_err(|e| format!("tool_access lookup failed: {e}"))?
        {
            if !row.enabled {
                return Err(format!("not authorized: tool '{tool_name}' is disabled"));
            }
            if row.removed_at.is_some() {
                return Err(format!(
                    "not authorized: tool '{tool_name}' is no longer registered by its source"
                ));
            }
            let caller = self.caller_trust.as_str();
            if !row.allowed_classes.iter().any(|c| c == caller) {
                return Err(format!(
                    "not authorized: tool '{tool_name}' is not allowed for trust class {caller}"
                ));
            }
        }
        self.check_safety_profile(tool_name)
    }

    fn check_safety_profile(&self, tool_name: &str) -> Result<(), String> {
        let Some(profile) = &self.safety_profile else {
            return Ok(());
        };
        if matches!(tool_name, "execlaw.discover_tool" | "execlaw.read_artifact") {
            return Ok(());
        }
        let allowed = if let Some(tool) = self.host.registry().builtin(tool_name) {
            profile.allows_builtin(tool_name, &tool.descriptor().capabilities)
        } else if tool_name.starts_with(MCP_TOOL_PREFIX) {
            profile.allows_mcp_tool(tool_name)
        } else if let Some(tool) = self.host.registry().tool(tool_name) {
            profile.allows_plugin_tool(tool_name, &tool.required_capabilities)
        } else {
            false
        };
        if allowed {
            Ok(())
        } else {
            Err(format!(
                "not authorized: active safety profile '{}' denies this tool",
                profile.profile_id.as_str()
            ))
        }
    }

    fn check_live_principal(&self) -> Result<(), String> {
        let Some((principal_id, Some(snapshot))) = &self.live_principal else {
            if self.live_principal.is_some() {
                return Err("not authorized: caller principal is unavailable".into());
            }
            return Ok(());
        };
        let db = self
            .access_db
            .as_ref()
            .ok_or_else(|| "not authorized: live principal checks are unavailable".to_owned())?;
        let current = PrincipalStore::new(db)
            .get(principal_id)
            .map_err(|error| format!("live principal authority lookup failed: {error}"))?
            .ok_or_else(|| "not authorized: caller principal was removed".to_owned())?;
        if &current.trust_level != snapshot {
            return Err("not authorized: caller authority changed during the turn".into());
        }
        if current.trust_level.class_tag() != self.caller_trust.as_str() {
            return Err("not authorized: caller trust no longer matches this turn".into());
        }
        match &current.trust_level {
            StoredTrustLevel::Blocked { .. } | StoredTrustLevel::UnknownPending { .. } => {
                Err("not authorized: caller principal is not active".into())
            }
            StoredTrustLevel::Delegated {
                expires_at: Some(expires_at),
                ..
            } if *expires_at <= chrono::Utc::now().timestamp() => {
                Err("not authorized: delegated authority has expired".into())
            }
            _ => Ok(()),
        }
    }
}

#[async_trait]
impl<B: BuiltinTools + 'static> ToolDispatch for ChainedToolDispatch<B> {
    fn outbox_idempotency_key(
        &self,
        tool_name: &str,
        turn_seq: i64,
        ordinal: u32,
    ) -> Option<String> {
        let conversation_id = self.conversation_id.as_ref()?;
        let transports = self.host_transports.as_ref()?;
        let tool = self.host.registry().tool(tool_name)?;
        transports.channel_for_send_tool(&tool.plugin_id, tool_name)?;
        if turn_seq <= 0 {
            return None;
        }
        Some(
            execlaw_core::ids::IdempotencyKey::mint(
                conversation_id,
                execlaw_core::ids::TurnSeq(turn_seq),
                ordinal,
            )
            .as_str()
            .to_owned(),
        )
    }

    fn set_effect_ordinal(&self, ordinal: u32) {
        self.transport_effect_ordinal
            .store(ordinal, std::sync::atomic::Ordering::SeqCst);
    }

    async fn call(
        &self,
        tool_name: &str,
        args_json: &serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        // Phase-8a access gate runs FIRST so a denied call never
        // reaches a builtin's side-effect, a plugin subprocess, or an
        // MCP server.
        if let Err(error) = self.check_access(tool_name) {
            self.record_policy_decision(
                tool_name,
                execlaw_core::policy_simulation::PolicyDecisionOutcome::Denied,
                policy_reason_code(&error),
            )?;
            return Err(error);
        }

        // 2026-05-16 — fix #4: built-in capability gate. The plugin
        // tier already enforces `caller_caps ⊇ required_capabilities`
        // at dispatch (see `PluginHost::call_tool`), but registry
        // built-ins were getting their `ToolCtx` APIs wired up based
        // on the tool's descriptor alone — meaning a KnownLimited
        // caller invoking a `Memory` built-in got a fully populated
        // memory API and silently bypassed `capability_set` policy.
        // Walk the descriptor's declared capabilities up front and
        // reject when the caller lacks the matching policy tag.
        // Wildcard `"*"` (Controller) satisfies any. Tools registered
        // through the legacy `BuiltinTools` API (no descriptor)
        // remain ungated here — they're a shrinking surface and the
        // `check_access` trust-class gate above still applies.
        if let Some(tool) = self.host.registry().builtin(tool_name) {
            let caps: Vec<&str> = self.caller_caps.iter().map(|s| s.as_str()).collect();
            for c in &tool.descriptor().capabilities {
                if let Err(missing) = execlaw_policy::trust::check_builtin_capability(*c, &caps) {
                    let error = format!(
                        "not authorized: tool '{tool_name}' requires capability \
                         '{missing}' not in caller's set"
                    );
                    self.record_policy_decision(
                        tool_name,
                        execlaw_core::policy_simulation::PolicyDecisionOutcome::Denied,
                        "caller_capability_denied",
                    )?;
                    return Err(error);
                }
            }
        }

        if let Some(registered) = self.host.registry().tool(tool_name) {
            if !self.caller_caps.iter().any(|cap| cap == "*")
                && registered
                    .required_capabilities
                    .iter()
                    .any(|required| !self.caller_caps.iter().any(|cap| cap == required))
            {
                let error = format!(
                    "not authorized: plugin tool '{tool_name}' requires an ungranted capability"
                );
                self.record_policy_decision(
                    tool_name,
                    execlaw_core::policy_simulation::PolicyDecisionOutcome::Denied,
                    "caller_capability_denied",
                )?;
                return Err(error);
            }
            if let Some(floor) = registered.trust_floor.as_deref()
                && TrustLevel::parse(floor)
                    .is_some_and(|required| self.caller_trust.rank() < required.rank())
            {
                let error =
                    format!("not authorized: plugin tool '{tool_name}' requires trust >= {floor}");
                self.record_policy_decision(
                    tool_name,
                    execlaw_core::policy_simulation::PolicyDecisionOutcome::Denied,
                    "trust_floor_denied",
                )?;
                return Err(error);
            }
        }
        self.record_policy_decision(
            tool_name,
            execlaw_core::policy_simulation::PolicyDecisionOutcome::Allowed,
            "allowed",
        )?;

        if tool_name.starts_with("workspace.")
            && let Some(contract) = self.host.registry().host_tool(tool_name)
        {
            return self
                .dispatch_manifest_host_tool(&contract, tool_name, args_json)
                .await;
        }

        // Phase-8d: prefix-route MCP-sourced tools to the connection
        // manager. Falling back to builtins/plugins for an
        // `mcp:`-prefixed name is wrong — those tiers don't speak
        // the prefix.
        if tool_name.starts_with(MCP_TOOL_PREFIX) {
            return match &self.mcp_host {
                Some(host) => host.call_tool(tool_name, args_json.clone()).await,
                None => Err(format!("no MCP host configured to dispatch '{tool_name}'")),
            };
        }

        // 2026-05-16 — data-ref resolution. Walk the args and replace
        // any `{"$data_ref": "<id>"}` object with the JSON value the
        // host stored under that id (typically a previous tool's
        // structured output). This avoids forcing the model to
        // re-emit large arrays (e.g. 125 OHLC candles flowing from
        // `yahoo_finance.historical_candles` into `chart.render` —
        // the 6-month NKE turn that produced 130+ seconds of decode
        // and a vLLM read-timeout). The resolver runs BEFORE every
        // tier (registry / legacy builtin / plugin / MCP-fallback)
        // so consuming tools see inline data regardless of where
        // they're dispatched. Tools that don't need data refs see
        // their args pass through unchanged (the resolver is a
        // no-op on plain values).
        let resolved_args_owned;
        let args_json = if let Some(db) = self.access_db.as_ref() {
            match resolve_data_refs(args_json, db) {
                Ok(Some(resolved)) => {
                    resolved_args_owned = resolved;
                    &resolved_args_owned
                }
                Ok(None) => args_json,
                Err(e) => return Err(format!("data_ref resolution failed: {e}")),
            }
        } else {
            args_json
        };

        // 2026-04-29 — registry-based built-in tier (new
        // `Arc<dyn ToolImpl>` path). Runs before the legacy
        // `BuiltinTools::call` so refactored built-ins hit the
        // capability-scoped path and uncrefactored ones still work.
        if let Some(r) = self.try_registry_builtin(tool_name, args_json).await {
            return r;
        }
        if let Some(r) = self.builtins.call(tool_name, args_json).await {
            return r;
        }
        if let Some(result) = self.queue_transport_tool(tool_name, args_json) {
            return result;
        }
        let caps: Vec<&str> = self.caller_caps.iter().map(|s| s.as_str()).collect();
        // 2026-05-03 — pass `caller_trust` so the host can enforce
        // `[[tools]].trust_floor` (selfhosted-claw's `controllerOnly`
        // generalised). Without this, a Signal contact mapped to
        // `KnownLimited` could invoke `signal.send_message` and use
        // the controller's outbound transport to spam other people.
        self.host
            .call_tool(
                tool_name,
                args_json.clone(),
                &caps,
                Some(self.caller_trust.as_str()),
            )
            .await
    }
}

impl<B: BuiltinTools> ChainedToolDispatch<B> {
    fn queue_transport_tool(
        &self,
        tool_name: &str,
        args: &serde_json::Value,
    ) -> Option<Result<serde_json::Value, String>> {
        let conversation_id = self.conversation_id.as_ref()?;
        let transports = self.host_transports.as_ref()?;
        let registered = self.host.registry().tool(tool_name)?;
        let channel = transports.channel_for_send_tool(&registered.plugin_id, tool_name)?;
        let caps = self
            .caller_caps
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>();
        if let Err(error) =
            self.host
                .validate_tool_call(tool_name, args, &caps, Some(self.caller_trust.as_str()))
        {
            return Some(Err(error));
        }
        let Some(text) = args.get("text").and_then(serde_json::Value::as_str) else {
            return Some(Err("transport send tool requires a text string".into()));
        };
        if text.trim().is_empty() {
            return Some(Err("transport send text must not be empty".into()));
        }
        let recipient = match args.get("to").and_then(serde_json::Value::as_str) {
            Some(recipient) if !recipient.trim().is_empty() => recipient.to_owned(),
            _ => match latest_transport_recipient(&self.host.db(), conversation_id, channel) {
                Ok(Some(recipient)) => recipient,
                Ok(None) => return Some(Err("transport reply has no inbound recipient".into())),
                Err(error) => return Some(Err(error)),
            },
        };
        let turn_seq = self
            .parent_run_id
            .as_deref()
            .and_then(|run_id| {
                run_id
                    .strip_prefix(&format!("turn:{}:", conversation_id.as_str()))
                    .and_then(|seq| seq.parse::<i64>().ok())
            })
            .map(Ok)
            .unwrap_or_else(|| latest_user_event_seq(&self.host.db(), conversation_id));
        let turn_seq = match turn_seq {
            Ok(seq) if seq > 0 => seq,
            Ok(_) => return Some(Err("transport send has no durable user turn".into())),
            Err(error) => return Some(Err(error)),
        };
        let ordinal = self
            .transport_effect_ordinal
            .load(std::sync::atomic::Ordering::SeqCst);
        let (principal_id, fingerprint) = self.live_authority_label_fields();
        let authority = principal_id.as_deref().zip(fingerprint.as_deref());
        Some(
            crate::transport_outbox::stage_plugin_text(
                &self.host.db(),
                conversation_id,
                turn_seq,
                ordinal,
                channel,
                &recipient,
                text,
                authority,
            )
            .map(|outbox_id| {
                serde_json::json!({
                    "queued": true,
                    "delivery_status": "queued",
                    "outbox_id": outbox_id,
                })
            }),
        )
    }
}

fn latest_user_event_seq(db: &Database, conversation_id: &ConversationId) -> Result<i64, String> {
    let events = execlaw_core::EventLog::new(db)
        .replay_since(conversation_id, execlaw_core::ids::EventSeq(0))
        .map_err(|error| format!("read transport send turn: {error}"))?;
    events
        .iter()
        .rev()
        .find(|event| event.kind == execlaw_core::events::EventKind::UserMsg)
        .map(|event| event.seq.0)
        .ok_or_else(|| "transport send has no user event".into())
}

fn latest_transport_recipient(
    db: &Database,
    conversation_id: &ConversationId,
    channel: &str,
) -> Result<Option<String>, String> {
    let group_id = execlaw_core::principal_groups::PrincipalGroupStore::new(db)
        .principal_group_id_for(conversation_id.as_str())
        .map_err(|error| format!("resolve transport reply group: {error}"))?;
    let Some(group_id) = group_id else {
        return Ok(None);
    };
    let bindings = execlaw_core::transport_bindings::TransportBindingStore::new(db)
        .bindings_for_group_any_channel(&group_id)
        .map_err(|error| format!("resolve transport reply binding: {error}"))?;
    Ok(bindings
        .into_iter()
        .filter(|binding| binding.channel == channel)
        .max_by_key(|binding| {
            (
                binding.last_seen_at.unwrap_or(binding.created_at),
                binding.created_at,
            )
        })
        .map(|binding| binding.foreign_id))
}

/// Walk `args` and substitute every `{"$data_ref": "<id>"}` object
/// with the JSON value the host stored under that id. Returns
/// `Ok(None)` when no `$data_ref` appears anywhere in the tree (the
/// fast path — caller keeps the borrowed `args` and avoids the
/// clone); `Ok(Some(resolved))` when at least one substitution
/// happened.
///
/// Recognition is strict: an object qualifies as a data-ref wrapper
/// **only** when it has exactly one key `"$data_ref"` whose value is
/// a non-empty string. Objects with extra fields, or with
/// `"$data_ref"` set to a non-string value, are treated as regular
/// data and recursed into normally — this keeps the conservative
/// surface so a tool legitimately receiving an object with a key
/// named `$data_ref` (unlikely but possible) doesn't get its data
/// silently replaced.
///
/// Resolution failure (unknown id, expired ref, wrong mime, disk
/// read error) propagates as an error — the dispatcher surfaces it
/// to the model in the next round's `tool_result.error` so the
/// model can correct.
fn resolve_data_refs(
    args: &serde_json::Value,
    db: &Database,
) -> Result<Option<serde_json::Value>, String> {
    fn walk(
        v: &serde_json::Value,
        db: &Database,
        changed: &mut bool,
    ) -> Result<serde_json::Value, String> {
        use serde_json::Value;
        match v {
            Value::Object(map) => {
                if map.len() == 1 {
                    if let Some(Value::String(id)) = map.get("$data_ref") {
                        if !id.is_empty() {
                            let resolved = crate::chats::fetch_data_ref(db, id)?;
                            *changed = true;
                            // The resolved value itself may legitimately
                            // contain further `$data_ref` wrappers (a tool
                            // could chain refs). Walk it too.
                            return walk(&resolved, db, changed);
                        }
                    }
                }
                let mut out = serde_json::Map::with_capacity(map.len());
                for (k, val) in map {
                    out.insert(k.clone(), walk(val, db, changed)?);
                }
                Ok(Value::Object(out))
            }
            Value::Array(items) => {
                let mut out = Vec::with_capacity(items.len());
                for item in items {
                    out.push(walk(item, db, changed)?);
                }
                Ok(Value::Array(out))
            }
            other => Ok(other.clone()),
        }
    }

    let mut changed = false;
    let walked = walk(args, db, &mut changed)?;
    if changed { Ok(Some(walked)) } else { Ok(None) }
}

/// An empty built-ins set — useful when the server is serving turns
/// with no runner-local tools at all (Phase 2 dev path when the memory
/// shim hasn't been wired through yet).
pub struct NoBuiltinTools;

#[async_trait]
impl BuiltinTools for NoBuiltinTools {
    async fn call(
        &self,
        _tool_name: &str,
        _args: &serde_json::Value,
    ) -> Option<Result<serde_json::Value, String>> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Default)]
    struct MockWorkspaceJobExecutor {
        runs: Arc<std::sync::Mutex<Vec<execlaw_container_manager::WorkspaceRunRequest>>>,
        diagnostics:
            Arc<std::sync::Mutex<Vec<execlaw_container_manager::WorkspaceDiagnosticsRequest>>>,
    }

    #[async_trait::async_trait]
    impl execlaw_container_manager::WorkspaceJobExecutor for MockWorkspaceJobExecutor {
        async fn run(
            &self,
            request: execlaw_container_manager::WorkspaceRunRequest,
        ) -> Result<
            execlaw_container_manager::WorkspaceRunResult,
            execlaw_container_manager::WorkspaceExecutionError,
        > {
            assert!(request.checkout_path.join("src.txt").is_file());
            assert!(!request.checkout_path.join(".env").exists());
            self.runs.lock().unwrap().push(request);
            Ok(execlaw_container_manager::WorkspaceRunResult {
                exit_code: Some(0),
                timed_out: false,
                output_truncated: false,
                output: "test suite passed".into(),
                elapsed_ms: 12,
            })
        }

        async fn diagnostics(
            &self,
            request: execlaw_container_manager::WorkspaceDiagnosticsRequest,
        ) -> Result<
            execlaw_container_manager::WorkspaceDiagnosticsResult,
            execlaw_container_manager::WorkspaceExecutionError,
        > {
            assert!(request.checkout_path.join(&request.path).is_file());
            assert!(!request.checkout_path.join(".env").exists());
            self.diagnostics.lock().unwrap().push(request.clone());
            Ok(execlaw_container_manager::WorkspaceDiagnosticsResult {
                language_id: request.language_id,
                path: request.path,
                diagnostics: vec![execlaw_container_manager::WorkspaceDiagnostic {
                    range: serde_json::json!({"start":{"line":0,"character":0},"end":{"line":0,"character":1}}),
                    severity: Some(1),
                    code: Some(serde_json::json!("E0001")),
                    source: Some("rust-analyzer".into()),
                    message: "fixture diagnostic".into(),
                }],
                elapsed_ms: 8,
            })
        }
    }
    use execlaw_core::db::{Database, DbConfig};
    use execlaw_core::migrations::MigrationRunner;
    use execlaw_plugin_host::HookRegistry;

    fn test_host() -> PluginHost {
        let db = Database::open(&DbConfig::in_memory_unencrypted()).unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        let dir = tempfile::tempdir().unwrap();
        // Leak: tests that need cleanup manage their own TempDir. For
        // the in-memory cases below the dir is never written to.
        let path = dir.keep();
        PluginHost::new(db, HookRegistry::new(), path)
    }

    /// Built-ins take precedence over plugins — if a built-in handles
    /// the call, the plugin registry isn't even consulted.
    #[tokio::test]
    async fn builtin_takes_precedence_over_plugin() {
        struct EchoBuiltin;
        #[async_trait]
        impl BuiltinTools for EchoBuiltin {
            async fn call(
                &self,
                name: &str,
                args: &serde_json::Value,
            ) -> Option<Result<serde_json::Value, String>> {
                if name == "echo" {
                    Some(Ok(serde_json::json!({"builtin": args})))
                } else {
                    None
                }
            }
        }
        let disp = ChainedToolDispatch::new(test_host(), vec!["*".into()], EchoBuiltin);
        let got = disp
            .call("echo", &serde_json::json!({"x": 1}))
            .await
            .unwrap();
        assert_eq!(got["builtin"]["x"], 1);
    }

    /// Unknown tool falls through both layers and returns an error
    /// the TurnExecutor pairs with a cancellation tool_result.
    #[tokio::test]
    async fn unknown_tool_produces_err_not_panic() {
        let disp = ChainedToolDispatch::new(test_host(), vec!["*".into()], NoBuiltinTools);
        let err = disp
            .call("nonexistent", &serde_json::json!({}))
            .await
            .unwrap_err();
        assert!(err.contains("not registered"));
    }

    #[tokio::test]
    async fn manifest_workspace_tools_dispatch_to_the_run_scoped_checkout() {
        use execlaw_core::runs::{NewRun, RunStore};
        use execlaw_core::workspaces::WorkspaceStore;
        use execlaw_plugin_sdk::manifest::PluginManifest;

        let host = test_host();
        let db = host.db().clone();
        db.with_conn(|connection| {
            connection.execute(
                "INSERT INTO state_conversations (conversation_id,kind,phase,trust_class,modality) VALUES ('workspace-tool-test','ControllerDM','idle','Controller','Text')",
                [],
            )?;
            connection.execute(
                "INSERT INTO state_events (conversation_id,seq,kind,payload,committed_at,actor) VALUES ('workspace-tool-test',1,'user_msg',X'00',1,'controller')",
                [],
            )?;
            Ok(())
        }).unwrap();
        let run_id = RunStore::new(&db)
            .create_run(&NewRun {
                conversation_id: execlaw_core::ConversationId::from("workspace-tool-test"),
                parent_run_id: None,
                input_event_seq: execlaw_core::EventSeq(1),
                started_at: 1,
                deadline_at: None,
            })
            .unwrap();
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let checkout_root = temp.path().join("checkouts");
        let checkout = checkout_root.join("run");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(&checkout).unwrap();
        std::fs::write(source.join("src.txt"), "operator file").unwrap();
        std::fs::copy(source.join("src.txt"), checkout.join("src.txt")).unwrap();
        std::fs::write(checkout.join(".env"), "API_KEY=must-not-enter-container").unwrap();
        let workspace = WorkspaceStore::new(&db);
        let registered = workspace
            .register_root(
                &std::fs::canonicalize(&source).unwrap().to_string_lossy(),
                "controller",
                1,
            )
            .unwrap();
        let checkpoint = workspace
            .create_checkpoint(
                &registered.workspace_id,
                &run_id,
                None,
                &[("src.txt".into(), b"operator file".to_vec())],
                2,
            )
            .unwrap();
        workspace
            .bind_run_checkout(
                &run_id,
                &registered.workspace_id,
                &checkpoint.checkpoint_id,
                &std::fs::canonicalize(&checkout).unwrap().to_string_lossy(),
                2,
            )
            .unwrap();
        workspace
            .set_execution_config(
                "controller",
                &format!("execlaw/workspace-toolchain@sha256:{}", "a".repeat(64)),
                &std::collections::BTreeMap::from([(
                    "rust".to_owned(),
                    vec!["rust-analyzer".to_owned()],
                )]),
                3,
            )
            .unwrap();
        host.registry()
            .enable(
                &PluginManifest::parse(
                    r#"
[plugin]
id = "workspace-tools-fixture"
name = "Workspace tools fixture"
version = "0.1.0"
[[tools]]
name = "workspace.read_file"
host_implemented = true
latency = "low"
required_capabilities = ["workspace.read"]
trust_floor = "Controller"
[[tools]]
name = "workspace.search"
host_implemented = true
latency = "medium"
required_capabilities = ["workspace.read"]
trust_floor = "Controller"
[[tools]]
name = "workspace.run"
host_implemented = true
latency = "high"
required_capabilities = ["workspace.read", "workspace.process"]
trust_floor = "Controller"
[[tools]]
name = "workspace.diagnostics"
host_implemented = true
latency = "high"
required_capabilities = ["workspace.read", "workspace.process"]
trust_floor = "Controller"
"#,
                )
                .unwrap(),
            )
            .unwrap();
        let dispatch = ChainedToolDispatch::with_access_gate(
            host,
            vec!["workspace.read".into(), "workspace.process".into()],
            TrustLevel::Controller,
            NoBuiltinTools,
            db,
        )
        .with_parent_run(run_id)
        .with_workspace_checkout_root(checkout_root)
        .with_workspace_job_executor(Arc::new(MockWorkspaceJobExecutor::default()));
        let read = dispatch
            .call(
                "workspace.read_file",
                &serde_json::json!({"path":"src.txt"}),
            )
            .await
            .unwrap();
        assert_eq!(read["text"], "operator file");
        let matches = dispatch
            .call("workspace.search", &serde_json::json!({"query":"operator"}))
            .await
            .unwrap();
        assert_eq!(matches["matches"][0]["path"], "src.txt");
        dispatch.set_effect_ordinal(2);
        let run = dispatch
            .call(
                "workspace.run",
                &serde_json::json!({"argv":["cargo","test"],"timeout_ms":30000}),
            )
            .await
            .unwrap();
        assert_eq!(run["exit_code"], 0);
        assert_eq!(run["output"], "test suite passed");
        dispatch.set_effect_ordinal(3);
        let diagnostics = dispatch
            .call(
                "workspace.diagnostics",
                &serde_json::json!({"path":"src.txt","language_id":"rust"}),
            )
            .await
            .unwrap();
        assert_eq!(
            diagnostics["diagnostics"][0]["message"],
            "fixture diagnostic"
        );
        assert!(
            dispatch
                .call(
                    "workspace.read_file",
                    &serde_json::json!({"path":"../secret.txt"})
                )
                .await
                .is_err()
        );
        assert_eq!(
            std::fs::read_to_string(source.join("src.txt")).unwrap(),
            "operator file"
        );
    }

    /// Phase-8a gate: a tool that has a policy row but the caller's
    /// trust class isn't on the allowlist must be denied BEFORE
    /// builtins or plugins are consulted.
    #[tokio::test]
    async fn access_gate_denies_caller_outside_allowed_classes() {
        use execlaw_core::tool_access::{ToolAccessSeed, ToolAccessStore, ToolSource};
        let host = test_host();
        let db = host.db().clone();
        // Seed a policy row that allows ONLY Controller — but the
        // caller is KnownTrusted, so the gate should fire before the
        // builtin even gets a chance.
        ToolAccessStore::new(&db)
            .upsert_seen(
                &ToolAccessSeed {
                    tool_name: "echo".into(),
                    source: ToolSource::Builtin,
                    source_id: None,
                    description: None,
                    input_schema: None,
                    default_allowed_classes: vec!["Controller".into()],
                },
                0,
            )
            .unwrap();

        struct EchoBuiltin;
        #[async_trait]
        impl BuiltinTools for EchoBuiltin {
            async fn call(
                &self,
                name: &str,
                _: &serde_json::Value,
            ) -> Option<Result<serde_json::Value, String>> {
                if name == "echo" {
                    Some(Ok(serde_json::json!({"reached": true})))
                } else {
                    None
                }
            }
        }

        let disp = ChainedToolDispatch::with_access_gate(
            host,
            vec!["*".into()],
            TrustLevel::KnownTrusted,
            EchoBuiltin,
            db,
        );
        let err = disp.call("echo", &serde_json::json!({})).await.unwrap_err();
        assert!(
            err.contains("not authorized") && err.contains("KnownTrusted"),
            "expected denial mentioning trust class, got: {err}",
        );
    }

    /// Phase-8a gate: when the caller IS in `allowed_classes`, the
    /// dispatch proceeds normally and the builtin's side-effect runs.
    #[tokio::test]
    async fn access_gate_allows_caller_in_allowed_classes() {
        use execlaw_core::tool_access::{ToolAccessSeed, ToolAccessStore, ToolSource};
        let host = test_host();
        let db = host.db().clone();
        ToolAccessStore::new(&db)
            .upsert_seen(
                &ToolAccessSeed {
                    tool_name: "echo".into(),
                    source: ToolSource::Builtin,
                    source_id: None,
                    description: None,
                    input_schema: None,
                    default_allowed_classes: vec!["Controller".into(), "KnownTrusted".into()],
                },
                0,
            )
            .unwrap();

        struct EchoBuiltin;
        #[async_trait]
        impl BuiltinTools for EchoBuiltin {
            async fn call(
                &self,
                name: &str,
                _: &serde_json::Value,
            ) -> Option<Result<serde_json::Value, String>> {
                if name == "echo" {
                    Some(Ok(serde_json::json!({"reached": true})))
                } else {
                    None
                }
            }
        }

        let disp = ChainedToolDispatch::with_access_gate(
            host,
            vec!["*".into()],
            TrustLevel::KnownTrusted,
            EchoBuiltin,
            db,
        );
        let v = disp.call("echo", &serde_json::json!({})).await.unwrap();
        assert_eq!(v["reached"], true);
    }

    #[tokio::test]
    async fn inspect_only_profile_denies_an_unclassified_write_before_builtin_execution() {
        use execlaw_core::tool_access::{ToolAccessSeed, ToolAccessStore, ToolSource};
        use std::sync::atomic::{AtomicBool, Ordering};

        struct LegacyWrite(Arc<AtomicBool>);
        #[async_trait]
        impl BuiltinTools for LegacyWrite {
            async fn call(
                &self,
                name: &str,
                _: &serde_json::Value,
            ) -> Option<Result<serde_json::Value, String>> {
                if name == "workspace.write_file" {
                    self.0.store(true, Ordering::SeqCst);
                    Some(Ok(serde_json::json!({"written":true})))
                } else {
                    None
                }
            }
        }

        let host = test_host();
        let db = host.db().clone();
        ToolAccessStore::new(&db)
            .upsert_seen(
                &ToolAccessSeed {
                    tool_name: "workspace.write_file".into(),
                    source: ToolSource::Builtin,
                    source_id: None,
                    description: None,
                    input_schema: None,
                    default_allowed_classes: vec!["Controller".into()],
                },
                1,
            )
            .unwrap();
        let profile = execlaw_core::safety_profiles::SafetyProfileStore::new(&db)
            .get(execlaw_core::safety_profiles::SafetyProfileId::InspectOnly)
            .unwrap();
        let invoked = Arc::new(AtomicBool::new(false));
        let dispatch = ChainedToolDispatch::with_access_gate(
            host,
            vec!["*".into()],
            TrustLevel::Controller,
            LegacyWrite(invoked.clone()),
            db,
        )
        .with_safety_profile(Some(
            execlaw_core::safety_profiles::SafetyProfileSnapshot::from_profile(&profile),
        ));

        let error = dispatch
            .call(
                "workspace.write_file",
                &serde_json::json!({"path":"safe.txt"}),
            )
            .await
            .unwrap_err();
        assert!(error.contains("active safety profile"));
        assert!(!invoked.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn revoked_principal_is_denied_at_tool_dispatch_after_turn_start() {
        use execlaw_core::ids::PrincipalId;
        use execlaw_core::principal::{Principal, PrincipalStore};
        use execlaw_core::tool_access::{ToolAccessSeed, ToolAccessStore, ToolSource};
        use std::sync::atomic::{AtomicBool, Ordering};

        struct EchoBuiltin(Arc<AtomicBool>);
        #[async_trait]
        impl BuiltinTools for EchoBuiltin {
            async fn call(
                &self,
                name: &str,
                _: &serde_json::Value,
            ) -> Option<Result<serde_json::Value, String>> {
                if name == "echo" {
                    self.0.store(true, Ordering::SeqCst);
                    Some(Ok(serde_json::json!({"reached": true})))
                } else {
                    None
                }
            }
        }

        let host = test_host();
        let db = host.db().clone();
        let principal_id = PrincipalId::from("live-authority-test");
        let store = PrincipalStore::new(&db);
        let trusted = Principal {
            id: principal_id.clone(),
            identifiers: Vec::new(),
            trust_level: StoredTrustLevel::KnownTrusted {
                resolvers: Vec::new(),
                approved_by: PrincipalId::from("controller"),
                approved_at: 1,
            },
            resolved_by: Vec::new(),
            metadata: serde_json::json!({}),
            first_seen: 1,
            last_seen: Some(1),
            controller_notes: None,
        };
        store.upsert(&trusted).unwrap();
        ToolAccessStore::new(&db)
            .upsert_seen(
                &ToolAccessSeed {
                    tool_name: "echo".into(),
                    source: ToolSource::Builtin,
                    source_id: None,
                    description: None,
                    input_schema: None,
                    default_allowed_classes: vec!["KnownTrusted".into()],
                },
                1,
            )
            .unwrap();

        let invoked = Arc::new(AtomicBool::new(false));
        let dispatch = ChainedToolDispatch::with_access_gate(
            host,
            vec!["*".into()],
            TrustLevel::KnownTrusted,
            EchoBuiltin(invoked.clone()),
            db.clone(),
        )
        .with_live_principal(principal_id.as_str());

        let mut revoked = trusted;
        revoked.trust_level = StoredTrustLevel::Blocked {
            blocked_by: PrincipalId::from("controller"),
            blocked_at: chrono::Utc::now().timestamp(),
            reason: Some("revoked while model request was in flight".into()),
        };
        store.upsert(&revoked).unwrap();

        let error = dispatch
            .call("echo", &serde_json::json!({}))
            .await
            .unwrap_err();
        assert!(error.contains("authority changed"));
        assert!(!invoked.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn changed_policy_revision_denies_a_tool_catalogued_earlier_in_the_turn() {
        use execlaw_core::tool_access::{ToolAccessSeed, ToolAccessStore, ToolSource};
        use std::sync::atomic::{AtomicBool, Ordering};

        struct EchoBuiltin(Arc<AtomicBool>);
        #[async_trait]
        impl BuiltinTools for EchoBuiltin {
            async fn call(
                &self,
                name: &str,
                _: &serde_json::Value,
            ) -> Option<Result<serde_json::Value, String>> {
                if name == "echo" {
                    self.0.store(true, Ordering::SeqCst);
                    Some(Ok(serde_json::json!({"reached": true})))
                } else {
                    None
                }
            }
        }

        let host = test_host();
        let db = host.db().clone();
        let access = ToolAccessStore::new(&db);
        access
            .upsert_seen(
                &ToolAccessSeed {
                    tool_name: "echo".into(),
                    source: ToolSource::Builtin,
                    source_id: None,
                    description: None,
                    input_schema: None,
                    default_allowed_classes: vec!["Controller".into()],
                },
                1,
            )
            .unwrap();
        let invoked = Arc::new(AtomicBool::new(false));
        let dispatch = ChainedToolDispatch::with_access_gate(
            host,
            vec!["*".into()],
            TrustLevel::Controller,
            EchoBuiltin(invoked.clone()),
            db.clone(),
        );
        access
            .set_policy_with_actor("echo", true, &["Controller".into()], "controller", 2, None)
            .unwrap();

        let error = dispatch
            .call("echo", &serde_json::json!({}))
            .await
            .unwrap_err();
        assert!(error.contains("policy changed during this turn"));
        assert!(!invoked.load(Ordering::SeqCst));
    }

    #[test]
    fn missing_controller_mapping_stays_fail_closed_in_live_principal_state() {
        let dispatch = ChainedToolDispatch::new(test_host(), vec!["*".into()], NoBuiltinTools)
            .with_live_principal_opt(Some("controller"));
        assert!(dispatch.live_principal.is_some());
        assert!(dispatch.live_principal.as_ref().unwrap().1.is_none());
    }

    /// Phase-8a gate: a tool flipped `enabled = false` is denied
    /// regardless of trust class.
    #[tokio::test]
    async fn access_gate_denies_disabled_tool() {
        use execlaw_core::tool_access::{ToolAccessSeed, ToolAccessStore, ToolSource};
        let host = test_host();
        let db = host.db().clone();
        let store = ToolAccessStore::new(&db);
        store
            .upsert_seen(
                &ToolAccessSeed {
                    tool_name: "echo".into(),
                    source: ToolSource::Builtin,
                    source_id: None,
                    description: None,
                    input_schema: None,
                    default_allowed_classes: vec!["Controller".into()],
                },
                0,
            )
            .unwrap();
        store
            .set_policy("echo", false, &["Controller".into()])
            .unwrap();

        struct EchoBuiltin;
        #[async_trait]
        impl BuiltinTools for EchoBuiltin {
            async fn call(
                &self,
                _: &str,
                _: &serde_json::Value,
            ) -> Option<Result<serde_json::Value, String>> {
                Some(Ok(serde_json::json!({"reached": true})))
            }
        }
        let disp = ChainedToolDispatch::with_access_gate(
            host,
            vec!["*".into()],
            TrustLevel::Controller,
            EchoBuiltin,
            db,
        );
        let err = disp.call("echo", &serde_json::json!({})).await.unwrap_err();
        assert!(err.contains("disabled"), "got: {err}");
    }

    /// Phase-8d routing: a tool name with the `mcp:` prefix routes
    /// to the McpHost dispatcher. With no actor connected for the
    /// named server, we expect a "not connected" error rather than
    /// fall-through to builtins/plugins.
    #[tokio::test]
    async fn mcp_prefixed_name_routes_to_mcp_host() {
        struct NoneBuiltin;
        #[async_trait]
        impl BuiltinTools for NoneBuiltin {
            async fn call(
                &self,
                _: &str,
                _: &serde_json::Value,
            ) -> Option<Result<serde_json::Value, String>> {
                None
            }
        }
        let host = test_host();
        let db = host.db().clone();
        let mcp = crate::mcp_host::McpHost::new(db.clone());
        let disp = ChainedToolDispatch::with_access_gate(
            host,
            vec!["*".into()],
            TrustLevel::Controller,
            NoneBuiltin,
            db,
        )
        .with_mcp(mcp);
        let err = disp
            .call("mcp:github:create_pr", &serde_json::json!({}))
            .await
            .unwrap_err();
        // Two acceptable wordings depending on whether the access
        // gate fired first (no row → allow → routes to mcp_host →
        // not connected) or another error path.
        assert!(
            err.contains("not connected") || err.contains("not authorized"),
            "expected MCP routing error, got: {err}",
        );
    }

    /// Phase-8d safety: when no McpHost is wired into the dispatch,
    /// an `mcp:`-prefixed tool name returns a structured error
    /// rather than falling through and confusing the runner.
    #[tokio::test]
    async fn mcp_prefixed_without_host_returns_structured_error() {
        struct NoneBuiltin;
        #[async_trait]
        impl BuiltinTools for NoneBuiltin {
            async fn call(
                &self,
                _: &str,
                _: &serde_json::Value,
            ) -> Option<Result<serde_json::Value, String>> {
                None
            }
        }
        let disp = ChainedToolDispatch::new(test_host(), vec!["*".into()], NoneBuiltin);
        let err = disp
            .call("mcp:github:create_pr", &serde_json::json!({}))
            .await
            .unwrap_err();
        assert!(
            err.contains("no MCP host configured"),
            "expected structured no-host error, got: {err}",
        );
    }

    /// Phase-8a gate: with NO policy row at all (the test default),
    /// the legacy "allow" path is preserved so existing tests don't
    /// need rewrites.
    #[tokio::test]
    async fn access_gate_falls_back_to_allow_when_no_row_exists() {
        struct EchoBuiltin;
        #[async_trait]
        impl BuiltinTools for EchoBuiltin {
            async fn call(
                &self,
                _: &str,
                _: &serde_json::Value,
            ) -> Option<Result<serde_json::Value, String>> {
                Some(Ok(serde_json::json!({"reached": true})))
            }
        }
        let host = test_host();
        let db = host.db().clone();
        let disp = ChainedToolDispatch::with_access_gate(
            host,
            vec!["*".into()],
            TrustLevel::Blocked, // even Blocked allowed when no row exists
            EchoBuiltin,
            db,
        );
        let v = disp.call("echo", &serde_json::json!({})).await.unwrap();
        assert_eq!(v["reached"], true);
    }

    // ---- New trait-based built-in dispatch tests --------------------

    use execlaw_core::builtin_tools::{ReadMemoryTool, SetThreadNameTool, WriteMemoryTool};
    use execlaw_core::conversation::{
        ConversationKind, ConversationRow, ConversationStore, Modality, Phase,
    };
    use execlaw_core::ids::{ConversationId, EventSeq};
    use std::sync::Arc;

    fn seed_conv(db: &execlaw_core::Database, id: &str) -> ConversationId {
        let cid = ConversationId::from(id);
        ConversationStore::new(db)
            .upsert(&ConversationRow {
                conversation_id: cid.clone(),
                kind: ConversationKind::ControllerDM,
                last_seq: EventSeq(0),
                phase: Phase::Idle,
                controller_id: None,
                trust_class: "Controller".into(),
                snapshot_blob: None,
                snapshot_seq: None,
                lease_owner: None,
                lease_expires: None,
                modality: Modality::Text,
                display_name: None,
                display_name_source: "auto".into(),
                is_pinned: false,
                is_ephemeral: false,
                ephemeral_expires_at: None,
                last_activity_at: 0,
                context_window_policy: None,
            })
            .unwrap();
        cid
    }

    /// A registry-resolved built-in (`Arc<dyn ToolImpl>`) is invoked
    /// through the new path with a capability-scoped `ToolCtx`. The
    /// underlying conversation store reflects the write — proving
    /// the entire chain (registry lookup → cap construction →
    /// invoke → store mutation) is wired.
    #[tokio::test]
    async fn registry_builtin_set_thread_name_writes_through_dispatcher() {
        let host = test_host();
        let db = host.db().clone();
        host.registry()
            .register_builtin(Arc::new(SetThreadNameTool::new()))
            .unwrap();
        let cid = seed_conv(&db, "c1");
        let disp = ChainedToolDispatch::new(host, vec!["*".into()], NoBuiltinTools)
            .with_conversation(cid.clone());
        let out = disp
            .call("set_thread_name", &serde_json::json!({"name": "Branding"}))
            .await
            .unwrap();
        assert_eq!(out["ok"], true);
        let row = ConversationStore::new(&db).get(&cid).unwrap().unwrap();
        assert_eq!(row.display_name.as_deref(), Some("Branding"));
    }

    /// `write_memory` registered through the new path, then
    /// `read_memory` reads it back — both at Controller trust. Proves
    /// the per-call `MemoryApi` is constructed correctly with
    /// caller_trust baked in.
    #[tokio::test]
    async fn registry_builtin_memory_round_trip_via_dispatcher() {
        let host = test_host();
        let db = host.db().clone();
        host.registry()
            .register_builtin(Arc::new(WriteMemoryTool::new()))
            .unwrap();
        host.registry()
            .register_builtin(Arc::new(ReadMemoryTool::new()))
            .unwrap();
        let cid = seed_conv(&db, "c2");
        let disp = ChainedToolDispatch::with_access_gate(
            host,
            vec!["*".into()],
            TrustLevel::Controller,
            NoBuiltinTools,
            db,
        )
        .with_conversation(cid);

        disp.call(
            "write_memory",
            &serde_json::json!({"scope": "g", "key": "k", "value": "v"}),
        )
        .await
        .unwrap();
        let v = disp
            .call(
                "read_memory",
                &serde_json::json!({"scope": "g", "key": "k"}),
            )
            .await
            .unwrap();
        assert_eq!(v, serde_json::json!("v"));
    }

    /// A capability the descriptor didn't declare must surface as a
    /// `Denied` outcome. Here the dispatcher picks `set_thread_name`
    /// (declares `ConversationWrite` only) and the test calls a
    /// no-args invocation of `read_memory` against an unregistered
    /// name to assert the no-op fallthrough — and then registers
    /// `read_memory` and triggers the missing-capability branch by
    /// stripping `MemoryApi` via a dispatcher with no conversation
    /// id (which short-circuits the new path).
    #[tokio::test]
    async fn registry_builtin_denied_when_capability_unmet() {
        struct PartialMemoryTool {
            d: execlaw_core::tool::ToolDescriptor,
        }
        #[async_trait]
        impl execlaw_core::tool::ToolImpl for PartialMemoryTool {
            fn descriptor(&self) -> &execlaw_core::tool::ToolDescriptor {
                &self.d
            }
            async fn invoke(
                &self,
                ctx: execlaw_core::tool::ToolCtx,
                _args: serde_json::Value,
            ) -> execlaw_core::tool::ToolOutcome {
                if ctx.memory.is_some() {
                    execlaw_core::tool::ToolOutcome::ok(serde_json::json!({"ok": true}))
                } else {
                    execlaw_core::tool::ToolOutcome::denied("memory missing")
                }
            }
        }
        let tool = Arc::new(PartialMemoryTool {
            d: execlaw_core::tool::ToolDescriptor {
                name: "needs_mem".into(),
                description: "x".into(),
                schema: serde_json::json!({"type": "object"}),
                source: execlaw_core::tool::ToolSource::Builtin,
                latency: execlaw_core::tool::ToolLatency::Low,
                // Intentionally empty — the dispatcher must NOT
                // populate `ctx.memory` even though we'd need it.
                capabilities: vec![],
                default_allowed_classes: vec!["Controller".into()],
                sensitive: false,
            },
        });
        let host = test_host();
        let db = host.db().clone();
        host.registry().register_builtin(tool).unwrap();
        let cid = seed_conv(&db, "c3");
        let disp =
            ChainedToolDispatch::new(host, vec!["*".into()], NoBuiltinTools).with_conversation(cid);
        let err = disp
            .call("needs_mem", &serde_json::json!({}))
            .await
            .unwrap_err();
        assert!(err.contains("denied"));
        assert!(err.contains("memory missing"));
    }

    /// When the dispatcher has no `conversation_id`, the new path
    /// short-circuits and the legacy `BuiltinTools::call` chain runs.
    /// This preserves the pre-2026-04-29 contract for fixtures that
    /// don't construct conversations.
    #[tokio::test]
    async fn registry_builtin_no_conversation_falls_through_to_legacy() {
        let host = test_host();
        host.registry()
            .register_builtin(Arc::new(SetThreadNameTool::new()))
            .unwrap();

        struct LegacyEcho;
        #[async_trait]
        impl BuiltinTools for LegacyEcho {
            async fn call(
                &self,
                _: &str,
                _: &serde_json::Value,
            ) -> Option<Result<serde_json::Value, String>> {
                Some(Ok(serde_json::json!({"legacy": true})))
            }
        }
        let disp = ChainedToolDispatch::new(host, vec!["*".into()], LegacyEcho); // no conversation id
        let v = disp
            .call("set_thread_name", &serde_json::json!({"name": "n"}))
            .await
            .unwrap();
        assert_eq!(v["legacy"], true);
    }

    /// The new built-in tier runs BEFORE the legacy `BuiltinTools`
    /// path. If both are configured for the same name, the
    /// trait-based one wins.
    #[tokio::test]
    async fn registry_builtin_takes_precedence_over_legacy_builtins() {
        let host = test_host();
        let db = host.db().clone();
        host.registry()
            .register_builtin(Arc::new(SetThreadNameTool::new()))
            .unwrap();
        let cid = seed_conv(&db, "c4");

        struct LegacyShadowing;
        #[async_trait]
        impl BuiltinTools for LegacyShadowing {
            async fn call(
                &self,
                name: &str,
                _: &serde_json::Value,
            ) -> Option<Result<serde_json::Value, String>> {
                if name == "set_thread_name" {
                    Some(Ok(serde_json::json!({"from_legacy": true})))
                } else {
                    None
                }
            }
        }
        let disp = ChainedToolDispatch::new(host, vec!["*".into()], LegacyShadowing)
            .with_conversation(cid.clone());
        let v = disp
            .call("set_thread_name", &serde_json::json!({"name": "Won"}))
            .await
            .unwrap();
        assert_eq!(v["ok"], true); // from registry, not legacy
        assert!(v.get("from_legacy").is_none());
        let row = ConversationStore::new(&db).get(&cid).unwrap().unwrap();
        assert_eq!(row.display_name.as_deref(), Some("Won"));
    }

    /// 2026-05-16 — built-in capability gate (fix #4). A built-in tool
    /// whose descriptor declares `Capability::MemoryWrite` is denied
    /// when the caller's `caller_caps` doesn't include `memory.write`.
    /// Pre-fix the dispatcher checked only `config_tool_access` and
    /// then handed control to the registry's `invoke()` — which
    /// populated `ctx.memory` based on the *descriptor's* declared
    /// capabilities, not the caller's policy capability set. A
    /// KnownLimited contact could write memory entries it had no
    /// policy right to touch.
    #[tokio::test]
    async fn builtin_capability_gate_denies_caller_missing_required_cap() {
        use async_trait::async_trait;
        use execlaw_core::tool::{
            Capability, ToolCtx, ToolDescriptor, ToolImpl, ToolLatency, ToolOutcome,
            ToolSource as CoreToolSource,
        };

        struct MemoryWriter {
            d: ToolDescriptor,
        }
        #[async_trait]
        impl ToolImpl for MemoryWriter {
            fn descriptor(&self) -> &ToolDescriptor {
                &self.d
            }
            async fn invoke(&self, _ctx: ToolCtx, _args: serde_json::Value) -> ToolOutcome {
                ToolOutcome::ok(serde_json::json!({"reached": true}))
            }
        }

        let host = test_host();
        host.registry()
            .register_builtin(Arc::new(MemoryWriter {
                d: ToolDescriptor {
                    name: "test_mem_write".into(),
                    description: "test".into(),
                    schema: serde_json::json!({"type": "object"}),
                    source: CoreToolSource::Builtin,
                    latency: ToolLatency::Low,
                    capabilities: vec![Capability::MemoryWrite],
                    default_allowed_classes: vec!["Controller".into(), "KnownTrusted".into()],
                    sensitive: false,
                },
            }))
            .unwrap();

        // KnownLimited caller — caller_caps contains ONLY
        // `messaging.reply_current_transport`, so memory.write is not
        // satisfied. Dispatch MUST refuse before the tool's `invoke`
        // body runs.
        let disp = ChainedToolDispatch::new(
            host,
            vec!["messaging.reply_current_transport".into()],
            NoBuiltinTools,
        )
        .with_conversation(ConversationId::from("c"));
        let err = disp
            .call("test_mem_write", &serde_json::json!({}))
            .await
            .unwrap_err();
        assert!(
            err.contains("memory.write") && err.contains("not authorized"),
            "expected capability-denial mentioning the missing cap, got: {err}"
        );
    }

    /// Companion: when the caller DOES hold the required policy cap,
    /// the gate lets the tool run.
    #[tokio::test]
    async fn builtin_capability_gate_allows_caller_with_required_cap() {
        use async_trait::async_trait;
        use execlaw_core::tool::{
            Capability, ToolCtx, ToolDescriptor, ToolImpl, ToolLatency, ToolOutcome,
            ToolSource as CoreToolSource,
        };

        struct MemoryWriter {
            d: ToolDescriptor,
        }
        #[async_trait]
        impl ToolImpl for MemoryWriter {
            fn descriptor(&self) -> &ToolDescriptor {
                &self.d
            }
            async fn invoke(&self, _ctx: ToolCtx, _args: serde_json::Value) -> ToolOutcome {
                ToolOutcome::ok(serde_json::json!({"reached": true}))
            }
        }

        let host = test_host();
        host.registry()
            .register_builtin(Arc::new(MemoryWriter {
                d: ToolDescriptor {
                    name: "test_mem_write_2".into(),
                    description: "test".into(),
                    schema: serde_json::json!({"type": "object"}),
                    source: CoreToolSource::Builtin,
                    latency: ToolLatency::Low,
                    capabilities: vec![Capability::MemoryWrite],
                    default_allowed_classes: vec!["Controller".into(), "KnownTrusted".into()],
                    sensitive: false,
                },
            }))
            .unwrap();

        // KnownTrusted-tier caps include memory.write.
        let disp = ChainedToolDispatch::new(
            host,
            vec![
                "messaging.reply_current_transport".into(),
                "memory.read".into(),
                "memory.write".into(),
                "tools.safe".into(),
            ],
            NoBuiltinTools,
        )
        .with_conversation(ConversationId::from("c"));
        let v = disp
            .call("test_mem_write_2", &serde_json::json!({}))
            .await
            .unwrap();
        assert_eq!(v["reached"], true);
    }

    /// Wildcard `"*"` (Controller) bypasses the cap gate even for
    /// Controller-only capabilities like `McpAdmin`.
    #[tokio::test]
    async fn builtin_capability_gate_wildcard_passes_mcp_admin() {
        use async_trait::async_trait;
        use execlaw_core::tool::{
            Capability, ToolCtx, ToolDescriptor, ToolImpl, ToolLatency, ToolOutcome,
            ToolSource as CoreToolSource,
        };

        struct McpTool {
            d: ToolDescriptor,
        }
        #[async_trait]
        impl ToolImpl for McpTool {
            fn descriptor(&self) -> &ToolDescriptor {
                &self.d
            }
            async fn invoke(&self, _ctx: ToolCtx, _args: serde_json::Value) -> ToolOutcome {
                ToolOutcome::ok(serde_json::json!({"reached": true}))
            }
        }

        let host = test_host();
        host.registry()
            .register_builtin(Arc::new(McpTool {
                d: ToolDescriptor {
                    name: "test_mcp_admin".into(),
                    description: "test".into(),
                    schema: serde_json::json!({"type": "object"}),
                    source: CoreToolSource::Builtin,
                    latency: ToolLatency::Low,
                    capabilities: vec![Capability::McpAdmin],
                    default_allowed_classes: vec!["Controller".into()],
                    sensitive: false,
                },
            }))
            .unwrap();

        let disp = ChainedToolDispatch::new(host, vec!["*".into()], NoBuiltinTools)
            .with_conversation(ConversationId::from("c"));
        let v = disp
            .call("test_mcp_admin", &serde_json::json!({}))
            .await
            .unwrap();
        assert_eq!(v["reached"], true);
    }

    // ---- resolve_data_refs --------------------------------------
    //
    // 2026-05-16 — pin the wire-shape contract the dispatcher
    // relies on. The resolver runs BEFORE every tool tier sees
    // args, so changes here propagate across the entire tool
    // surface; a regression that mis-substitutes or fails to
    // substitute would silently break data-ref-using tools
    // (yahoo_finance → chart.render today; more pipelines later).

    fn write_ref(
        db: &Database,
        artifacts_root: &std::path::Path,
        value: &serde_json::Value,
    ) -> String {
        use execlaw_core::attachments::AttachmentStore;
        let bytes = serde_json::to_vec(value).unwrap();
        let store = AttachmentStore::new(db);
        // Use wall-clock so the 1-hour TTL the fetcher checks is
        // genuinely in the future regardless of when the test runs.
        let now = chrono::Utc::now().timestamp();
        store
            .insert_plugin_artifact(
                artifacts_root,
                "test-plugin",
                "data_ref.json",
                "application/json",
                &bytes,
                Some(3600),
                now,
            )
            .unwrap()
            .attachment_id
    }

    fn fresh_db_and_root() -> (Database, tempfile::TempDir) {
        let db = Database::open(&DbConfig::in_memory_unencrypted()).unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        let dir = tempfile::tempdir().unwrap();
        (db, dir)
    }

    #[test]
    fn resolve_data_refs_no_op_returns_none() {
        // Hot path: args with no `$data_ref` anywhere skip the
        // clone and the caller keeps the borrowed value.
        let (db, _root) = fresh_db_and_root();
        let args = serde_json::json!({
            "title": "Hello",
            "points": [{"x": 1, "y": 2}, {"x": 3, "y": 4}],
        });
        let out = resolve_data_refs(&args, &db).unwrap();
        assert!(
            out.is_none(),
            "args with no $data_ref must return None to skip the clone"
        );
    }

    #[test]
    fn resolve_data_refs_substitutes_top_level() {
        let (db, root) = fresh_db_and_root();
        let stored = serde_json::json!([{"x": 1, "y": 10}, {"x": 2, "y": 20}]);
        let id = write_ref(&db, root.path(), &stored);
        let args = serde_json::json!({"$data_ref": id});
        let out = resolve_data_refs(&args, &db).unwrap().expect("ref present");
        assert_eq!(out, stored);
    }

    #[test]
    fn resolve_data_refs_substitutes_nested_under_named_field() {
        // The yahoo_finance → chart.render shape: refs appear at
        // `series[].points`, not at the top.
        let (db, root) = fresh_db_and_root();
        let points = serde_json::json!([{"x": 1, "y": 10}, {"x": 2, "y": 20}]);
        let id = write_ref(&db, root.path(), &points);
        let args = serde_json::json!({
            "title": "Close",
            "series": [
                {"name": "NKE", "points": {"$data_ref": id}}
            ]
        });
        let out = resolve_data_refs(&args, &db).unwrap().expect("ref present");
        assert_eq!(out["series"][0]["points"], points);
        // Sibling fields preserved verbatim.
        assert_eq!(out["title"], "Close");
        assert_eq!(out["series"][0]["name"], "NKE");
    }

    #[test]
    fn resolve_data_refs_does_not_substitute_object_with_extra_keys() {
        // Conservative recognition: `{"$data_ref": "id", "other": ...}`
        // is NOT a ref wrapper — it might be a tool legitimately
        // receiving an object that happens to have a `$data_ref`
        // key. We recurse into it normally.
        let (db, root) = fresh_db_and_root();
        let stored = serde_json::json!({"hello": "world"});
        let id = write_ref(&db, root.path(), &stored);
        let args = serde_json::json!({
            "wrapper": {"$data_ref": id, "extra": "stay"}
        });
        let out = resolve_data_refs(&args, &db).unwrap();
        // No substitution happened because the inner object had two
        // keys → returned None (no change).
        assert!(
            out.is_none(),
            "wrapper with extra keys must NOT be treated as a ref"
        );
    }

    #[test]
    fn resolve_data_refs_recurses_into_resolved_value() {
        // A ref pointing at a value that itself contains a ref —
        // resolver must walk recursively. Useful for plugins that
        // chain refs (output of step A → input to step B → final
        // bundle).
        let (db, root) = fresh_db_and_root();
        let inner_stored = serde_json::json!([1, 2, 3]);
        let inner_id = write_ref(&db, root.path(), &inner_stored);
        let outer_value = serde_json::json!({
            "wrapped": {"$data_ref": inner_id}
        });
        let outer_id = write_ref(&db, root.path(), &outer_value);
        let args = serde_json::json!({"$data_ref": outer_id});
        let out = resolve_data_refs(&args, &db).unwrap().expect("ref present");
        assert_eq!(out, serde_json::json!({"wrapped": [1, 2, 3]}));
    }

    #[test]
    fn resolve_data_refs_missing_id_errors() {
        // Unknown id → error bubbles back to the dispatcher which
        // turns it into a tool_result.error in the next round so
        // the model can correct.
        let (db, _root) = fresh_db_and_root();
        let args = serde_json::json!({"$data_ref": "nonexistent-uuid"});
        let err = resolve_data_refs(&args, &db).unwrap_err();
        assert!(
            err.contains("not found") && err.contains("nonexistent-uuid"),
            "error must name the missing id; got: {err}"
        );
    }

    #[test]
    fn resolve_data_refs_ignores_non_string_value_under_marker_key() {
        // `{"$data_ref": 42}` (integer) — not a valid ref wrapper.
        // Recurse normally rather than erroring.
        let (db, _root) = fresh_db_and_root();
        let args = serde_json::json!({"$data_ref": 42});
        let out = resolve_data_refs(&args, &db).unwrap();
        assert!(out.is_none());
    }
}
