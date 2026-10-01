//! execlaw-inference-api
//!
//! The single internal contract for LLM access. Anything in the workspace
//! that wants to talk to a model goes through this crate, which speaks an
//! **OpenAI-compatible API** (§2.8, §3.2 of MIGRATION_PLAN.md).
//!
//! **No cloud-vendor SDKs. Ever.** Not `anthropic-sdk`, not `openai`, not
//! `google-genai`. The endpoint this client talks to is always a local
//! inference server — vLLM (default: `QuantTrio/Qwen3.5-27B-AWQ`), OpenArc,
//! llama.cpp server, Ollama. This rule is invariant (§0 axiom #1,
//! 2026-04-23 locked decisions).

#![forbid(unsafe_code)]

mod ollama;

use execlaw_local_endpoint_policy::{
    EndpointResolution, LocalEndpointPolicy, PolicyError, Resolver, SystemResolver,
    normalize_mapped_ip,
};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::sync::Arc;
use thiserror::Error;

/// RAII reservation held until a request completes or a stream is dropped.
pub trait InferenceAdmissionPermit: Send + Sync {}

impl<T: Send + Sync> InferenceAdmissionPermit for T {}

/// Workload-aware admission provider injected by the host scheduler.
#[async_trait::async_trait]
pub trait InferenceRequestAdmission: Send + Sync {
    /// Reserve one slot for this model/workload pair.
    async fn acquire(
        &self,
        model: &str,
        workload: &str,
        budget_scope: Option<&str>,
    ) -> Result<Box<dyn InferenceAdmissionPermit>, String>;
}

#[derive(Clone)]
struct AdmissionBinding {
    provider: Arc<dyn InferenceRequestAdmission>,
    workload: &'static str,
    budget_scope: Option<Arc<str>>,
}

impl fmt::Debug for AdmissionBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AdmissionBinding")
            .field("workload", &self.workload)
            .finish_non_exhaustive()
    }
}

/// Which wire protocol an [`InferenceClient`] speaks. The default —
/// `OpenAICompat` — works for vLLM / llama-server / OpenArc / the
/// vast majority of self-hosted endpoints. `Ollama` switches the
/// client to Ollama's native `/api/chat` endpoint because the
/// daemon's `/v1/chat/completions` shim has been observed to drop
/// `tool_calls` on small models — the agent would see plain
/// `content` text like `(web_search "…")` instead of a structured
/// call. See `crates/inference-api/src/ollama.rs` for the
/// translation layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InferenceEngine {
    /// vLLM, llama-server, OpenArc, or anything else that speaks
    /// OpenAI's `/v1/chat/completions`. Default.
    #[default]
    OpenAICompat,
    /// Native Ollama daemon. Same URL, different path
    /// (`/api/chat` instead of `/v1/chat/completions`); the
    /// translation happens inside the client.
    Ollama,
}

// ---------------------------------------------------------------------------
// Model + chat types (OpenAI function-calling schema)
// ---------------------------------------------------------------------------

/// Model identifier as understood by the configured backend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelId(pub String);

impl ModelId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// Body of a chat message. OpenAI's chat schema accepts EITHER a plain
/// string OR an array of typed content parts (text + image_url for
/// vision-enabled models like Qwen3-VL / Qwen3.6 / LLaVA / Pixtral).
/// The untagged enum serialises to whichever shape matches the input
/// so existing text-only call sites are byte-identical on the wire.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    Parts(Vec<ContentPart>),
}

impl MessageContent {
    /// Extract the plain-text portion for logging / history projection.
    /// Concatenates the text parts of a parts-array with newlines; an
    /// image-only message returns the empty string.
    pub fn as_text(&self) -> String {
        match self {
            MessageContent::Text(s) => s.clone(),
            MessageContent::Parts(parts) => parts
                .iter()
                .filter_map(|p| match p {
                    ContentPart::Text { text } => Some(text.as_str()),
                    ContentPart::ImageUrl { .. } => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
        }
    }
}

/// One typed content part inside a parts-array `MessageContent`. The
/// `image_url` variant follows OpenAI's vision schema verbatim — Qwen
/// VL, LLaVA, Llama-3.2-Vision, Pixtral, and Phi-3.5-Vision all accept
/// it via the OpenAI-compatible bridge.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    Text { text: String },
    ImageUrl { image_url: ImageUrl },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageUrl {
    /// Either an https:// URL or a `data:image/<mime>;base64,<bytes>`
    /// data URL. execlaw uses the data-URL form so the inference
    /// backend doesn't need network access to fetch attachments.
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: Role,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<MessageContent>,
    /// Some local OpenAI-compatible reasoning backends place their
    /// response text here while leaving `content` empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Set by the assistant when the model chose to call tools.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: Some(MessageContent::Text(content.into())),
            reasoning_content: None,
            tool_call_id: None,
            name: None,
            tool_calls: Vec::new(),
        }
    }
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: Some(MessageContent::Text(content.into())),
            reasoning_content: None,
            tool_call_id: None,
            name: None,
            tool_calls: Vec::new(),
        }
    }
    /// Build a user message with attached images. Each `images` entry
    /// is a data URL (e.g. `data:image/png;base64,...`). The text is
    /// emitted as the first content part; images follow in the order
    /// provided. An empty `text` is allowed (image-only message).
    pub fn user_with_images(
        text: impl Into<String>,
        images: impl IntoIterator<Item = String>,
    ) -> Self {
        let mut parts: Vec<ContentPart> = Vec::new();
        let text = text.into();
        if !text.is_empty() {
            parts.push(ContentPart::Text { text });
        }
        for url in images {
            parts.push(ContentPart::ImageUrl {
                image_url: ImageUrl { url },
            });
        }
        Self {
            role: Role::User,
            content: Some(MessageContent::Parts(parts)),
            reasoning_content: None,
            tool_call_id: None,
            name: None,
            tool_calls: Vec::new(),
        }
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: Some(MessageContent::Text(content.into())),
            reasoning_content: None,
            tool_call_id: None,
            name: None,
            tool_calls: Vec::new(),
        }
    }
    pub fn tool_result(tool_call_id: impl Into<String>, result_json: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: Some(MessageContent::Text(result_json.into())),
            reasoning_content: None,
            tool_call_id: Some(tool_call_id.into()),
            name: None,
            tool_calls: Vec::new(),
        }
    }
}

/// A tool call emitted by the model (assistant role).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String, // always "function"
    pub function: ToolCallFunction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallFunction {
    pub name: String,
    /// JSON-encoded arguments as a string per the OpenAI spec.
    pub arguments: String,
}

/// A tool the agent exposes to the model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDeclaration {
    #[serde(rename = "type")]
    pub kind: String, // always "function"
    pub function: FunctionDecl,
}

impl ToolDeclaration {
    pub fn function(
        name: impl Into<String>,
        description: impl Into<String>,
        params: serde_json::Value,
    ) -> Self {
        Self {
            kind: "function".into(),
            function: FunctionDecl {
                name: name.into(),
                description: description.into(),
                parameters: params,
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionDecl {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value, // JSON Schema
}

/// A `/v1/chat/completions` request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatRequest {
    pub model: ModelId,
    pub messages: Vec<ChatMessage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<ToolDeclaration>>,
    #[serde(default)]
    pub stream: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    /// 2026-04-28 — vLLM-extension knob, forwarded verbatim into the
    /// chat-template render via the `chat_template_kwargs` field on
    /// the OpenAI-compatible POST body. Qwen3.5 honours
    /// `{"enable_thinking": false}` to suppress its native `<think>`
    /// blocks; without it the local model emits a "Thinking Process:"
    /// monologue ahead of every reply. Other models silently ignore
    /// the field, so passing it unconditionally is safe.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_template_kwargs: Option<serde_json::Value>,
    /// 2026-05-16 — explicit `tool_choice` so vLLM's
    /// auto-tool-choice path engages on every tool-bearing request.
    /// Per OpenAI spec the default when `tools` is set is `"auto"`,
    /// but passing it explicitly works around vLLM versions where
    /// omission falls into a no-tools fast path. Accepts every
    /// shape OpenAI does: `"auto"`, `"none"`, `"required"`, or
    /// `{"type":"function","function":{"name":"x"}}` — typed as
    /// `Value` to keep the wire shape flexible.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<serde_json::Value>,
    /// Optional OpenAI structured-output contract. Backends receive this only
    /// after the exact model/template/backend profile has been qualified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_format: Option<serde_json::Value>,
    /// 2026-05-16 — vLLM-extension knob. When set, vLLM uses the
    /// named backend (`"outlines"`, `"lm-format-enforcer"`,
    /// `"xgrammar"`) to grammar-constrain decoding for any
    /// `guided_*` field on this request. On vLLM ≥ 0.7 with
    /// `--enable-auto-tool-choice`, this engages schema-constrained
    /// decoding on `tools.function.parameters` for the selected
    /// tool so `function.arguments` is guaranteed to be valid JSON
    /// matching the schema — the failure class that produced
    /// Signal-channel chart 400s. Older vLLM versions ignore the
    /// field. Always passing `"outlines"` for tool-bearing requests
    /// is safe.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guided_decoding_backend: Option<String>,
}

/// `GET /v1/models` response shape (OpenAI list endpoint). vLLM,
/// llama.cpp server, Ollama, OpenArc all return this envelope; the
/// only field the SPA reads is `data[].id`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelListResponse {
    #[serde(default)]
    pub object: Option<String>,
    #[serde(default)]
    pub data: Vec<ModelEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelEntry {
    pub id: String,
    #[serde(default)]
    pub object: Option<String>,
    #[serde(default)]
    pub owned_by: Option<String>,
}

/// Heuristic check — does the given model id name a known
/// multimodal (vision-capable) family? Returns true on a match
/// against the curated pattern set; false otherwise.
///
/// Curated list (case-insensitive substring match):
///   * Qwen vision: `qwen2-vl`, `qwen2.5-vl`, `qwen3-vl`, `qwen3.6`
///   * LLaVA: `llava`, `llava-onevision`
///   * Llama 3.2 Vision: `llama-3.2-11b-vision`, `llama-3.2-90b-vision`
///   * Pixtral: `pixtral`
///   * Phi-3.5/Phi-4 vision: `phi-3.5-vision`, `phi-4-multimodal`
///   * MiniCPM-V: `minicpm-v`
///   * InternVL: `internvl`
///   * Generic suffixes operators commonly use: `-vision`, `-vl`,
///     `-multimodal`, `-mm`
///
/// New families land here as they ship. The probe is heuristic by
/// design — vLLM / llama.cpp's /v1/models response doesn't carry an
/// explicit multimodal flag, so id-pattern matching is the most
/// reliable signal available short of an actual image probe (which
/// would cost a real inference round-trip).
pub fn is_known_multimodal_model(model_id: &str) -> bool {
    let id = model_id.to_lowercase();
    const PATTERNS: &[&str] = &[
        // Qwen
        "qwen2-vl",
        "qwen2.5-vl",
        "qwen2_5-vl",
        "qwen3-vl",
        "qwen3.5-vl",
        "qwen3_5-vl",
        "qwen3.6",
        "qwen3_6",
        // LLaVA
        "llava",
        // Llama 3.2 vision
        "llama-3.2-11b-vision",
        "llama-3.2-90b-vision",
        "llama-3-vision",
        // Pixtral
        "pixtral",
        // Phi vision
        "phi-3.5-vision",
        "phi-3-vision",
        "phi-4-multimodal",
        // MiniCPM-V
        "minicpm-v",
        // InternVL
        "internvl",
        // Generic suffixes
        "-vision",
        "-multimodal",
    ];
    for p in PATTERNS {
        if id.contains(p) {
            return true;
        }
    }
    // Cheaper standalone token matches that benefit from word-boundary
    // checks to avoid false positives like "qwen2-7b" → matching "vl"
    // anywhere. Use suffix/segment guards.
    for tail in ["-vl", "_vl", "-mm"] {
        if id.ends_with(tail)
            || id.contains(&format!("{tail}-"))
            || id.contains(&format!("{tail}_"))
        {
            return true;
        }
    }
    false
}

/// Non-streaming response shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatResponse {
    pub id: String,
    pub model: String,
    pub choices: Vec<Choice>,
    #[serde(default)]
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Choice {
    pub index: u32,
    pub message: ChatMessage,
    #[serde(default)]
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub prompt_tokens: u32,
    #[serde(default)]
    pub completion_tokens: u32,
    #[serde(default)]
    pub total_tokens: u32,
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum InferenceError {
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("decode error: {0}")]
    Decode(String),
    #[error("backend returned status {status}: {body}")]
    BadStatus { status: u16, body: String },
    #[error("request timed out")]
    Timeout,
    #[error("inference request cancelled")]
    Cancelled,
    #[error("inference admission failed: {0}")]
    Admission(String),
    #[error("streaming response ended before its terminal marker")]
    IncompleteStream,
    #[error("SSE event exceeded the {0}-byte frame limit")]
    FrameTooLarge(usize),
    #[error("local endpoint policy rejected the inference endpoint: {0}")]
    EndpointPolicy(String),
    #[error("could not persist inference retry metadata: {0}")]
    AttemptTracking(String),
}

impl InferenceError {
    /// Stable error category safe for operational logs (never includes upstream bodies).
    pub fn safe_class(&self) -> &'static str {
        match self {
            Self::Http(error) if error.is_timeout() => "http_timeout",
            Self::Http(error) if error.is_connect() => "http_connect",
            Self::Http(_) => "http_transport",
            Self::Decode(_) => "decode",
            Self::BadStatus { status: 429, .. } => "http_429",
            Self::BadStatus { status: 502, .. } => "http_502",
            Self::BadStatus { status: 503, .. } => "http_503",
            Self::BadStatus { status: 504, .. } => "http_504",
            Self::BadStatus { .. } => "http_status",
            Self::Timeout => "timeout",
            Self::Cancelled => "cancelled",
            Self::Admission(_) => "admission",
            Self::IncompleteStream => "incomplete_stream",
            Self::FrameTooLarge(_) => "frame_too_large",
            Self::EndpointPolicy(_) => "endpoint_policy",
            Self::AttemptTracking(_) => "attempt_tracking",
        }
    }

    /// Only transient local failures may be retried; invalid requests and
    /// endpoint-policy failures must be fixed rather than replayed.
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Http(error) => error.is_timeout() || error.is_connect(),
            Self::BadStatus { status, .. } => matches!(status, 429 | 502 | 503 | 504),
            Self::Timeout => true,
            Self::Cancelled => false,
            Self::Decode(_)
            | Self::IncompleteStream
            | Self::FrameTooLarge(_)
            | Self::Admission(_)
            | Self::EndpointPolicy(_)
            | Self::AttemptTracking(_) => false,
        }
    }
}

/// Bounds retrying a local inference request.
///
/// `max_attempts` includes the initial request. Retry is restricted to
/// [`InferenceError::is_retryable`] failures and stops when `deadline` expires.
#[derive(Debug, Clone)]
pub struct InferenceRetryPolicy {
    pub max_attempts: u32,
    pub deadline: std::time::Duration,
    pub initial_backoff: std::time::Duration,
}

impl Default for InferenceRetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 2,
            deadline: std::time::Duration::from_secs(30),
            initial_backoff: std::time::Duration::from_millis(200),
        }
    }
}

impl InferenceRetryPolicy {
    /// Choose a bounded deadline for the selected local protocol. A cold
    /// native Ollama model can take longer to load than a warm vLLM request.
    ///
    /// ```
    /// use execlaw_inference_api::{InferenceEngine, InferenceRetryPolicy};
    /// assert!(InferenceRetryPolicy::for_engine(InferenceEngine::Ollama).deadline
    ///     > InferenceRetryPolicy::for_engine(InferenceEngine::OpenAICompat).deadline);
    /// ```
    pub fn for_engine(engine: InferenceEngine) -> Self {
        let mut policy = Self::default();
        if engine == InferenceEngine::Ollama {
            policy.deadline = std::time::Duration::from_secs(120);
        }
        policy
    }
}

/// Construct the reqwest client every `InferenceClient::new` uses.
/// Centralised so the timeout / pool / keepalive knobs that bit
/// operators in production stay in one obvious place.
fn configure_inference_http_client(builder: reqwest::ClientBuilder) -> reqwest::ClientBuilder {
    builder
        .connect_timeout(std::time::Duration::from_secs(10))
        .pool_idle_timeout(std::time::Duration::from_secs(15))
        .read_timeout(std::time::Duration::from_secs(120))
        .tcp_keepalive(std::time::Duration::from_secs(30))
        // 2026-05-16 — pin HTTP/1.1. vLLM v0.20+ negotiates HTTP/2
        // by default; on long-lived SSE streams (multi-second
        // decode windows of a single tool-bearing chat completion)
        // the HTTP/2 flow-control window deadlocked between the
        // runner container's hyper client and uvicorn's h2 server.
        // The smoking-gun trace: vLLM happily generated at 31
        // tok/s with `Running: 1 reqs, KV cache 12%` for 120s
        // while the runner received the first 5 SSE chunks and
        // then nothing — classic stalled-window symptom. HTTP/1.1
        // uses chunked transfer encoding for SSE and has no
        // window mechanism to deadlock; it's the historical
        // default for SSE streaming and the safer floor for our
        // self-hosted-vLLM topology. If a future backend
        // *requires* HTTP/2 (e.g. multiplexed gRPC over HTTP/2 on
        // a different port), that backend should construct its
        // own client; the LLM streaming path stays on 1.1.
        .http1_only()
}

fn trusted_runner_endpoint(
    raw_url: &str,
    approved_addresses: &[std::net::IpAddr],
    gateway_host: Option<&str>,
    resolver: &dyn Resolver,
) -> Result<(url::Url, Vec<std::net::IpAddr>), PolicyError> {
    use std::collections::BTreeSet;
    use std::net::IpAddr;
    use url::Host;

    let mut url =
        url::Url::parse(raw_url).map_err(|error| PolicyError::InvalidUrl(error.to_string()))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(PolicyError::Scheme);
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(PolicyError::Userinfo);
    }
    let port = url
        .port_or_known_default()
        .ok_or_else(|| PolicyError::InvalidUrl("missing port".into()))?;
    let host = url
        .host()
        .ok_or_else(|| PolicyError::InvalidUrl("missing host".into()))?;

    let addresses = if let Some(gateway_host) = gateway_host {
        if url.host_str() != Some(gateway_host) || !matches!(host, Host::Domain(_)) {
            return Err(PolicyError::InvalidUrl(
                "runner gateway authorization does not match the inference URL host".into(),
            ));
        }
        let resolved = resolver
            .resolve(gateway_host, port)
            .map_err(|error| PolicyError::Resolution {
                host: gateway_host.to_owned(),
                message: error.to_string(),
            })?
            .into_iter()
            .map(|socket| normalize_mapped_ip(socket.ip()))
            .collect::<BTreeSet<_>>();
        if resolved.is_empty() {
            return Err(PolicyError::EmptyResolution(gateway_host.to_owned()));
        }
        for address in &resolved {
            if !is_private_runner_gateway(*address) {
                return Err(PolicyError::UnapprovedAddress(*address));
            }
        }
        resolved.into_iter().collect::<Vec<_>>()
    } else {
        let approved = approved_addresses
            .iter()
            .copied()
            .map(normalize_mapped_ip)
            .collect::<BTreeSet<_>>();
        if approved.is_empty() {
            return Err(PolicyError::EmptyResolution(
                url.host_str().unwrap_or_default().to_owned(),
            ));
        }
        let resolved = match host {
            Host::Ipv4(address) => vec![IpAddr::V4(address)],
            Host::Ipv6(address) => vec![normalize_mapped_ip(IpAddr::V6(address))],
            Host::Domain(name) => resolver
                .resolve(name, port)
                .map_err(|error| PolicyError::Resolution {
                    host: name.to_owned(),
                    message: error.to_string(),
                })?
                .into_iter()
                .map(|socket| normalize_mapped_ip(socket.ip()))
                .collect(),
        }
        .into_iter()
        .collect::<BTreeSet<_>>();
        if resolved.is_empty() {
            return Err(PolicyError::EmptyResolution(
                url.host_str().unwrap_or_default().to_owned(),
            ));
        }
        for address in &resolved {
            if !approved.contains(address) {
                return Err(PolicyError::UnapprovedAddress(*address));
            }
            if address.is_loopback() {
                return Err(PolicyError::UnapprovedAddress(*address));
            }
        }
        resolved.into_iter().collect::<Vec<_>>()
    };

    if let Some(Host::Ipv6(address)) = url.host()
        && let Some(mapped) = address.to_ipv4_mapped()
    {
        url.set_host(Some(&mapped.to_string()))
            .map_err(|error| PolicyError::InvalidUrl(error.to_string()))?;
    }
    Ok((url, addresses))
}

fn is_private_runner_gateway(address: std::net::IpAddr) -> bool {
    match normalize_mapped_ip(address) {
        std::net::IpAddr::V4(address) => address.is_private() || address.is_link_local(),
        std::net::IpAddr::V6(address) => {
            (address.octets()[0] & 0xfe) == 0xfc
                || (address.octets()[0] == 0xfe && (address.octets()[1] & 0xc0) == 0x80)
        }
    }
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

/// An OpenAI-compatible inference client. Points at a LOCAL endpoint —
/// never a cloud provider. Typical `base_url` values:
///
/// - `http://127.0.0.1:8000/v1` — vLLM (default for execlaw's nvidia path)
/// - `http://127.0.0.1:8793/v1` — OpenArc (Intel GPU, used for voice stack)
/// - `http://127.0.0.1:11434/v1` — Ollama
#[derive(Debug, Clone)]
pub struct InferenceClient {
    pub base_url: String,
    pub api_key: Option<String>,
    /// Optional OpenAI-compatible reasoning control for the configured model.
    /// It is sent on both streaming and non-streaming requests.
    reasoning_effort: Option<String>,
    /// Native Ollama request context, when the backend declares an override.
    ollama_context_tokens: Option<u32>,
    /// Wire protocol the client speaks. `OpenAICompat` is the
    /// default; callers (typically `inference_resolver` in the
    /// server crate) flip to `Ollama` for Apple-Silicon backends
    /// where the OpenAI-compat shim's tool-call extraction is
    /// unreliable.
    pub engine: InferenceEngine,
    http: reqwest::Client,
    endpoint_resolution: Option<EndpointResolution>,
    endpoint_policy_error: Option<String>,
    admission: Option<AdmissionBinding>,
}

impl InferenceClient {
    pub fn new(base_url: impl Into<String>) -> Self {
        let base_url = base_url.into();
        match Self::new_with_policy(base_url.clone(), &LocalEndpointPolicy::loopback_only()) {
            Ok(client) => client,
            Err(error) => Self {
                base_url,
                api_key: None,
                reasoning_effort: None,
                ollama_context_tokens: None,
                engine: InferenceEngine::default(),
                http: configure_inference_http_client(reqwest::Client::builder())
                    .redirect(reqwest::redirect::Policy::none())
                    .build()
                    .expect("reqwest client build"),
                endpoint_resolution: None,
                endpoint_policy_error: Some(error.to_string()),
                admission: None,
            },
        }
    }

    /// Construct a client inside a runner container.
    ///
    /// The control plane validates and pins the endpoint before placing it
    /// in the authenticated [`TurnRequest`]. Runner containers do not mount
    /// the control-plane database, so re-applying the host's SQLite endpoint
    /// approvals here would reject valid Docker-host endpoints such as
    /// `host.docker.internal`. The supervisor boundary is the authorization
    /// boundary for this constructor; callers must never expose it directly
    /// to untrusted input.
    pub fn new_for_trusted_runner(
        base_url: impl Into<String>,
        approved_addresses: &[std::net::IpAddr],
        gateway_host: Option<&str>,
    ) -> Result<Self, PolicyError> {
        let base_url = base_url.into();
        let (url, addresses) =
            trusted_runner_endpoint(&base_url, approved_addresses, gateway_host, &SystemResolver)?;
        let host = url.host_str().expect("validated runner inference host");
        let port = url
            .port_or_known_default()
            .expect("validated runner inference port");
        let sockets = addresses
            .iter()
            .map(|address| std::net::SocketAddr::new(*address, port))
            .collect::<Vec<_>>();
        let mut builder = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none());
        if matches!(url.host(), Some(url::Host::Domain(_))) {
            builder = builder.resolve_to_addrs(host, &sockets);
        }
        let http = configure_inference_http_client(builder)
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| PolicyError::Client(error.to_string()))?;
        Ok(Self {
            base_url: url.to_string(),
            api_key: None,
            reasoning_effort: None,
            ollama_context_tokens: None,
            engine: InferenceEngine::default(),
            http,
            endpoint_resolution: None,
            endpoint_policy_error: None,
            admission: None,
        })
    }

    /// Construct an inference client under an operator-loaded local endpoint
    /// policy. DNS answers are validated once and pinned into reqwest so a
    /// later DNS response cannot rebind an established client to a public IP.
    pub fn new_with_policy(
        base_url: impl Into<String>,
        policy: &LocalEndpointPolicy,
    ) -> Result<Self, PolicyError> {
        let base_url = base_url.into();
        let resolution = policy.validate(&base_url)?;
        let http = policy.reqwest_client(&resolution, configure_inference_http_client)?;
        Ok(Self {
            base_url,
            api_key: None,
            reasoning_effort: None,
            ollama_context_tokens: None,
            engine: InferenceEngine::default(),
            http,
            endpoint_resolution: Some(resolution),
            endpoint_policy_error: None,
            admission: None,
        })
    }

    pub fn endpoint_resolution(&self) -> Option<&EndpointResolution> {
        self.endpoint_resolution.as_ref()
    }

    fn enforce_endpoint_policy(&self) -> Result<(), InferenceError> {
        match &self.endpoint_policy_error {
            Some(error) => Err(InferenceError::EndpointPolicy(error.clone())),
            None => Ok(()),
        }
    }

    pub fn with_api_key(mut self, key: impl Into<String>) -> Self {
        self.api_key = Some(key.into());
        self
    }

    /// Set the OpenAI-compatible reasoning effort for every request on this
    /// client. The owning backend must supply a supported value.
    pub fn with_reasoning_effort(mut self, effort: impl Into<String>) -> Self {
        self.reasoning_effort = Some(effort.into());
        self
    }

    /// Set the context window sent on native Ollama `/api/chat` requests.
    /// OpenAI-compatible requests cannot set this Ollama option.
    pub fn with_ollama_context_tokens(mut self, tokens: u32) -> Self {
        self.ollama_context_tokens = Some(tokens);
        self
    }

    /// Builder hop that selects the wire protocol. Set to
    /// [`InferenceEngine::Ollama`] for Apple-Silicon native
    /// Ollama backends; defaults to OpenAI-compat otherwise.
    pub fn with_engine(mut self, engine: InferenceEngine) -> Self {
        self.engine = engine;
        self
    }

    /// Attach the host's shared inference scheduler and workload class.
    pub fn with_admission(
        mut self,
        provider: Arc<dyn InferenceRequestAdmission>,
        workload: &'static str,
    ) -> Self {
        self.admission = Some(AdmissionBinding {
            provider,
            workload,
            budget_scope: None,
        });
        self
    }

    /// Reclassify a resolved client while retaining its shared scheduler.
    pub fn with_workload(mut self, workload: &'static str) -> Self {
        if let Some(admission) = &mut self.admission {
            admission.workload = workload;
        }
        self
    }

    /// Bind child requests to a durable parent so admission limits siblings together.
    pub fn with_budget_scope(mut self, scope: impl Into<Arc<str>>) -> Self {
        if let Some(admission) = &mut self.admission {
            admission.budget_scope = Some(scope.into());
        }
        self
    }

    async fn acquire_admission(
        &self,
        model: &str,
    ) -> Result<Option<Box<dyn InferenceAdmissionPermit>>, InferenceError> {
        match &self.admission {
            Some(binding) => binding
                .provider
                .acquire(model, binding.workload, binding.budget_scope.as_deref())
                .await
                .map(Some)
                .map_err(InferenceError::Admission),
            None => Ok(None),
        }
    }

    /// Non-streaming chat completion.
    ///
    /// Request is sent with `stream = false` regardless of the `req.stream`
    /// flag — streaming uses [`chat_completions_stream`](Self::chat_completions_stream).
    pub async fn chat_completions(
        &self,
        req: &ChatRequest,
    ) -> Result<ChatResponse, InferenceError> {
        self.enforce_endpoint_policy()?;
        let _admission = self.acquire_admission(req.model.as_str()).await?;
        if self.engine == InferenceEngine::Ollama {
            // Route to the native /api/chat endpoint. Ollama's
            // OpenAI shim has been observed to drop tool_calls on
            // small qwen quants — the native path returns them
            // structured.
            return ollama::chat_completions(
                &self.http,
                &self.base_url,
                self.api_key.as_deref(),
                req,
                self.ollama_context_tokens,
            )
            .await;
        }
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        // 2026-05-12 — HTTP-layer timing instrumented on the
        // `agent::turn_timing` target so the operator can split
        // "vLLM is slow" from "client is slow" without correlating
        // by hand. `send_ms` = time to get response headers (which
        // for non-streaming usually means vLLM accepted the request
        // — generation hasn't started writing the body yet);
        // `body_ms` = headers → final body byte (this is where
        // generation latency lives for non-streaming, since vLLM
        // buffers the entire response server-side).
        let started_at = std::time::Instant::now();
        let mut r = self.http.post(&url).json(&ChatRequestNonStreaming(
            req,
            self.reasoning_effort.as_deref(),
        ));
        if let Some(key) = &self.api_key {
            r = r.bearer_auth(key);
        }
        let resp = r.send().await?;
        let send_ms = started_at.elapsed().as_millis() as u64;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            tracing::debug!(
                target: "agent::turn_timing",
                url = %url,
                send_ms,
                http_status = status.as_u16(),
                body_chars = body.chars().count(),
                "chat_completions non-streaming returned non-success status"
            );
            return Err(InferenceError::BadStatus {
                status: status.as_u16(),
                body,
            });
        }
        let body_started_at = std::time::Instant::now();
        let text = resp.text().await?;
        let body_ms = body_started_at.elapsed().as_millis() as u64;
        tracing::debug!(
            target: "agent::turn_timing",
            url = %url,
            send_ms,
            body_ms,
            response_chars = text.chars().count(),
            "chat_completions non-streaming HTTP round-trip"
        );
        serde_json::from_str::<ChatResponse>(&text).map_err(|e| {
            InferenceError::Decode(format!(
                "bad /v1/chat/completions response: {e}; response_chars={}",
                text.chars().count()
            ))
        })
    }

    /// Create a local text embedding through the configured backend.
    /// Input and output are bounded so retrieval cannot turn a prompt into an
    /// unbounded inference or index-write operation.
    pub async fn embeddings(&self, model: &str, input: &str) -> Result<Vec<f32>, InferenceError> {
        self.enforce_endpoint_policy()?;
        if model.trim().is_empty() || model.len() > 256 || input.is_empty() || input.len() > 32_768
        {
            return Err(InferenceError::Decode(
                "embedding model or input exceeds its bounds".into(),
            ));
        }
        let _admission = self.acquire_admission(model).await?;
        let (url, body, provider_shape) = if self.engine == InferenceEngine::Ollama {
            let url = format!("{}/api/embed", ollama::daemon_root(&self.base_url));
            (
                url,
                serde_json::json!({"model": model, "input": [input]}),
                true,
            )
        } else {
            (
                format!("{}/embeddings", self.base_url.trim_end_matches('/')),
                serde_json::json!({"model": model, "input": input}),
                false,
            )
        };
        let mut request = self.http.post(&url).json(&body);
        if let Some(key) = &self.api_key {
            request = request.bearer_auth(key);
        }
        let response = request.send().await?;
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body = response.text().await.unwrap_or_default();
            return Err(InferenceError::BadStatus { status, body });
        }
        let value: serde_json::Value = response.json().await?;
        let vector = if provider_shape {
            value
                .get("embeddings")
                .and_then(serde_json::Value::as_array)
                .and_then(|items| items.first())
                .and_then(serde_json::Value::as_array)
        } else {
            value
                .get("data")
                .and_then(serde_json::Value::as_array)
                .and_then(|items| items.first())
                .and_then(|item| item.get("embedding"))
                .and_then(serde_json::Value::as_array)
        }
        .ok_or_else(|| InferenceError::Decode("embedding response had no vector".into()))?;
        let vector = vector
            .iter()
            .map(|value| {
                value
                    .as_f64()
                    .filter(|value| value.is_finite())
                    .map(|value| value as f32)
                    .filter(|value| value.is_finite())
                    .ok_or_else(|| InferenceError::Decode("embedding contains a non-number".into()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        if vector.is_empty() || vector.len() > 8192 {
            return Err(InferenceError::Decode(
                "embedding dimensions are outside 1..=8192".into(),
            ));
        }
        Ok(vector)
    }

    /// Execute a non-streaming completion with a bounded retry policy.
    ///
    /// This intentionally does not retry a stream after bytes have been
    /// exposed to the caller: replaying a partial stream could duplicate text
    /// or tool-call deltas. Callers persist the surrounding durable step before
    /// invoking this helper, so a completed response remains replay-safe.
    pub async fn chat_completions_with_retry(
        &self,
        req: &ChatRequest,
        policy: &InferenceRetryPolicy,
    ) -> Result<ChatResponse, InferenceError> {
        self.chat_completions_with_retry_observed(req, policy, |_| Ok(()), |_, _| Ok(()))
            .await
    }

    /// Retry a completion and invoke `before_attempt` before every network
    /// request. Durable callers use the hook to persist attempt metadata.
    pub async fn chat_completions_with_retry_observed<F, R>(
        &self,
        req: &ChatRequest,
        policy: &InferenceRetryPolicy,
        before_attempt: F,
        on_retry: R,
    ) -> Result<ChatResponse, InferenceError>
    where
        F: FnMut(u32) -> Result<(), InferenceError>,
        R: FnMut(u32, &InferenceError) -> Result<(), InferenceError>,
    {
        self.chat_completions_with_retry_observed_cancelled(
            req,
            policy,
            before_attempt,
            on_retry,
            || false,
        )
        .await
    }

    /// Retry a non-streaming completion while observing caller cancellation
    /// during both the request and retry delay. Durable hooks run only for
    /// attempts that actually begin.
    pub async fn chat_completions_with_retry_observed_cancelled<F, R, C>(
        &self,
        req: &ChatRequest,
        policy: &InferenceRetryPolicy,
        mut before_attempt: F,
        mut on_retry: R,
        cancelled: C,
    ) -> Result<ChatResponse, InferenceError>
    where
        F: FnMut(u32) -> Result<(), InferenceError>,
        R: FnMut(u32, &InferenceError) -> Result<(), InferenceError>,
        C: Fn() -> bool,
    {
        let started_at = tokio::time::Instant::now();
        let max_attempts = policy.max_attempts.max(1);
        let mut attempt = 0;

        loop {
            if cancelled() {
                return Err(InferenceError::Cancelled);
            }
            attempt += 1;
            before_attempt(attempt)?;
            let elapsed = started_at.elapsed();
            let Some(remaining) = policy.deadline.checked_sub(elapsed) else {
                return Err(InferenceError::Timeout);
            };
            let result =
                cancellable_timeout(self.chat_completions(req), remaining, &cancelled).await;
            match result {
                Ok(Ok(response)) => return Ok(response),
                Ok(Err(error)) if !error.is_retryable() || attempt >= max_attempts => {
                    return Err(error);
                }
                Err(InferenceError::Timeout) if attempt >= max_attempts => {
                    return Err(InferenceError::Timeout);
                }
                Err(error @ InferenceError::Cancelled) => return Err(error),
                Err(_) if attempt >= max_attempts => return Err(InferenceError::Timeout),
                Ok(Err(error)) => {
                    let exponent = attempt.saturating_sub(1).min(10);
                    let multiplier = 1_u32 << exponent;
                    let backoff = policy.initial_backoff.saturating_mul(multiplier);
                    if backoff >= remaining {
                        return Err(error);
                    }
                    on_retry(attempt, &error)?;
                    tracing::debug!(
                        attempt,
                        max_attempts,
                        backoff_ms = backoff.as_millis(),
                        error_class = error.safe_class(),
                        "retrying transient local inference failure"
                    );
                    cancellable_sleep(backoff, &cancelled).await?;
                }
                Err(InferenceError::Timeout) => {
                    let backoff = policy.initial_backoff;
                    if backoff >= remaining {
                        return Err(InferenceError::Timeout);
                    }
                    on_retry(attempt, &InferenceError::Timeout)?;
                    cancellable_sleep(backoff, &cancelled).await?;
                }
                Err(error) => return Err(error),
            }
        }
    }

    /// Fetch the model list from `GET /v1/models` on the configured
    /// backend. Used by the SPA's multimodal-capability probe — the
    /// model id loaded into vLLM / Ollama / llama.cpp drives whether
    /// the chat composer surfaces an image-attach affordance.
    ///
    /// Returns the first model entry's id when present, alongside the
    /// raw JSON so future probes (e.g. context length) can read it
    /// without a second round-trip.
    pub async fn list_models(&self) -> Result<ModelListResponse, InferenceError> {
        self.enforce_endpoint_policy()?;
        let url = format!("{}/models", self.base_url.trim_end_matches('/'));
        let mut r = self.http.get(&url);
        if let Some(key) = &self.api_key {
            r = r.bearer_auth(key);
        }
        let resp = r.send().await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(InferenceError::BadStatus {
                status: status.as_u16(),
                body,
            });
        }
        let text = resp.text().await?;
        serde_json::from_str::<ModelListResponse>(&text).map_err(|e| {
            InferenceError::Decode(format!(
                "bad /v1/models response: {e}; response_chars={}",
                text.chars().count()
            ))
        })
    }

    /// Streaming chat completion.
    ///
    /// Sends `stream = true` and parses the OpenAI SSE format
    /// (`data: {json}\n\n`, terminated by `data: [DONE]`). Yields one
    /// [`ChatStreamChunk`] per SSE event. Invalid JSON inside a `data:`
    /// line is skipped with a warning — a single malformed chunk must
    /// not kill the whole stream.
    ///
    /// The caller aggregates chunk deltas into a full assistant
    /// message; streaming termination is signaled by the stream
    /// ending (no trailing `[DONE]` is yielded as a chunk).
    pub async fn chat_completions_stream(
        &self,
        req: &ChatRequest,
    ) -> Result<
        std::pin::Pin<
            Box<dyn futures::Stream<Item = Result<ChatStreamChunk, InferenceError>> + Send>,
        >,
        InferenceError,
    > {
        use futures::StreamExt;

        self.enforce_endpoint_policy()?;
        let admission = self.acquire_admission(req.model.as_str()).await?;

        if self.engine == InferenceEngine::Ollama {
            // Native NDJSON stream from /api/chat. The translation
            // layer wraps each frame in a ChatStreamChunk so the
            // upstream aggregator stays on its OpenAI-flavored
            // consumer.
            let stream = ollama::chat_completions_stream(
                &self.http,
                &self.base_url,
                self.api_key.as_deref(),
                req,
                self.ollama_context_tokens,
            )
            .await;
            let mut stream = stream?;
            return Ok(Box::pin(async_stream::stream! {
                let _admission = admission;
                while let Some(chunk) = stream.next().await {
                    yield chunk;
                }
            }));
        }

        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));
        let mut r = self
            .http
            .post(&url)
            .json(&ChatRequestStreaming(req, self.reasoning_effort.as_deref()));
        if let Some(key) = &self.api_key {
            r = r.bearer_auth(key);
        }
        let resp = r.send().await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(InferenceError::BadStatus {
                status: status.as_u16(),
                body,
            });
        }

        // Raw byte stream from reqwest, framed into SSE events.
        let bytes_stream = resp.bytes_stream();
        let events = Box::pin(sse_parser::parse(
            bytes_stream.map(|r| r.map_err(InferenceError::from)),
        ));

        // Decode each complete SSE event and require the protocol terminal
        // marker. A clean TCP EOF alone must not certify a complete model turn.
        let chunk_stream = async_stream::stream! {
            let _admission = admission;
            let mut events = Box::pin(events);
            let mut saw_done = false;
            let mut saw_finish_reason = false;
            let mut stream_error = false;
            while let Some(event) = events.next().await {
                match event {
                    Ok(SseEvent { data }) if data.trim() == "[DONE]" => {
                        saw_done = true;
                        if !saw_finish_reason {
                            stream_error = true;
                            yield Err(InferenceError::IncompleteStream);
                        }
                        break;
                    }
                    Ok(SseEvent { data }) => {
                        let trimmed = data.trim();
                        match serde_json::from_str::<ChatStreamChunk>(trimmed) {
                            Ok(chunk) => {
                                saw_finish_reason |= chunk
                                    .choices
                                    .iter()
                                    .any(|choice| choice.finish_reason.is_some());
                                yield Ok(chunk);
                            }
                            Err(error) => yield Err(InferenceError::Decode(format!(
                                "bad SSE chunk: {error}; chunk_chars={}",
                                trimmed.chars().count()
                            ))),
                        }
                    }
                    Err(error) => {
                        stream_error = true;
                        yield Err(error);
                        break;
                    }
                }
            }
            if !saw_done && !stream_error {
                yield Err(InferenceError::IncompleteStream);
            }
        };

        Ok(Box::pin(chunk_stream))
    }

    /// Open a streaming completion with bounded retries before any stream
    /// bytes are exposed to the caller. Once this returns a stream, failures
    /// while reading it are surfaced directly: replaying then could duplicate
    /// visible assistant deltas or tool calls.
    pub async fn chat_completions_stream_with_retry(
        &self,
        req: &ChatRequest,
        policy: &InferenceRetryPolicy,
    ) -> Result<
        std::pin::Pin<
            Box<dyn futures::Stream<Item = Result<ChatStreamChunk, InferenceError>> + Send>,
        >,
        InferenceError,
    > {
        self.chat_completions_stream_with_retry_cancelled(req, policy, || false)
            .await
    }

    /// As [`Self::chat_completions_stream_with_retry`], while observing a
    /// caller-owned cancellation predicate during request opening and backoff.
    pub async fn chat_completions_stream_with_retry_cancelled<F>(
        &self,
        req: &ChatRequest,
        policy: &InferenceRetryPolicy,
        cancelled: F,
    ) -> Result<
        std::pin::Pin<
            Box<dyn futures::Stream<Item = Result<ChatStreamChunk, InferenceError>> + Send>,
        >,
        InferenceError,
    >
    where
        F: Fn() -> bool,
    {
        self.chat_completions_stream_with_retry_observed_cancelled(
            req,
            policy,
            cancelled,
            |_, _| Ok(()),
        )
        .await
    }

    /// Open a stream with cancellation and a metadata-only retry observer.
    /// The observer runs before a retry request is issued.
    pub async fn chat_completions_stream_with_retry_observed_cancelled<F, R>(
        &self,
        req: &ChatRequest,
        policy: &InferenceRetryPolicy,
        cancelled: F,
        mut on_retry: R,
    ) -> Result<
        std::pin::Pin<
            Box<dyn futures::Stream<Item = Result<ChatStreamChunk, InferenceError>> + Send>,
        >,
        InferenceError,
    >
    where
        F: Fn() -> bool,
        R: FnMut(u32, &InferenceError) -> Result<(), InferenceError>,
    {
        let started = std::time::Instant::now();
        let attempts = policy.max_attempts.max(1);
        let mut backoff = policy.initial_backoff;
        for attempt in 1..=attempts {
            if cancelled() {
                return Err(InferenceError::Cancelled);
            }
            let remaining = policy.deadline.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                return Err(InferenceError::Timeout);
            }
            let result =
                cancellable_timeout(self.chat_completions_stream(req), remaining, &cancelled).await;
            let error = match result {
                Ok(Ok(stream)) => return Ok(stream),
                Ok(Err(error)) | Err(error) => error,
            };
            if attempt == attempts || !error.is_retryable() {
                return Err(error);
            }
            on_retry(attempt, &error)?;
            let remaining = policy.deadline.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                return Err(InferenceError::Timeout);
            }
            cancellable_sleep(backoff.min(remaining), &cancelled).await?;
            backoff = backoff.saturating_mul(2);
        }
        Err(InferenceError::Timeout)
    }
}

async fn cancellable_timeout<F, T>(
    future: F,
    deadline: std::time::Duration,
    cancelled: &impl Fn() -> bool,
) -> Result<T, InferenceError>
where
    F: std::future::Future<Output = T>,
{
    tokio::pin!(future);
    let timeout = tokio::time::sleep(deadline);
    tokio::pin!(timeout);
    loop {
        if cancelled() {
            return Err(InferenceError::Cancelled);
        }
        tokio::select! {
            result = &mut future => return Ok(result),
            _ = &mut timeout => return Err(InferenceError::Timeout),
            _ = tokio::time::sleep(std::time::Duration::from_millis(10)) => {},
        }
    }
}

async fn cancellable_sleep(
    duration: std::time::Duration,
    cancelled: &impl Fn() -> bool,
) -> Result<(), InferenceError> {
    let sleep = tokio::time::sleep(duration);
    tokio::pin!(sleep);
    loop {
        if cancelled() {
            return Err(InferenceError::Cancelled);
        }
        tokio::select! {
            _ = &mut sleep => return Ok(()),
            _ = tokio::time::sleep(std::time::Duration::from_millis(10)) => {},
        }
    }
}

// ---------------------------------------------------------------------------
// Streaming chunk types (OpenAI SSE shape)
// ---------------------------------------------------------------------------

/// One SSE event from `/v1/chat/completions?stream=true`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatStreamChunk {
    pub id: String,
    pub model: String,
    pub choices: Vec<ChatStreamChoice>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatStreamChoice {
    pub index: u32,
    pub delta: ChatStreamDelta,
    #[serde(default)]
    pub finish_reason: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChatStreamDelta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// Tool-call deltas are serialized across chunks per the OpenAI
    /// spec: the first delta has the `id` + `type` + `function.name`,
    /// subsequent deltas have `function.arguments` appended.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCallDelta>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallDelta {
    pub index: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function: Option<ToolCallFunctionDelta>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallFunctionDelta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments: Option<String>,
}

/// Serializer adapter that forces `stream = true` for streaming calls.
struct ChatRequestStreaming<'a>(&'a ChatRequest, Option<&'a str>);

impl<'a> Serialize for ChatRequestStreaming<'a> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut st = s.serialize_struct("ChatRequest", 9)?;
        st.serialize_field("model", &self.0.model)?;
        st.serialize_field("messages", &self.0.messages)?;
        if let Some(tools) = &self.0.tools {
            st.serialize_field("tools", tools)?;
        }
        if let Some(tc) = &self.0.tool_choice {
            st.serialize_field("tool_choice", tc)?;
        }
        if let Some(format) = &self.0.response_format {
            st.serialize_field("response_format", format)?;
        }
        st.serialize_field("stream", &true)?;
        if let Some(t) = &self.0.temperature {
            st.serialize_field("temperature", t)?;
        }
        if let Some(m) = &self.0.max_tokens {
            st.serialize_field("max_tokens", m)?;
        }
        if let Some(kw) = &self.0.chat_template_kwargs {
            st.serialize_field("chat_template_kwargs", kw)?;
        }
        if let Some(effort) = self.1 {
            st.serialize_field("reasoning_effort", effort)?;
        }
        if let Some(g) = &self.0.guided_decoding_backend {
            st.serialize_field("guided_decoding_backend", g)?;
        }
        st.end()
    }
}

// ---------------------------------------------------------------------------
// Minimal SSE parser — OpenAI's wire uses `data: {json}\n\n`. We don't
// need event types or IDs, just the `data:` payloads.
// ---------------------------------------------------------------------------

/// Maximum UTF-8 payload size accepted for one server-sent event.
pub const MAX_SSE_EVENT_BYTES: usize = 1024 * 1024;

/// One decoded SSE event. Multiple `data:` lines are joined with a newline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseEvent {
    pub data: String,
}

/// Incremental bounded SSE decoder shared by inference and HTTP MCP clients.
#[derive(Debug, Default)]
pub struct SseDecoder {
    pending: Vec<u8>,
    data: Vec<u8>,
    has_data: bool,
}

impl SseDecoder {
    /// Feed a byte fragment and return every complete SSE event it contains.
    ///
    /// CR, LF, and CRLF line endings are accepted. UTF-8 decoding happens only
    /// after a complete event is assembled, so splitting a code point across
    /// network chunks cannot corrupt streamed text or tool arguments.
    pub fn push(&mut self, bytes: &[u8], eof: bool) -> Result<Vec<SseEvent>, InferenceError> {
        self.pending.extend_from_slice(bytes);
        let mut events = Vec::new();
        loop {
            let Some(index) = self
                .pending
                .iter()
                .position(|byte| *byte == b'\r' || *byte == b'\n')
            else {
                if self.pending.len() + self.data.len() > MAX_SSE_EVENT_BYTES {
                    return Err(InferenceError::FrameTooLarge(MAX_SSE_EVENT_BYTES));
                }
                break;
            };
            if index > MAX_SSE_EVENT_BYTES {
                return Err(InferenceError::FrameTooLarge(MAX_SSE_EVENT_BYTES));
            }
            if self.pending[index] == b'\r' && index + 1 == self.pending.len() && !eof {
                if index + self.data.len() > MAX_SSE_EVENT_BYTES {
                    return Err(InferenceError::FrameTooLarge(MAX_SSE_EVENT_BYTES));
                }
                break;
            }

            let line: Vec<u8> = self.pending.drain(..index).collect();
            let ending =
                if self.pending.first() == Some(&b'\r') && self.pending.get(1) == Some(&b'\n') {
                    2
                } else {
                    1
                };
            self.pending.drain(..ending);
            self.process_line(&line, &mut events)?;
        }

        if eof {
            if !self.pending.is_empty() {
                let tail = std::mem::take(&mut self.pending);
                self.process_line(&tail, &mut events)?;
            }
            if self.has_data {
                events.push(self.finish_event()?);
            }
        }
        Ok(events)
    }

    fn process_line(
        &mut self,
        line: &[u8],
        events: &mut Vec<SseEvent>,
    ) -> Result<(), InferenceError> {
        if line.is_empty() {
            if self.has_data {
                events.push(self.finish_event()?);
            }
            return Ok(());
        }
        if line.first() == Some(&b':') {
            return Ok(());
        }
        let Some(colon) = line.iter().position(|byte| *byte == b':') else {
            return Ok(());
        };
        if &line[..colon] != b"data" {
            return Ok(());
        }
        let mut value = &line[colon + 1..];
        if value.first() == Some(&b' ') {
            value = &value[1..];
        }
        let extra = value.len() + usize::from(self.has_data);
        if self.data.len().saturating_add(extra) > MAX_SSE_EVENT_BYTES {
            return Err(InferenceError::FrameTooLarge(MAX_SSE_EVENT_BYTES));
        }
        if self.has_data {
            self.data.push(b'\n');
        }
        self.data.extend_from_slice(value);
        self.has_data = true;
        Ok(())
    }

    fn finish_event(&mut self) -> Result<SseEvent, InferenceError> {
        let data = std::str::from_utf8(&self.data)
            .map_err(|error| {
                InferenceError::Decode(format!("SSE data is not valid UTF-8: {error}"))
            })?
            .to_owned();
        self.data.clear();
        self.has_data = false;
        Ok(SseEvent { data })
    }
}

mod sse_parser {
    use super::{InferenceError, SseDecoder, SseEvent};
    use futures::{Stream, StreamExt};

    pub fn parse<S>(
        bytes: S,
    ) -> impl Stream<Item = Result<SseEvent, InferenceError>> + Send + 'static
    where
        S: Stream<Item = Result<bytes::Bytes, InferenceError>> + Send + 'static,
    {
        async_stream::stream! {
            let mut decoder = SseDecoder::default();
            let mut bytes = Box::pin(bytes);
            while let Some(chunk) = bytes.next().await {
                match chunk {
                    Ok(chunk) => match decoder.push(&chunk, false) {
                        Ok(events) => {
                            for event in events {
                                yield Ok(event);
                            }
                        }
                        Err(error) => {
                            yield Err(error);
                            return;
                        }
                    },
                    Err(error) => {
                        yield Err(error);
                        return;
                    }
                }
            }
            match decoder.push(&[], true) {
                Ok(events) => {
                    for event in events {
                        yield Ok(event);
                    }
                }
                Err(error) => yield Err(error),
            }
        }
    }
}

/// Serializer adapter that forces `stream = false` regardless of input.
/// Ensures non-streaming calls don't get SSE back by accident.
struct ChatRequestNonStreaming<'a>(&'a ChatRequest, Option<&'a str>);

impl<'a> Serialize for ChatRequestNonStreaming<'a> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        let mut st = s.serialize_struct("ChatRequest", 9)?;
        st.serialize_field("model", &self.0.model)?;
        st.serialize_field("messages", &self.0.messages)?;
        if let Some(tools) = &self.0.tools {
            st.serialize_field("tools", tools)?;
        }
        if let Some(tc) = &self.0.tool_choice {
            st.serialize_field("tool_choice", tc)?;
        }
        if let Some(format) = &self.0.response_format {
            st.serialize_field("response_format", format)?;
        }
        st.serialize_field("stream", &false)?;
        if let Some(t) = &self.0.temperature {
            st.serialize_field("temperature", t)?;
        }
        if let Some(m) = &self.0.max_tokens {
            st.serialize_field("max_tokens", m)?;
        }
        if let Some(kw) = &self.0.chat_template_kwargs {
            st.serialize_field("chat_template_kwargs", kw)?;
        }
        if let Some(effort) = self.1 {
            st.serialize_field("reasoning_effort", effort)?;
        }
        if let Some(g) = &self.0.guided_decoding_backend {
            st.serialize_field("guided_decoding_backend", g)?;
        }
        st.end()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::net::IpAddr;

    #[tokio::test]
    async fn openai_embeddings_call_is_local_and_decodes_bounded_vectors() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0u8; 4096];
            let count = socket.read(&mut request).await.unwrap();
            let request = String::from_utf8_lossy(&request[..count]);
            assert!(request.starts_with("POST /v1/embeddings "));
            let body = r#"{"data":[{"embedding":[0.25,-0.5,1.0]}]}"#;
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        });
        let client = InferenceClient::new(format!("http://{address}/v1"));
        let embedding = client
            .embeddings("local-embed-v1", "synthetic query")
            .await
            .unwrap();
        assert_eq!(embedding, vec![0.25, -0.5, 1.0]);
        server.await.unwrap();
    }

    #[test]
    fn configured_reasoning_effort_is_sent_on_both_chat_wire_shapes() {
        let request = ChatRequest {
            model: ModelId("local-model".into()),
            messages: vec![ChatMessage::user("READY")],
            tools: None,
            stream: false,
            temperature: None,
            max_tokens: Some(64),
            chat_template_kwargs: Some(json!({"enable_thinking": false})),
            tool_choice: None,
            response_format: None,
            guided_decoding_backend: None,
        };
        let non_streaming =
            serde_json::to_value(ChatRequestNonStreaming(&request, Some("none"))).unwrap();
        let streaming = serde_json::to_value(ChatRequestStreaming(&request, Some("none"))).unwrap();
        assert_eq!(non_streaming["reasoning_effort"], "none");
        assert_eq!(streaming["reasoning_effort"], "none");
        assert_eq!(non_streaming["stream"], false);
        assert_eq!(streaming["stream"], true);
        assert!(
            serde_json::to_value(ChatRequestNonStreaming(&request, None))
                .unwrap()
                .get("reasoning_effort")
                .is_none()
        );
    }

    #[test]
    fn retries_only_transient_inference_errors() {
        for status in [429, 502, 503, 504] {
            assert!(
                InferenceError::BadStatus {
                    status,
                    body: String::new()
                }
                .is_retryable()
            );
        }
        for status in [400, 401, 403, 404, 422, 500] {
            assert!(
                !InferenceError::BadStatus {
                    status,
                    body: String::new()
                }
                .is_retryable()
            );
        }
        assert!(InferenceError::Timeout.is_retryable());
        assert!(!InferenceError::Decode("bad response".into()).is_retryable());
        assert!(!InferenceError::EndpointPolicy("blocked".into()).is_retryable());
    }

    #[test]
    fn sse_decoder_is_stable_at_every_utf8_and_line_ending_split() {
        let wire = "data: {\"text\":\"café 世界🌍\",\"tool\":\"{\\\"name\\\":\\\"🚀\\\"}\"}\r\n\r\ndata: [DONE]\r\n\r\n";
        let expected = vec![
            SseEvent {
                data: "{\"text\":\"café 世界🌍\",\"tool\":\"{\\\"name\\\":\\\"🚀\\\"}\"}".into(),
            },
            SseEvent {
                data: "[DONE]".into(),
            },
        ];
        let bytes = wire.as_bytes();

        for split in 0..=bytes.len() {
            let mut decoder = SseDecoder::default();
            let mut events = decoder.push(&bytes[..split], false).unwrap();
            events.extend(decoder.push(&bytes[split..], false).unwrap());
            events.extend(decoder.push(&[], true).unwrap());
            assert_eq!(events, expected, "decoder changed at byte split {split}");
        }

        let mut decoder = SseDecoder::default();
        let mut events = Vec::new();
        for byte in bytes {
            events.extend(decoder.push(std::slice::from_ref(byte), false).unwrap());
        }
        events.extend(decoder.push(&[], true).unwrap());
        assert_eq!(
            events, expected,
            "single-byte network chunks changed output"
        );
    }

    #[test]
    fn chat_request_serializes_openai_shape() {
        let req = ChatRequest {
            model: ModelId("QuantTrio/Qwen3.5-27B-AWQ".to_owned()),
            messages: vec![
                ChatMessage::system("you are execlaw"),
                ChatMessage::user("hi"),
            ],
            tools: Some(vec![ToolDeclaration::function(
                "read_memory",
                "read a long-term memory entry",
                json!({"type": "object"}),
            )]),
            stream: true,
            temperature: None,
            max_tokens: Some(512),
            chat_template_kwargs: None,
            tool_choice: None,
            response_format: None,
            guided_decoding_backend: None,
        };
        let s = serde_json::to_string(&req).unwrap();
        assert!(s.contains("\"model\""));
        assert!(s.contains("\"messages\""));
        assert!(s.contains("\"tools\""));
        // No cloud-vendor-specific fields.
        assert!(!s.contains("anthropic"));
        assert!(!s.to_lowercase().contains("gemini"));
    }

    #[test]
    fn client_defaults_to_no_api_key() {
        let c = InferenceClient::new("http://127.0.0.1:8000/v1");
        assert!(c.api_key.is_none());
    }

    #[test]
    fn chat_response_decodes_tool_calls() {
        let json_str = r#"{
            "id": "abc",
            "model": "Qwen3.5-27B-AWQ",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": "read_memory",
                            "arguments": "{\"scope\":\"global\",\"key\":\"x\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        }"#;
        let resp: ChatResponse = serde_json::from_str(json_str).unwrap();
        assert_eq!(resp.choices.len(), 1);
        assert_eq!(resp.choices[0].message.tool_calls.len(), 1);
        assert_eq!(
            resp.choices[0].message.tool_calls[0].function.name,
            "read_memory"
        );
        assert_eq!(resp.choices[0].finish_reason.as_deref(), Some("tool_calls"));
    }

    #[test]
    fn chat_response_decodes_reasoning_content_when_content_is_empty() {
        let json_str = r#"{
            "id": "abc",
            "model": "Qwen3.5-27B-AWQ",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": null,
                    "reasoning_content": "{\"thesis\":\"t\",\"steps\":[{\"query\":\"q\"}]}"
                },
                "finish_reason": "stop"
            }]
        }"#;

        let response: ChatResponse = serde_json::from_str(json_str).unwrap();
        assert_eq!(
            response.choices[0].message.reasoning_content.as_deref(),
            Some(r#"{"thesis":"t","steps":[{"query":"q"}]}"#)
        );
    }

    #[test]
    fn tool_result_message_round_trips() {
        let m = ChatMessage::tool_result("call_1", r#"{"value":"bf_emma"}"#);
        let s = serde_json::to_string(&m).unwrap();
        assert!(s.contains("\"role\":\"tool\""));
        assert!(s.contains("\"tool_call_id\":\"call_1\""));
    }

    /// SSE parser must collect `data:` lines between `\n\n` boundaries
    /// and skip the terminal `[DONE]`.
    #[tokio::test]
    async fn streaming_parses_openai_sse_frames() {
        use futures::StreamExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            use tokio::io::AsyncReadExt;
            use tokio::io::AsyncWriteExt;
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 4096];
            let _ =
                tokio::time::timeout(std::time::Duration::from_millis(500), sock.read(&mut buf))
                    .await;
            let body_chunks = [
                "data: {\"id\":\"a\",\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"He\"}}]}\n\n",
                "data: {\"id\":\"a\",\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"llo\"}}]}\n\n",
                "data: {\"id\":\"a\",\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
                "data: [DONE]\n\n",
            ];
            let body: String = body_chunks.join("");
            let response = format!(
                "HTTP/1.1 200 OK\r\n\
                 content-type: text/event-stream\r\n\
                 content-length: {}\r\n\
                 connection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = sock.write_all(response.as_bytes()).await;
            let _ = sock.flush().await;
        });

        let client = InferenceClient::new(format!("http://{addr}/v1"));
        let req = ChatRequest {
            model: ModelId("m".into()),
            messages: vec![ChatMessage::user("hi")],
            tools: None,
            stream: true,
            temperature: None,
            max_tokens: None,
            chat_template_kwargs: None,
            tool_choice: None,
            response_format: None,
            guided_decoding_backend: None,
        };
        let mut stream = client.chat_completions_stream(&req).await.unwrap();
        let mut text = String::new();
        let mut finished = false;
        while let Some(chunk) = stream.next().await {
            let c = chunk.unwrap();
            for ch in &c.choices {
                if let Some(t) = &ch.delta.content {
                    text.push_str(t);
                }
                if ch.finish_reason.is_some() {
                    finished = true;
                }
            }
        }
        assert_eq!(text, "Hello");
        assert!(finished, "expected finish_reason on a chunk");
    }

    #[tokio::test]
    async fn truncated_stream_surfaces_partial_output_without_replaying_request() {
        use futures::StreamExt;
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let requests = Arc::new(AtomicUsize::new(0));
        let served = requests.clone();
        let body = "data: {\"id\":\"partial\",\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"partial\"},\"finish_reason\":\"stop\"}]}\n\n";
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            served.fetch_add(1, Ordering::SeqCst);
            let mut request = [0_u8; 4096];
            let _ = socket.read(&mut request).await;
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        });

        let client = InferenceClient::new(format!("http://{addr}/v1"));
        let request = ChatRequest {
            model: ModelId("m".into()),
            messages: vec![ChatMessage::user("partial")],
            tools: None,
            stream: true,
            temperature: None,
            max_tokens: None,
            chat_template_kwargs: None,
            tool_choice: None,
            response_format: None,
            guided_decoding_backend: None,
        };
        let policy = InferenceRetryPolicy {
            max_attempts: 3,
            deadline: std::time::Duration::from_secs(2),
            initial_backoff: std::time::Duration::ZERO,
        };
        let mut stream = client
            .chat_completions_stream_with_retry(&request, &policy)
            .await
            .unwrap();
        let chunk = stream.next().await.unwrap().unwrap();
        assert_eq!(
            chunk.choices[0].delta.content.as_deref(),
            Some("partial"),
            "already emitted content remains available to the caller"
        );
        assert!(matches!(
            stream.next().await.unwrap(),
            Err(InferenceError::IncompleteStream)
        ));
        assert!(stream.next().await.is_none());
        assert_eq!(requests.load(Ordering::SeqCst), 1);
        server.await.unwrap();
    }

    /// A malformed `data:` line must NOT poison the whole stream — the
    /// consumer gets an `Err` for that chunk but subsequent chunks
    /// still flow.
    #[tokio::test]
    async fn streaming_surfaces_per_chunk_decode_errors() {
        use futures::StreamExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        tokio::spawn(async move {
            use tokio::io::AsyncReadExt;
            use tokio::io::AsyncWriteExt;
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 4096];
            let _ =
                tokio::time::timeout(std::time::Duration::from_millis(500), sock.read(&mut buf))
                    .await;
            let body = String::from(
                "data: not-json\n\n\
                 data: {\"id\":\"x\",\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"}}]}\n\n\
                 data: [DONE]\n\n",
            );
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = sock.write_all(response.as_bytes()).await;
        });

        let client = InferenceClient::new(format!("http://{addr}/v1"));
        let req = ChatRequest {
            model: ModelId("m".into()),
            messages: vec![ChatMessage::user("hi")],
            tools: None,
            stream: true,
            temperature: None,
            max_tokens: None,
            chat_template_kwargs: None,
            tool_choice: None,
            response_format: None,
            guided_decoding_backend: None,
        };
        let mut stream = client.chat_completions_stream(&req).await.unwrap();
        let mut saw_err = false;
        let mut ok_content = String::new();
        while let Some(c) = stream.next().await {
            match c {
                Err(_) => saw_err = true,
                Ok(chunk) => {
                    for ch in chunk.choices {
                        if let Some(t) = ch.delta.content {
                            ok_content.push_str(&t);
                        }
                    }
                }
            }
        }
        assert!(saw_err, "expected a decode error for malformed chunk");
        assert_eq!(ok_content, "ok", "subsequent chunks must still stream");
    }

    #[tokio::test]
    async fn model_decode_errors_do_not_echo_response_body() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let body = "private-response-sentinel";
        let server_body = body.to_owned();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 4096];
            let _ = socket.read(&mut request).await;
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                server_body.len(),
                server_body
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        });
        let error = InferenceClient::new(format!("http://{addr}/v1"))
            .list_models()
            .await
            .unwrap_err();
        assert!(!error.to_string().contains(body));
        assert!(matches!(error, InferenceError::Decode(_)));
        server.await.unwrap();
    }

    /// Integration-style test: spin up a tokio TCP listener that pretends to
    /// be an OpenAI-compatible endpoint and verify the client serializes
    /// correctly + parses the canned response.
    #[tokio::test]
    async fn end_to_end_chat_completion_against_mock_server() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let canned = r#"{
            "id": "test-1",
            "model": "Qwen3.5-27B-AWQ",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "hello back"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 5, "completion_tokens": 2, "total_tokens": 7}
        }"#;

        let handle = tokio::spawn(async move {
            use tokio::io::AsyncReadExt;
            use tokio::io::AsyncWriteExt;
            let (mut sock, _) = listener.accept().await.unwrap();
            // Read the request headers+body (enough bytes to clear the buffer).
            let mut buf = [0u8; 4096];
            let _ =
                tokio::time::timeout(std::time::Duration::from_millis(500), sock.read(&mut buf))
                    .await;
            let body = canned.as_bytes();
            let response = format!(
                "HTTP/1.1 200 OK\r\n\
                 content-type: application/json\r\n\
                 content-length: {}\r\n\
                 connection: close\r\n\r\n{}",
                body.len(),
                canned
            );
            let _ = sock.write_all(response.as_bytes()).await;
            let _ = sock.flush().await;
        });

        let client = InferenceClient::new(format!("http://{addr}/v1"));
        let req = ChatRequest {
            model: ModelId("QuantTrio/Qwen3.5-27B-AWQ".to_owned()),
            messages: vec![ChatMessage::user("hello")],
            tools: None,
            stream: false,
            temperature: Some(0.0),
            max_tokens: Some(16),
            chat_template_kwargs: None,
            tool_choice: None,
            response_format: None,
            guided_decoding_backend: None,
        };
        let resp = client.chat_completions(&req).await.unwrap();
        assert_eq!(resp.id, "test-1");
        assert_eq!(
            resp.choices[0]
                .message
                .content
                .as_ref()
                .map(|c| c.as_text()),
            Some("hello back".to_owned())
        );
        let _ = handle.await;
    }

    #[tokio::test]
    async fn retries_transient_status_within_policy_budget() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let requests = Arc::new(AtomicUsize::new(0));
        let served = requests.clone();
        let handle = tokio::spawn(async move {
            for status in [503_u16, 200] {
                let (mut socket, _) = listener.accept().await.unwrap();
                served.fetch_add(1, Ordering::SeqCst);
                let mut request = [0_u8; 4096];
                let _ = socket.read(&mut request).await;
                let body = if status == 200 {
                    r#"{"id":"retry","model":"m","choices":[{"index":0,"message":{"role":"assistant","content":"recovered"},"finish_reason":"stop"}]}"#
                } else {
                    "temporarily unavailable"
                };
                let response = format!(
                    "HTTP/1.1 {status} test\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len(),
                );
                socket.write_all(response.as_bytes()).await.unwrap();
            }
        });
        let client = InferenceClient::new(format!("http://{addr}/v1"));
        let request = ChatRequest {
            model: ModelId("m".into()),
            messages: vec![ChatMessage::user("retry")],
            tools: None,
            stream: false,
            temperature: None,
            max_tokens: None,
            chat_template_kwargs: None,
            tool_choice: None,
            response_format: None,
            guided_decoding_backend: None,
        };
        let response = client
            .chat_completions_with_retry(
                &request,
                &InferenceRetryPolicy {
                    max_attempts: 2,
                    deadline: std::time::Duration::from_secs(2),
                    initial_backoff: std::time::Duration::ZERO,
                },
            )
            .await
            .unwrap();

        assert_eq!(response.id, "retry");
        assert_eq!(requests.load(Ordering::SeqCst), 2);
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn streaming_open_retries_transient_status_before_exposing_stream() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let requests = Arc::new(AtomicUsize::new(0));
        let served = requests.clone();
        let server = tokio::spawn(async move {
            for status in [503_u16, 200] {
                let (mut socket, _) = listener.accept().await.unwrap();
                served.fetch_add(1, Ordering::SeqCst);
                let mut request = [0_u8; 4096];
                let _ = socket.read(&mut request).await;
                let response = if status == 503 {
                    "HTTP/1.1 503 unavailable\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                } else {
                    "HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                };
                socket.write_all(response.as_bytes()).await.unwrap();
            }
        });
        let client = InferenceClient::new(format!("http://{addr}/v1"));
        let request = ChatRequest {
            model: ModelId("m".into()),
            messages: vec![ChatMessage::user("retry")],
            tools: None,
            stream: true,
            temperature: None,
            max_tokens: None,
            chat_template_kwargs: None,
            tool_choice: None,
            response_format: None,
            guided_decoding_backend: None,
        };
        let stream = client
            .chat_completions_stream_with_retry(
                &request,
                &InferenceRetryPolicy {
                    max_attempts: 2,
                    deadline: std::time::Duration::from_secs(2),
                    initial_backoff: std::time::Duration::ZERO,
                },
            )
            .await
            .unwrap();
        drop(stream);
        assert_eq!(requests.load(Ordering::SeqCst), 2);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn streaming_retry_backoff_stops_when_cancelled() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let requests = Arc::new(AtomicUsize::new(0));
        let served = requests.clone();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            served.fetch_add(1, Ordering::SeqCst);
            let mut request = [0_u8; 4096];
            let _ = socket.read(&mut request).await;
            socket
                .write_all(
                    b"HTTP/1.1 503 unavailable\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                )
                .await
                .unwrap();
        });
        let client = InferenceClient::new(format!("http://{addr}/v1"));
        let request = ChatRequest {
            model: ModelId("m".into()),
            messages: vec![ChatMessage::user("cancel")],
            tools: None,
            stream: true,
            temperature: None,
            max_tokens: None,
            chat_template_kwargs: None,
            tool_choice: None,
            response_format: None,
            guided_decoding_backend: None,
        };
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancel_after = cancelled.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            cancel_after.store(true, Ordering::SeqCst);
        });
        let error = client
            .chat_completions_stream_with_retry_cancelled(
                &request,
                &InferenceRetryPolicy {
                    max_attempts: 3,
                    deadline: std::time::Duration::from_secs(5),
                    initial_backoff: std::time::Duration::from_secs(2),
                },
                || cancelled.load(Ordering::SeqCst),
            )
            .await
            .err()
            .unwrap();
        assert!(matches!(error, InferenceError::Cancelled));
        assert_eq!(requests.load(Ordering::SeqCst), 1);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn streaming_open_stops_when_cancelled_before_headers() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        use tokio::io::AsyncReadExt;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 4096];
            let _ = socket.read(&mut request).await;
            // Keep the response headers open until the cancelled client
            // drops its request future.
            let mut byte = [0_u8; 1];
            let _ = socket.read(&mut byte).await;
        });
        let client = InferenceClient::new(format!("http://{addr}/v1"));
        let request = ChatRequest {
            model: ModelId("m".into()),
            messages: vec![ChatMessage::user("cancel opening")],
            tools: None,
            stream: true,
            temperature: None,
            max_tokens: None,
            chat_template_kwargs: None,
            tool_choice: None,
            response_format: None,
            guided_decoding_backend: None,
        };
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancel_after = cancelled.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            cancel_after.store(true, Ordering::SeqCst);
        });
        let error = client
            .chat_completions_stream_with_retry_cancelled(
                &request,
                &InferenceRetryPolicy {
                    max_attempts: 3,
                    deadline: std::time::Duration::from_secs(5),
                    initial_backoff: std::time::Duration::ZERO,
                },
                || cancelled.load(Ordering::SeqCst),
            )
            .await
            .err()
            .unwrap();
        assert!(matches!(error, InferenceError::Cancelled));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn non_streaming_retry_backoff_stops_when_cancelled() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let served = calls.clone();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            served.fetch_add(1, Ordering::SeqCst);
            let mut request = [0_u8; 4096];
            let _ = socket.read(&mut request).await;
            socket
                .write_all(
                    b"HTTP/1.1 503 unavailable\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                )
                .await
                .unwrap();
        });
        let client = InferenceClient::new(format!("http://{addr}/v1"));
        let request = ChatRequest {
            model: ModelId("m".into()),
            messages: vec![ChatMessage::user("cancel retry")],
            tools: None,
            stream: false,
            temperature: None,
            max_tokens: None,
            chat_template_kwargs: None,
            tool_choice: None,
            response_format: None,
            guided_decoding_backend: None,
        };
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancel_after = cancelled.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            cancel_after.store(true, Ordering::SeqCst);
        });
        let error = client
            .chat_completions_with_retry_observed_cancelled(
                &request,
                &InferenceRetryPolicy {
                    max_attempts: 3,
                    deadline: std::time::Duration::from_secs(5),
                    initial_backoff: std::time::Duration::from_secs(2),
                },
                |_| Ok(()),
                |_, _| Ok(()),
                || cancelled.load(Ordering::SeqCst),
            )
            .await
            .unwrap_err();
        assert!(matches!(error, InferenceError::Cancelled));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn non_streaming_open_stops_when_cancelled_before_headers() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        use tokio::io::AsyncReadExt;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 4096];
            let _ = socket.read(&mut request).await;
            let mut byte = [0_u8; 1];
            let _ = socket.read(&mut byte).await;
        });
        let client = InferenceClient::new(format!("http://{addr}/v1"));
        let request = ChatRequest {
            model: ModelId("m".into()),
            messages: vec![ChatMessage::user("cancel open")],
            tools: None,
            stream: false,
            temperature: None,
            max_tokens: None,
            chat_template_kwargs: None,
            tool_choice: None,
            response_format: None,
            guided_decoding_backend: None,
        };
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancel_after = cancelled.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            cancel_after.store(true, Ordering::SeqCst);
        });
        let error = client
            .chat_completions_with_retry_observed_cancelled(
                &request,
                &InferenceRetryPolicy {
                    max_attempts: 3,
                    deadline: std::time::Duration::from_secs(5),
                    initial_backoff: std::time::Duration::ZERO,
                },
                |_| Ok(()),
                |_, _| Ok(()),
                || cancelled.load(Ordering::SeqCst),
            )
            .await
            .unwrap_err();
        assert!(matches!(error, InferenceError::Cancelled));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn does_not_retry_permanent_status() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let requests = Arc::new(AtomicUsize::new(0));
        let served = requests.clone();
        let handle = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            served.fetch_add(1, Ordering::SeqCst);
            let mut request = [0_u8; 4096];
            let _ = socket.read(&mut request).await;
            socket
                .write_all(b"HTTP/1.1 400 bad request\r\ncontent-length: 7\r\nconnection: close\r\n\r\ninvalid")
                .await
                .unwrap();
        });
        let client = InferenceClient::new(format!("http://{addr}/v1"));
        let request = ChatRequest {
            model: ModelId("m".into()),
            messages: vec![ChatMessage::user("invalid")],
            tools: None,
            stream: false,
            temperature: None,
            max_tokens: None,
            chat_template_kwargs: None,
            tool_choice: None,
            response_format: None,
            guided_decoding_backend: None,
        };
        let error = client
            .chat_completions_with_retry(&request, &InferenceRetryPolicy::default())
            .await
            .unwrap_err();

        assert!(matches!(
            error,
            InferenceError::BadStatus { status: 400, .. }
        ));
        assert_eq!(requests.load(Ordering::SeqCst), 1);
        handle.await.unwrap();
    }

    #[tokio::test]
    async fn bounded_retry_stops_at_deadline_when_local_backend_stalls() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::io::AsyncReadExt;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(AtomicUsize::new(0));
        let accepted = requests.clone();
        let server = tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                accepted.fetch_add(1, Ordering::SeqCst);
                let mut request = [0_u8; 4096];
                let _ = socket.read(&mut request).await;
                // Keep the connection open without headers/body; the client
                // deadline, not a server response, must terminate the call.
                tokio::spawn(async move {
                    let _socket = socket;
                    std::future::pending::<()>().await;
                });
            }
        });
        let client = InferenceClient::new(format!("http://{address}/v1"));
        let request = ChatRequest {
            model: ModelId("m".into()),
            messages: vec![ChatMessage::user("wait")],
            tools: None,
            stream: false,
            temperature: None,
            max_tokens: None,
            chat_template_kwargs: None,
            tool_choice: None,
            response_format: None,
            guided_decoding_backend: None,
        };
        let policy = InferenceRetryPolicy {
            max_attempts: 4,
            deadline: std::time::Duration::from_millis(100),
            initial_backoff: std::time::Duration::from_millis(5),
        };
        let started = tokio::time::Instant::now();
        let error = client
            .chat_completions_with_retry(&request, &policy)
            .await
            .unwrap_err();
        assert!(matches!(error, InferenceError::Timeout));
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        assert!(requests.load(Ordering::SeqCst) >= 1);
        server.abort();
    }

    /// `MessageContent::Text` serialises as a plain string; the wire
    /// is byte-identical to the pre-vision shape so every existing
    /// backend keeps working.
    #[test]
    fn text_content_serialises_as_a_plain_string() {
        let m = ChatMessage::user("hi");
        let s = serde_json::to_string(&m).unwrap();
        assert!(s.contains("\"content\":\"hi\""), "got {s}");
        assert!(!s.contains("\"type\":\"text\""));
    }

    /// `MessageContent::Parts` serialises as OpenAI's vision content
    /// array — `[{type:"text",text:"..."},{type:"image_url",image_url:{url:"..."}}]`.
    /// This is what Qwen3-VL / Qwen3.6 / LLaVA / Pixtral expect.
    #[test]
    fn parts_content_serialises_as_openai_vision_array() {
        let m = ChatMessage::user_with_images(
            "describe this",
            vec!["data:image/png;base64,iVBOR".to_owned()],
        );
        let s = serde_json::to_string(&m).unwrap();
        assert!(s.contains("\"type\":\"text\""));
        assert!(s.contains("\"type\":\"image_url\""));
        assert!(s.contains("data:image/png;base64,iVBOR"));
    }

    #[test]
    fn known_multimodal_models_match() {
        for id in [
            "Qwen/Qwen2.5-VL-7B-Instruct",
            "Qwen/Qwen2-VL-72B-Instruct-AWQ",
            "Qwen/Qwen3-VL-32B",
            "Qwen3.6-27B-AWQ",
            "liuhaotian/llava-v1.6-mistral-7b",
            "meta-llama/Llama-3.2-11B-Vision-Instruct",
            "mistralai/Pixtral-12B-2409",
            "microsoft/Phi-3.5-vision-instruct",
            "microsoft/Phi-4-Multimodal-Instruct",
            "openbmb/MiniCPM-V-2_6",
            "OpenGVLab/InternVL2-26B",
        ] {
            assert!(
                is_known_multimodal_model(id),
                "expected {id} to be classified multimodal"
            );
        }
    }

    #[test]
    fn known_text_only_models_do_not_match() {
        for id in [
            "Qwen/Qwen3.5-27B-AWQ",
            "QuantTrio/Qwen3.5-27B-AWQ",
            "meta-llama/Llama-3.1-70B-Instruct",
            "mistralai/Mistral-7B-Instruct-v0.3",
            "google/gemma-2-27b-it",
            "openai/gpt-oss-20b",
        ] {
            assert!(
                !is_known_multimodal_model(id),
                "did not expect {id} to be classified multimodal"
            );
        }
    }

    /// Round-trip: an inbound parts-array deserialises back into a
    /// `Parts` variant (covers replay paths that feed assistant
    /// messages back into the LLM).
    #[test]
    fn parts_content_round_trips_through_serde() {
        let wire = serde_json::json!({
            "role": "user",
            "content": [
                {"type":"text","text":"hi"},
                {"type":"image_url","image_url":{"url":"data:image/png;base64,xyz"}},
            ],
        });
        let m: ChatMessage = serde_json::from_value(wire).unwrap();
        match m.content {
            Some(MessageContent::Parts(parts)) => {
                assert_eq!(parts.len(), 2);
            }
            other => panic!("expected Parts, got {other:?}"),
        }
    }

    struct RunnerResolver(Vec<std::net::SocketAddr>);

    impl Resolver for RunnerResolver {
        fn resolve(&self, _host: &str, _port: u16) -> std::io::Result<Vec<std::net::SocketAddr>> {
            Ok(self.0.clone())
        }
    }

    #[test]
    fn trusted_runner_rejects_dns_answers_outside_host_approval() {
        let resolver = RunnerResolver(vec!["198.51.100.5:8000".parse().unwrap()]);
        let result = trusted_runner_endpoint(
            "http://model.home.arpa:8000/v1",
            &["10.20.0.7".parse().unwrap()],
            None,
            &resolver,
        );
        assert!(matches!(result, Err(PolicyError::UnapprovedAddress(_))));
    }

    #[test]
    fn trusted_runner_resolves_and_pins_only_private_host_gateway_addresses() {
        let resolver = RunnerResolver(vec!["172.18.0.1:8101".parse().unwrap()]);
        let (url, addresses) = trusted_runner_endpoint(
            "http://host.docker.internal:8101/v1",
            &[],
            Some("host.docker.internal"),
            &resolver,
        )
        .unwrap();
        assert_eq!(url.host_str(), Some("host.docker.internal"));
        assert_eq!(addresses, vec!["172.18.0.1".parse::<IpAddr>().unwrap()]);

        let public_resolver = RunnerResolver(vec!["8.8.8.8:8101".parse().unwrap()]);
        assert!(matches!(
            trusted_runner_endpoint(
                "http://host.docker.internal:8101/v1",
                &[],
                Some("host.docker.internal"),
                &public_resolver,
            ),
            Err(PolicyError::UnapprovedAddress(_))
        ));
    }
}
