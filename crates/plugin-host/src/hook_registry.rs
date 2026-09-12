//! Hook registry (§4.2).
//!
//! When a plugin is enabled, its manifest hook declarations register
//! into live lookup maps keyed by the appropriate primary key:
//!
//! - `tools_by_name` — every declared `[[tools]]` entry, keyed by tool name
//! - `ui_panels_by_mount` — `[[ui_panels]]` keyed by mount path
//! - `transports_by_id` — the plugin's `[transport]` keyed by transport id
//! - `identity_providers` — all enabled `[identity_provider]` plugins
//! - `event_subscriptions_by_kind` — which plugins listen for which events
//! - `alert_sources_by_prefix` — which plugins own alert fingerprint prefixes
//!
//! The registry is **additive per plugin, atomic per enable**: enabling
//! registers every hook the manifest declares; disabling removes them
//! all at once.

use execlaw_core::tool::{ToolImpl, compile_tool_schema, tool_schema_hash};
use execlaw_plugin_sdk::PluginManifest;
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

fn schema_references(
    value: &serde_json::Value,
    references: &mut Vec<String>,
) -> Result<(), String> {
    match value {
        serde_json::Value::Object(fields) => {
            for (key, value) in fields {
                if matches!(key.as_str(), "$ref" | "$dynamicRef")
                    && let Some(reference) = value.as_str()
                {
                    if reference.starts_with("http://") || reference.starts_with("https://") {
                        return Err(format!(
                            "contains forbidden network reference '{reference}'"
                        ));
                    }
                    if !reference.starts_with('#') {
                        references.push(reference.to_owned());
                    }
                }
                schema_references(value, references)?;
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                schema_references(value, references)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn load_tool_schemas(
    manifest: &PluginManifest,
    stage_path: Option<&Path>,
) -> Result<HashMap<String, LoadedToolSchemas>, String> {
    let Some(stage) = stage_path else {
        if manifest
            .tools
            .iter()
            .any(|tool| tool.schema.is_some() || tool.result_schema.is_some())
        {
            return Err("plugin tool schemas require an on-disk stage path".into());
        }
        return Ok(HashMap::new());
    };
    let stage_root = stage
        .canonicalize()
        .map_err(|error| format!("plugin stage '{}' is unreadable: {error}", stage.display()))?;
    let mut schemas = HashMap::new();
    for tool in &manifest.tools {
        let input = tool
            .schema
            .as_deref()
            .map(|path| load_schema_bundle(&manifest.plugin.id, &tool.name, &stage_root, path))
            .transpose()?;
        let result = tool
            .result_schema
            .as_deref()
            .map(|path| load_schema_bundle(&manifest.plugin.id, &tool.name, &stage_root, path))
            .transpose()?;
        if input.is_none() && result.is_none() {
            continue;
        }
        schemas.insert(tool.name.clone(), LoadedToolSchemas { input, result });
    }
    Ok(schemas)
}

fn load_schema_bundle(
    plugin_id: &str,
    tool_name: &str,
    stage_root: &Path,
    relative_path: &str,
) -> Result<LoadedToolSchema, String> {
    let requested = Path::new(relative_path);
    if requested.is_absolute()
        || requested.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
    {
        return Err(format!(
            "tool '{tool_name}' schema path '{relative_path}' escapes the plugin stage"
        ));
    }

    let root_path = checked_schema_path(stage_root, &stage_root.join(requested), tool_name)?;
    let mut pending = VecDeque::from([root_path.clone()]);
    let mut documents = BTreeMap::<PathBuf, serde_json::Value>::new();
    while let Some(path) = pending.pop_front() {
        if documents.contains_key(&path) {
            continue;
        }
        if documents.len() >= 64 {
            return Err(format!(
                "tool '{tool_name}' schema bundle exceeds 64 documents"
            ));
        }
        let text = std::fs::read_to_string(&path).map_err(|error| {
            format!(
                "tool '{tool_name}' schema '{}' is unreadable: {error}",
                path.display()
            )
        })?;
        if text.len() > 1024 * 1024 {
            return Err(format!(
                "tool '{tool_name}' schema '{}' exceeds 1 MiB",
                path.display()
            ));
        }
        let document: serde_json::Value = serde_json::from_str(&text).map_err(|error| {
            format!(
                "tool '{tool_name}' schema '{}' is not valid JSON: {error}",
                path.display()
            )
        })?;
        if !document.is_object() {
            return Err(format!(
                "tool '{tool_name}' schema '{}' must be a JSON object",
                path.display()
            ));
        }
        let mut references = Vec::new();
        schema_references(&document, &mut references)
            .map_err(|error| format!("tool '{tool_name}' schema '{}': {error}", path.display()))?;
        for reference in references {
            let path_part = reference.split('#').next().unwrap_or_default();
            if path_part.is_empty() || path_part.contains(':') || path_part.starts_with('/') {
                return Err(format!(
                    "tool '{tool_name}' schema '{}' contains unsupported external reference '{reference}'",
                    path.display()
                ));
            }
            let referenced = path.parent().unwrap_or(stage_root).join(path_part);
            pending.push_back(checked_schema_path(stage_root, &referenced, tool_name)?);
        }
        documents.insert(path, document);
    }

    let root_schema = documents
        .get(&root_path)
        .cloned()
        .expect("root schema was loaded");
    let root_uri = schema_uri(plugin_id, stage_root, &root_path)?;
    let resources = documents
        .iter()
        .filter(|(path, _)| *path != &root_path)
        .map(|(path, value)| {
            let uri = schema_uri(plugin_id, stage_root, path)?;
            let resource = jsonschema::Resource::from_contents(value.clone())
                .map_err(|error| format!("tool '{tool_name}' schema resource: {error}"))?;
            Ok((uri, resource))
        })
        .collect::<Result<Vec<_>, String>>()?;
    let validator = jsonschema::options()
        .with_draft(jsonschema::Draft::Draft202012)
        .with_base_uri(root_uri)
        .with_resources(resources.into_iter())
        .build(&root_schema)
        .map_err(|error| {
            format!("tool '{tool_name}' schema is not valid Draft 2020-12: {error}")
        })?;
    let hash_material = serde_json::Value::Object(
        documents
            .iter()
            .map(|(path, value)| {
                let relative = path.strip_prefix(stage_root).expect("checked stage path");
                (relative.to_string_lossy().replace('\\', "/"), value.clone())
            })
            .collect(),
    );
    Ok(LoadedToolSchema {
        schema: root_schema,
        validator: Arc::new(validator),
        hash: tool_schema_hash(&hash_material),
    })
}

fn checked_schema_path(stage_root: &Path, path: &Path, tool_name: &str) -> Result<PathBuf, String> {
    let canonical = path.canonicalize().map_err(|error| {
        format!(
            "tool '{tool_name}' schema '{}' is unreadable: {error}",
            path.display()
        )
    })?;
    if !canonical.starts_with(stage_root) {
        return Err(format!(
            "tool '{tool_name}' schema path '{}' escapes the plugin stage",
            path.display()
        ));
    }
    Ok(canonical)
}

fn schema_uri(plugin_id: &str, stage_root: &Path, path: &Path) -> Result<String, String> {
    let relative = path
        .strip_prefix(stage_root)
        .map_err(|_| format!("schema '{}' escapes the plugin stage", path.display()))?;
    Ok(format!(
        "execlaw://plugin/{plugin_id}/{}",
        relative.to_string_lossy().replace('\\', "/")
    ))
}

struct LoadedToolSchema {
    schema: serde_json::Value,
    validator: Arc<jsonschema::Validator>,
    hash: String,
}

struct LoadedToolSchemas {
    input: Option<LoadedToolSchema>,
    result: Option<LoadedToolSchema>,
}

/// A tool handler resolved to its owning plugin.
#[derive(Clone)]
pub struct RegisteredTool {
    pub plugin_id: String,
    pub tool_name: String,
    pub latency: String,
    pub required_capabilities: Vec<String>,
    /// Manifest's `[[tools]].schema` field — relative path to a JSON
    /// Schema file inside the plugin stage. Kept for diagnostics
    /// even when the loaded `schema_json` below is `None`.
    pub schema_path: Option<String>,
    /// Manifest's `[[tools]].description` — operator-facing prose
    /// the agent uses to pick which tool to call. Critical for
    /// model tool-pick quality; pre-fix this was dropped on the
    /// floor and the model saw `format!("Plugin tool '{name}'
    /// (latency: {l})")` as the only description.
    pub description: Option<String>,
    /// JSON Schema parsed from the file at `schema_path`, loaded at
    /// `enable` time so per-turn catalogue construction doesn't have
    /// to re-read disk. `None` when the manifest didn't supply a
    /// schema file, or when load failed (logged warn, falls back to
    /// `{"type":"object"}` at dispatch time).
    pub schema_json: Option<serde_json::Value>,
    /// Draft 2020-12 validator compiled alongside `schema_json`.
    /// Dispatch uses this before OAuth lookup or plugin execution.
    pub schema_validator: Option<Arc<jsonschema::Validator>>,
    /// Content hash of the root input schema and all bundled local references.
    pub schema_hash: Option<String>,
    pub result_schema_path: Option<String>,
    pub result_schema_json: Option<serde_json::Value>,
    pub result_schema_validator: Option<Arc<jsonschema::Validator>>,
    pub result_schema_hash: Option<String>,
    /// Manifest's `[[tools]].trust_floor` (optional). Stored as the
    /// raw string so `plugin-host` doesn't have to depend on
    /// `execlaw-policy`; the dispatch layer parses + ranks it. Tools
    /// with no floor accept any caller that passes the existing
    /// capability gate.
    pub trust_floor: Option<String>,
    /// Manifest's `[[tools]].host_internal`. When true, the tool is
    /// registered with the host's `call_tool` dispatch table (so
    /// auto-bridge code in chats.rs / research/runner.rs can dial
    /// in) but EXCLUDED from the agent's tool catalog enumeration.
    /// Used for "host calls these on behalf of the agent" tools
    /// like `signal.set_typing` and `signal.send_with_attachments`
    /// where surfacing them to the planner causes spurious
    /// tool-call loops.
    pub host_internal: bool,
}

impl std::fmt::Debug for RegisteredTool {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RegisteredTool")
            .field("plugin_id", &self.plugin_id)
            .field("tool_name", &self.tool_name)
            .field("schema_path", &self.schema_path)
            .field("has_schema_validator", &self.schema_validator.is_some())
            .field("schema_hash", &self.schema_hash)
            .field(
                "has_result_schema_validator",
                &self.result_schema_validator.is_some(),
            )
            .finish_non_exhaustive()
    }
}

/// A built-in tool registered via [`HookRegistry::register_builtin`].
/// Distinct from `RegisteredTool` because built-ins are first-class
/// `ToolImpl` instances — they ship with a live executable handle, an
/// inline JSON schema, and a typed capability set, none of which the
/// plugin-manifest path can express today.
#[derive(Clone)]
pub struct RegisteredBuiltin {
    /// Owning impl. The dispatch layer pulls this out and calls
    /// `invoke(ctx, args)` directly.
    pub tool: Arc<dyn ToolImpl>,
    pub schema_hash: String,
    input_validator: Arc<jsonschema::Validator>,
    result_validator: Option<Arc<jsonschema::Validator>>,
}

impl std::fmt::Debug for RegisteredBuiltin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegisteredBuiltin")
            .field("name", &self.tool.descriptor().name)
            .finish()
    }
}

/// A UI-panel mount (admin-UI sub-route).
#[derive(Debug, Clone)]
pub struct RegisteredUiPanel {
    pub plugin_id: String,
    pub mount: String,
    pub entry: String,
}

/// A transport connection (Signal, email, etc.).
///
/// Today this is only metadata — the actual `dyn Transport`
/// instances land when the plugin host learns to spawn transport
/// plugin processes (Phase 11+). When that lands, the host will
/// also subscribe to the server's event bus and fan
/// `UiEvent::ConversationPhaseChanged` out to every registered
/// transport via `Transport::on_phase_changed` (Phase 10.1
/// established the trait; the relay is the missing-but-trivial
/// glue).
#[derive(Debug, Clone)]
pub struct RegisteredTransport {
    pub plugin_id: String,
    pub transport_id: String,
    pub supports_attachments: bool,
    pub supports_groups: bool,
}

/// An identity provider (matches transport identifiers → principals).
#[derive(Debug, Clone)]
pub struct RegisteredIdentityProvider {
    pub plugin_id: String,
    pub resolves: Vec<String>,
    pub trust_hint_default: String,
}

/// Event subscription keyed by event kind (e.g. `conversation.message_inbound`).
#[derive(Debug, Clone)]
pub struct RegisteredEventSubscription {
    pub plugin_id: String,
    pub kind: String,
    pub handler: String,
}

/// Alert-source namespace a plugin owns.
#[derive(Debug, Clone)]
pub struct RegisteredAlertSource {
    pub plugin_id: String,
    pub fingerprint_prefix: String,
}

/// A supervised sidecar — registered when a plugin's `[[services]]`
/// entry declares a `[services.sidecar]` table. The sidecar supervisor
/// reads this to learn which companion containers need managing.
///
/// The sidecar's identity is the parent service's `name`, which the
/// hook registry enforces is globally unique across installed
/// plugins. (Within-plugin uniqueness is caught at manifest-parse
/// time; cross-plugin collisions surface here.) Forcing global
/// uniqueness keeps docker container names distinct without
/// composite-key surgery in the supervisor.
///
/// Cheap to clone: every field is a small string or scalar.
#[derive(Debug, Clone)]
pub struct RegisteredSidecar {
    /// Owning plugin id — used by `disable` to drop sidecars when a
    /// plugin is uninstalled, AND to compose the supervisor's stable
    /// container name (`execlaw-sidecar-<plugin>-<name>`).
    pub plugin_id: String,
    /// Manifest's `[[services]].name`. The supervisor's primary key
    /// — exactly one sidecar with this name may be registered at a
    /// time across the whole control plane.
    pub name: String,
    /// Container image reference, copied verbatim from the manifest.
    pub image: String,
    /// Container port serving the sidecar's local RPC. The supervisor
    /// publishes this on a host port and probes the health path
    /// against it.
    pub rpc_port: u16,
    /// HTTP path on the RPC port the supervisor probes for liveness.
    pub rpc_health_path: String,
    /// Environment variables to set in the spawned container, copied
    /// from the manifest's `[[services]].env`. Plumbed through to
    /// `ServiceSpec.env` at spawn time. Sidecars that need
    /// configuration (e.g. signal-cli's `MODE=json-rpc`) declare
    /// these so the operator doesn't have to.
    pub env: Vec<(String, String)>,
    /// Bind mounts copied from the manifest's `[[services]].mounts`.
    /// Resolved against the plugin's stage directory and the
    /// per-sidecar managed state directory at supervisor spawn
    /// time — kept in raw `MountDecl` form here so the host paths
    /// can be computed against the right roots without leaking
    /// stage-path knowledge into the registry.
    pub mounts: Vec<execlaw_plugin_sdk::manifest::MountDecl>,
    /// Override for the container image's `ENTRYPOINT`. None means
    /// "use the image's built-in entrypoint." Used by sidecars that
    /// wrap the upstream entrypoint for in-container patching
    /// (e.g. signal-cli's `enable-read-receipts.sh` wrapper).
    pub entrypoint: Option<Vec<String>>,
    /// Absolute path to the plugin's extracted stage directory at
    /// the time of registration. Used to resolve `stage://` mount
    /// sources to host paths the supervisor can pass to dockerd.
    /// `None` for tests that register sidecars without a stage.
    pub stage_path: Option<std::path::PathBuf>,
}

/// One `[[oauth_accounts]]` declaration cached in the registry so
/// the hot dispatch path can skip a manifest TOML re-parse on every
/// tool.call / identity.resolve.
#[derive(Debug, Clone)]
pub struct RegisteredOauthAccount {
    pub plugin_id: String,
    pub account_name: String,
    pub provider: String,
    /// Scopes the plugin's manifest currently requires. Surfaced
    /// here (not just provider+name) so the OAuth admin path can
    /// compare them against the persisted `state_oauth_clients`
    /// row's stale snapshot — when a plugin upgrade adds scopes
    /// (e.g. google-calendar v0.1 read-only → v0.2 with
    /// `calendar.events`), the persisted row never auto-syncs
    /// without this field, leaving write tools 401-ing because
    /// Google issued the token under the old narrower set. The
    /// connect handler reads this and reconciles before each
    /// authorize URL build.
    pub scopes: Vec<String>,
}

/// One `[[admin_routes]]` declaration cached in the registry so
/// the host's HTTP dispatcher (`/api/admin/plugins/{plugin_id}/...`)
/// can look up the matching Rhai handler without re-parsing the
/// manifest TOML on every request.
#[derive(Debug, Clone)]
pub struct RegisteredAdminRoute {
    pub plugin_id: String,
    /// Uppercased HTTP method — `"GET"`, `"POST"`, etc. The host
    /// uppercases at registration time so dispatch can compare
    /// without copying.
    pub method: String,
    /// Path under `/api/admin/plugins/{plugin_id}`. Always begins
    /// with `/` post-normalisation; the registration step adds one
    /// if the manifest omitted it.
    pub path: String,
    /// Rhai top-level function name to invoke per request.
    pub handler: String,
    pub description: Option<String>,
}

/// Same shape as `RegisteredAdminRoute` but for the public
/// `[[webhook_routes]]` surface mounted at
/// `/api/webhooks/{plugin_id}{path}`. Held in its own map so the
/// public-webhook dispatcher can't accidentally pick up an admin
/// route, and vice-versa — keeping the two surfaces strictly
/// disjoint avoids accidental auth bypass.
///
/// `auth` mirrors the manifest's `WebhookAuthDecl`. `None` here
/// means the manifest omitted `auth` entirely (legacy "handler
/// validates" behavior, deprecation-warned at enable); a `Some`
/// value is enforced by the host BEFORE bus publish or handler
/// dispatch.
#[derive(Debug, Clone)]
pub struct RegisteredWebhookRoute {
    pub plugin_id: String,
    pub method: String,
    pub path: String,
    pub handler: String,
    pub description: Option<String>,
    pub auth: Option<execlaw_plugin_sdk::manifest::WebhookAuthDecl>,
}

/// Result of [`HookRegistry::lookup_any`]. The dispatch layer pattern-
/// matches on this so it can either invoke a built-in directly or
/// drop into the plugin RPC path with the metadata.
#[derive(Clone)]
pub enum RegisteredAny {
    Builtin(Arc<dyn ToolImpl>),
    Plugin(Arc<RegisteredTool>),
}

impl RegisteredAny {
    pub fn name(&self) -> &str {
        match self {
            Self::Builtin(t) => &t.descriptor().name,
            Self::Plugin(t) => &t.tool_name,
        }
    }

    pub fn is_builtin(&self) -> bool {
        matches!(self, Self::Builtin(_))
    }
}

/// The live hook registry. Cheap to clone (Arc inside); `RwLock`
/// guards mutation on plugin enable/disable.
#[derive(Debug, Default, Clone)]
pub struct HookRegistry {
    inner: Arc<RwLock<HookRegistryInner>>,
}

#[derive(Debug, Default)]
struct HookRegistryInner {
    /// Tools are wrapped in `Arc` so the per-call lookup only clones
    /// a refcount, not the underlying 4 Strings + Vec. Saves
    /// ~250 ns per tool lookup (§0 axiom #14 benchmark).
    tools_by_name: BTreeMap<String, Arc<RegisteredTool>>,
    /// First-class built-in tools — distinct from plugin tools so the
    /// dispatch layer can pull a live `Arc<dyn ToolImpl>` out and
    /// invoke it directly. Tool names CANNOT collide with the plugin
    /// `tools_by_name` map: registering a built-in with a name that's
    /// already owned by a plugin (or vice versa) is rejected.
    builtins_by_name: BTreeMap<String, RegisteredBuiltin>,
    ui_panels_by_mount: BTreeMap<String, RegisteredUiPanel>,
    transports_by_id: BTreeMap<String, RegisteredTransport>,
    /// Sidecars keyed by channel — at most one per channel across the
    /// whole control plane. The supervisor reads this; conflicts are
    /// rejected at `enable` time so the supervisor only ever sees
    /// well-formed state.
    sidecars_by_name: BTreeMap<String, RegisteredSidecar>,
    identity_providers: BTreeMap<String, RegisteredIdentityProvider>,
    event_subs: HashMap<String, Vec<RegisteredEventSubscription>>,
    alert_sources: Vec<RegisteredAlertSource>,
    /// Per-plugin `[[oauth_accounts]]` cache. The dispatch layer
    /// reads this on every RPC to know which account_names to
    /// fetch tokens for; without the cache it would re-parse the
    /// manifest TOML on every call.
    oauth_accounts: BTreeMap<String, Vec<RegisteredOauthAccount>>,
    /// Per-plugin admin routes (`[[admin_routes]]`). Mounted under
    /// `/api/admin/plugins/{plugin_id}` by the host's dispatcher;
    /// every entry resolves to a Rhai handler in the plugin's
    /// script. Cleared on plugin disable/uninstall.
    admin_routes: BTreeMap<String, Vec<RegisteredAdminRoute>>,
    /// Per-plugin webhook routes (`[[webhook_routes]]`). Mounted
    /// UNAUTHENTICATED under `/api/webhooks/{plugin_id}` by the
    /// host's webhook dispatcher. Plugins are responsible for
    /// validating the request inside the Rhai handler — typically
    /// matching a secret query-token against a vault row.
    webhook_routes: BTreeMap<String, Vec<RegisteredWebhookRoute>>,
    enabled_plugins: BTreeSet<String>,
}

impl HookRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Validate every declared tool schema without mutating the registry.
    /// Upgrade uses this before tearing down the currently working version.
    pub fn validate_schemas_with_stage(
        &self,
        manifest: &PluginManifest,
        stage_path: &Path,
    ) -> Result<(), String> {
        load_tool_schemas(manifest, Some(stage_path)).map(|_| ())
    }

    /// Reject upgrades that change an existing tool contract. Exact bundle
    /// hash equality is intentionally conservative until a complete JSON
    /// Schema subsumption checker is available.
    pub fn validate_upgrade_schemas_with_stage(
        &self,
        manifest: &PluginManifest,
        stage_path: &Path,
    ) -> Result<(), String> {
        let schemas = load_tool_schemas(manifest, Some(stage_path))?;
        let r = self.inner.read().unwrap();
        for tool in &manifest.tools {
            let Some(existing) = r.tools_by_name.get(&tool.name) else {
                continue;
            };
            if existing.plugin_id != manifest.plugin.id {
                continue;
            }
            let replacement = schemas.get(&tool.name);
            let replacement_input = replacement
                .and_then(|loaded| loaded.input.as_ref())
                .map(|loaded| loaded.hash.as_str());
            let replacement_result = replacement
                .and_then(|loaded| loaded.result.as_ref())
                .map(|loaded| loaded.hash.as_str());
            if existing.schema_hash.as_deref() != replacement_input
                || existing.result_schema_hash.as_deref() != replacement_result
            {
                return Err(format!(
                    "tool '{}' schema changed incompatibly; publish a new tool name or retain the prior schema",
                    tool.name
                ));
            }
        }
        Ok(())
    }

    /// Enable a plugin: register every hook declared by its manifest.
    ///
    /// Returns `Err` with a conflict description if a tool name / ui
    /// panel mount / transport id is already owned by another plugin.
    /// The registry is left untouched on error (all-or-nothing).
    pub fn enable(&self, manifest: &PluginManifest) -> Result<(), String> {
        self.enable_with_stage(manifest, None)
    }

    /// Same as [`enable`] but with the plugin's on-disk stage path,
    /// used to load per-tool JSON Schema files at register time.
    /// Production install / hydrate / upgrade paths pass
    /// `Some(stage_path)`; tests that don't care about schemas can
    /// keep using the no-arg form.
    pub fn enable_with_stage(
        &self,
        manifest: &PluginManifest,
        stage_path: Option<&std::path::Path>,
    ) -> Result<(), String> {
        // Compile every declared schema before taking the registry lock or
        // inserting any hook, preserving enable's all-or-nothing contract.
        let mut tool_schemas = load_tool_schemas(manifest, stage_path)?;
        let mut w = self.inner.write().unwrap();
        let plugin_id = &manifest.plugin.id;

        if w.enabled_plugins.contains(plugin_id) {
            return Err(format!("plugin '{plugin_id}' is already enabled"));
        }

        // Validate conflicts first, then insert.
        //
        // `host_implemented` tools are deliberately exempt from BOTH
        // checks: a builtin under the same name is the *intended*
        // implementation (the manifest declaration is just metadata
        // for catalog/attribution), and a second plugin attempting to
        // host-implement the same name is a tooling-author error
        // caught here only if the duplicate is itself host-implemented
        // — let the builtin layer surface that conflict.
        for t in &manifest.tools {
            if t.host_implemented {
                continue;
            }
            if let Some(existing) = w.tools_by_name.get(&t.name) {
                return Err(format!(
                    "tool '{}' is already registered by plugin '{}'",
                    t.name, existing.plugin_id
                ));
            }
            if w.builtins_by_name.contains_key(&t.name) {
                return Err(format!(
                    "tool '{}' is already registered as a built-in",
                    t.name
                ));
            }
        }
        for p in &manifest.ui_panels {
            if let Some(existing) = w.ui_panels_by_mount.get(&p.mount) {
                return Err(format!(
                    "ui_panel mount '{}' is already registered by plugin '{}'",
                    p.mount, existing.plugin_id
                ));
            }
        }
        if let Some(t) = &manifest.transport
            && let Some(existing) = w.transports_by_id.get(&t.transport_id)
        {
            return Err(format!(
                "transport id '{}' is already registered by plugin '{}'",
                t.transport_id, existing.plugin_id
            ));
        }
        // Sidecars — at most one per name across the whole control
        // plane. Within-plugin name dups were caught at parse time;
        // this check catches *cross-plugin* collisions (two plugins
        // both wanting to register a sidecar named "signal-cli", or
        // both wanting an "ocr-worker"). Globally-unique names keep
        // docker container names distinct without composite-key
        // surgery in the supervisor.
        for s in &manifest.services {
            if s.sidecar.is_some()
                && let Some(existing) = w.sidecars_by_name.get(&s.name)
            {
                return Err(format!(
                    "sidecar name '{}' is already registered by plugin '{}'",
                    s.name, existing.plugin_id,
                ));
            }
        }

        // Insert.
        for t in &manifest.tools {
            if t.host_implemented {
                // Manifest-declared but host-implemented: skip
                // tools_by_name. The host's builtin (registered via
                // `register_builtin`) is the dispatch target. Catalog
                // surfaces should still attribute the tool to this
                // plugin — that's wired by listing both the builtin
                // descriptor and the plugin's manifest tools, then
                // joining on name.
                continue;
            }
            let latency = match t.latency {
                execlaw_plugin_sdk::manifest::ToolLatency::Low => "low",
                execlaw_plugin_sdk::manifest::ToolLatency::Medium => "medium",
                execlaw_plugin_sdk::manifest::ToolLatency::High => "high",
            };
            let loaded_schemas = tool_schemas.remove(&t.name);
            let input_schema = loaded_schemas
                .as_ref()
                .and_then(|loaded| loaded.input.as_ref());
            let result_schema = loaded_schemas
                .as_ref()
                .and_then(|loaded| loaded.result.as_ref());
            w.tools_by_name.insert(
                t.name.clone(),
                Arc::new(RegisteredTool {
                    plugin_id: plugin_id.clone(),
                    tool_name: t.name.clone(),
                    latency: latency.to_owned(),
                    required_capabilities: t.required_capabilities.clone(),
                    schema_path: t.schema.clone(),
                    description: t.description.clone(),
                    schema_json: input_schema.map(|loaded| loaded.schema.clone()),
                    schema_validator: input_schema.map(|loaded| loaded.validator.clone()),
                    schema_hash: input_schema.map(|loaded| loaded.hash.clone()),
                    result_schema_path: t.result_schema.clone(),
                    result_schema_json: result_schema.map(|loaded| loaded.schema.clone()),
                    result_schema_validator: result_schema.map(|loaded| loaded.validator.clone()),
                    result_schema_hash: result_schema.map(|loaded| loaded.hash.clone()),
                    trust_floor: t.trust_floor.clone(),
                    host_internal: t.host_internal,
                }),
            );
        }
        for p in &manifest.ui_panels {
            w.ui_panels_by_mount.insert(
                p.mount.clone(),
                RegisteredUiPanel {
                    plugin_id: plugin_id.clone(),
                    mount: p.mount.clone(),
                    entry: p.entry.clone(),
                },
            );
        }
        if let Some(t) = &manifest.transport {
            w.transports_by_id.insert(
                t.transport_id.clone(),
                RegisteredTransport {
                    plugin_id: plugin_id.clone(),
                    transport_id: t.transport_id.clone(),
                    supports_attachments: t.supports_attachments,
                    supports_groups: t.supports_groups,
                },
            );
        }
        for s in &manifest.services {
            if let Some(b) = &s.sidecar {
                w.sidecars_by_name.insert(
                    s.name.clone(),
                    RegisteredSidecar {
                        plugin_id: plugin_id.clone(),
                        name: s.name.clone(),
                        image: s.image.clone(),
                        rpc_port: b.rpc_port,
                        rpc_health_path: b.rpc_health_path.clone(),
                        env: s.env.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
                        mounts: s.mounts.clone(),
                        entrypoint: s.entrypoint.clone(),
                        stage_path: stage_path.map(|p| p.to_path_buf()),
                    },
                );
            }
        }
        if let Some(ip) = &manifest.identity_provider {
            w.identity_providers.insert(
                plugin_id.clone(),
                RegisteredIdentityProvider {
                    plugin_id: plugin_id.clone(),
                    resolves: ip.resolves.clone(),
                    trust_hint_default: ip.trust_hint_default.clone(),
                },
            );
        }
        for sub in &manifest.event_subscriptions {
            w.event_subs
                .entry(sub.on.clone())
                .or_default()
                .push(RegisteredEventSubscription {
                    plugin_id: plugin_id.clone(),
                    kind: sub.on.clone(),
                    handler: sub.handler.clone().unwrap_or_default(),
                });
        }
        for src in &manifest.alert_sources {
            w.alert_sources.push(RegisteredAlertSource {
                plugin_id: plugin_id.clone(),
                fingerprint_prefix: src.fingerprint_prefix.clone(),
            });
        }
        if !manifest.oauth_accounts.is_empty() {
            let cached: Vec<RegisteredOauthAccount> = manifest
                .oauth_accounts
                .iter()
                .map(|a| RegisteredOauthAccount {
                    plugin_id: plugin_id.clone(),
                    account_name: a.name.clone(),
                    provider: a.provider.clone(),
                    scopes: a.scopes.clone(),
                })
                .collect();
            w.oauth_accounts.insert(plugin_id.clone(), cached);
        }
        if !manifest.admin_routes.is_empty() {
            let cached: Vec<RegisteredAdminRoute> = manifest
                .admin_routes
                .iter()
                .map(|r| {
                    let mut path = r.path.clone();
                    if !path.starts_with('/') {
                        path.insert(0, '/');
                    }
                    RegisteredAdminRoute {
                        plugin_id: plugin_id.clone(),
                        method: r.method.to_uppercase(),
                        path,
                        handler: r.handler.clone(),
                        description: r.description.clone(),
                    }
                })
                .collect();
            w.admin_routes.insert(plugin_id.clone(), cached);
        }
        if !manifest.webhook_routes.is_empty() {
            let cached: Vec<RegisteredWebhookRoute> = manifest
                .webhook_routes
                .iter()
                .map(|r| {
                    let mut path = r.path.clone();
                    if !path.starts_with('/') {
                        path.insert(0, '/');
                    }
                    if r.auth.is_none() {
                        // Loud, one-line, structured warning so an operator
                        // grep'ing for `webhook_route_auth_unset` finds every
                        // route still on the legacy "handler validates" path.
                        tracing::warn!(
                            target: "plugin_host::webhook_routes",
                            plugin_id = %plugin_id,
                            method = %r.method.to_uppercase(),
                            path = %path,
                            "webhook_route_auth_unset: this route relies on the plugin handler \
                             to validate the caller. Declare `auth = {{ kind = \"query_token\", ... }}` \
                             (or `kind = \"none\"` to silence) in plugin.toml to have the host \
                             enforce authentication before bus publish."
                        );
                    }
                    RegisteredWebhookRoute {
                        plugin_id: plugin_id.clone(),
                        method: r.method.to_uppercase(),
                        path,
                        handler: r.handler.clone(),
                        description: r.description.clone(),
                        auth: r.auth.clone(),
                    }
                })
                .collect();
            w.webhook_routes.insert(plugin_id.clone(), cached);
        }
        w.enabled_plugins.insert(plugin_id.clone());
        Ok(())
    }

    /// Disable a plugin: remove every hook it owns.
    pub fn disable(&self, plugin_id: &str) {
        let mut w = self.inner.write().unwrap();
        w.tools_by_name.retain(|_, v| v.plugin_id != plugin_id);
        w.ui_panels_by_mount.retain(|_, v| v.plugin_id != plugin_id);
        w.transports_by_id.retain(|_, v| v.plugin_id != plugin_id);
        w.sidecars_by_name.retain(|_, v| v.plugin_id != plugin_id);
        w.identity_providers.remove(plugin_id);
        for subs in w.event_subs.values_mut() {
            subs.retain(|s| s.plugin_id != plugin_id);
        }
        w.event_subs.retain(|_, v| !v.is_empty());
        w.alert_sources.retain(|s| s.plugin_id != plugin_id);
        w.oauth_accounts.remove(plugin_id);
        w.admin_routes.remove(plugin_id);
        w.webhook_routes.remove(plugin_id);
        w.enabled_plugins.remove(plugin_id);
    }

    pub fn is_enabled(&self, plugin_id: &str) -> bool {
        self.inner
            .read()
            .unwrap()
            .enabled_plugins
            .contains(plugin_id)
    }

    /// Look up a plugin-owned tool by name. The returned `Arc`
    /// clones in ~10 ns (a refcount bump) vs ~250 ns for a full
    /// struct clone — the per-call lookup is on the turn-dispatch
    /// hot path (§0 axiom #14). Returns `None` for built-ins;
    /// callers that need uniform lookup across both tiers should use
    /// [`Self::builtin`] alongside this method or
    /// [`Self::lookup_any`] for a single combined check.
    pub fn tool(&self, name: &str) -> Option<Arc<RegisteredTool>> {
        self.inner.read().unwrap().tools_by_name.get(name).cloned()
    }

    pub fn all_tools(&self) -> Vec<Arc<RegisteredTool>> {
        self.inner
            .read()
            .unwrap()
            .tools_by_name
            .values()
            .cloned()
            .collect()
    }

    /// Like `all_tools` but excludes tools the plugin marked
    /// `host_internal = true` in its manifest. Use this when
    /// building the agent's tool catalog — those tools are
    /// reachable via `call_tool` but should NOT be surfaced to
    /// the planner (typing-indicator, attachment-bridge convention
    /// tools, etc.).
    pub fn agent_callable_tools(&self) -> Vec<Arc<RegisteredTool>> {
        self.inner
            .read()
            .unwrap()
            .tools_by_name
            .values()
            .filter(|t| !t.host_internal)
            .cloned()
            .collect()
    }

    /// Register a first-class built-in tool. Errors if a plugin tool
    /// or another built-in already owns the name — built-ins are
    /// singletons across the registry like every other hook.
    ///
    /// Built-ins land all-or-nothing: this call mutates exactly one
    /// entry, so there's no atomicity hazard. Boot-time registrars
    /// pass every core tool through here in sequence; if one fails
    /// the operator sees a clear error rather than a half-populated
    /// registry.
    pub fn register_builtin(&self, tool: Arc<dyn ToolImpl>) -> Result<(), String> {
        let descriptor = tool.descriptor();
        let input_validator = compile_tool_schema(
            &descriptor.schema,
            &format!("built-in tool '{}' input schema", descriptor.name),
        )?;
        let result_validator = tool
            .result_schema()
            .map(|schema| {
                compile_tool_schema(
                    schema,
                    &format!("built-in tool '{}' result schema", descriptor.name),
                )
                .map(Arc::new)
            })
            .transpose()?;
        let mut w = self.inner.write().unwrap();
        let name = descriptor.name.clone();
        if let Some(existing) = w.tools_by_name.get(&name) {
            return Err(format!(
                "built-in tool '{name}' would shadow plugin '{}'",
                existing.plugin_id
            ));
        }
        if w.builtins_by_name.contains_key(&name) {
            return Err(format!("built-in tool '{name}' is already registered"));
        }
        w.builtins_by_name.insert(
            name,
            RegisteredBuiltin {
                schema_hash: tool_schema_hash(&descriptor.schema),
                tool,
                input_validator: Arc::new(input_validator),
                result_validator,
            },
        );
        Ok(())
    }

    /// Validate a built-in invocation before constructing its capability
    /// context. Unknown names return `Ok(())` so callers can continue routing.
    pub fn validate_builtin_input(
        &self,
        name: &str,
        args: &serde_json::Value,
    ) -> Result<(), String> {
        let r = self.inner.read().unwrap();
        let Some(builtin) = r.builtins_by_name.get(name) else {
            return Ok(());
        };
        builtin.input_validator.validate(args).map_err(|error| {
            format!("tool '{name}' arguments do not match its JSON Schema: {error}")
        })
    }

    /// Validate a successful built-in result when the implementation declares
    /// a result schema.
    pub fn validate_builtin_result(
        &self,
        name: &str,
        value: &serde_json::Value,
    ) -> Result<(), String> {
        let r = self.inner.read().unwrap();
        let Some(validator) = r
            .builtins_by_name
            .get(name)
            .and_then(|builtin| builtin.result_validator.as_ref())
        else {
            return Ok(());
        };
        validator.validate(value).map_err(|error| {
            format!("tool '{name}' result does not match its JSON Schema: {error}")
        })
    }

    pub fn builtin_schema_hash(&self, name: &str) -> Option<String> {
        self.inner
            .read()
            .unwrap()
            .builtins_by_name
            .get(name)
            .map(|builtin| builtin.schema_hash.clone())
    }

    /// Look up a built-in by name. `None` if no built-in owns this
    /// name (it might be a plugin tool — use [`Self::tool`] for that).
    pub fn builtin(&self, name: &str) -> Option<Arc<dyn ToolImpl>> {
        self.inner
            .read()
            .unwrap()
            .builtins_by_name
            .get(name)
            .map(|b| b.tool.clone())
    }

    /// Every registered built-in. Used by the registrar at boot to
    /// drive the `config_tool_access` seed and by the Settings
    /// page's tool catalog.
    pub fn all_builtins(&self) -> Vec<Arc<dyn ToolImpl>> {
        self.inner
            .read()
            .unwrap()
            .builtins_by_name
            .values()
            .map(|b| b.tool.clone())
            .collect()
    }

    /// Combined lookup: returns the built-in impl if one owns the
    /// name, otherwise the plugin metadata wrapper. Lets the dispatch
    /// layer present one resolution call regardless of source.
    pub fn lookup_any(&self, name: &str) -> Option<RegisteredAny> {
        let r = self.inner.read().unwrap();
        if let Some(b) = r.builtins_by_name.get(name) {
            return Some(RegisteredAny::Builtin(b.tool.clone()));
        }
        r.tools_by_name
            .get(name)
            .map(|t| RegisteredAny::Plugin(t.clone()))
    }

    pub fn transport(&self, id: &str) -> Option<RegisteredTransport> {
        self.inner.read().unwrap().transports_by_id.get(id).cloned()
    }

    /// Snapshot every plugin-registered `[transport]` declaration.
    /// Used by the Settings → User "My identities" surface to
    /// populate the transport dropdown dynamically — the SPA
    /// shouldn't be guessing what channels are valid before a
    /// plugin lands. Order is stable (`BTreeMap`) so the dropdown
    /// presents transports in a consistent order across reloads.
    pub fn all_transports(&self) -> Vec<RegisteredTransport> {
        self.inner
            .read()
            .unwrap()
            .transports_by_id
            .values()
            .cloned()
            .collect()
    }

    /// Look up the sidecar registered with `name`, if any. Returns
    /// a clone — the supervisor calls this on every reconcile tick
    /// so it pays the clone cost in exchange for a lock-free read.
    pub fn sidecar(&self, name: &str) -> Option<RegisteredSidecar> {
        self.inner
            .read()
            .unwrap()
            .sidecars_by_name
            .get(name)
            .cloned()
    }

    /// Snapshot every registered sidecar. The supervisor's
    /// reconcile-loop reads this each tick to compute "desired" vs
    /// "running" diff. Order is stable (BTreeMap) so the
    /// supervisor's tick log lines stay grep-able.
    pub fn all_sidecars(&self) -> Vec<RegisteredSidecar> {
        self.inner
            .read()
            .unwrap()
            .sidecars_by_name
            .values()
            .cloned()
            .collect()
    }

    pub fn identity_providers(&self) -> Vec<RegisteredIdentityProvider> {
        self.inner
            .read()
            .unwrap()
            .identity_providers
            .values()
            .cloned()
            .collect()
    }

    /// `[[oauth_accounts]]` declared by `plugin_id`'s manifest.
    /// Empty when the plugin declares none, isn't enabled, or
    /// hasn't been registered yet. Returned by clone — caller is
    /// expected to walk it once per RPC.
    pub fn oauth_accounts_for(&self, plugin_id: &str) -> Vec<RegisteredOauthAccount> {
        self.inner
            .read()
            .unwrap()
            .oauth_accounts
            .get(plugin_id)
            .cloned()
            .unwrap_or_default()
    }

    /// Admin routes (`[[admin_routes]]`) declared by `plugin_id`.
    /// Returned by clone — caller walks once per HTTP request to
    /// match (method, path) → handler. Empty when the plugin
    /// declares none.
    pub fn admin_routes_for(&self, plugin_id: &str) -> Vec<RegisteredAdminRoute> {
        self.inner
            .read()
            .unwrap()
            .admin_routes
            .get(plugin_id)
            .cloned()
            .unwrap_or_default()
    }

    /// Webhook routes (`[[webhook_routes]]`) declared by `plugin_id`.
    /// Same shape as admin_routes_for but for the unauthenticated
    /// public-callback surface. Empty when the plugin declares none.
    pub fn webhook_routes_for(&self, plugin_id: &str) -> Vec<RegisteredWebhookRoute> {
        self.inner
            .read()
            .unwrap()
            .webhook_routes
            .get(plugin_id)
            .cloned()
            .unwrap_or_default()
    }

    pub fn ui_panels(&self) -> Vec<RegisteredUiPanel> {
        self.inner
            .read()
            .unwrap()
            .ui_panels_by_mount
            .values()
            .cloned()
            .collect()
    }

    pub fn subscribers_for(&self, event_kind: &str) -> Vec<RegisteredEventSubscription> {
        self.inner
            .read()
            .unwrap()
            .event_subs
            .get(event_kind)
            .cloned()
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest_with_tools(id: &str, tool_names: &[&str]) -> PluginManifest {
        let mut t = format!("[plugin]\nid = \"{id}\"\nname = \"{id}\"\nversion = \"1.0.0\"\n");
        for name in tool_names {
            t.push_str(&format!(
                "\n[[tools]]\nname = \"{name}\"\nschema = \"schemas/{name}.json\"\nlatency = \"low\"\nrequired_capabilities = []\n"
            ));
        }
        PluginManifest::parse(&t).unwrap()
    }

    #[test]
    fn enable_registers_tools() {
        let reg = HookRegistry::new();
        reg.enable(&manifest_with_tools("p1", &["a", "b"])).unwrap();
        assert!(reg.tool("a").is_some());
        assert!(reg.tool("b").is_some());
        assert_eq!(reg.all_tools().len(), 2);
        assert!(reg.is_enabled("p1"));
    }

    #[test]
    fn enable_carries_manifest_description_through_to_registered_tool() {
        // Pre-fix the description was dropped on the floor and the
        // model saw `Plugin tool 'X' (latency: Y)` instead. This
        // pins the regression.
        let manifest = PluginManifest::parse(
            r#"
[plugin]
id = "desc-test"
name = "Description Test"
version = "1.0.0"

[[tools]]
name = "search.things"
description = "Find a thing the operator asked about. Use when the user names a topic without enough context to answer directly."
latency = "low"
"#,
        )
        .unwrap();
        let reg = HookRegistry::new();
        reg.enable(&manifest).unwrap();
        let t = reg.tool("search.things").expect("tool registered");
        assert_eq!(
            t.description.as_deref(),
            Some(
                "Find a thing the operator asked about. Use when the user names a topic without enough context to answer directly."
            ),
        );
    }

    #[test]
    fn enable_with_stage_loads_json_schema_file_into_registered_tool() {
        // Manifest declares `schema = "schemas/search.json"`; the
        // file lives under the stage path. After enable the parsed
        // JSON Schema MUST be on the RegisteredTool so chats.rs can
        // hand it to vLLM verbatim instead of an empty object.
        let stage = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(stage.path().join("schemas")).unwrap();
        std::fs::write(
            stage.path().join("schemas/search.json"),
            r#"{"type":"object","properties":{"q":{"type":"string"}},"required":["q"]}"#,
        )
        .unwrap();
        let manifest = PluginManifest::parse(
            r#"
[plugin]
id = "schema-test"
name = "Schema Test"
version = "1.0.0"

[[tools]]
name = "search"
description = "Search."
schema = "schemas/search.json"
latency = "low"
"#,
        )
        .unwrap();
        let reg = HookRegistry::new();
        reg.enable_with_stage(&manifest, Some(stage.path()))
            .unwrap();
        let t = reg.tool("search").expect("tool registered");
        let schema = t
            .schema_json
            .as_ref()
            .expect("schema must be loaded into the registry");
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["properties"]["q"]["type"], "string");
        assert_eq!(schema["required"][0], "q");
        assert_eq!(t.schema_hash.as_deref().map(str::len), Some(64));
    }

    #[test]
    fn enable_with_stage_compiles_bundled_local_refs() {
        let stage = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(stage.path().join("schemas/defs")).unwrap();
        std::fs::write(
            stage.path().join("schemas/input.json"),
            r#"{"type":"object","properties":{"q":{"$ref":"defs/query.json"}},"required":["q"]}"#,
        )
        .unwrap();
        std::fs::write(
            stage.path().join("schemas/defs/query.json"),
            r#"{"type":"string","minLength":1}"#,
        )
        .unwrap();
        let manifest = PluginManifest::parse(
            r#"
[plugin]
id = "local-ref"
name = "Local Ref"
version = "1.0.0"

[[tools]]
name = "search"
schema = "schemas/input.json"
"#,
        )
        .unwrap();
        let reg = HookRegistry::new();
        reg.enable_with_stage(&manifest, Some(stage.path()))
            .unwrap();
        let tool = reg.tool("search").unwrap();
        let validator = tool.schema_validator.as_ref().unwrap();
        assert!(validator.is_valid(&serde_json::json!({"q": "rust"})));
        assert!(!validator.is_valid(&serde_json::json!({"q": ""})));
        assert_eq!(tool.schema_hash.as_deref().map(str::len), Some(64));
    }

    #[test]
    fn enable_with_stage_rejects_missing_schema_file_atomically() {
        let stage = tempfile::tempdir().unwrap();
        let manifest = PluginManifest::parse(
            r#"
[plugin]
id = "missing-schema"
name = "Missing Schema"
version = "1.0.0"

[[tools]]
name = "x"
description = "x"
schema = "schemas/does_not_exist.json"
latency = "low"
"#,
        )
        .unwrap();
        let reg = HookRegistry::new();
        let error = reg
            .enable_with_stage(&manifest, Some(stage.path()))
            .expect_err("missing declared schemas must reject registration");
        assert!(error.contains("is unreadable"), "unexpected error: {error}");
        assert!(!reg.is_enabled("missing-schema"));
        assert!(reg.tool("x").is_none());
    }

    #[test]
    fn enable_with_stage_rejects_remote_schema_ref() {
        let stage = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(stage.path().join("schemas")).unwrap();
        std::fs::write(
            stage.path().join("schemas/x.json"),
            r#"{"type":"object","properties":{"x":{"$ref":"https://example.invalid/x.json"}}}"#,
        )
        .unwrap();
        let manifest = manifest_with_tools("remote-ref", &["x"]);
        let reg = HookRegistry::new();
        let error = reg
            .enable_with_stage(&manifest, Some(stage.path()))
            .expect_err("network schema references must be rejected");
        assert!(
            error.contains("forbidden network reference"),
            "unexpected error: {error}"
        );
        assert!(!reg.is_enabled("remote-ref"));
    }

    #[test]
    fn enable_with_stage_rejects_external_dynamic_ref() {
        let stage = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(stage.path().join("schemas")).unwrap();
        std::fs::write(
            stage.path().join("schemas/x.json"),
            r#"{"type":"object","$dynamicRef":"https:example.invalid/x.json"}"#,
        )
        .unwrap();
        let manifest = manifest_with_tools("dynamic-ref", &["x"]);
        let reg = HookRegistry::new();
        let error = reg
            .enable_with_stage(&manifest, Some(stage.path()))
            .expect_err("external dynamic references must be rejected");
        assert!(
            error.contains("external reference"),
            "unexpected error: {error}"
        );
        assert!(!reg.is_enabled("dynamic-ref"));
    }

    #[test]
    fn enable_with_stage_rejects_schema_path_traversal() {
        let parent = tempfile::tempdir().unwrap();
        let stage = parent.path().join("plugin");
        std::fs::create_dir(&stage).unwrap();
        std::fs::write(parent.path().join("outside.json"), r#"{"type":"object"}"#).unwrap();
        let manifest = PluginManifest::parse(
            r#"
[plugin]
id = "path-escape"
name = "Path Escape"
version = "1.0.0"

[[tools]]
name = "x"
schema = "../outside.json"
"#,
        )
        .unwrap();
        let reg = HookRegistry::new();
        let error = reg
            .enable_with_stage(&manifest, Some(&stage))
            .expect_err("schema paths must remain inside the plugin stage");
        assert!(error.contains("escapes the plugin stage"));
        assert!(!reg.is_enabled("path-escape"));
    }

    #[test]
    fn enable_with_no_stage_path_skips_schema_loading() {
        // Test path / hydrate-without-disk path: enable() (no stage)
        // returns the same shape but with schema_json = None even
        // when the manifest declared a schema. Description still
        // carries through.
        let manifest = PluginManifest::parse(
            r#"
[plugin]
id = "no-stage"
name = "No Stage"
version = "1.0.0"

[[tools]]
name = "x"
description = "still here"
schema = "schemas/x.json"
latency = "low"
"#,
        )
        .unwrap();
        let reg = HookRegistry::new();
        reg.enable(&manifest).unwrap();
        let t = reg.tool("x").expect("tool registered");
        assert_eq!(t.description.as_deref(), Some("still here"));
        assert!(t.schema_json.is_none());
    }

    #[test]
    fn duplicate_tool_name_rejected() {
        let reg = HookRegistry::new();
        reg.enable(&manifest_with_tools("p1", &["shared"])).unwrap();
        let err = reg
            .enable(&manifest_with_tools("p2", &["shared"]))
            .unwrap_err();
        assert!(err.contains("already registered by plugin 'p1'"));
        // p2 should not be registered at all.
        assert!(!reg.is_enabled("p2"));
    }

    #[test]
    fn disable_removes_all_owned_hooks() {
        let reg = HookRegistry::new();
        reg.enable(&manifest_with_tools("p1", &["x", "y"])).unwrap();
        reg.disable("p1");
        assert!(reg.tool("x").is_none());
        assert!(reg.tool("y").is_none());
        assert!(!reg.is_enabled("p1"));
    }

    #[test]
    fn already_enabled_plugin_errors_on_second_enable() {
        let reg = HookRegistry::new();
        reg.enable(&manifest_with_tools("p1", &["a"])).unwrap();
        let err = reg.enable(&manifest_with_tools("p1", &["a"])).unwrap_err();
        assert!(err.contains("already enabled"));
    }

    #[test]
    fn enable_then_disable_then_reenable_works() {
        let reg = HookRegistry::new();
        let m = manifest_with_tools("p1", &["a"]);
        reg.enable(&m).unwrap();
        reg.disable("p1");
        reg.enable(&m).unwrap();
        assert!(reg.tool("a").is_some());
    }

    fn manifest_with_sidecar(plugin_id: &str, sidecar_name: &str, port: u16) -> PluginManifest {
        PluginManifest::parse(&format!(
            r#"
[plugin]
id = "{plugin_id}"
name = "P"
version = "0.1.0"

[[services]]
name = "{sidecar_name}"
image = "execlaw/{sidecar_name}:0.1"

[services.sidecar]
rpc_port = {port}
"#
        ))
        .unwrap()
    }

    #[test]
    fn enable_registers_sidecar_service() {
        let reg = HookRegistry::new();
        reg.enable(&manifest_with_sidecar("p-signal", "signal-cli", 8080))
            .unwrap();
        let b = reg.sidecar("signal-cli").expect("sidecar registered");
        assert_eq!(b.plugin_id, "p-signal");
        assert_eq!(b.name, "signal-cli");
        assert_eq!(b.image, "execlaw/signal-cli:0.1");
        assert_eq!(b.rpc_port, 8080);
        // Default health path applied via the manifest default.
        assert_eq!(b.rpc_health_path, "/healthz");
    }

    #[test]
    fn cross_plugin_sidecar_name_collision_rejected() {
        // Two distinct plugins both wanting a sidecar named
        // "signal-cli" — the supervisor would have no way to pick
        // a winner (and docker container names would collide), so
        // refuse the second enable. The first plugin keeps its
        // sidecar; the second isn't registered at all.
        let reg = HookRegistry::new();
        reg.enable(&manifest_with_sidecar("p-signal-a", "signal-cli", 8080))
            .unwrap();
        let err = reg
            .enable(&manifest_with_sidecar("p-signal-b", "signal-cli", 8081))
            .unwrap_err();
        assert!(
            err.contains("sidecar name 'signal-cli' is already registered"),
            "expected name-collision message, got: {err}",
        );
        assert!(!reg.is_enabled("p-signal-b"));
        let b = reg.sidecar("signal-cli").unwrap();
        assert_eq!(b.plugin_id, "p-signal-a");
        assert_eq!(b.rpc_port, 8080);
    }

    #[test]
    fn disable_drops_sidecar_for_owning_plugin() {
        let reg = HookRegistry::new();
        reg.enable(&manifest_with_sidecar("p-signal", "signal-cli", 8080))
            .unwrap();
        reg.enable(&manifest_with_sidecar("p-wa", "whatsapp-bridge", 8081))
            .unwrap();
        reg.disable("p-signal");
        // Signal sidecar gone…
        assert!(reg.sidecar("signal-cli").is_none());
        // …but WhatsApp sidecar survives. Important — disable must
        // not be over-broad.
        let wa = reg.sidecar("whatsapp-bridge").expect("wa sidecar survives");
        assert_eq!(wa.plugin_id, "p-wa");
    }

    #[test]
    fn all_sidecars_returns_stable_order_across_calls() {
        // BTreeMap order — useful for deterministic supervisor logs.
        let reg = HookRegistry::new();
        reg.enable(&manifest_with_sidecar("p-z", "z-sidecar", 9000))
            .unwrap();
        reg.enable(&manifest_with_sidecar("p-a", "a-sidecar", 9001))
            .unwrap();
        let names: Vec<String> = reg.all_sidecars().into_iter().map(|b| b.name).collect();
        assert_eq!(names, vec!["a-sidecar", "z-sidecar"]);
    }

    #[test]
    fn non_sidecar_service_does_not_register_in_sidecars_map() {
        // A helper service with no [services.sidecar] table is
        // unsupervised — must NOT show up in the sidecars map.
        let m = PluginManifest::parse(
            r#"
[plugin]
id = "p-helper"
name = "P"
version = "0.1.0"

[[services]]
name = "ocr-worker"
image = "x"
"#,
        )
        .unwrap();
        let reg = HookRegistry::new();
        reg.enable(&m).unwrap();
        assert!(reg.sidecar("ocr-worker").is_none());
        assert_eq!(reg.all_sidecars().len(), 0);
    }

    #[test]
    fn ffmpeg_or_ocr_sidecar_registers_just_like_a_transport_one() {
        // The simplification's whole point: the registry doesn't
        // care that signal is a transport. A plugin shipping an
        // ffmpeg pool with [services.sidecar] gets the same
        // treatment.
        let reg = HookRegistry::new();
        reg.enable(&manifest_with_sidecar("p-ffmpeg", "ffmpeg-pool", 7000))
            .unwrap();
        let b = reg.sidecar("ffmpeg-pool").unwrap();
        assert_eq!(b.name, "ffmpeg-pool");
        assert_eq!(b.rpc_port, 7000);
    }

    /// All-or-nothing atomicity: if ONE tool from a manifest conflicts
    /// with an existing registration, NONE of its siblings may land in
    /// the registry. A leaked unique tool here would be a trust-class
    /// bypass — a plugin whose install failed could still have tools
    /// callable by the agent.
    #[test]
    fn partial_conflict_leaves_registry_clean() {
        let reg = HookRegistry::new();
        reg.enable(&manifest_with_tools("p1", &["shared"])).unwrap();

        // p2 wants to register both "shared" (conflict) AND "unique"
        // (fine). The whole enable must fail, and "unique" must not
        // leak into the registry.
        let err = reg
            .enable(&manifest_with_tools("p2", &["unique", "shared"]))
            .unwrap_err();
        assert!(err.contains("shared"));
        assert!(
            reg.tool("unique").is_none(),
            "partial-install left a leaked tool in the registry"
        );
        assert!(!reg.is_enabled("p2"));
    }

    /// Transport IDs are singleton across plugins — two plugins cannot
    /// both claim `transport_id = "signal"`.
    #[test]
    fn transport_id_conflict_rejected() {
        let m1 = r#"[plugin]
id = "p1"
name = "p1"
version = "1.0.0"

[transport]
transport_id = "signal"
supports_attachments = true
supports_groups = false
"#;
        let m2 = r#"[plugin]
id = "p2"
name = "p2"
version = "1.0.0"

[transport]
transport_id = "signal"
supports_attachments = false
supports_groups = false
"#;
        let reg = HookRegistry::new();
        reg.enable(&PluginManifest::parse(m1).unwrap()).unwrap();
        let err = reg.enable(&PluginManifest::parse(m2).unwrap()).unwrap_err();
        assert!(err.contains("transport id 'signal'"));
        assert!(
            !reg.is_enabled("p2"),
            "p2 must not be marked enabled when its transport clashed"
        );
        // p1's transport is still registered correctly.
        assert_eq!(reg.transport("signal").unwrap().plugin_id, "p1");
    }

    /// UI-panel mount paths are also singleton — two plugins cannot
    /// both claim `/plugins/whatever`.
    #[test]
    fn ui_panel_mount_conflict_rejected() {
        let m1 = r#"[plugin]
id = "p1"
name = "p1"
version = "1.0.0"

[[ui_panels]]
mount = "/plugins/thing"
entry = "index.js"
"#;
        let m2 = r#"[plugin]
id = "p2"
name = "p2"
version = "1.0.0"

[[ui_panels]]
mount = "/plugins/thing"
entry = "other.js"
"#;
        let reg = HookRegistry::new();
        reg.enable(&PluginManifest::parse(m1).unwrap()).unwrap();
        let err = reg.enable(&PluginManifest::parse(m2).unwrap()).unwrap_err();
        assert!(err.contains("ui_panel mount"));
        assert_eq!(reg.ui_panels().len(), 1);
    }

    /// Event subscriptions from multiple plugins coexist, and
    /// `subscribers_for` returns them all.
    #[test]
    fn multiple_plugins_can_subscribe_to_same_event_kind() {
        let m1 = r#"[plugin]
id = "p1"
name = "p1"
version = "1.0.0"

[[event_subscriptions]]
on = "conversation.message_inbound"
handler = "handle_p1"
"#;
        let m2 = r#"[plugin]
id = "p2"
name = "p2"
version = "1.0.0"

[[event_subscriptions]]
on = "conversation.message_inbound"
handler = "handle_p2"
"#;
        let reg = HookRegistry::new();
        reg.enable(&PluginManifest::parse(m1).unwrap()).unwrap();
        reg.enable(&PluginManifest::parse(m2).unwrap()).unwrap();
        let subs = reg.subscribers_for("conversation.message_inbound");
        assert_eq!(subs.len(), 2);
        let plugin_ids: Vec<&str> = subs.iter().map(|s| s.plugin_id.as_str()).collect();
        assert!(plugin_ids.contains(&"p1"));
        assert!(plugin_ids.contains(&"p2"));
    }

    /// Disabling one of two plugins that share an event kind must leave
    /// the other's subscription intact.
    #[test]
    fn disable_preserves_other_plugins_subscriptions() {
        let m1 = r#"[plugin]
id = "p1"
name = "p1"
version = "1.0.0"

[[event_subscriptions]]
on = "x"
handler = "h"
"#;
        let m2 = r#"[plugin]
id = "p2"
name = "p2"
version = "1.0.0"

[[event_subscriptions]]
on = "x"
handler = "h"
"#;
        let reg = HookRegistry::new();
        reg.enable(&PluginManifest::parse(m1).unwrap()).unwrap();
        reg.enable(&PluginManifest::parse(m2).unwrap()).unwrap();
        reg.disable("p1");
        let subs = reg.subscribers_for("x");
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0].plugin_id, "p2");
    }

    /// Identity providers are keyed by plugin — enable+disable is clean.
    #[test]
    fn identity_provider_registration_and_removal() {
        let m = r#"[plugin]
id = "idp-google"
name = "idp"
version = "1.0.0"

[identity_provider]
resolves = ["email", "phone"]
trust_hint_default = "Contact"
"#;
        let reg = HookRegistry::new();
        reg.enable(&PluginManifest::parse(m).unwrap()).unwrap();
        assert_eq!(reg.identity_providers().len(), 1);
        assert_eq!(reg.identity_providers()[0].resolves.len(), 2);
        reg.disable("idp-google");
        assert!(reg.identity_providers().is_empty());
    }

    // -------- Built-in tool registration tests -------------------

    use async_trait::async_trait;
    use execlaw_core::tool::{
        ToolCtx, ToolDescriptor, ToolImpl, ToolLatency, ToolOutcome, ToolSource,
    };

    fn dummy_descriptor(name: &str) -> ToolDescriptor {
        ToolDescriptor {
            name: name.into(),
            description: "test".into(),
            schema: serde_json::json!({"type": "object"}),
            source: ToolSource::Builtin,
            latency: ToolLatency::Low,
            capabilities: vec![],
            default_allowed_classes: vec!["Controller".into()],
            sensitive: false,
        }
    }

    struct DummyTool {
        d: ToolDescriptor,
    }
    #[async_trait]
    impl ToolImpl for DummyTool {
        fn descriptor(&self) -> &ToolDescriptor {
            &self.d
        }
        async fn invoke(&self, _ctx: ToolCtx, _args: serde_json::Value) -> ToolOutcome {
            ToolOutcome::ok(serde_json::json!({"ran": self.d.name}))
        }
    }

    fn arc_dummy(name: &str) -> Arc<dyn ToolImpl> {
        Arc::new(DummyTool {
            d: dummy_descriptor(name),
        })
    }

    #[test]
    fn register_builtin_then_lookup_returns_same_tool() {
        let reg = HookRegistry::new();
        reg.register_builtin(arc_dummy("read_memory")).unwrap();
        let got = reg.builtin("read_memory").unwrap();
        assert_eq!(got.descriptor().name, "read_memory");
    }

    #[test]
    fn register_builtin_rejects_duplicate_name() {
        let reg = HookRegistry::new();
        reg.register_builtin(arc_dummy("foo")).unwrap();
        let err = reg.register_builtin(arc_dummy("foo")).unwrap_err();
        assert!(err.contains("already registered"));
    }

    #[test]
    fn register_builtin_rejects_invalid_or_networked_schema() {
        let reg = HookRegistry::new();
        let mut descriptor = dummy_descriptor("bad");
        descriptor.schema = serde_json::json!({"type": 7});
        let error = reg
            .register_builtin(Arc::new(DummyTool { d: descriptor }))
            .unwrap_err();
        assert!(error.contains("Draft 2020-12"));

        let mut descriptor = dummy_descriptor("networked");
        descriptor.schema = serde_json::json!({"$ref": "https://example.invalid/schema"});
        let error = reg
            .register_builtin(Arc::new(DummyTool { d: descriptor }))
            .unwrap_err();
        assert!(error.contains("forbidden network reference"));
    }

    #[test]
    fn builtins_validate_input_before_invoke() {
        let reg = HookRegistry::new();
        let mut descriptor = dummy_descriptor("typed");
        descriptor.schema = serde_json::json!({
            "type": "object",
            "properties": {"count": {"type": "integer"}},
            "required": ["count"]
        });
        reg.register_builtin(Arc::new(DummyTool { d: descriptor }))
            .unwrap();
        assert!(
            reg.validate_builtin_input("typed", &serde_json::json!({"count": 1}))
                .is_ok()
        );
        assert!(
            reg.validate_builtin_input("typed", &serde_json::json!({"count": "one"}))
                .is_err()
        );
        assert_eq!(reg.builtin_schema_hash("typed").unwrap().len(), 64);
    }

    /// Critical: a plugin must NOT be able to shadow a built-in by
    /// claiming the same tool name. The registry rejects the plugin
    /// install and the built-in continues to win.
    #[test]
    fn plugin_cannot_shadow_existing_builtin() {
        let reg = HookRegistry::new();
        reg.register_builtin(arc_dummy("read_memory")).unwrap();
        let manifest = manifest_with_tools("rogue", &["read_memory"]);
        let err = reg.enable(&manifest).unwrap_err();
        assert!(err.contains("already registered as a built-in"));
        assert!(reg.builtin("read_memory").is_some());
        assert!(reg.tool("read_memory").is_none());
        assert!(!reg.is_enabled("rogue"));
    }

    /// Phase 3: tools marked `host_implemented = true` in the
    /// manifest are deliberately exempt from the
    /// no-shadowing-a-builtin check — the builtin IS the intended
    /// implementation, the manifest entry just attributes it to
    /// the plugin in catalog UIs. Registering a builtin first then
    /// enabling the plugin must succeed without conflict.
    #[test]
    fn host_implemented_tool_does_not_conflict_with_builtin() {
        let reg = HookRegistry::new();
        // Builtin landed first — happens at boot before plugin
        // hydrate.
        reg.register_builtin(arc_dummy("signal.send_message"))
            .unwrap();
        // Plugin manifest declares the same name as host-implemented.
        let manifest = PluginManifest::parse(
            r#"
[plugin]
id = "signal"
name = "Signal"
version = "0.1.0"

[[tools]]
name = "signal.send_message"
host_implemented = true
latency = "low"
"#,
        )
        .unwrap();
        reg.enable(&manifest)
            .expect("host-implemented tool must not conflict with builtin");
        // Builtin still wins on lookup; the host-implemented entry
        // does NOT pollute tools_by_name.
        assert!(reg.builtin("signal.send_message").is_some());
        assert!(reg.tool("signal.send_message").is_none());
        assert!(reg.is_enabled("signal"));
    }

    /// Reverse order: plugin enabled first (host_implemented=true),
    /// then builtin registered. The builtin must still register
    /// cleanly because the plugin entry never landed in tools_by_name.
    #[test]
    fn host_implemented_does_not_block_subsequent_builtin_registration() {
        let reg = HookRegistry::new();
        let manifest = PluginManifest::parse(
            r#"
[plugin]
id = "signal"
name = "Signal"
version = "0.1.0"

[[tools]]
name = "signal.send_message"
host_implemented = true
latency = "low"
"#,
        )
        .unwrap();
        reg.enable(&manifest).unwrap();
        // Now register the actual builtin — must succeed.
        reg.register_builtin(arc_dummy("signal.send_message"))
            .expect("builtin must land cleanly when plugin entry was host_implemented");
    }

    /// Symmetric guard: registering a built-in whose name is already
    /// owned by an enabled plugin must fail rather than silently
    /// taking precedence at lookup time.
    #[test]
    fn builtin_cannot_shadow_existing_plugin_tool() {
        let reg = HookRegistry::new();
        reg.enable(&manifest_with_tools("p1", &["overlap"]))
            .unwrap();
        let err = reg.register_builtin(arc_dummy("overlap")).unwrap_err();
        assert!(err.contains("would shadow plugin 'p1'"));
        assert!(reg.builtin("overlap").is_none());
    }

    #[test]
    fn disable_does_not_remove_builtins() {
        let reg = HookRegistry::new();
        reg.register_builtin(arc_dummy("read_memory")).unwrap();
        reg.enable(&manifest_with_tools("p1", &["a"])).unwrap();
        reg.disable("p1");
        assert!(reg.tool("a").is_none());
        // The built-in stays.
        assert!(reg.builtin("read_memory").is_some());
    }

    #[test]
    fn all_builtins_returns_every_registered() {
        let reg = HookRegistry::new();
        reg.register_builtin(arc_dummy("a")).unwrap();
        reg.register_builtin(arc_dummy("b")).unwrap();
        reg.register_builtin(arc_dummy("c")).unwrap();
        let names: Vec<String> = reg
            .all_builtins()
            .iter()
            .map(|t| t.descriptor().name.clone())
            .collect();
        assert_eq!(names.len(), 3);
        assert!(names.contains(&"a".to_string()));
        assert!(names.contains(&"b".to_string()));
        assert!(names.contains(&"c".to_string()));
    }

    #[test]
    fn lookup_any_returns_builtin_for_builtin_name() {
        let reg = HookRegistry::new();
        reg.register_builtin(arc_dummy("read_memory")).unwrap();
        let got = reg.lookup_any("read_memory").unwrap();
        assert!(got.is_builtin());
        assert_eq!(got.name(), "read_memory");
    }

    #[test]
    fn lookup_any_returns_plugin_for_plugin_name() {
        let reg = HookRegistry::new();
        reg.enable(&manifest_with_tools("p1", &["plugin_tool"]))
            .unwrap();
        let got = reg.lookup_any("plugin_tool").unwrap();
        assert!(!got.is_builtin());
        assert_eq!(got.name(), "plugin_tool");
    }

    #[test]
    fn lookup_any_returns_none_for_unknown() {
        let reg = HookRegistry::new();
        assert!(reg.lookup_any("nonexistent").is_none());
    }
}
