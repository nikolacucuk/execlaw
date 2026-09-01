//! Built-in `graphify` tool.
//!
//! Gives the model a first-class local entrypoint for Graphify CLI
//! operations so tool-use turns can query/update repository structure
//! without hallucinating an unavailable command.

use async_trait::async_trait;
use execlaw_core::tool::{ToolCtx, ToolDescriptor, ToolImpl, ToolLatency, ToolOutcome, ToolSource};
use serde::Deserialize;
use serde_json::{Value, json};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::process::Command;

const OUTPUT_CAP: usize = 32 * 1024;
const BUILD_TIMEOUT_SECS: u64 = 20 * 60;
const QUERY_TIMEOUT_SECS: u64 = 120;

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum GraphifyAction {
    Build,
    Update,
    Query,
    Path,
    Explain,
}

#[derive(Deserialize)]
struct GraphifyArgs {
    action: GraphifyAction,
    #[serde(default)]
    target_path: Option<String>,
    #[serde(default)]
    wiki: Option<bool>,
    #[serde(default)]
    backend: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    question: Option<String>,
    #[serde(default)]
    from: Option<String>,
    #[serde(default)]
    to: Option<String>,
    #[serde(default)]
    force: Option<bool>,
}

pub struct GraphifyTool {
    descriptor: ToolDescriptor,
}

impl GraphifyTool {
    pub fn new() -> Self {
        Self {
            descriptor: ToolDescriptor {
                name: "graphify".into(),
                description: "Run local Graphify CLI operations for repository graph generation and lookup. Supports: build, update, query, path, explain.".into(),
                schema: json!({
                    "type": "object",
                    "properties": {
                        "action": {
                            "type": "string",
                            "enum": ["build", "update", "query", "path", "explain"]
                        },
                        "target_path": { "type": "string" },
                        "wiki": { "type": "boolean" },
                        "backend": { "type": "string" },
                        "model": { "type": "string" },
                        "question": { "type": "string" },
                        "from": { "type": "string" },
                        "to": { "type": "string" },
                        "force": { "type": "boolean" }
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
                    "UnknownPending".into(),
                ],
                sensitive: false,
            },
        }
    }
}

fn trim_output(s: String) -> String {
    if s.len() <= OUTPUT_CAP {
        return s;
    }
    let cut = s.len() - OUTPUT_CAP;
    format!("[truncated {cut} bytes]\n{}", &s[s.len() - OUTPUT_CAP..])
}

fn graphify_candidates() -> Vec<(String, Vec<String>)> {
    if let Ok(bin) = std::env::var("EXECLAW_GRAPHIFY_BIN") {
        let trimmed = bin.trim();
        if !trimmed.is_empty() {
            return vec![(trimmed.to_owned(), vec![])];
        }
    }

    let mut out = Vec::new();
    let venv_graphify = std::path::Path::new(".venv")
        .join("Scripts")
        .join("graphify.exe");
    if venv_graphify.exists() {
        out.push((venv_graphify.to_string_lossy().to_string(), vec![]));
    }
    out.push(("graphify".to_owned(), vec![]));
    #[cfg(windows)]
    {
        out.push((
            "py".to_owned(),
            vec!["-m".to_owned(), "graphify".to_owned()],
        ));
    }
    out.push((
        "python".to_owned(),
        vec!["-m".to_owned(), "graphify".to_owned()],
    ));
    out
}

#[async_trait]
impl ToolImpl for GraphifyTool {
    fn descriptor(&self) -> &ToolDescriptor {
        &self.descriptor
    }

    async fn invoke(&self, ctx: ToolCtx, args: Value) -> ToolOutcome {
        let _caller_trust = ctx.caller_trust;

        let parsed: GraphifyArgs = match serde_json::from_value(args) {
            Ok(v) => v,
            Err(e) => return ToolOutcome::err("invalid_argument", e.to_string()),
        };

        let target = parsed
            .target_path
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or(".");

        let mut sub_args: Vec<String> = Vec::new();
        let mut env_overrides: Vec<(String, String)> = Vec::new();

        let timeout = match parsed.action {
            GraphifyAction::Build => Duration::from_secs(BUILD_TIMEOUT_SECS),
            _ => Duration::from_secs(QUERY_TIMEOUT_SECS),
        };

        match parsed.action {
            GraphifyAction::Build => {
                sub_args.push(target.to_owned());
                if parsed.wiki.unwrap_or(false) {
                    sub_args.push("--wiki".to_owned());
                }
                if let Some(backend) = parsed.backend.as_deref().filter(|s| !s.is_empty()) {
                    sub_args.push("--backend".to_owned());
                    sub_args.push(backend.to_owned());
                    if backend == "ollama" {
                        env_overrides.push(("OLLAMA_API_KEY".to_owned(), "local".to_owned()));
                        if let Some(model) = parsed.model.as_deref().filter(|s| !s.is_empty()) {
                            env_overrides.push(("OLLAMA_MODEL".to_owned(), model.to_owned()));
                        }
                    }
                }
            }
            GraphifyAction::Update => {
                sub_args.push("update".to_owned());
                sub_args.push(target.to_owned());
                if parsed.force.unwrap_or(false) {
                    sub_args.push("--force".to_owned());
                }
            }
            GraphifyAction::Query => {
                let q = match parsed.question.as_deref().filter(|s| !s.is_empty()) {
                    Some(v) => v,
                    None => {
                        return ToolOutcome::err(
                            "invalid_argument",
                            "action=query requires non-empty `question`",
                        );
                    }
                };
                sub_args.push("query".to_owned());
                sub_args.push(q.to_owned());
            }
            GraphifyAction::Path => {
                let from = match parsed.from.as_deref().filter(|s| !s.is_empty()) {
                    Some(v) => v,
                    None => {
                        return ToolOutcome::err(
                            "invalid_argument",
                            "action=path requires non-empty `from`",
                        );
                    }
                };
                let to = match parsed.to.as_deref().filter(|s| !s.is_empty()) {
                    Some(v) => v,
                    None => {
                        return ToolOutcome::err(
                            "invalid_argument",
                            "action=path requires non-empty `to`",
                        );
                    }
                };
                sub_args.push("path".to_owned());
                sub_args.push(from.to_owned());
                sub_args.push(to.to_owned());
            }
            GraphifyAction::Explain => {
                let q = match parsed.question.as_deref().filter(|s| !s.is_empty()) {
                    Some(v) => v,
                    None => {
                        return ToolOutcome::err(
                            "invalid_argument",
                            "action=explain requires non-empty `question`",
                        );
                    }
                };
                sub_args.push("explain".to_owned());
                sub_args.push(q.to_owned());
            }
        }

        let mut out = None;
        let mut last_spawn_error = None;
        for (program, prefix_args) in graphify_candidates() {
            let mut command = Command::new(&program);
            command.stdin(Stdio::null());
            command.stdout(Stdio::piped()).stderr(Stdio::piped());
            if !prefix_args.is_empty() {
                command.args(prefix_args);
            }
            command.args(&sub_args);
            for (k, v) in &env_overrides {
                command.env(k, v);
            }

            let child = match command.spawn() {
                Ok(c) => c,
                Err(e) => {
                    if e.kind() == std::io::ErrorKind::NotFound {
                        last_spawn_error = Some(format!("{program}: {e}"));
                        continue;
                    }
                    return ToolOutcome::err(
                        "tool_unavailable",
                        format!("failed to execute graphify candidate `{program}`: {e}"),
                    );
                }
            };

            match tokio::time::timeout(timeout, child.wait_with_output()).await {
                Ok(Ok(v)) => {
                    out = Some(v);
                    break;
                }
                Ok(Err(e)) => return ToolOutcome::err("exec_error", e.to_string()),
                Err(_) => {
                    return ToolOutcome::err(
                        "timeout",
                        format!("graphify command exceeded {}s timeout", timeout.as_secs()),
                    );
                }
            }
        }

        let out = match out {
            Some(v) => v,
            None => {
                return ToolOutcome::err(
                    "tool_unavailable",
                    format!(
                        "no graphify executable found (tried graphify and python launchers): {}",
                        last_spawn_error.unwrap_or_else(|| "none".to_owned())
                    ),
                );
            }
        };

        let stdout = trim_output(String::from_utf8_lossy(&out.stdout).to_string());
        let stderr = trim_output(String::from_utf8_lossy(&out.stderr).to_string());

        if !out.status.success() {
            return ToolOutcome::err(
                "graphify_failed",
                format!(
                    "graphify exited with status {:?}\nstdout:\n{}\nstderr:\n{}",
                    out.status.code(),
                    stdout,
                    stderr
                ),
            );
        }

        ToolOutcome::ok(json!({
            "ok": true,
            "status": out.status.code(),
            "stdout": stdout,
            "stderr": stderr,
        }))
    }
}

pub fn graphify_tools() -> Vec<Arc<dyn ToolImpl>> {
    vec![Arc::new(GraphifyTool::new())]
}
