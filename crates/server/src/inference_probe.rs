//! `POST /api/admin/inference/probe` — direct inference diagnostic.
//!
//! Bypasses the entire agent loop (no event log writes, no tool
//! dispatch, no supervisor RPC, no history hydration) and sends a
//! controlled request straight to the resolved inference backend
//! using the same `InferenceClient` the runner uses. Returns the
//! same timing splits the runner emits at `agent::turn_timing`
//! (`open_stream_ms`, `first_chunk_ms`, `decode_ms`,
//! `chunks_per_sec`) plus a preview of the model's response.
//!
//! Use it to localize where the latency in a slow turn lives:
//!
//!   * Probe with a small prompt + no tools → if this is fast,
//!     vLLM and the network are healthy; the slowness is in the
//!     server's prompt assembly or the live tool catalog.
//!   * Probe with the same prompt + `include_tools=true` → if this
//!     is suddenly slow, the live tool catalog is the suspect
//!     (size, schema complexity).
//!   * Probe with tools + `guided_decoding_backend="outlines"` vs
//!     `null` → isolates whether outlines is stalling vLLM prefill
//!     on a complex schema (the chart.render `additionalProperties:
//!     false` case).
//!   * Probe with a large synthetic prompt (e.g. 50 KiB) → tells
//!     you how prefill scales with prompt size on your hardware.
//!
//! The endpoint is auth-gated (Controller-only). It runs the same
//! idle-watchdog pattern the runner uses, so a stalled probe
//! produces the same `runner::turn_loop` heartbeat logs.

use crate::auth_extract::AuthedUser;
use crate::routes::ApiError;
use crate::state::AppState;
use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Json;
use axum::routing::post;
use base64::Engine as _;
use execlaw_core::backends::{BackendPurpose, BackendStore};
use execlaw_inference_api::{
    ChatMessage, ChatRequest, ChatStreamChunk, FunctionDecl, InferenceClient, InferenceEngine,
    ModelId, ToolDeclaration,
};
use futures::StreamExt;
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct InferenceProbeRequest {
    /// System prompt to send. Optional — when absent, a short
    /// fixed string is used so the experiment is deterministic.
    /// Use to test prompt-size sensitivity by sending a large
    /// padded string.
    #[serde(default)]
    pub system_prompt: Option<String>,
    /// User message to send. Optional — defaults to a short
    /// instruction asking for a one-sentence reply.
    #[serde(default)]
    pub user_text: Option<String>,
    /// `max_tokens` cap on the response. Defaults to 100 — small
    /// enough that a working backend completes in under a second,
    /// so a probe that takes longer points clearly at backend
    /// latency rather than just "lots to generate."
    #[serde(default)]
    pub max_tokens: Option<u32>,
    /// Temperature override. Defaults to 0.0 for deterministic
    /// repeat runs.
    #[serde(default)]
    pub temperature: Option<f32>,
    /// When `true`, attach the live agent-callable tool catalogue
    /// (built-ins ∪ plugin tools) to the request. Use to A/B-test
    /// whether the catalogue itself is slowing prefill.
    #[serde(default)]
    pub include_tools: bool,
    /// Override `guided_decoding_backend`. `Some("")` disables.
    /// `None` (default) leaves it unset (default: outlines when
    /// tools are present, otherwise nothing).
    #[serde(default)]
    pub guided_decoding_backend: Option<String>,
    /// Force `tool_choice: "auto"` even without tools. Mostly
    /// useful for testing vLLM's `--enable-auto-tool-choice` flag.
    #[serde(default)]
    pub force_tool_choice_auto: bool,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct InferenceProbeTimings {
    pub open_stream_ms: u64,
    pub first_chunk_ms: u64,
    pub decode_ms: u64,
    pub stream_total_ms: u64,
    pub chunks_per_sec: u64,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct InferenceProbeResponse {
    pub timings: InferenceProbeTimings,
    pub chunks_received: u64,
    pub text_chars: usize,
    pub text_preview: String,
    pub finish_reason: Option<String>,
    pub model: String,
    pub request_body_chars: usize,
    pub tool_count: usize,
    /// Number of distinct tool-call indices emitted by the model; no tools are executed.
    pub tool_calls_observed: usize,
    pub errored: bool,
    /// Anyhow-chain-walked error message when `errored = true`.
    /// Absent on success.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

fn record_tool_calls(indices: &mut std::collections::HashSet<(u32, u32)>, chunk: &ChatStreamChunk) {
    for choice in &chunk.choices {
        for call in &choice.delta.tool_calls {
            indices.insert((choice.index, call.index));
        }
    }
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct ConformanceCheck {
    pub passed: bool,
    pub code: &'static str,
    pub elapsed_ms: Option<u64>,
    pub request_bytes: Option<u64>,
    pub prompt_tokens: Option<u32>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct InferenceConformanceResponse {
    pub model: String,
    pub protocol: &'static str,
    pub text: ConformanceCheck,
    pub streaming: ConformanceCheck,
    pub tools: ConformanceCheck,
    pub structured_json: ConformanceCheck,
    pub context: ConformanceCheck,
    pub vision: ConformanceCheck,
    pub total_elapsed_ms: u64,
}

#[derive(Debug, Deserialize)]
pub struct ModelQualificationRequest {
    pub context_tokens: u32,
}

#[derive(Debug, Serialize)]
pub struct ModelQualificationResponse {
    pub identity: execlaw_core::harness::ModelIdentity,
    pub qualified: bool,
    pub context_tokens: u32,
    pub qualified_at: Option<i64>,
    pub checks: InferenceConformanceResponse,
}

/// Build the qualification identity from the active backend row. Required
/// fields are operator-owned backend metadata; profiles are never keyed only
/// by a friendly model alias.
pub(crate) fn current_model_identity(
    db: &execlaw_core::Database,
    purpose: BackendPurpose,
    model_id: &str,
) -> Option<execlaw_core::harness::ModelIdentity> {
    let row = BackendStore::new(db).get(purpose).ok().flatten()?;
    let quantization = row
        .model_spec_json
        .get("quantization")?
        .as_str()?
        .to_owned();
    let chat_template = row
        .model_spec_json
        .get("chat_template")?
        .as_str()?
        .to_owned();
    let backend_version = row
        .model_spec_json
        .get("backend_version")
        .and_then(serde_json::Value::as_str)
        .or_else(|| {
            row.model_spec_json
                .get("image")
                .and_then(serde_json::Value::as_str)
        })?
        .to_owned();
    if backend_version.trim().is_empty() || backend_version.ends_with(":latest") {
        return None;
    }
    Some(execlaw_core::harness::ModelIdentity {
        model_id: model_id.to_owned(),
        quantization,
        chat_template,
        backend_version,
        parser_version: execlaw_model_adapter::PARSER_IDENTITY.to_owned(),
    })
}

pub async fn qualify_model_handler(
    State(state): State<AppState>,
    user: AuthedUser,
    Json(request): Json<ModelQualificationRequest>,
) -> Result<Json<ModelQualificationResponse>, ApiError> {
    require_controller(&state, &user)?;
    qualify_local_model(&state.db, &state.inference, request.context_tokens)
        .await
        .map(Json)
}

/// Qualify the configured Standard backend from a local database owner context.
///
/// The HTTP handler authenticates the Controller before calling this function.
/// A native CLI can call it directly when it already has access to the local
/// database and must not depend on an older running server process.
///
/// ```no_run
/// # async fn inspect(db: &execlaw_core::Database) {
/// let resolver = execlaw_server::inference_resolver::InferenceResolver::new(None);
/// let result = execlaw_server::inference_probe::qualify_local_model(db, &resolver, 4096).await;
/// assert!(result.is_ok());
/// # }
/// ```
pub async fn qualify_local_model(
    db: &execlaw_core::Database,
    resolver: &crate::inference_resolver::InferenceResolver,
    context_tokens: u32,
) -> Result<ModelQualificationResponse, ApiError> {
    if !(4_096..=262_144).contains(&context_tokens) {
        return Err(ApiError {
            status: StatusCode::BAD_REQUEST,
            code: "invalid_context_budget",
            message: "qualification context must be between 4096 and 262144 tokens".into(),
        });
    }
    let resolved = resolver
        .resolve(db, BackendPurpose::Standard)
        .map(|resolved| resolved.with_workload("background"))
        .ok_or_else(|| ApiError {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "inference_unavailable",
            message: "no inference backend resolvable for Standard purpose".into(),
        })?;
    let identity = current_model_identity(db, BackendPurpose::Standard, &resolved.model_id).ok_or_else(|| ApiError {
        status: StatusCode::BAD_REQUEST,
        code: "model_identity_incomplete",
        message: "Standard backend model_spec_json must include exact quantization, chat_template, and backend_version metadata before qualification".into(),
    })?;
    let mut checks = run_conformance(&resolved.client, &resolved.model_id, context_tokens).await;
    checks.vision = run_vision_matrix(&resolved.client, &resolved.model_id).await;
    // The probe adapts its length until the backend reports a prompt inside
    // the requested window. Record the tested window, rather than the exact
    // token count of the probe text, which varies across tokenizers.
    let qualified_context_tokens = if checks.context.passed {
        context_tokens
    } else {
        0
    };
    let qualified = checks.text.passed
        && checks.streaming.passed
        && checks.tools.passed
        && checks.structured_json.passed
        && checks.context.passed
        && qualified_context_tokens >= 4_096;
    let profile_store = execlaw_core::harness::HarnessStore::new(db);
    let qualified_at = if qualified {
        let at = chrono::Utc::now().timestamp();
        let profile = execlaw_core::harness::ModelCapabilityProfile {
            identity: identity.clone(),
            context_tokens: qualified_context_tokens,
            observed: serde_json::to_value(&checks).map_err(|error| ApiError {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                code: "qualification_encode_error",
                message: error.to_string(),
            })?,
            qualified_at: at,
            invalidated_at: None,
        };
        profile_store
            .save_profile(&profile)
            .map_err(|error| ApiError {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                code: "qualification_persist_error",
                message: error.to_string(),
            })?;
        Some(at)
    } else {
        profile_store
            .invalidate_profile(&identity, chrono::Utc::now().timestamp())
            .map_err(|error| ApiError {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                code: "qualification_invalidate_error",
                message: error.to_string(),
            })?;
        None
    };
    Ok(ModelQualificationResponse {
        identity,
        qualified,
        context_tokens: qualified_context_tokens,
        qualified_at,
        checks,
    })
}

async fn run_vision_matrix(client: &InferenceClient, model: &str) -> ConformanceCheck {
    let started = std::time::Instant::now();
    let cases = [(255, 0, 0, "red"), (0, 0, 255, "blue")];
    let mut passed = 0;
    for (red, green, blue, expected) in cases {
        let mut pixels = Vec::with_capacity(8 * 8 * 4);
        for _ in 0..64 {
            pixels.extend_from_slice(&[red, green, blue, 255]);
        }
        let mut png_bytes = Vec::new();
        let encoded = (|| -> Result<(), String> {
            let mut encoder = png::Encoder::new(&mut png_bytes, 8, 8);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().map_err(|error| error.to_string())?;
            writer
                .write_image_data(&pixels)
                .map_err(|error| error.to_string())?;
            Ok(())
        })();
        if encoded.is_err() {
            continue;
        }
        let image_url = format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(png_bytes)
        );
        let request = ChatRequest {
            model: ModelId(model.to_owned()),
            messages: vec![
                ChatMessage::system(
                    "Identify the single solid color in the attached image. Reply with one color word only.",
                ),
                ChatMessage::user_with_images("What is the color?", [image_url]),
            ],
            tools: None,
            stream: false,
            temperature: Some(0.0),
            max_tokens: Some(16),
            chat_template_kwargs: Some(serde_json::json!({"enable_thinking": false})),
            tool_choice: None,
            response_format: None,
            guided_decoding_backend: None,
        };
        if let Ok(Ok(response)) = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            client.chat_completions(&request),
        )
        .await
        {
            let output = response
                .choices
                .first()
                .and_then(|choice| choice.message.content.as_ref())
                .map(|content| content.as_text().to_lowercase())
                .unwrap_or_default();
            passed += usize::from(output.contains(expected));
        }
    }
    ConformanceCheck {
        passed: passed == 2,
        code: if passed == 2 {
            "2_of_2_images_identified"
        } else {
            "vision_matrix_failed"
        },
        elapsed_ms: Some(started.elapsed().as_millis().min(u64::MAX as u128) as u64),
        request_bytes: None,
        prompt_tokens: None,
    }
}

fn conformance_check(passed: bool, code: &'static str) -> ConformanceCheck {
    ConformanceCheck {
        passed,
        code,
        elapsed_ms: None,
        request_bytes: None,
        prompt_tokens: None,
    }
}

pub(crate) async fn run_conformance(
    client: &InferenceClient,
    model: &str,
    context_tokens: u32,
) -> InferenceConformanceResponse {
    let qualification_started = std::time::Instant::now();
    let protocol = match client.engine {
        InferenceEngine::Ollama => "ollama",
        InferenceEngine::OpenAICompat => "openai_compatible",
    };
    let mut request = ChatRequest {
        model: ModelId(model.to_owned()),
        messages: vec![
            ChatMessage::system("Diagnostic request. Reply with one word."),
            ChatMessage::user("Say READY."),
        ],
        tools: None,
        stream: false,
        temperature: Some(0.0),
        max_tokens: Some(64),
        chat_template_kwargs: Some(serde_json::json!({"enable_thinking": false})),
        tool_choice: None,
        response_format: None,
        guided_decoding_backend: None,
    };
    let text = match tokio::time::timeout(
        std::time::Duration::from_secs(20),
        client.chat_completions(&request),
    )
    .await
    {
        Err(_) => conformance_check(false, "timeout"),
        Ok(Err(_)) => conformance_check(false, "request_failed"),
        Ok(Ok(response)) => {
            let passed = response.choices.iter().any(|choice| {
                choice
                    .message
                    .content
                    .as_ref()
                    .is_some_and(|content| !content.as_text().trim().is_empty())
            });
            conformance_check(passed, if passed { "ok" } else { "no_text" })
        }
    };
    let streaming = match tokio::time::timeout(std::time::Duration::from_secs(20), async {
        let mut stream = client
            .chat_completions_stream(&request)
            .await
            .map_err(|_| ())?;
        let mut has_text = false;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| ())?;
            has_text |= chunk.choices.iter().any(|choice| {
                choice
                    .delta
                    .content
                    .as_ref()
                    .is_some_and(|text| !text.trim().is_empty())
            });
        }
        Ok::<_, ()>(has_text)
    })
    .await
    {
        Err(_) => conformance_check(false, "timeout"),
        Ok(Err(())) => conformance_check(false, "stream_failed"),
        Ok(Ok(passed)) => conformance_check(passed, if passed { "ok" } else { "no_stream_text" }),
    };
    request.messages = vec![
        ChatMessage::system(
            "Diagnostic request. Call only the declared probe_noop tool with empty JSON arguments. Do not answer with text.",
        ),
        ChatMessage::user("Call probe_noop now."),
    ];
    request.tools = Some(vec![ToolDeclaration::function(
        "probe_noop",
        "Diagnostic no-op; this tool is never executed.",
        serde_json::json!({"type": "object", "properties": {}, "additionalProperties": false}),
    )]);
    request.tool_choice = Some(serde_json::Value::String("auto".into()));
    let tools = match tokio::time::timeout(
        std::time::Duration::from_secs(20),
        client.chat_completions(&request),
    )
    .await
    {
        Err(_) => conformance_check(false, "timeout"),
        Ok(Err(_)) => conformance_check(false, "request_failed"),
        Ok(Ok(response)) => {
            let passed = response
                .choices
                .iter()
                .flat_map(|choice| &choice.message.tool_calls)
                .any(|call| {
                    call.function.name == "probe_noop"
                        && serde_json::from_str::<serde_json::Value>(&call.function.arguments)
                            .is_ok_and(|arguments| jsonschema::validator_for(
                                &serde_json::json!({"type":"object","properties":{},"additionalProperties":false})
                            ).is_ok_and(|validator| validator.is_valid(&arguments)))
                });
            conformance_check(passed, if passed { "ok" } else { "no_valid_tool_call" })
        }
    };
    let structured_json = run_structured_matrix(client, model).await;
    let context = run_context_probe(client, model, context_tokens).await;
    InferenceConformanceResponse {
        model: model.to_owned(),
        protocol,
        text,
        streaming,
        tools,
        structured_json,
        context,
        // A text protocol probe does not establish vision understanding. Keep
        // the capability unavailable until a model-specific image fixture is qualified.
        vision: conformance_check(false, "vision_fixture_not_qualified"),
        total_elapsed_ms: qualification_started
            .elapsed()
            .as_millis()
            .min(u64::MAX as u128) as u64,
    }
}

async fn run_structured_matrix(client: &InferenceClient, model: &str) -> ConformanceCheck {
    let started = std::time::Instant::now();
    let cases = [
        (
            "status",
            serde_json::json!({"type":"object","properties":{"status":{"type":"string","enum":["READY"]}},"required":["status"],"additionalProperties":false}),
            "Return status READY.",
        ),
        (
            "count",
            serde_json::json!({"type":"object","properties":{"count":{"type":"integer","minimum":2,"maximum":2}},"required":["count"],"additionalProperties":false}),
            "Return count 2.",
        ),
        (
            "labels",
            serde_json::json!({"type":"array","items":{"type":"string","enum":["local"]},"minItems":1,"maxItems":1}),
            "Return the one-item array [local].",
        ),
    ];
    let mut passed = 0;
    for (name, schema, instruction) in cases {
        let request = ChatRequest {
            model: ModelId(model.to_owned()),
            messages: vec![
                ChatMessage::system("Follow the response schema exactly."),
                ChatMessage::user(instruction),
            ],
            tools: None,
            stream: false,
            temperature: Some(0.0),
            max_tokens: Some(64),
            chat_template_kwargs: Some(serde_json::json!({"enable_thinking": false})),
            tool_choice: None,
            response_format: Some(serde_json::json!({
                "type":"json_schema",
                "json_schema":{"name":name,"strict":true,"schema":schema}
            })),
            guided_decoding_backend: Some("outlines".into()),
        };
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            client.chat_completions(&request),
        )
        .await;
        let valid = matches!(result, Ok(Ok(ref response)) if response.choices.first()
            .and_then(|choice| choice.message.content.as_ref())
            .and_then(|content| serde_json::from_str::<serde_json::Value>(&content.as_text()).ok())
            .is_some_and(|value| jsonschema::validator_for(&schema).is_ok_and(|validator| validator.is_valid(&value))));
        passed += usize::from(valid);
    }
    ConformanceCheck {
        passed: passed == 3,
        code: if passed == 3 {
            "3_of_3_valid"
        } else {
            "structured_schema_matrix_failed"
        },
        elapsed_ms: Some(started.elapsed().as_millis().min(u64::MAX as u128) as u64),
        request_bytes: None,
        prompt_tokens: None,
    }
}

async fn run_context_probe(
    client: &InferenceClient,
    model: &str,
    context_tokens: u32,
) -> ConformanceCheck {
    let started = std::time::Instant::now();
    let mut target_chars = (context_tokens as usize)
        .saturating_mul(6)
        .clamp(256, 2_000_000);
    let mut last_tokens = None;
    let mut last_request_bytes = None;
    for _attempt in 0..3 {
        let probe = "context-probe ".repeat(target_chars / 14);
        let request = ChatRequest {
            model: ModelId(model.to_owned()),
            messages: vec![
                ChatMessage::system("Read the probe and reply OK."),
                ChatMessage::user(probe),
            ],
            tools: None,
            stream: false,
            temperature: Some(0.0),
            max_tokens: Some(64),
            chat_template_kwargs: Some(serde_json::json!({"enable_thinking": false})),
            tool_choice: None,
            response_format: None,
            guided_decoding_backend: None,
        };
        let request_bytes = serde_json::to_vec(&request)
            .map(|bytes| bytes.len() as u64)
            .unwrap_or(0);
        last_request_bytes = Some(request_bytes);
        let response = match tokio::time::timeout(
            std::time::Duration::from_secs(60),
            client.chat_completions(&request),
        )
        .await
        {
            Err(_) => {
                return context_probe_result(
                    false,
                    "context_probe_timeout",
                    started,
                    last_request_bytes,
                    last_tokens,
                );
            }
            Ok(Err(_)) => {
                return context_probe_result(
                    false,
                    "context_probe_rejected",
                    started,
                    last_request_bytes,
                    last_tokens,
                );
            }
            Ok(Ok(response)) => response,
        };
        let Some(usage) = response.usage.as_ref() else {
            return context_probe_result(
                false,
                "context_probe_missing_usage",
                started,
                last_request_bytes,
                None,
            );
        };
        let prompt_tokens = usage.prompt_tokens;
        last_tokens = Some(prompt_tokens);
        let in_range = prompt_tokens >= context_tokens.saturating_mul(3) / 4
            && prompt_tokens.saturating_add(64) <= context_tokens;
        if in_range {
            let has_text = response.choices.iter().any(|choice| {
                choice
                    .message
                    .content
                    .as_ref()
                    .is_some_and(|content| !content.as_text().trim().is_empty())
            });
            return context_probe_result(
                has_text,
                if has_text {
                    "context_probe_passed"
                } else {
                    "context_probe_no_text"
                },
                started,
                last_request_bytes,
                last_tokens,
            );
        }
        target_chars = next_context_probe_chars(target_chars, prompt_tokens, context_tokens);
    }
    context_probe_result(
        false,
        "context_probe_token_count_mismatch",
        started,
        last_request_bytes,
        last_tokens,
    )
}

fn next_context_probe_chars(
    current_chars: usize,
    observed_tokens: u32,
    context_tokens: u32,
) -> usize {
    let target_tokens = u64::from(context_tokens).saturating_mul(85) / 100;
    (current_chars as u64)
        .saturating_mul(target_tokens)
        .checked_div(u64::from(observed_tokens.max(1)))
        .unwrap_or(2_000_000)
        .clamp(256, 2_000_000) as usize
}

fn context_probe_result(
    passed: bool,
    code: &'static str,
    started: std::time::Instant,
    request_bytes: Option<u64>,
    prompt_tokens: Option<u32>,
) -> ConformanceCheck {
    ConformanceCheck {
        passed,
        code,
        elapsed_ms: Some(started.elapsed().as_millis().min(u64::MAX as u128) as u64),
        request_bytes,
        prompt_tokens,
    }
}

#[utoipa::path(
    post,
    path = "/api/admin/inference/conformance",
    responses(
        (status = 200, description = "Local inference protocol conformance; no tools dispatched", body = InferenceConformanceResponse),
        (status = 503, description = "No inference backend configured"),
    ),
    security(("bearer_jwt" = [])),
    tag = "diagnostics"
)]
pub async fn inference_conformance_handler(
    State(state): State<AppState>,
    user: AuthedUser,
) -> Result<Json<InferenceConformanceResponse>, ApiError> {
    require_controller(&state, &user)?;
    let resolved = state
        .inference
        .resolve(&state.db, BackendPurpose::Standard)
        .map(|resolved| resolved.with_workload("background"))
        .ok_or_else(|| ApiError {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "inference_unavailable",
            message: "no inference backend resolvable for Standard purpose".into(),
        })?;
    Ok(Json(
        run_conformance(&resolved.client, &resolved.model_id, 4096).await,
    ))
}

#[utoipa::path(
    post,
    path = "/api/admin/inference/probe",
    request_body = InferenceProbeRequest,
    responses(
        (status = 200, description = "Probe completed (success OR caught error in `errored`)", body = InferenceProbeResponse),
        (status = 503, description = "No inference backend configured"),
    ),
    security(("bearer_jwt" = [])),
    tag = "diagnostics"
)]
pub async fn inference_probe_handler(
    State(state): State<AppState>,
    user: AuthedUser,
    Json(req): Json<InferenceProbeRequest>,
) -> Result<Json<InferenceProbeResponse>, ApiError> {
    // 2026-05-16 — fix #7: enforce the Controller-only contract the
    // module doc claims. Pre-fix the handler bound `_user` and
    // discarded the role, so any authenticated user (including a
    // future KnownLimited admin / read-only viewer role) could
    // dispatch arbitrary prompts at the inference backend, see the
    // raw model output, and stress-test prompt-prefill latency
    // — none of which a non-Controller principal should be able to do.
    // Mirrors the same `UserStore::get_by_id`-based role check every
    // other admin route uses (`alerts`, `backends`, `mcp_admin`,
    // `factory_reset`, `personality`, `oauth_admin`,
    // `plugin_settings_admin`).
    require_controller(&state, &user)?;

    let resolved = state
        .inference
        .resolve(&state.db, BackendPurpose::Standard)
        .map(|resolved| resolved.with_workload("background"))
        .ok_or_else(|| ApiError {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "inference_unavailable",
            message: "no inference backend resolvable for `Standard` purpose".into(),
        })?;
    let client = resolved.client;
    let model_id = resolved.model_id;

    let system_prompt = req
        .system_prompt
        .unwrap_or_else(|| "You are a diagnostic probe target. Respond concisely.".to_owned());
    let user_text = req
        .user_text
        .unwrap_or_else(|| "Say hello in one short sentence.".to_owned());

    // Live tool catalog when requested. Built from the same
    // registry the runner sees so timing comparisons are faithful.
    let tools: Option<Vec<ToolDeclaration>> = if req.include_tools {
        let registry = state.plugin_host.registry();
        let mut decls: Vec<ToolDeclaration> = registry
            .all_builtins()
            .iter()
            .map(|t| {
                let d = t.descriptor();
                ToolDeclaration::function(d.name.clone(), d.description.clone(), d.schema.clone())
            })
            .collect();
        decls.extend(registry.agent_callable_tools().iter().map(|t| {
            let description = t.description.clone().unwrap_or_else(|| {
                format!(
                    "Plugin tool '{}' from '{}' (latency: {}).",
                    t.tool_name, t.plugin_id, t.latency,
                )
            });
            let schema = t
                .schema_json
                .clone()
                .unwrap_or_else(|| serde_json::json!({"type": "object"}));
            ToolDeclaration {
                kind: "function".into(),
                function: FunctionDecl {
                    name: t.tool_name.clone(),
                    description,
                    parameters: schema,
                },
            }
        }));
        Some(decls)
    } else {
        None
    };

    let tool_count = tools.as_ref().map(|v| v.len()).unwrap_or(0);

    // `guided_decoding_backend` override semantics:
    //   * `None` → no override; behave like the runner default
    //     (outlines if tools present, otherwise unset).
    //   * `Some("")` → explicit disable (operator wants to see if
    //     outlines is the stall cause).
    //   * `Some(name)` → use that backend.
    let guided_decoding_backend = match (&req.guided_decoding_backend, tools.is_some()) {
        (Some(v), _) => {
            let trimmed = v.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_owned())
            }
        }
        (None, true) => Some("outlines".to_owned()),
        (None, false) => None,
    };

    let tool_choice = if tools.is_some() || req.force_tool_choice_auto {
        Some(serde_json::Value::String("auto".to_owned()))
    } else {
        None
    };

    let chat_req = ChatRequest {
        model: ModelId(model_id.clone()),
        messages: vec![
            ChatMessage::system(system_prompt),
            ChatMessage::user(user_text),
        ],
        tools,
        stream: true,
        temperature: req.temperature.or(Some(0.0)),
        max_tokens: req.max_tokens.or(Some(100)),
        chat_template_kwargs: Some(serde_json::json!({"enable_thinking": false})),
        tool_choice,
        guided_decoding_backend,
        response_format: None,
    };
    let request_body_chars = serde_json::to_string(&chat_req)
        .map(|s| s.chars().count())
        .unwrap_or(0);

    // Run the stream with the same idle-watchdog pattern the
    // runner uses so probe stalls produce the same heartbeat
    // logs at `runner::turn_loop`.
    let round_started_at = std::time::Instant::now();
    let stream_result = client.chat_completions_stream(&chat_req).await;
    let mut stream = match stream_result {
        Ok(s) => s,
        Err(e) => {
            return Ok(Json(InferenceProbeResponse {
                timings: InferenceProbeTimings {
                    open_stream_ms: round_started_at.elapsed().as_millis() as u64,
                    first_chunk_ms: 0,
                    decode_ms: 0,
                    stream_total_ms: round_started_at.elapsed().as_millis() as u64,
                    chunks_per_sec: 0,
                },
                chunks_received: 0,
                text_chars: 0,
                text_preview: String::new(),
                finish_reason: None,
                model: model_id,
                request_body_chars,
                tool_count,
                tool_calls_observed: 0,
                errored: true,
                error: Some(format!("stream-open failure: {e:#}")),
            }));
        }
    };
    let open_stream_ms = round_started_at.elapsed().as_millis() as u64;

    let mut text_acc = String::new();
    let mut tool_call_indices = std::collections::HashSet::new();
    let mut chunks_received: u64 = 0;
    let mut finish_reason: Option<String> = None;
    let mut first_chunk_at: Option<std::time::Instant> = None;
    let mut last_chunk_at = std::time::Instant::now();
    let mut errored: Option<String> = None;

    let idle_warn_secs: u64 = std::env::var("EXECLAW_INFERENCE_IDLE_WARN_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(5);
    let mut idle_interval = tokio::time::interval(std::time::Duration::from_secs(idle_warn_secs));
    idle_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    idle_interval.tick().await;

    let probe_id = uuid::Uuid::new_v4().to_string();
    loop {
        tokio::select! {
            biased;
            maybe_chunk = stream.next() => {
                let Some(chunk) = maybe_chunk else { break; };
                match chunk {
                    Ok(c) => {
                        if first_chunk_at.is_none() {
                            first_chunk_at = Some(std::time::Instant::now());
                        }
                        last_chunk_at = std::time::Instant::now();
                        chunks_received = chunks_received.saturating_add(1);
                        record_tool_calls(&mut tool_call_indices, &c);
                        for choice in &c.choices {
                            if let Some(t) = &choice.delta.content {
                                text_acc.push_str(t);
                            }
                            if let Some(fr) = &choice.finish_reason {
                                finish_reason = Some(fr.clone());
                            }
                        }
                    }
                    Err(e) => {
                        errored = Some(format!("mid-stream read failure: {e:#}"));
                        break;
                    }
                }
            }
            _ = idle_interval.tick() => {
                let idle_ms = last_chunk_at.elapsed().as_millis() as u64;
                tracing::warn!(
                    target: "runner::turn_loop",
                    probe_id = %probe_id,
                    idle_ms,
                    chunks_so_far = chunks_received,
                    text_chars_so_far = text_acc.chars().count(),
                    first_chunk_seen = first_chunk_at.is_some(),
                    stall_phase = if first_chunk_at.is_some() { "decode" } else { "prefill" },
                    "inference probe idle — no chunks arrived in the last interval"
                );
            }
        }
    }
    drop(stream);

    let stream_total_ms = round_started_at.elapsed().as_millis() as u64;
    let first_chunk_ms = first_chunk_at
        .map(|t| t.duration_since(round_started_at).as_millis() as u64)
        .unwrap_or(0);
    let decode_ms = if first_chunk_at.is_some() {
        stream_total_ms.saturating_sub(first_chunk_ms)
    } else {
        0
    };
    let chunks_per_sec = if decode_ms > 0 {
        (chunks_received as f64 * 1000.0 / decode_ms as f64) as u64
    } else {
        0
    };

    let text_preview: String = text_acc.chars().take(512).collect();
    let text_chars = text_acc.chars().count();

    Ok(Json(InferenceProbeResponse {
        timings: InferenceProbeTimings {
            open_stream_ms,
            first_chunk_ms,
            decode_ms,
            stream_total_ms,
            chunks_per_sec,
        },
        chunks_received,
        text_chars,
        text_preview,
        finish_reason,
        model: model_id,
        request_body_chars,
        tool_count,
        tool_calls_observed: tool_call_indices.len(),
        errored: errored.is_some(),
        error: errored,
    }))
}

/// 2026-05-16 — fix #7: Controller-only role check. Same shape as
/// `alerts::require_controller`, `backends::require_controller`,
/// etc. Duplicated here rather than lifted to a shared module so
/// fix #7's diff stays minimal; a follow-up can consolidate the
/// seven copies of this helper at once.
fn require_controller(state: &AppState, user: &AuthedUser) -> Result<(), ApiError> {
    use execlaw_core::users::{UserRole, UserStore};
    let row = UserStore::new(&state.db)
        .get_by_id(&user.user_id)
        .map_err(|e| ApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "db_error",
            message: e.to_string(),
        })?;
    match row.map(|u| u.role) {
        Some(UserRole::Controller) => Ok(()),
        _ => Err(ApiError {
            status: StatusCode::FORBIDDEN,
            code: "controller_only",
            message: "only a Controller can run the inference probe".into(),
        }),
    }
}

pub fn inference_probe_router() -> Router<AppState> {
    Router::new()
        .route("/api/admin/inference/probe", post(inference_probe_handler))
        .route(
            "/api/admin/inference/conformance",
            post(inference_conformance_handler),
        )
        .route("/api/admin/inference/qualify", post(qualify_model_handler))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes::{build_router, test_app_state};
    use axum::body::Body;
    use axum::http::{HeaderValue, Method, Request, header};
    use axum::response::IntoResponse;
    use execlaw_core::backends::{BackendMode, BackendUpsert};
    use execlaw_core::users::{UserRole, UserRow, UserStore};
    use std::sync::atomic::{AtomicBool, Ordering};
    use tower::ServiceExt;

    #[test]
    fn context_probe_resizes_from_observed_token_usage() {
        let initial = 4_096 * 6;
        let grown = next_context_probe_chars(initial, 2_050, 4_096);
        assert!(grown > initial);
        assert!(grown < 2_000_000);
        assert!(next_context_probe_chars(initial, 5_000, 4_096) < initial);
    }

    async fn openai_fixture(Json(request): Json<serde_json::Value>) -> axum::response::Response {
        if let Some(schema) = request.pointer("/response_format/json_schema/schema") {
            let content = if schema["type"] == "array" {
                "[\"local\"]"
            } else if schema["properties"]["status"].is_object() {
                "{\"status\":\"READY\"}"
            } else {
                "{\"count\":2}"
            };
            return Json(serde_json::json!({"id":"probe","model":"local-test","choices":[{"index":0,"message":{"role":"assistant","content":content},"finish_reason":"stop"}]})).into_response();
        }
        if request.get("tools").is_some() {
            return Json(serde_json::json!({
                "id": "probe", "model": "local-test", "choices": [{"index": 0,
                    "message": {"role": "assistant", "content": null, "tool_calls": [{
                        "id": "call-probe", "type": "function",
                        "function": {"name": "probe_noop", "arguments": "{}"}
                    }]}, "finish_reason": "tool_calls"}]
            }))
            .into_response();
        }
        if request["stream"] == true {
            return ([(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                "data: {\"id\":\"probe\",\"model\":\"local-test\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"READY\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n")
                .into_response();
        }
        let prompt_tokens = if request["messages"].to_string().len() > 10_000 {
            3500
        } else {
            8
        };
        Json(serde_json::json!({"id": "probe", "model": "local-test", "usage":{"prompt_tokens":prompt_tokens,"completion_tokens":1}, "choices": [{
            "index": 0, "message": {"role": "assistant", "content": "READY"}, "finish_reason": "stop"
        }]})).into_response()
    }

    async fn ollama_fixture(Json(request): Json<serde_json::Value>) -> axum::response::Response {
        if let Some(schema) = request.get("format").filter(|format| format.is_object()) {
            let content = if schema["type"] == "array" {
                "[\"local\"]"
            } else if schema["properties"]["status"].is_object() {
                "{\"status\":\"READY\"}"
            } else {
                "{\"count\":2}"
            };
            return Json(serde_json::json!({"model":"local-test","message":{"role":"assistant","content":content},"done":true,"prompt_eval_count":8,"eval_count":1})).into_response();
        }
        if request.get("tools").is_some() {
            return Json(serde_json::json!({"model": "local-test", "message": {
                "role": "assistant", "content": "", "tool_calls": [{
                    "id": "call-probe", "function": {"name": "probe_noop", "arguments": {}}
                }]}, "done": true, "done_reason": "stop"}))
            .into_response();
        }
        if request["stream"] == true {
            return ([(axum::http::header::CONTENT_TYPE, "application/x-ndjson")],
                "{\"model\":\"local-test\",\"message\":{\"role\":\"assistant\",\"content\":\"READY\"},\"done\":true}\n")
                .into_response();
        }
        let prompt_eval_count = if request["messages"].to_string().len() > 10_000 {
            3500
        } else {
            8
        };
        Json(serde_json::json!({"model": "local-test", "message": {"role": "assistant", "content": "READY"}, "done": true,"prompt_eval_count":prompt_eval_count,"eval_count":1})).into_response()
    }

    #[tokio::test]
    async fn conformance_checks_text_stream_and_noop_tool_on_both_protocols() {
        let router = Router::new()
            .route("/v1/chat/completions", post(openai_fixture))
            .route("/api/chat", post(ollama_fixture));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(axum::serve(listener, router).into_future());
        for engine in [InferenceEngine::OpenAICompat, InferenceEngine::Ollama] {
            let url = if engine == InferenceEngine::Ollama {
                format!("http://{address}")
            } else {
                format!("http://{address}/v1")
            };
            let client = InferenceClient::new(url).with_engine(engine);
            let result = run_conformance(&client, "local-test", 4096).await;
            assert!(
                result.text.passed,
                "{} text: {}",
                result.protocol, result.text.code
            );
            assert!(
                result.streaming.passed,
                "{} streaming: {}",
                result.protocol, result.streaming.code
            );
            assert!(
                result.tools.passed,
                "{} tools: {}",
                result.protocol, result.tools.code
            );
        }
        server.abort();
    }

    #[tokio::test]
    async fn conformance_reports_missing_tool_call_without_dispatching() {
        async fn text_only(_: Json<serde_json::Value>) -> Json<serde_json::Value> {
            Json(
                serde_json::json!({"id": "probe", "model": "local-test", "choices": [{
                    "index": 0, "message": {"role": "assistant", "content": "READY"}, "finish_reason": "stop"
                }]}),
            )
        }
        let router = Router::new().route("/v1/chat/completions", post(text_only));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(axum::serve(listener, router).into_future());
        let client = InferenceClient::new(format!("http://{address}/v1"));
        let result = run_conformance(&client, "local-test", 4096).await;
        assert!(!result.tools.passed);
        assert_eq!(result.tools.code, "no_valid_tool_call");
        server.abort();
    }

    #[test]
    fn tool_call_fragments_count_once_per_choice_and_index() {
        let first: ChatStreamChunk = serde_json::from_value(serde_json::json!({
            "id": "probe", "model": "local", "choices": [{
                "index": 0, "delta": {"tool_calls": [{"index": 0, "id": "call-1", "function": {"name": "probe_noop"}}]}
            }]
        })).unwrap();
        let continuation: ChatStreamChunk = serde_json::from_value(serde_json::json!({
            "id": "probe", "model": "local", "choices": [{
                "index": 0, "delta": {"tool_calls": [{"index": 0, "function": {"arguments": "{}"}}]}
            }]
        }))
        .unwrap();
        let mut indices = std::collections::HashSet::new();
        record_tool_calls(&mut indices, &first);
        record_tool_calls(&mut indices, &continuation);
        assert_eq!(indices.len(), 1);
    }

    /// Helper mirroring `skills_admin::tests::seed_user_and_token`.
    /// Inserts a user with the given role and mints an access token.
    async fn seed_user_and_token(role: UserRole) -> (axum::Router, String) {
        seed_user_and_token_for_state(test_app_state(), role)
    }

    fn seed_user_and_token_for_state(state: AppState, role: UserRole) -> (axum::Router, String) {
        UserStore::new(&state.db)
            .insert(&UserRow {
                user_id: "u-probe-test".into(),
                username: "probe-tester".into(),
                display_name: "Probe Tester".into(),
                email: None,
                password_hash: "argon2-placeholder".into(),
                role,
                created_at: 0,
                last_login_at: None,
            })
            .expect("insert user");
        let token = state
            .signer
            .issue_access_token("u-probe-test", "session-test", 600)
            .expect("issue token");
        state
            .refresh_store
            .issue(
                "u-probe-test",
                "session-test",
                state.config.refresh_token_ttl_secs,
            )
            .expect("persist test session");
        (build_router(state), format!("Bearer {token}"))
    }

    #[tokio::test]
    async fn qualification_persists_exact_identity_and_failed_recheck_invalidates_it() {
        async fn conditional_fixture(
            State(fail_tools): State<std::sync::Arc<AtomicBool>>,
            Json(request): Json<serde_json::Value>,
        ) -> axum::response::Response {
            if fail_tools.load(Ordering::SeqCst) && request.get("tools").is_some() {
                return Json(serde_json::json!({
                    "id":"probe", "model":"local-test", "choices":[{
                        "index":0, "message":{"role":"assistant","content":"READY"},
                        "finish_reason":"stop"
                    }]
                }))
                .into_response();
            }
            openai_fixture(Json(request)).await
        }

        let fail_tools = std::sync::Arc::new(AtomicBool::new(false));
        let fixture = Router::new()
            .route("/v1/chat/completions", post(conditional_fixture))
            .with_state(fail_tools.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let fixture_server = tokio::spawn(axum::serve(listener, fixture).into_future());

        let state = test_app_state();
        BackendStore::new(&state.db)
            .upsert(
                &BackendUpsert {
                    purpose: BackendPurpose::Standard,
                    inference_backend: "service-vllm".into(),
                    model_spec_json: serde_json::json!({
                        "model":"local-test", "quantization":"Q4",
                        "chat_template":"test-template-v1", "backend_version":"fixture-v1"
                    }),
                    gpu_id: None,
                    endpoint: Some(format!("http://{address}/v1")),
                    notes: None,
                    reasoning_enabled: false,
                    mode: BackendMode::External,
                },
                chrono::Utc::now().timestamp(),
            )
            .unwrap();
        let db = state.db.clone();
        let (app, bearer) = seed_user_and_token_for_state(state, UserRole::Controller);
        let request = || {
            Request::builder()
                .method(Method::POST)
                .uri("/api/admin/inference/qualify")
                .header(header::AUTHORIZATION, &bearer)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"context_tokens":4096}"#))
                .unwrap()
        };
        let response = app.clone().oneshot(request()).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let result: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(result["qualified"], true, "{result}");
        let identity: execlaw_core::harness::ModelIdentity =
            serde_json::from_value(result["identity"].clone()).unwrap();
        assert_eq!(identity.chat_template, "test-template-v1");
        assert!(
            execlaw_core::harness::HarnessStore::new(&db)
                .get_profile(&identity)
                .unwrap()
                .is_some()
        );

        fail_tools.store(true, Ordering::SeqCst);
        let response = app.oneshot(request()).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let result: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(result["qualified"], false, "{result}");
        assert!(
            execlaw_core::harness::HarnessStore::new(&db)
                .get_profile(&identity)
                .unwrap()
                .is_none()
        );
        fixture_server.abort();
    }

    /// 2026-05-16 — fix #7: the doc comment says Controller-only, the
    /// handler must actually enforce it. Pre-fix the handler bound
    /// `_user: AuthedUser` and discarded the role — any authenticated
    /// user (including the Operator/Viewer roles that the user-management
    /// flow can mint) could POST to /api/admin/inference/probe and
    /// run arbitrary prompts at the inference backend. With the fix,
    /// non-Controller callers get 403 from `require_controller`.
    #[tokio::test]
    async fn inference_probe_rejects_non_controller_caller() {
        let (app, bearer) = seed_user_and_token(UserRole::Operator).await;
        let req = Request::builder()
            .method(Method::POST)
            .uri("/api/admin/inference/probe")
            .header(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            )
            .header(
                header::AUTHORIZATION,
                HeaderValue::from_str(&bearer).unwrap(),
            )
            .body(Body::from(b"{}".to_vec()))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::FORBIDDEN,
            "Operator role must NOT be able to run the inference probe"
        );
    }

    #[tokio::test]
    async fn conformance_requires_controller_and_a_resolvable_backend() {
        for (role, expected) in [
            (UserRole::Viewer, StatusCode::FORBIDDEN),
            (UserRole::Controller, StatusCode::SERVICE_UNAVAILABLE),
        ] {
            let (app, bearer) = seed_user_and_token(role).await;
            let request = Request::builder()
                .method(Method::POST)
                .uri("/api/admin/inference/conformance")
                .header(
                    header::AUTHORIZATION,
                    HeaderValue::from_str(&bearer).unwrap(),
                )
                .body(Body::empty())
                .unwrap();
            assert_eq!(app.oneshot(request).await.unwrap().status(), expected);
        }
    }

    /// Same coverage for Viewer — anyone below Controller is rejected.
    #[tokio::test]
    async fn inference_probe_rejects_viewer_caller() {
        let (app, bearer) = seed_user_and_token(UserRole::Viewer).await;
        let req = Request::builder()
            .method(Method::POST)
            .uri("/api/admin/inference/probe")
            .header(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            )
            .header(
                header::AUTHORIZATION,
                HeaderValue::from_str(&bearer).unwrap(),
            )
            .body(Body::from(b"{}".to_vec()))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    /// Sanity: a Controller caller passes the role check. The probe
    /// itself will surface 503 (no inference backend configured in
    /// the test fixture), which proves the role check let the
    /// request through — not 403 from `require_controller`.
    #[tokio::test]
    async fn inference_probe_passes_role_check_for_controller() {
        let (app, bearer) = seed_user_and_token(UserRole::Controller).await;
        let req = Request::builder()
            .method(Method::POST)
            .uri("/api/admin/inference/probe")
            .header(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            )
            .header(
                header::AUTHORIZATION,
                HeaderValue::from_str(&bearer).unwrap(),
            )
            .body(Body::from(b"{}".to_vec()))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "Controller passes the role check; test fixture has no inference backend so 503 is the next gate"
        );
    }
}
