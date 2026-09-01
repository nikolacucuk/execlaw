//! Built-in `graphiti` tool.
//!
//! Thin bridge for a Graphiti service endpoint. This keeps Graphiti
//! integration architecture-native (tool registry + policy gates) and
//! lets local/remote models call temporal-memory APIs without custom
//! plugin wiring.

use async_trait::async_trait;
use execlaw_core::tool::{ToolCtx, ToolDescriptor, ToolImpl, ToolLatency, ToolOutcome, ToolSource};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;

const TIMEOUT_SECS: u64 = 30;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum GraphitiAction {
    Status,
    IngestEpisode,
    Search,
    RawRequest,
}

#[derive(Debug, Deserialize)]
struct GraphitiArgs {
    action: GraphitiAction,
    #[serde(default)]
    base_url: Option<String>,
    #[serde(default)]
    api_key: Option<String>,
    #[serde(default)]
    group_id: Option<String>,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    episode_body: Option<Value>,
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    top_k: Option<usize>,
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    body: Option<Value>,
}

pub struct GraphitiTool {
    descriptor: ToolDescriptor,
}

impl GraphitiTool {
    pub fn new() -> Self {
        Self {
            descriptor: ToolDescriptor {
                name: "graphiti".into(),
                description: "Query or ingest temporal memory via a Graphiti-compatible HTTP service. Supports status, ingest_episode, search, and raw_request.".into(),
                schema: json!({
                    "type": "object",
                    "properties": {
                        "action": {
                            "type": "string",
                            "enum": ["status", "ingest_episode", "search", "raw_request"]
                        },
                        "base_url": { "type": "string", "description": "Override Graphiti base URL" },
                        "api_key": { "type": "string", "description": "Override Graphiti API key" },
                        "group_id": { "type": "string" },
                        "source": { "type": "string" },
                        "episode_body": { "type": "object" },
                        "query": { "type": "string" },
                        "top_k": { "type": "integer", "minimum": 1, "maximum": 100 },
                        "method": { "type": "string" },
                        "path": { "type": "string" },
                        "body": { "type": ["object", "array", "string", "number", "boolean", "null"] }
                    },
                    "required": ["action"],
                    "additionalProperties": false
                }),
                source: ToolSource::Builtin,
                latency: ToolLatency::High,
                capabilities: vec![],
                default_allowed_classes: vec![
                    "Controller".into(),
                    "Delegated".into(),
                    "KnownTrusted".into(),
                    "KnownLimited".into(),
                ],
                sensitive: false,
            },
        }
    }
}

fn env_or_default_base_url() -> String {
    std::env::var("EXECLAW_GRAPHITI_BASE_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:8000".to_owned())
}

fn env_api_key() -> Option<String> {
    std::env::var("EXECLAW_GRAPHITI_API_KEY").ok()
}

fn strip_trailing_slash(s: &str) -> String {
    s.trim_end_matches('/').to_owned()
}

#[async_trait]
impl ToolImpl for GraphitiTool {
    fn descriptor(&self) -> &ToolDescriptor {
        &self.descriptor
    }

    async fn invoke(&self, ctx: ToolCtx, args: Value) -> ToolOutcome {
        let _caller_trust = ctx.caller_trust;

        invoke_graphiti(args).await
    }
}

/// Execute a Graphiti request using the same validation + transport
/// path as the model-callable `graphiti` tool.
pub async fn invoke_graphiti(args: Value) -> ToolOutcome {
    let parsed: GraphitiArgs = match serde_json::from_value(args) {
        Ok(v) => v,
        Err(e) => return ToolOutcome::err("invalid_argument", e.to_string()),
    };

    let base_url = strip_trailing_slash(
        parsed
            .base_url
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or(&env_or_default_base_url()),
    );
    let api_key = parsed
        .api_key
        .filter(|s| !s.is_empty())
        .or_else(env_api_key);

    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(TIMEOUT_SECS))
        .build()
    {
        Ok(c) => c,
        Err(e) => return ToolOutcome::err("client_build_failed", e.to_string()),
    };

    let (method, path, body) = match parsed.action {
        GraphitiAction::Status => (reqwest::Method::GET, "/health".to_owned(), None),
        GraphitiAction::IngestEpisode => {
            let payload = parsed.episode_body.unwrap_or_else(|| {
                json!({
                    "group_id": parsed.group_id,
                    "source": parsed.source.unwrap_or_else(|| "execlaw".to_owned()),
                })
            });
            (reqwest::Method::POST, "/episodes".to_owned(), Some(payload))
        }
        GraphitiAction::Search => {
            let query = match parsed.query {
                Some(q) if !q.trim().is_empty() => q,
                _ => {
                    return ToolOutcome::err(
                        "invalid_argument",
                        "action=search requires non-empty `query`",
                    );
                }
            };
            let payload = json!({
                "group_id": parsed.group_id,
                "query": query,
                "top_k": parsed.top_k.unwrap_or(8),
            });
            (reqwest::Method::POST, "/search".to_owned(), Some(payload))
        }
        GraphitiAction::RawRequest => {
            let method = parsed
                .method
                .as_deref()
                .and_then(|m| reqwest::Method::from_bytes(m.as_bytes()).ok())
                .unwrap_or(reqwest::Method::POST);
            let path = match parsed.path {
                Some(p) if p.starts_with('/') => p,
                Some(p) => format!("/{p}"),
                None => {
                    return ToolOutcome::err(
                        "invalid_argument",
                        "action=raw_request requires `path`",
                    );
                }
            };
            (method, path, parsed.body)
        }
    };

    let url = format!("{base_url}{path}");
    let mut req = client.request(method, &url);
    if let Some(ref key) = api_key {
        req = req.header("Authorization", format!("Bearer {key}"));
    }
    if let Some(b) = body {
        req = req.json(&b);
    }

    let resp = match req.send().await {
        Ok(r) => r,
        Err(e) => return ToolOutcome::err("request_failed", e.to_string()),
    };

    let status = resp.status().as_u16();
    let text = match resp.text().await {
        Ok(t) => t,
        Err(e) => return ToolOutcome::err("read_response_failed", e.to_string()),
    };

    if status >= 400 {
        return ToolOutcome::err("graphiti_failed", format!("status={status} body={text}"));
    }

    let parsed_json = serde_json::from_str::<Value>(&text).unwrap_or_else(|_| json!({"raw": text}));
    ToolOutcome::ok(json!({
        "ok": true,
        "status": status,
        "base_url": base_url,
        "result": parsed_json,
    }))
}

pub fn graphiti_tools() -> Vec<Arc<dyn ToolImpl>> {
    vec![Arc::new(GraphitiTool::new())]
}
