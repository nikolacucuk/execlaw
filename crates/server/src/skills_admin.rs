//! Admin HTTP surface for the top-level Skills page (Phase B.3,
//! 2026-05-03).
//!
//! Routes:
//!   * `GET  /api/admin/skills`              — list visible skills
//!   * `GET  /api/admin/skills/{name}`       — full skill detail
//!   * `POST /api/admin/skills/{name}/promote` (controller-only)
//!   * `POST /api/admin/skills/{name}/archive` (controller-only)
//!
//! Backed by `execlaw_skills::SkillStore`. Constructed per-request
//! from the shared `state.db` handle (the store is a thin wrapper
//! around the `Database` so creation is cheap and there's no need to
//! plumb another field through `AppState`).

use crate::auth_extract::AuthedUser;
use crate::routes::ApiError;
use crate::state::AppState;
use axum::Router;
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::StatusCode;
use axum::response::Json;
use axum::routing::{get, post};
use execlaw_core::skills_config::{SkillsConfig, SkillsConfigStore, SkillsConfigUpdate};
use execlaw_skills::{NewSkillVersion, ProposalId, ProposalState, SkillStore, Strictness};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use utoipa::ToSchema;

/// Compact list entry — what the SPA renders in the left rail.
#[derive(Debug, Serialize, ToSchema)]
pub struct SkillListEntry {
    pub name: String,
    pub description: String,
    /// `trial` | `stable` | `archived`
    pub state: String,
    pub version: u32,
    /// `authored` | `shipped` | `registered`
    pub registration_kind: String,
    pub source: String,
    pub owning_plugin_id: Option<String>,
    pub updated_at: i64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SkillListResponse {
    pub skills: Vec<SkillListEntry>,
}

/// Detail view — full body + frontmatter + bundled-resource paths.
#[derive(Debug, Serialize, ToSchema)]
pub struct SkillDetail {
    pub name: String,
    pub description: String,
    pub state: String,
    pub registration_kind: String,
    pub source: String,
    pub owning_plugin_id: Option<String>,
    pub current_version: u32,
    pub body_md: String,
    pub frontmatter_json: String,
    pub authored_by: String,
    pub authored_at: i64,
    pub created_at: i64,
    pub updated_at: i64,
    pub archived_at: Option<i64>,
    pub resource_paths: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    /// When `true`, archived skills appear in the list.
    /// Default `false` — UI shows them via a checkbox toggle.
    #[serde(default)]
    pub include_archived: bool,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct PromoteRequest {
    #[serde(default)]
    pub notes: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct RollbackSkillRequest {
    pub target_version: u32,
}

#[derive(Debug, Deserialize)]
pub struct SkillEvalSuiteRequest {
    pub cases: Vec<SkillEvalCaseInput>,
}

#[derive(Debug, Deserialize)]
pub struct SkillEvalCaseInput {
    pub case_id: String,
    pub prompt: String,
    pub required_terms: Vec<String>,
    #[serde(default)]
    pub forbidden_terms: Vec<String>,
    #[serde(default)]
    pub min_output_chars: usize,
    #[serde(default = "default_skill_eval_max_output_chars")]
    pub max_output_chars: usize,
    #[serde(default = "default_skill_eval_max_output_tokens")]
    pub max_output_tokens: u32,
    #[serde(default)]
    pub workspace_files: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub expected_workspace_files: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub mock_integrations: std::collections::BTreeMap<String, serde_json::Value>,
    #[serde(default)]
    pub expected_integration_calls: Vec<String>,
    #[serde(default)]
    pub forbidden_actions: Vec<String>,
}

fn default_skill_eval_max_output_chars() -> usize {
    4000
}

fn default_skill_eval_max_output_tokens() -> u32 {
    256
}

fn valid_skill_eval_path(path: &str) -> bool {
    let candidate = std::path::Path::new(path);
    !path.is_empty()
        && path.len() <= 240
        && !candidate.is_absolute()
        && candidate
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_)))
        && !path.contains('\\')
        && !path.contains(':')
        && !path.split('/').any(|component| {
            let lower = component.to_ascii_lowercase();
            lower.starts_with('.')
                && [".env", ".ssh", ".git-credentials", ".npmrc", ".pypirc"]
                    .contains(&lower.as_str())
                || ["credentials", "secrets", "id_rsa", "id_ed25519"]
                    .iter()
                    .any(|secret| lower.contains(secret))
        })
}

fn validate_eval_suite(cases: &[SkillEvalCaseInput]) -> Result<(), ApiError> {
    let mut case_ids = std::collections::HashSet::with_capacity(cases.len());
    if cases.is_empty()
        || cases.len() > 20
        || cases.iter().any(|case| {
            let requires_workspace_change = case.expected_workspace_files != case.workspace_files;
            let normalized_terms: std::collections::HashSet<String> = case
                .required_terms
                .iter()
                .map(|term| term.trim().to_lowercase())
                .collect();
            let normalized_forbidden: std::collections::HashSet<String> = case
                .forbidden_terms
                .iter()
                .map(|term| term.trim().to_lowercase())
                .collect();
            case.case_id.trim().is_empty()
                || case.case_id.len() > 64
                || !case_ids.insert(case.case_id.trim().to_owned())
                || case.prompt.trim().is_empty()
                || case.prompt.len() > 4000
                || case.required_terms.is_empty()
                || case.required_terms.len() > 12
                || case
                    .required_terms
                    .iter()
                    .any(|term| term.trim().is_empty() || term.len() > 128)
                || normalized_terms.len() != case.required_terms.len()
                || case.forbidden_terms.len() > 12
                || case
                    .forbidden_terms
                    .iter()
                    .any(|term| term.trim().is_empty() || term.len() > 128)
                || normalized_forbidden.len() != case.forbidden_terms.len()
                || case
                    .required_terms
                    .iter()
                    .any(|term| normalized_forbidden.contains(&term.trim().to_lowercase()))
                || case.min_output_chars > case.max_output_chars
                || case.max_output_chars == 0
                || case.max_output_chars > 20_000
                || case.max_output_tokens == 0
                || case.max_output_tokens > 1024
                || case.workspace_files.len() > 16
                || case.expected_workspace_files.is_empty()
                    && case.expected_integration_calls.is_empty()
                || !requires_workspace_change && case.expected_integration_calls.is_empty()
                || case.mock_integrations.len() > 16
                || case.expected_integration_calls.len() > 32
                || case.forbidden_actions.len() > 32
                || case
                    .workspace_files
                    .iter()
                    .chain(case.expected_workspace_files.iter())
                    .any(|(path, content)| {
                        !valid_skill_eval_path(path) || content.len() > 64 * 1024
                    })
                || case
                    .workspace_files
                    .values()
                    .chain(case.expected_workspace_files.values())
                    .map(String::len)
                    .sum::<usize>()
                    > SKILL_EVAL_MAX_WORKSPACE_BYTES
                || case.mock_integrations.iter().any(|(name, response)| {
                    name.trim().is_empty()
                        || name.len() > 128
                        || serde_json::to_vec(response)
                            .map_or(true, |bytes| bytes.len() > 16 * 1024)
                })
                || case.expected_integration_calls.iter().any(|name| {
                    name.trim().is_empty()
                        || name.len() > 128
                        || !case.mock_integrations.contains_key(name)
                })
                || case
                    .forbidden_actions
                    .iter()
                    .any(|name| name.trim().is_empty() || name.len() > 128)
        })
    {
        return Err(ApiError {
            status: StatusCode::BAD_REQUEST,
            code: "invalid_eval_suite",
            message: "suite cases need bounded task assertions, a mock workspace or integration, and unique IDs"
                .into(),
        });
    }
    Ok(())
}

fn backend_fingerprint(endpoint: &str, model_id: &str, engine: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(endpoint.as_bytes());
    digest.update([0]);
    digest.update(model_id.as_bytes());
    digest.update([0]);
    digest.update(engine.as_bytes());
    hex::encode(digest.finalize())
}

type SkillEvalSuiteHashRow = (
    String,
    String,
    String,
    String,
    usize,
    usize,
    u32,
    String,
    String,
    String,
    String,
    String,
);

fn canonical_suite_hash(cases: &[SkillEvalSuiteHashRow]) -> Result<String, ApiError> {
    let encoded = serde_json::to_vec(cases).map_err(|error| ApiError {
        status: StatusCode::BAD_REQUEST,
        code: "invalid_eval_suite",
        message: error.to_string(),
    })?;
    Ok(hex::encode(Sha256::digest(encoded)))
}

const SKILL_EVAL_MAX_ROUNDS: usize = 8;
const SKILL_EVAL_MAX_FILES: usize = 16;
const SKILL_EVAL_MAX_FILE_BYTES: usize = 64 * 1024;
const SKILL_EVAL_MAX_WORKSPACE_BYTES: usize = 512 * 1024;

#[derive(Debug, Clone)]
struct SkillEvalOutcome {
    passed: bool,
    matched_terms: usize,
    forbidden_matches: usize,
    output_chars: usize,
    workspace_matches: usize,
    expected_workspace_files: usize,
    integration_matches: usize,
    expected_integration_calls: usize,
    forbidden_actions: usize,
    denied_actions: usize,
    action_count: usize,
    output_tokens: u32,
    token_usage_complete: bool,
}

async fn execute_skill_case(
    client: &execlaw_inference_api::InferenceClient,
    model_id: &str,
    skill_body: &str,
    prompt: &str,
    required_terms: &[String],
    forbidden_terms: &[String],
    min_output_chars: usize,
    max_output_chars: usize,
    max_output_tokens: u32,
    initial_files: &std::collections::BTreeMap<String, String>,
    expected_files: &std::collections::BTreeMap<String, String>,
    mock_integrations: &std::collections::BTreeMap<String, serde_json::Value>,
    expected_calls: &[String],
    forbidden_actions: &[String],
) -> Result<SkillEvalOutcome, ApiError> {
    let workspace = tempfile::tempdir().map_err(|error| ApiError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        code: "skill_eval_workspace_failed",
        message: error.to_string(),
    })?;
    for (path, content) in initial_files {
        write_skill_eval_file(workspace.path(), path, content).map_err(|error| ApiError {
            status: StatusCode::BAD_REQUEST,
            code: "invalid_eval_workspace",
            message: error,
        })?;
    }
    read_skill_eval_workspace(workspace.path()).map_err(|error| ApiError {
        status: StatusCode::BAD_REQUEST,
        code: "invalid_eval_workspace",
        message: error,
    })?;
    let tools = vec![
        execlaw_inference_api::ToolDeclaration::function(
            "workspace.read_file",
            "Read a UTF-8 file from this isolated evaluation workspace.",
            serde_json::json!({"type":"object","properties":{"path":{"type":"string"}},"required":["path"],"additionalProperties":false}),
        ),
        execlaw_inference_api::ToolDeclaration::function(
            "workspace.write_file",
            "Replace or create a UTF-8 file in this isolated evaluation workspace.",
            serde_json::json!({"type":"object","properties":{"path":{"type":"string"},"content":{"type":"string"}},"required":["path","content"],"additionalProperties":false}),
        ),
        execlaw_inference_api::ToolDeclaration::function(
            "integration.mock_call",
            "Call a deterministic mock integration. No external request is sent.",
            serde_json::json!({"type":"object","properties":{"name":{"type":"string"},"input":{}},"required":["name"],"additionalProperties":false}),
        ),
    ];
    let mut messages = vec![
        execlaw_inference_api::ChatMessage::system(format!(
            "Apply this skill to the task. Use the supplied workspace and mock integration tools when needed. These tools operate only on an isolated temporary directory and deterministic in-memory mocks. Do not claim that an action happened unless its tool returned success.\n\n{skill_body}"
        )),
        execlaw_inference_api::ChatMessage::user(prompt),
    ];
    let mut actions = Vec::new();
    let mut integration_calls = Vec::new();
    let mut output = String::new();
    let mut completed = false;
    let mut output_tokens = 0u32;
    let mut token_usage_complete = true;
    let mut action_budget_exceeded = false;
    for _round in 0..SKILL_EVAL_MAX_ROUNDS {
        let request = execlaw_inference_api::ChatRequest {
            model: execlaw_inference_api::ModelId(model_id.to_owned()),
            messages: messages.clone(),
            tools: Some(tools.clone()),
            stream: false,
            temperature: Some(0.0),
            max_tokens: Some(max_output_tokens),
            chat_template_kwargs: None,
            tool_choice: Some(serde_json::json!("auto")),
            response_format: None,
            guided_decoding_backend: None,
        };
        let response = tokio::time::timeout(
            std::time::Duration::from_secs(60),
            client.chat_completions(&request),
        )
        .await
        .map_err(|_| ApiError {
            status: StatusCode::GATEWAY_TIMEOUT,
            code: "skill_eval_timeout",
            message: "local skill evaluation exceeded its inference time budget".into(),
        })?
        .map_err(|error| ApiError {
            status: StatusCode::BAD_GATEWAY,
            code: "skill_eval_inference_failed",
            message: format!("local inference failed ({})", error.safe_class()),
        })?;
        let Some(choice) = response.choices.first() else {
            break;
        };
        if let Some(usage) = response.usage.as_ref() {
            output_tokens = output_tokens.saturating_add(usage.completion_tokens);
        } else {
            token_usage_complete = false;
        }
        if choice.message.tool_calls.is_empty() {
            output = choice
                .message
                .content
                .as_ref()
                .map(|content| content.as_text())
                .unwrap_or_default();
            completed = true;
            break;
        }
        if actions
            .len()
            .saturating_add(choice.message.tool_calls.len())
            > 32
        {
            action_budget_exceeded = true;
        }
        messages.push(choice.message.clone());
        for call in &choice.message.tool_calls {
            if actions.len() >= 32 {
                break;
            }
            let args = serde_json::from_str::<serde_json::Value>(&call.function.arguments)
                .unwrap_or(serde_json::Value::Null);
            let result = execute_skill_eval_tool(
                workspace.path(),
                &call.function.name,
                &args,
                mock_integrations,
                &mut actions,
                &mut integration_calls,
            );
            let body = match result {
                Ok(value) => serde_json::json!({"ok":true,"result":value}),
                Err(code) => {
                    if let Some(action) = actions.last_mut() {
                        *action = format!("denied:{action}:{code}");
                    } else {
                        actions.push(format!("denied:{}:{code}", call.function.name));
                    }
                    serde_json::json!({"ok":false,"error":code})
                }
            };
            messages.push(execlaw_inference_api::ChatMessage::tool_result(
                call.id.clone(),
                serde_json::to_string(&body).unwrap_or_else(|_| "{}".into()),
            ));
        }
    }
    let final_files = read_skill_eval_workspace(workspace.path()).unwrap_or_default();
    let workspace_matches = expected_files
        .iter()
        .filter(|(path, expected)| final_files.get(*path) == Some(*expected))
        .count();
    let integration_matches = usize::from(integration_calls == expected_calls);
    let lower = output.to_lowercase();
    let matched_terms = required_terms
        .iter()
        .filter(|term| lower.contains(&term.to_lowercase()))
        .count();
    let forbidden_matches = forbidden_terms
        .iter()
        .filter(|term| lower.contains(&term.to_lowercase()))
        .count();
    let forbidden_actions_found = actions
        .iter()
        .filter(|action| forbidden_actions.contains(action))
        .count();
    let denied_actions = actions
        .iter()
        .filter(|action| action.starts_with("denied:"))
        .count();
    let output_chars = output.chars().count();
    let passed = completed
        && !action_budget_exceeded
        && final_files == *expected_files
        && integration_calls == expected_calls
        && forbidden_actions_found == 0
        && denied_actions == 0
        && matched_terms == required_terms.len()
        && forbidden_matches == 0
        && output_chars >= min_output_chars
        && output_chars <= max_output_chars
        && token_usage_complete
        && output_tokens <= max_output_tokens;
    Ok(SkillEvalOutcome {
        passed,
        matched_terms,
        forbidden_matches,
        output_chars,
        workspace_matches,
        expected_workspace_files: expected_files.len(),
        integration_matches,
        expected_integration_calls: expected_calls.len(),
        forbidden_actions: forbidden_actions_found,
        denied_actions,
        action_count: actions.len(),
        output_tokens,
        token_usage_complete,
    })
}

fn parse_eval_json<T: serde::de::DeserializeOwned>(value: &str) -> Result<T, ApiError> {
    serde_json::from_str(value).map_err(|error| ApiError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        code: "invalid_eval_suite",
        message: error.to_string(),
    })
}

#[allow(clippy::too_many_arguments)]
fn record_skill_eval_run(
    state: &AppState,
    skill_name: &str,
    version_id: i64,
    body_hash: &str,
    passed: bool,
    score: f64,
    before_score: Option<f64>,
    results_json: &str,
    now: i64,
    suite_hash: &str,
    model_id: &str,
    backend_fingerprint: &str,
) -> Result<(), ApiError> {
    state
        .db
        .with_conn(|connection| {
            connection.execute(
                "INSERT INTO state_skill_eval_runs \
                 (skill_name, version_id, body_sha256, evaluator_version, passed, score, before_score, \
                  results_json, created_at, suite_sha256, model_id, backend_fingerprint) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
                    skill_name,
                    version_id,
                    body_hash,
                    execlaw_skills::SKILL_EVAL_VERSION,
                    passed,
                    score,
                    before_score,
                    results_json,
                    now,
                    suite_hash,
                    model_id,
                    backend_fingerprint,
                ],
            )?;
            Ok(())
        })
        .map_err(|error| ApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "eval_result_save_failed",
            message: error.to_string(),
        })
}

fn execute_skill_eval_tool(
    root: &std::path::Path,
    name: &str,
    args: &serde_json::Value,
    mock_integrations: &std::collections::BTreeMap<String, serde_json::Value>,
    actions: &mut Vec<String>,
    integration_calls: &mut Vec<String>,
) -> Result<serde_json::Value, &'static str> {
    match name {
        "workspace.read_file" => {
            actions.push(name.to_owned());
            let path = args
                .get("path")
                .and_then(serde_json::Value::as_str)
                .ok_or("invalid_args")?;
            if !valid_skill_eval_path(path) {
                return Err("path_denied");
            }
            let target = root.join(path);
            let metadata = std::fs::symlink_metadata(&target).map_err(|_| "file_not_found")?;
            if metadata.file_type().is_symlink()
                || !metadata.is_file()
                || metadata.len() as usize > SKILL_EVAL_MAX_FILE_BYTES
            {
                return Err("file_denied");
            }
            let content = std::fs::read_to_string(target).map_err(|_| "file_not_utf8")?;
            Ok(serde_json::json!({"path":path,"content":content}))
        }
        "workspace.write_file" => {
            actions.push(name.to_owned());
            let path = args
                .get("path")
                .and_then(serde_json::Value::as_str)
                .ok_or("invalid_args")?;
            let content = args
                .get("content")
                .and_then(serde_json::Value::as_str)
                .ok_or("invalid_args")?;
            if !valid_skill_eval_path(path) || content.len() > SKILL_EVAL_MAX_FILE_BYTES {
                return Err("write_denied");
            }
            let target = root.join(path);
            let parent = target.parent().ok_or("path_denied")?;
            std::fs::create_dir_all(parent).map_err(|_| "write_failed")?;
            if let Ok(metadata) = std::fs::symlink_metadata(&target)
                && (metadata.file_type().is_symlink() || !metadata.is_file())
            {
                return Err("file_denied");
            }
            let current_total = read_skill_eval_workspace(root)
                .map_err(|_| "workspace_scan_failed")?
                .values()
                .map(String::len)
                .sum::<usize>();
            if current_total.saturating_add(content.len()) > SKILL_EVAL_MAX_WORKSPACE_BYTES {
                return Err("workspace_budget_exceeded");
            }
            std::fs::write(&target, content).map_err(|_| "write_failed")?;
            Ok(serde_json::json!({"path":path,"written_bytes":content.len()}))
        }
        "integration.mock_call" => {
            let integration = args
                .get("name")
                .and_then(serde_json::Value::as_str)
                .ok_or("invalid_args")?;
            actions.push(integration.to_owned());
            integration_calls.push(integration.to_owned());
            mock_integrations
                .get(integration)
                .cloned()
                .ok_or("mock_integration_not_configured")
        }
        _ => {
            actions.push(format!("denied:{name}"));
            Err("tool_not_available")
        }
    }
}

fn write_skill_eval_file(
    root: &std::path::Path,
    relative: &str,
    content: &str,
) -> Result<(), String> {
    if !valid_skill_eval_path(relative) || content.len() > SKILL_EVAL_MAX_FILE_BYTES {
        return Err("workspace fixture contains an unsafe path or oversized file".into());
    }
    let target = root.join(relative);
    std::fs::create_dir_all(
        target
            .parent()
            .ok_or("workspace fixture path has no parent")?,
    )
    .map_err(|error| error.to_string())?;
    std::fs::write(target, content).map_err(|error| error.to_string())
}

fn read_skill_eval_workspace(
    root: &std::path::Path,
) -> Result<std::collections::BTreeMap<String, String>, String> {
    fn walk(
        root: &std::path::Path,
        directory: &std::path::Path,
        output: &mut std::collections::BTreeMap<String, String>,
        bytes: &mut usize,
    ) -> Result<(), String> {
        for entry in std::fs::read_dir(directory).map_err(|error| error.to_string())? {
            let entry = entry.map_err(|error| error.to_string())?;
            let metadata =
                std::fs::symlink_metadata(entry.path()).map_err(|error| error.to_string())?;
            if metadata.file_type().is_symlink() {
                return Err("workspace contains a symbolic link".into());
            }
            if metadata.is_dir() {
                walk(root, &entry.path(), output, bytes)?;
            } else if metadata.is_file() {
                let relative = entry
                    .path()
                    .strip_prefix(root)
                    .map_err(|error| error.to_string())?
                    .to_string_lossy()
                    .replace('\\', "/");
                if !valid_skill_eval_path(&relative)
                    || metadata.len() as usize > SKILL_EVAL_MAX_FILE_BYTES
                {
                    return Err("workspace file violates path or size limits".into());
                }
                *bytes = bytes.saturating_add(metadata.len() as usize);
                if output.len() >= SKILL_EVAL_MAX_FILES || *bytes > SKILL_EVAL_MAX_WORKSPACE_BYTES {
                    return Err("workspace resource budget exceeded".into());
                }
                let content = std::fs::read_to_string(entry.path())
                    .map_err(|_| "workspace file is not UTF-8")?;
                output.insert(relative, content);
            }
        }
        Ok(())
    }
    let mut output = std::collections::BTreeMap::new();
    let mut bytes = 0;
    walk(root, root, &mut output, &mut bytes)?;
    Ok(output)
}

/// Replace a Controller-owned held-out suite. Prompts and expected terms are
/// deliberately not returned by the evaluation endpoint.
pub async fn save_skill_eval_suite(
    State(state): State<AppState>,
    user: AuthedUser,
    AxumPath(name): AxumPath<String>,
    axum::Json(request): axum::Json<SkillEvalSuiteRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_controller(&state, &user)?;
    validate_eval_suite(&request.cases)?;
    SkillStore::new(state.db.clone())
        .get(&name)
        .map_err(skill_err)?
        .ok_or_else(|| ApiError {
            status: StatusCode::NOT_FOUND,
            code: "skill_not_found",
            message: name.clone(),
        })?;
    let cases = request.cases;
    state.db.transaction(|tx| {
        tx.execute("DELETE FROM state_skill_eval_cases WHERE skill_name = ?1", params![name])?;
        for case in &cases {
            let terms = serde_json::to_string(&case.required_terms).map_err(|error| execlaw_core::db::DbError::Serde(error.to_string()))?;
            let forbidden = serde_json::to_string(&case.forbidden_terms).map_err(|error| execlaw_core::db::DbError::Serde(error.to_string()))?;
            let workspace_files = serde_json::to_string(&case.workspace_files).map_err(|error| execlaw_core::db::DbError::Serde(error.to_string()))?;
            let expected_workspace_files = serde_json::to_string(&case.expected_workspace_files).map_err(|error| execlaw_core::db::DbError::Serde(error.to_string()))?;
            let mock_integrations = serde_json::to_string(&case.mock_integrations).map_err(|error| execlaw_core::db::DbError::Serde(error.to_string()))?;
            let expected_calls = serde_json::to_string(&case.expected_integration_calls).map_err(|error| execlaw_core::db::DbError::Serde(error.to_string()))?;
            let forbidden_actions = serde_json::to_string(&case.forbidden_actions).map_err(|error| execlaw_core::db::DbError::Serde(error.to_string()))?;
            tx.execute("INSERT INTO state_skill_eval_cases (skill_name, case_id, prompt, required_terms_json, forbidden_terms_json, min_output_chars, max_output_chars, max_output_tokens, workspace_files_json, expected_workspace_files_json, mock_integrations_json, expected_integration_calls_json, forbidden_actions_json) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)", params![name, case.case_id, case.prompt, terms, forbidden, case.min_output_chars, case.max_output_chars, case.max_output_tokens, workspace_files, expected_workspace_files, mock_integrations, expected_calls, forbidden_actions])?;
        }
        Ok(())
    }).map_err(|error| ApiError { status: StatusCode::INTERNAL_SERVER_ERROR, code: "eval_suite_save_failed", message: error.to_string() })?;
    Ok(Json(
        serde_json::json!({"saved": true, "case_count": cases.len()}),
    ))
}

/// Run a saved held-out suite against the current skill version using the
/// configured local Standard inference backend, then persist its score.
pub async fn evaluate_skill(
    State(state): State<AppState>,
    user: AuthedUser,
    AxumPath(name): AxumPath<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_controller(&state, &user)?;
    let skill = SkillStore::new(state.db.clone())
        .get(&name)
        .map_err(skill_err)?
        .ok_or_else(|| ApiError {
            status: StatusCode::NOT_FOUND,
            code: "skill_not_found",
            message: name.clone(),
        })?;
    let cases: Vec<(String, String, String, String, usize, usize, u32, String, String, String, String, String)> = state.db.with_conn(|conn| {
        let mut statement = conn.prepare("SELECT case_id, prompt, required_terms_json, forbidden_terms_json, min_output_chars, max_output_chars, max_output_tokens, workspace_files_json, expected_workspace_files_json, mock_integrations_json, expected_integration_calls_json, forbidden_actions_json FROM state_skill_eval_cases WHERE skill_name = ?1 ORDER BY case_id")?;
        Ok(statement.query_map(params![name], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get::<_, i64>(4)? as usize, row.get::<_, i64>(5)? as usize, row.get::<_, i64>(6)? as u32, row.get(7)?, row.get(8)?, row.get(9)?, row.get(10)?, row.get(11)?)))?.collect::<Result<Vec<_>, _>>()?)
    }).map_err(|error| ApiError { status: StatusCode::INTERNAL_SERVER_ERROR, code: "eval_suite_read_failed", message: error.to_string() })?;
    if cases.is_empty() {
        return Err(ApiError {
            status: StatusCode::CONFLICT,
            code: "eval_suite_missing",
            message: "configure a held-out suite before evaluation".into(),
        });
    }
    let suite_hash = canonical_suite_hash(&cases)?;
    let resolved = state
        .inference
        .resolve(&state.db, execlaw_core::backends::BackendPurpose::Standard)
        .map(|resolved| resolved.with_workload("background"))
        .ok_or_else(|| ApiError {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "inference_unavailable",
            message: "configured local inference backend is unavailable".into(),
        })?;
    let engine = match resolved.client.engine {
        execlaw_inference_api::InferenceEngine::OpenAICompat => "openai_compatible",
        execlaw_inference_api::InferenceEngine::Ollama => "ollama_native",
    };
    let backend_fingerprint = backend_fingerprint(&resolved.endpoint, &resolved.model_id, engine);
    let parent = if let Some(parent_id) = skill.current_version.parent_version_id {
        state
            .db
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT v.body_md, v.body_sha256 FROM state_skill_versions v \
                 JOIN state_skills s ON s.id = v.skill_id \
                 WHERE v.id = ?1 AND s.name = ?2",
                    params![parent_id.0, name],
                    |row| {
                        Ok((
                            parent_id.0,
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                        ))
                    },
                )
                .optional()
                .map_err(execlaw_core::db::DbError::from)
            })
            .map_err(|error| ApiError {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                code: "eval_parent_read_failed",
                message: error.to_string(),
            })?
    } else {
        None
    };
    let started = std::time::Instant::now();
    let mut parent_passed = 0usize;
    let mut candidate_passed = 0usize;
    let mut parent_results = Vec::with_capacity(cases.len());
    let mut candidate_results = Vec::with_capacity(cases.len());
    for (
        case_id,
        prompt,
        required_json,
        forbidden_json,
        min_chars,
        max_chars,
        max_tokens,
        workspace_files_json,
        expected_files_json,
        mock_integrations_json,
        expected_calls_json,
        forbidden_actions_json,
    ) in &cases
    {
        if started.elapsed() > std::time::Duration::from_secs(600) {
            return Err(ApiError {
                status: StatusCode::GATEWAY_TIMEOUT,
                code: "skill_eval_budget_exceeded",
                message: "skill candidate and parent evaluation exceeded its 10-minute budget"
                    .into(),
            });
        }
        let required = parse_eval_json::<Vec<String>>(required_json)?;
        let forbidden = parse_eval_json::<Vec<String>>(forbidden_json)?;
        let workspace_files =
            parse_eval_json::<std::collections::BTreeMap<String, String>>(workspace_files_json)?;
        let expected_files =
            parse_eval_json::<std::collections::BTreeMap<String, String>>(expected_files_json)?;
        let mock_integrations = parse_eval_json::<
            std::collections::BTreeMap<String, serde_json::Value>,
        >(mock_integrations_json)?;
        let expected_calls = parse_eval_json::<Vec<String>>(expected_calls_json)?;
        let forbidden_actions = parse_eval_json::<Vec<String>>(forbidden_actions_json)?;
        if let Some((_, parent_body, _)) = parent.as_ref() {
            let outcome = execute_skill_case(
                &resolved.client,
                &resolved.model_id,
                parent_body,
                prompt,
                &required,
                &forbidden,
                *min_chars,
                *max_chars,
                *max_tokens,
                &workspace_files,
                &expected_files,
                &mock_integrations,
                &expected_calls,
                &forbidden_actions,
            )
            .await?;
            parent_passed += usize::from(outcome.passed);
            parent_results.push(serde_json::json!({"case_id":case_id,"passed":outcome.passed,"workspace_matches":outcome.workspace_matches,"expected_workspace_files":outcome.expected_workspace_files,"integration_matches":outcome.integration_matches,"expected_integration_calls":outcome.expected_integration_calls,"forbidden_actions":outcome.forbidden_actions,"denied_actions":outcome.denied_actions,"action_count":outcome.action_count,"matched_terms":outcome.matched_terms,"forbidden_matches":outcome.forbidden_matches,"output_chars":outcome.output_chars,"output_tokens":outcome.output_tokens,"token_usage_complete":outcome.token_usage_complete}));
        }
        let outcome = execute_skill_case(
            &resolved.client,
            &resolved.model_id,
            &skill.current_version.body_md,
            prompt,
            &required,
            &forbidden,
            *min_chars,
            *max_chars,
            *max_tokens,
            &workspace_files,
            &expected_files,
            &mock_integrations,
            &expected_calls,
            &forbidden_actions,
        )
        .await?;
        candidate_passed += usize::from(outcome.passed);
        candidate_results.push(serde_json::json!({"case_id":case_id,"passed":outcome.passed,"workspace_matches":outcome.workspace_matches,"expected_workspace_files":outcome.expected_workspace_files,"integration_matches":outcome.integration_matches,"expected_integration_calls":outcome.expected_integration_calls,"forbidden_actions":outcome.forbidden_actions,"denied_actions":outcome.denied_actions,"action_count":outcome.action_count,"matched_terms":outcome.matched_terms,"forbidden_matches":outcome.forbidden_matches,"output_chars":outcome.output_chars,"output_tokens":outcome.output_tokens,"token_usage_complete":outcome.token_usage_complete}));
    }
    let score = candidate_passed as f64 / cases.len() as f64;
    let before_score = parent
        .as_ref()
        .map(|_| parent_passed as f64 / cases.len() as f64);
    let passed = candidate_passed == cases.len();
    let now = chrono::Utc::now().timestamp();
    if let Some((parent_version_id, _, parent_hash)) = parent.as_ref() {
        record_skill_eval_run(
            &state,
            &name,
            *parent_version_id,
            parent_hash,
            parent_passed == cases.len(),
            before_score.unwrap_or(0.0),
            None,
            &serde_json::to_string(&parent_results).map_err(|error| ApiError {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                code: "eval_result_encode_failed",
                message: error.to_string(),
            })?,
            now,
            &suite_hash,
            &resolved.model_id,
            &backend_fingerprint,
        )?;
    }
    let results_json = serde_json::to_string(&candidate_results).map_err(|error| ApiError {
        status: StatusCode::INTERNAL_SERVER_ERROR,
        code: "eval_result_encode_failed",
        message: error.to_string(),
    })?;
    record_skill_eval_run(
        &state,
        &name,
        skill.current_version.id.0,
        &skill.current_version.body_sha256,
        passed,
        score,
        before_score,
        &results_json,
        now,
        &suite_hash,
        &resolved.model_id,
        &backend_fingerprint,
    )?;
    Ok(Json(
        serde_json::json!({"skill": name, "version": skill.current_version.version, "passed": passed, "score": score, "before_score": before_score, "score_delta": before_score.map(|before| score - before), "candidate_cases": candidate_results, "parent_cases": parent.as_ref().map(|_| parent_results)}),
    ))
}

#[utoipa::path(
    get,
    path = "/api/admin/skills",
    responses((status = 200, description = "Visible skills", body = SkillListResponse)),
    security(("bearer_jwt" = [])),
    tag = "skills"
)]
pub async fn list_handler(
    State(state): State<AppState>,
    _user: AuthedUser,
    Query(q): Query<ListQuery>,
) -> Result<Json<SkillListResponse>, ApiError> {
    let store = SkillStore::new(state.db.clone());
    // The store's `list_index` already filters out archived. For
    // include_archived=true we pull every row by name range via a
    // direct query — simpler than expanding the store's API surface
    // for a UI affordance.
    let entries = if q.include_archived {
        list_all_for_admin(&store).map_err(skill_err)?
    } else {
        let idx = store.list_index().map_err(skill_err)?;
        // Hydrate each entry with source / kind / owning_plugin / updated_at via a get().
        let mut out = Vec::with_capacity(idx.len());
        for e in idx {
            if let Some(s) = store.get(&e.name).map_err(skill_err)? {
                out.push(SkillListEntry {
                    name: s.name,
                    description: s.current_version.description,
                    state: s.state.as_str().to_owned(),
                    version: s.current_version.version,
                    registration_kind: s.registration_kind.as_str().to_owned(),
                    source: s.source,
                    owning_plugin_id: s.owning_plugin_id,
                    updated_at: s.updated_at,
                });
            }
        }
        out
    };
    Ok(Json(SkillListResponse { skills: entries }))
}

/// Pull every row including archived; used when include_archived=true.
fn list_all_for_admin(
    store: &SkillStore,
) -> Result<Vec<SkillListEntry>, execlaw_skills::SkillError> {
    use rusqlite::params;
    let names: Vec<String> = store
        .db()
        .with_conn(|c| {
            let mut stmt = c.prepare("SELECT name FROM state_skills ORDER BY name")?;
            let rows = stmt
                .query_map(params![], |r| r.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
        .map_err(execlaw_skills::SkillError::Db)?;
    let mut out = Vec::with_capacity(names.len());
    for name in names {
        if let Some(s) = store.get(&name)? {
            out.push(SkillListEntry {
                name: s.name,
                description: s.current_version.description,
                state: s.state.as_str().to_owned(),
                version: s.current_version.version,
                registration_kind: s.registration_kind.as_str().to_owned(),
                source: s.source,
                owning_plugin_id: s.owning_plugin_id,
                updated_at: s.updated_at,
            });
        }
    }
    Ok(out)
}

#[utoipa::path(
    get,
    path = "/api/admin/skills/{name}",
    params(("name" = String, Path, description = "Full skill name (`<namespace>/<segment>`)")),
    responses(
        (status = 200, description = "Skill detail", body = SkillDetail),
        (status = 404, description = "Skill not found")
    ),
    security(("bearer_jwt" = [])),
    tag = "skills"
)]
pub async fn get_handler(
    State(state): State<AppState>,
    _user: AuthedUser,
    AxumPath(name): AxumPath<String>,
) -> Result<Json<SkillDetail>, ApiError> {
    let store = SkillStore::new(state.db.clone());
    let s = store
        .get(&name)
        .map_err(skill_err)?
        .ok_or_else(|| ApiError {
            status: StatusCode::NOT_FOUND,
            code: "skill_not_found",
            message: format!("no skill named '{name}'"),
        })?;
    // Resource paths come from the view (which only returns paths for
    // non-archived skills); for archived skills we skip them.
    let resource_paths = if matches!(s.state, execlaw_skills::SkillState::Archived) {
        Vec::new()
    } else {
        store
            .view(&name)
            .map_err(skill_err)?
            .map(|v| v.resource_paths)
            .unwrap_or_default()
    };
    Ok(Json(SkillDetail {
        name: s.name,
        description: s.current_version.description.clone(),
        state: s.state.as_str().to_owned(),
        registration_kind: s.registration_kind.as_str().to_owned(),
        source: s.source.clone(),
        owning_plugin_id: s.owning_plugin_id.clone(),
        current_version: s.current_version.version,
        body_md: s.current_version.body_md.clone(),
        frontmatter_json: s.current_version.frontmatter_json.clone(),
        authored_by: s.current_version.authored_by.clone(),
        authored_at: s.current_version.authored_at,
        created_at: s.created_at,
        updated_at: s.updated_at,
        archived_at: s.archived_at,
        resource_paths,
    }))
}

// ----------------------------------------------------------------
// 2026-05-16 — operator-facing skill creation. The Skills page
// ships a "+ New skill" button that POSTs here. Pre-2026-05-16 the
// only way to create a skill from the SPA was indirectly via the
// auto-capture worker; manual authoring required calling the
// `skills.create` tool from chat. This endpoint closes that loop.
//
// Controller-only — matches the trust contract on every other write
// in this module. Strictness is `Warn` (same as the body editor)
// rather than `Strict`: an operator typing into a form is more
// trusted than an agent calling the tool surface, and false-positive
// scanner trips were a reported friction point during onboarding.
#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateSkillRequest {
    /// Skill name in `<namespace>/<segment>` form. Validated by the
    /// store; 400 `invalid_name` if it doesn't match the canonical
    /// shape (`[a-z0-9][a-z0-9-]*` per segment).
    pub name: String,
    pub description: String,
    pub body_md: String,
    /// Optional JSON-encoded frontmatter. Defaults to `"{}"` so the
    /// SPA form doesn't have to ship a literal `{}` when the
    /// operator leaves the field blank.
    #[serde(default = "default_frontmatter_admin")]
    pub frontmatter_json: String,
}

#[utoipa::path(
    post,
    path = "/api/admin/skills",
    request_body = CreateSkillRequest,
    responses(
        (status = 200, description = "Created — returns the new skill detail", body = SkillDetail),
        (status = 400, description = "Invalid name / frontmatter / body"),
        (status = 403, description = "Caller is not a Controller"),
        (status = 409, description = "Skill name already exists")
    ),
    security(("bearer_jwt" = [])),
    tag = "skills"
)]
pub async fn create_handler(
    State(state): State<AppState>,
    user: AuthedUser,
    Json(req): Json<CreateSkillRequest>,
) -> Result<Json<SkillDetail>, ApiError> {
    require_controller(&state, &user)?;
    let store = SkillStore::new(state.db.clone());
    // Pre-check: `SkillStore::create` raises a generic `DbError`
    // (mapped to 500) when the name collides, not a typed
    // `AlreadyExists`. Looking it up upfront lets us surface 409
    // `already_exists` for the operator without touching the
    // shared store contract.
    if let Some(_existing) = store.get(&req.name).map_err(skill_err)? {
        return Err(ApiError {
            status: StatusCode::CONFLICT,
            code: "already_exists",
            message: format!("skill '{}' already exists", req.name),
        });
    }
    let now_ms = chrono::Utc::now().timestamp() * 1000;
    let new = execlaw_skills::NewSkill {
        name: req.name.clone(),
        source: format!("admin:{}", user.user_id),
        registration_kind: execlaw_skills::RegistrationKind::Authored,
        owning_plugin_id: None,
        initial_version: NewSkillVersion {
            description: req.description,
            body_md: req.body_md,
            frontmatter_json: req.frontmatter_json,
            authored_by: format!("admin:{}", user.user_id),
            promotion_notes: None,
        },
        resources: vec![],
    };
    store
        .create(new, Strictness::Warn, now_ms)
        .map_err(skill_err)?;
    get_handler(State(state), user, AxumPath(req.name)).await
}

#[utoipa::path(
    post,
    path = "/api/admin/skills/{name}/promote",
    request_body = PromoteRequest,
    params(("name" = String, Path, description = "Full skill name")),
    responses(
        (status = 200, description = "Promoted (or no-op if already stable)", body = SkillDetail),
        (status = 403, description = "Caller is not a Controller"),
        (status = 404, description = "Skill not found")
    ),
    security(("bearer_jwt" = [])),
    tag = "skills"
)]
pub async fn promote_handler(
    State(state): State<AppState>,
    user: AuthedUser,
    AxumPath(name): AxumPath<String>,
    Json(req): Json<PromoteRequest>,
) -> Result<Json<SkillDetail>, ApiError> {
    require_controller(&state, &user)?;
    let store = SkillStore::new(state.db.clone());
    let now_ms = chrono::Utc::now().timestamp() * 1000;
    store.promote(&name, req.notes, now_ms).map_err(skill_err)?;
    get_handler(State(state), user, AxumPath(name)).await
}

/// Restore an earlier skill version as a new trial version.
#[utoipa::path(
    post,
    path = "/api/admin/skills/{name}/rollback",
    request_body = RollbackSkillRequest,
    params(("name" = String, Path, description = "Full skill name")),
    responses(
        (status = 200, description = "Restored content as a new trial version", body = SkillDetail),
        (status = 403, description = "Caller is not a Controller"),
        (status = 404, description = "Skill version not found"),
        (status = 409, description = "Rollback is not valid for the current skill state")
    ),
    security(("bearer_jwt" = [])),
    tag = "skills"
)]
pub async fn rollback_skill_handler(
    State(state): State<AppState>,
    user: AuthedUser,
    AxumPath(name): AxumPath<String>,
    Json(request): Json<RollbackSkillRequest>,
) -> Result<Json<SkillDetail>, ApiError> {
    require_controller(&state, &user)?;
    let store = SkillStore::new(state.db.clone());
    let now_ms = chrono::Utc::now().timestamp() * 1000;
    store
        .rollback_version(
            &name,
            request.target_version,
            &format!("admin:{}", user.user_id),
            now_ms,
        )
        .map_err(skill_err)?;
    get_handler(State(state), user, AxumPath(name)).await
}

#[utoipa::path(
    post,
    path = "/api/admin/skills/{name}/archive",
    params(("name" = String, Path, description = "Full skill name")),
    responses(
        (status = 200, description = "Archived (or no-op if already archived)", body = SkillDetail),
        (status = 403, description = "Caller is not a Controller"),
        (status = 404, description = "Skill not found")
    ),
    security(("bearer_jwt" = [])),
    tag = "skills"
)]
pub async fn archive_handler(
    State(state): State<AppState>,
    user: AuthedUser,
    AxumPath(name): AxumPath<String>,
) -> Result<Json<SkillDetail>, ApiError> {
    require_controller(&state, &user)?;
    let store = SkillStore::new(state.db.clone());
    let now_ms = chrono::Utc::now().timestamp() * 1000;
    store.archive(&name, now_ms).map_err(skill_err)?;
    get_handler(State(state), user, AxumPath(name)).await
}

fn skill_err(e: execlaw_skills::SkillError) -> ApiError {
    use execlaw_skills::SkillError::*;
    match e {
        Db(execlaw_core::db::DbError::Invariant(message)) if message.contains("held-out") => {
            ApiError {
                status: StatusCode::CONFLICT,
                code: "skill_eval_required",
                message,
            }
        }
        Db(execlaw_core::db::DbError::Invariant(message))
            if message.contains("skill not found") =>
        {
            ApiError {
                status: StatusCode::NOT_FOUND,
                code: "skill_not_found",
                message,
            }
        }
        Db(execlaw_core::db::DbError::Invariant(message)) if message.contains("has no version") => {
            ApiError {
                status: StatusCode::NOT_FOUND,
                code: "skill_version_not_found",
                message,
            }
        }
        Db(execlaw_core::db::DbError::Invariant(message))
            if message.contains("cannot roll back") || message.contains("already at version") =>
        {
            ApiError {
                status: StatusCode::CONFLICT,
                code: "skill_rollback_conflict",
                message,
            }
        }
        NotFound(n) => ApiError {
            status: StatusCode::NOT_FOUND,
            code: "skill_not_found",
            message: n,
        },
        InvalidName(n) => ApiError {
            status: StatusCode::BAD_REQUEST,
            code: "invalid_name",
            message: n,
        },
        AlreadyExists(n) => ApiError {
            status: StatusCode::CONFLICT,
            code: "already_exists",
            message: n,
        },
        Blocked { findings, fields } => ApiError {
            status: StatusCode::BAD_REQUEST,
            code: "secret_scanner_blocked",
            message: format!("scanner blocked: {findings} finding(s) in {fields:?}"),
        },
        Denied(r) => ApiError {
            status: StatusCode::FORBIDDEN,
            code: "denied",
            message: r,
        },
        // 2026-05-16 — fix #P3 (Codex review): these are caller-input
        // validation failures, not server errors. The SPA's composer
        // exposes the custom-frontmatter blob directly, so 400 lets
        // the UI render an inline error chip instead of the generic
        // "server error, retry" the 500 catch-all surfaced.
        InvalidFrontmatter(m) => ApiError {
            status: StatusCode::BAD_REQUEST,
            code: "invalid_frontmatter",
            message: format!("frontmatter is not valid JSON: {m}"),
        },
        BodyTooLarge { size, cap } => ApiError {
            // Size-violation; PAYLOAD_TOO_LARGE is the closer fit
            // than 400 since the SPA can short-circuit on this exact
            // status to surface a "trim your body" message without
            // re-parsing the error code string.
            status: StatusCode::PAYLOAD_TOO_LARGE,
            code: "body_too_large",
            message: format!("skill body size {size}B exceeds cap {cap}B"),
        },
        ResourceTooLarge { size, cap } => ApiError {
            status: StatusCode::PAYLOAD_TOO_LARGE,
            code: "resource_too_large",
            message: format!("skill resource size {size}B exceeds cap {cap}B"),
        },
        other => ApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "skill_error",
            message: other.to_string(),
        },
    }
}

fn require_controller(state: &AppState, user: &AuthedUser) -> Result<(), ApiError> {
    use execlaw_core::users::{UserRole, UserStore};
    let row = UserStore::new(&state.db)
        .get_by_id(&user.user_id)
        .map_err(ApiError::from)?;
    if matches!(row.map(|u| u.role), Some(UserRole::Controller)) {
        Ok(())
    } else {
        Err(ApiError {
            status: StatusCode::FORBIDDEN,
            code: "controller_only",
            message: "only a Controller can promote, roll back, or archive skills".into(),
        })
    }
}

// ----------------------------------------------------------------
// Phase C — config: GET / PUT auto-capture toggle
// ----------------------------------------------------------------

#[derive(Debug, Serialize, ToSchema)]
pub struct SkillsConfigView {
    pub auto_capture_enabled: bool,
    pub auto_capture_min_tool_calls: u32,
    pub auto_capture_dry_run: bool,
    pub reuse_update_enabled: bool,
    pub updated_at: i64,
}

impl From<SkillsConfig> for SkillsConfigView {
    fn from(c: SkillsConfig) -> Self {
        Self {
            auto_capture_enabled: c.auto_capture_enabled,
            auto_capture_min_tool_calls: c.auto_capture_min_tool_calls,
            auto_capture_dry_run: c.auto_capture_dry_run,
            reuse_update_enabled: c.reuse_update_enabled,
            updated_at: c.updated_at,
        }
    }
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateSkillsConfigRequest {
    pub auto_capture_enabled: Option<bool>,
    pub auto_capture_min_tool_calls: Option<u32>,
    pub auto_capture_dry_run: Option<bool>,
    pub reuse_update_enabled: Option<bool>,
}

pub async fn get_config_handler(
    State(state): State<AppState>,
    _user: AuthedUser,
) -> Result<Json<SkillsConfigView>, ApiError> {
    let cfg = SkillsConfigStore::new(&state.db)
        .get()
        .map_err(ApiError::from)?;
    Ok(Json(cfg.into()))
}

pub async fn update_config_handler(
    State(state): State<AppState>,
    user: AuthedUser,
    Json(req): Json<UpdateSkillsConfigRequest>,
) -> Result<Json<SkillsConfigView>, ApiError> {
    require_controller(&state, &user)?;
    let update = SkillsConfigUpdate {
        auto_capture_enabled: req.auto_capture_enabled,
        auto_capture_min_tool_calls: req.auto_capture_min_tool_calls,
        auto_capture_dry_run: req.auto_capture_dry_run,
        reuse_update_enabled: req.reuse_update_enabled,
    };
    let now_ms = chrono::Utc::now().timestamp() * 1000;
    let updated = SkillsConfigStore::new(&state.db)
        .update(&update, now_ms)
        .map_err(|e| ApiError {
            status: StatusCode::BAD_REQUEST,
            code: "config_update_rejected",
            message: e.to_string(),
        })?;
    Ok(Json(updated.into()))
}

// ----------------------------------------------------------------
// Phase D.1 — body editor: PUT /api/admin/skills/{name}
// ----------------------------------------------------------------

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateSkillBodyRequest {
    pub description: String,
    pub body_md: String,
    #[serde(default = "default_frontmatter_admin")]
    pub frontmatter_json: String,
    #[serde(default)]
    pub promotion_notes: Option<String>,
}

fn default_frontmatter_admin() -> String {
    "{}".into()
}

pub async fn update_body_handler(
    State(state): State<AppState>,
    user: AuthedUser,
    AxumPath(name): AxumPath<String>,
    Json(req): Json<UpdateSkillBodyRequest>,
) -> Result<Json<SkillDetail>, ApiError> {
    require_controller(&state, &user)?;
    let store = SkillStore::new(state.db.clone());
    let now_ms = chrono::Utc::now().timestamp() * 1000;
    let v = NewSkillVersion {
        description: req.description,
        body_md: req.body_md,
        frontmatter_json: req.frontmatter_json,
        authored_by: format!("admin:{}", user.user_id),
        promotion_notes: req.promotion_notes,
    };
    store
        .add_version(&name, v, Strictness::Warn, now_ms)
        .map_err(skill_err)?;
    get_handler(State(state), user, AxumPath(name)).await
}

// ----------------------------------------------------------------
// Phase D.1 — version history: GET /api/admin/skills/{name}/versions
// ----------------------------------------------------------------

#[derive(Debug, Serialize, ToSchema)]
pub struct SkillVersionView {
    pub version: u32,
    pub description: String,
    pub body_md: String,
    pub frontmatter_json: String,
    pub authored_by: String,
    pub authored_at: i64,
    pub promotion_notes: Option<String>,
    pub parent_version: Option<u32>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SkillVersionListResponse {
    pub versions: Vec<SkillVersionView>,
}

pub async fn list_versions_handler(
    State(state): State<AppState>,
    _user: AuthedUser,
    AxumPath(name): AxumPath<String>,
) -> Result<Json<SkillVersionListResponse>, ApiError> {
    let store = SkillStore::new(state.db.clone());
    let versions = store.list_versions(&name).map_err(skill_err)?;
    if versions.is_empty() {
        return Err(ApiError {
            status: StatusCode::NOT_FOUND,
            code: "skill_not_found",
            message: format!("no skill named '{name}'"),
        });
    }
    // Build a map from version_id -> version number so we can render
    // parent_version_id as an inline parent_version.
    let id_to_num: std::collections::HashMap<i64, u32> =
        versions.iter().map(|v| (v.id.0, v.version)).collect();
    let out: Vec<SkillVersionView> = versions
        .iter()
        .map(|v| SkillVersionView {
            version: v.version,
            description: v.description.clone(),
            body_md: v.body_md.clone(),
            frontmatter_json: v.frontmatter_json.clone(),
            authored_by: v.authored_by.clone(),
            authored_at: v.authored_at,
            promotion_notes: v.promotion_notes.clone(),
            parent_version: v
                .parent_version_id
                .and_then(|p| id_to_num.get(&p.0).copied()),
        })
        .collect();
    Ok(Json(SkillVersionListResponse { versions: out }))
}

// ----------------------------------------------------------------
// Phase D.1 — proposals: list + get + approve + reject
// ----------------------------------------------------------------

#[derive(Debug, Serialize, ToSchema)]
pub struct SkillProposalView {
    pub id: i64,
    pub kind: String,
    pub target_skill_id: Option<i64>,
    pub proposed_name: String,
    pub description: String,
    pub body_md: String,
    pub frontmatter_json: String,
    pub source_run_id: String,
    pub trajectory_summary: Option<String>,
    pub tool_calls_observed: u32,
    pub state: String,
    pub promoted_skill_id: Option<i64>,
    pub promoted_version_id: Option<i64>,
    pub created_at: i64,
    pub reviewed_at: Option<i64>,
    pub reviewer: Option<String>,
    pub decision_notes: Option<String>,
}

impl From<execlaw_skills::SkillProposal> for SkillProposalView {
    fn from(p: execlaw_skills::SkillProposal) -> Self {
        Self {
            id: p.id.0,
            kind: p.kind.as_str().to_string(),
            target_skill_id: p.target_skill_id.map(|s| s.0),
            proposed_name: p.proposed_name,
            description: p.description,
            body_md: p.body_md,
            frontmatter_json: p.frontmatter_json,
            source_run_id: p.source_run_id,
            trajectory_summary: p.trajectory_summary,
            tool_calls_observed: p.tool_calls_observed,
            state: p.state.as_str().to_string(),
            promoted_skill_id: p.promoted_skill_id.map(|s| s.0),
            promoted_version_id: p.promoted_version_id.map(|v| v.0),
            created_at: p.created_at,
            reviewed_at: p.reviewed_at,
            reviewer: p.reviewer,
            decision_notes: p.decision_notes,
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SkillProposalListResponse {
    pub proposals: Vec<SkillProposalView>,
}

#[derive(Debug, Deserialize)]
pub struct ListProposalsQuery {
    /// `pending` (default), `approved`, `rejected`, `superseded`,
    /// or `all` (omit filter).
    #[serde(default)]
    pub state: Option<String>,
}

pub async fn list_proposals_handler(
    State(state): State<AppState>,
    _user: AuthedUser,
    Query(q): Query<ListProposalsQuery>,
) -> Result<Json<SkillProposalListResponse>, ApiError> {
    let store = SkillStore::new(state.db.clone());
    let filter = match q.state.as_deref() {
        None | Some("pending") => Some(ProposalState::Pending),
        Some("approved") => Some(ProposalState::Approved),
        Some("rejected") => Some(ProposalState::Rejected),
        Some("superseded") => Some(ProposalState::Superseded),
        Some("all") => None,
        Some(other) => {
            return Err(ApiError {
                status: StatusCode::BAD_REQUEST,
                code: "invalid_state",
                message: format!("unknown state filter: {other}"),
            });
        }
    };
    let rows = store.list_proposals(filter).map_err(skill_err)?;
    Ok(Json(SkillProposalListResponse {
        proposals: rows.into_iter().map(Into::into).collect(),
    }))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct ReviewProposalRequest {
    #[serde(default)]
    pub notes: Option<String>,
}

pub async fn approve_proposal_handler(
    State(state): State<AppState>,
    user: AuthedUser,
    AxumPath(id): AxumPath<i64>,
    Json(req): Json<ReviewProposalRequest>,
) -> Result<Json<SkillProposalView>, ApiError> {
    require_controller(&state, &user)?;
    let store = SkillStore::new(state.db.clone());
    let now_ms = chrono::Utc::now().timestamp() * 1000;
    store
        .approve_proposal(ProposalId(id), &user.user_id, req.notes, now_ms)
        .map_err(skill_err)?;
    let p = store
        .get_proposal(ProposalId(id))
        .map_err(skill_err)?
        .ok_or_else(|| ApiError {
            status: StatusCode::NOT_FOUND,
            code: "proposal_not_found",
            message: format!("proposal {id} disappeared after approve"),
        })?;
    Ok(Json(p.into()))
}

pub async fn reject_proposal_handler(
    State(state): State<AppState>,
    user: AuthedUser,
    AxumPath(id): AxumPath<i64>,
    Json(req): Json<ReviewProposalRequest>,
) -> Result<Json<SkillProposalView>, ApiError> {
    require_controller(&state, &user)?;
    let store = SkillStore::new(state.db.clone());
    let now_ms = chrono::Utc::now().timestamp() * 1000;
    store
        .reject_proposal(ProposalId(id), &user.user_id, req.notes, now_ms)
        .map_err(skill_err)?;
    let p = store
        .get_proposal(ProposalId(id))
        .map_err(skill_err)?
        .ok_or_else(|| ApiError {
            status: StatusCode::NOT_FOUND,
            code: "proposal_not_found",
            message: format!("proposal {id} disappeared after reject"),
        })?;
    Ok(Json(p.into()))
}

#[cfg(test)]
mod tests {
    //! 2026-05-16 — coverage for the operator-facing skill creation
    //! path added alongside the standardized Skills page scaffolding.
    //! Hits the full HTTP route stack (auth extractor + handler +
    //! store) so a regression in any link of the chain trips here.
    use super::{
        SkillEvalCaseInput, backend_fingerprint, execute_skill_case, execute_skill_eval_tool,
        valid_skill_eval_path, validate_eval_suite,
    };
    use crate::routes::{build_router, test_app_state};
    use axum::body::{self, Body};
    use axum::http::{HeaderValue, Method, Request, StatusCode, header};
    use execlaw_core::users::{UserRole, UserRow, UserStore};
    use tower::ServiceExt;

    #[test]
    fn held_out_suite_rejects_duplicate_ids_and_duplicate_rubric_terms() {
        let duplicate_ids = vec![
            SkillEvalCaseInput {
                case_id: "same".into(),
                prompt: "task one".into(),
                required_terms: vec!["answer".into()],
                forbidden_terms: Vec::new(),
                min_output_chars: 0,
                max_output_chars: 4000,
                max_output_tokens: 256,
                workspace_files: std::collections::BTreeMap::new(),
                expected_workspace_files: [("result.txt".into(), "done".into())].into(),
                mock_integrations: std::collections::BTreeMap::new(),
                expected_integration_calls: Vec::new(),
                forbidden_actions: Vec::new(),
            },
            SkillEvalCaseInput {
                case_id: "same".into(),
                prompt: "task two".into(),
                required_terms: vec!["answer".into()],
                forbidden_terms: Vec::new(),
                min_output_chars: 0,
                max_output_chars: 4000,
                max_output_tokens: 256,
                workspace_files: std::collections::BTreeMap::new(),
                expected_workspace_files: [("result.txt".into(), "done".into())].into(),
                mock_integrations: std::collections::BTreeMap::new(),
                expected_integration_calls: Vec::new(),
                forbidden_actions: Vec::new(),
            },
        ];
        assert_eq!(
            validate_eval_suite(&duplicate_ids).unwrap_err().code,
            "invalid_eval_suite"
        );

        let duplicate_terms = vec![SkillEvalCaseInput {
            case_id: "case".into(),
            prompt: "task".into(),
            required_terms: vec!["Answer".into(), " answer ".into()],
            forbidden_terms: Vec::new(),
            min_output_chars: 0,
            max_output_chars: 4000,
            max_output_tokens: 256,
            workspace_files: std::collections::BTreeMap::new(),
            expected_workspace_files: [("result.txt".into(), "done".into())].into(),
            mock_integrations: std::collections::BTreeMap::new(),
            expected_integration_calls: Vec::new(),
            forbidden_actions: Vec::new(),
        }];
        assert_eq!(
            validate_eval_suite(&duplicate_terms).unwrap_err().code,
            "invalid_eval_suite"
        );
    }

    #[test]
    fn backend_fingerprint_tracks_endpoint_and_model_without_persisting_endpoint() {
        let baseline = backend_fingerprint("http://127.0.0.1:11434", "model-a", "ollama_native");
        assert_eq!(
            baseline,
            backend_fingerprint("http://127.0.0.1:11434", "model-a", "ollama_native")
        );
        assert_ne!(
            baseline,
            backend_fingerprint("http://127.0.0.1:11435", "model-a", "ollama_native")
        );
        assert_ne!(
            baseline,
            backend_fingerprint("http://127.0.0.1:11434", "model-b", "ollama_native")
        );
        assert_ne!(
            baseline,
            backend_fingerprint("http://127.0.0.1:11434", "model-a", "openai_compatible")
        );
    }

    #[test]
    fn skill_eval_workspace_and_integrations_are_confined_to_temporary_mocks() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(workspace.path().join("input.txt"), "source").unwrap();
        let integrations = std::collections::BTreeMap::from([(
            "calendar.create_event".to_owned(),
            serde_json::json!({"event_id":"mock-1"}),
        )]);
        let mut actions = Vec::new();
        let mut integration_calls = Vec::new();
        let read = execute_skill_eval_tool(
            workspace.path(),
            "workspace.read_file",
            &serde_json::json!({"path":"input.txt"}),
            &integrations,
            &mut actions,
            &mut integration_calls,
        )
        .unwrap();
        assert_eq!(read["content"], "source");
        execute_skill_eval_tool(
            workspace.path(),
            "workspace.write_file",
            &serde_json::json!({"path":"output/result.txt","content":"fixed"}),
            &integrations,
            &mut actions,
            &mut integration_calls,
        )
        .unwrap();
        let response = execute_skill_eval_tool(
            workspace.path(),
            "integration.mock_call",
            &serde_json::json!({"name":"calendar.create_event","input":{"title":"demo"}}),
            &integrations,
            &mut actions,
            &mut integration_calls,
        )
        .unwrap();
        assert_eq!(response["event_id"], "mock-1");
        assert_eq!(integration_calls, ["calendar.create_event"]);
        assert_eq!(
            std::fs::read_to_string(workspace.path().join("output/result.txt")).unwrap(),
            "fixed"
        );
        assert!(
            execute_skill_eval_tool(
                workspace.path(),
                "workspace.write_file",
                &serde_json::json!({"path":"../escape.txt","content":"no"}),
                &integrations,
                &mut actions,
                &mut integration_calls,
            )
            .is_err()
        );
        assert!(
            !workspace
                .path()
                .parent()
                .unwrap()
                .join("escape.txt")
                .exists()
        );
        assert!(!valid_skill_eval_path(".env"));
        assert!(!valid_skill_eval_path("src/../../secret.txt"));
    }

    #[tokio::test]
    async fn skill_eval_executes_model_tools_only_in_isolated_workspace() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let responses = [
                serde_json::json!({"id":"eval-1","model":"local","choices":[{"index":0,"message":{"role":"assistant","content":null,"tool_calls":[{"id":"call-1","type":"function","function":{"name":"workspace.write_file","arguments":"{\"path\":\"result.txt\",\"content\":\"done\"}"}}]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":8,"completion_tokens":4,"total_tokens":12}}),
                serde_json::json!({"id":"eval-2","model":"local","choices":[{"index":0,"message":{"role":"assistant","content":"done","tool_calls":[]},"finish_reason":"stop"}],"usage":{"prompt_tokens":12,"completion_tokens":3,"total_tokens":15}}),
            ];
            for response in responses {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = [0u8; 16_384];
                let _ = socket.read(&mut request).await.unwrap();
                let body = serde_json::to_vec(&response).unwrap();
                socket.write_all(format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                ).as_bytes()).await.unwrap();
                socket.write_all(&body).await.unwrap();
            }
        });
        let client = execlaw_inference_api::InferenceClient::new(format!("http://{address}/v1"));
        let expected_files =
            std::collections::BTreeMap::from([("result.txt".to_owned(), "done".to_owned())]);
        let outcome = execute_skill_case(
            &client,
            "local-model",
            "Write the requested result file.",
            "Create result.txt containing done.",
            &["done".to_owned()],
            &[],
            1,
            100,
            32,
            &std::collections::BTreeMap::new(),
            &expected_files,
            &std::collections::BTreeMap::new(),
            &[],
            &[],
        )
        .await
        .unwrap();
        assert!(outcome.passed);
        assert_eq!(outcome.workspace_matches, 1);
        assert_eq!(outcome.action_count, 1);
        assert_eq!(outcome.output_tokens, 7);
        assert!(outcome.token_usage_complete);
        server.await.unwrap();
    }

    /// Seed a user + issue a bearer access token for them. Returns
    /// `(app, "Bearer <jwt>")` ready to drop on a request.
    async fn seed_user_and_token(role: UserRole) -> (axum::Router, String) {
        let state = test_app_state();
        UserStore::new(&state.db)
            .insert(&UserRow {
                user_id: "u-test".into(),
                username: "tester".into(),
                display_name: "Tester".into(),
                email: None,
                password_hash: "argon2-placeholder".into(),
                role,
                created_at: 0,
                last_login_at: None,
            })
            .expect("insert user");
        let token = state
            .signer
            .issue_access_token("u-test", "session-test", 600)
            .expect("issue token");
        state
            .refresh_store
            .issue(
                "u-test",
                "session-test",
                state.config.refresh_token_ttl_secs,
            )
            .expect("persist test session");
        (build_router(state), format!("Bearer {token}"))
    }

    async fn post_create(
        app: axum::Router,
        bearer: &str,
        body: serde_json::Value,
    ) -> (StatusCode, serde_json::Value) {
        let req = Request::builder()
            .method(Method::POST)
            .uri("/api/admin/skills")
            .header(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            )
            .header(
                header::AUTHORIZATION,
                HeaderValue::from_str(bearer).unwrap(),
            )
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or_default();
        (status, v)
    }

    #[tokio::test]
    async fn controller_creates_skill_returns_detail() {
        let (app, bearer) = seed_user_and_token(UserRole::Controller).await;
        let (status, body) = post_create(
            app,
            &bearer,
            serde_json::json!({
                "name": "test/web-browsing",
                "description": "Use the search and fetch tools.",
                "body_md": "# Web Browsing\n\nReach for web_search first.",
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body={body}");
        assert_eq!(body["name"], "test/web-browsing");
        assert_eq!(body["state"], "trial");
        assert_eq!(body["registration_kind"], "authored");
        assert_eq!(body["current_version"], 1);
        assert!(
            body["body_md"]
                .as_str()
                .unwrap()
                .contains("Reach for web_search first"),
            "body must round-trip; got {body}"
        );
        assert_eq!(
            body["authored_by"], "admin:u-test",
            "operator id must land in authored_by"
        );
    }

    #[tokio::test]
    async fn non_controller_create_is_forbidden() {
        let (app, bearer) = seed_user_and_token(UserRole::Operator).await;
        let (status, body) = post_create(
            app,
            &bearer,
            serde_json::json!({
                "name": "test/foo",
                "description": "x",
                "body_md": "x",
            }),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body["error"]["code"], "controller_required");
    }

    #[tokio::test]
    async fn duplicate_name_returns_409() {
        let (app, bearer) = seed_user_and_token(UserRole::Controller).await;
        let payload = serde_json::json!({
            "name": "test/dup",
            "description": "d",
            "body_md": "b",
        });
        let (s1, _) = post_create(app.clone(), &bearer, payload.clone()).await;
        assert_eq!(s1, StatusCode::OK);
        let (s2, body) = post_create(app, &bearer, payload).await;
        assert_eq!(s2, StatusCode::CONFLICT);
        assert_eq!(body["error"]["code"], "already_exists");
    }

    #[tokio::test]
    async fn invalid_name_returns_400() {
        let (app, bearer) = seed_user_and_token(UserRole::Controller).await;
        let (status, body) = post_create(
            app,
            &bearer,
            serde_json::json!({
                "name": "NoSlash",
                "description": "x",
                "body_md": "x",
            }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], "invalid_name");
    }

    /// 2026-05-16 — fix #P3 (Codex review): malformed JSON in the
    /// composer's custom-frontmatter field surfaced as 500 pre-fix
    /// because `InvalidFrontmatter` fell through the catch-all
    /// branch of `skill_err`. The OpenAPI schema documents 400 for
    /// caller-input validation; the SPA's inline error chip relies
    /// on the 400 status to surface a "fix your JSON" message rather
    /// than the generic "server error, retry" loop.
    #[tokio::test]
    async fn invalid_frontmatter_returns_400() {
        let (app, bearer) = seed_user_and_token(UserRole::Controller).await;
        let (status, body) = post_create(
            app,
            &bearer,
            serde_json::json!({
                "name": "test/bad-fm",
                "description": "x",
                "body_md": "x",
                // Not parseable JSON.
                "frontmatter_json": "{not: valid json}",
            }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], "invalid_frontmatter");
    }

    /// Skill bodies above `MAX_BODY_BYTES` surface as 413 with an
    /// `body_too_large` code (was 500 pre-fix).
    #[tokio::test]
    async fn oversized_body_returns_413() {
        let (app, bearer) = seed_user_and_token(UserRole::Controller).await;
        // MAX_BODY_BYTES = 256 KiB; ship one byte over.
        let oversized = "a".repeat((execlaw_skills::MAX_BODY_BYTES as usize) + 1);
        let (status, body) = post_create(
            app,
            &bearer,
            serde_json::json!({
                "name": "test/big-body",
                "description": "x",
                "body_md": oversized,
            }),
        )
        .await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(body["error"]["code"], "body_too_large");
    }

    #[tokio::test]
    async fn create_without_auth_header_returns_401() {
        let state = test_app_state();
        let app = build_router(state);
        let req = Request::builder()
            .method(Method::POST)
            .uri("/api/admin/skills")
            .header(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            )
            .body(Body::from(
                serde_json::to_vec(&serde_json::json!({
                    "name": "test/foo",
                    "description": "x",
                    "body_md": "x",
                }))
                .unwrap(),
            ))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }
}

pub fn skills_admin_router() -> Router<AppState> {
    Router::new()
        .route("/api/admin/skills", get(list_handler).post(create_handler))
        .route(
            "/api/admin/skills/config",
            get(get_config_handler).put(update_config_handler),
        )
        .route("/api/admin/skills/proposals", get(list_proposals_handler))
        .route(
            "/api/admin/skills/proposals/{id}/approve",
            post(approve_proposal_handler),
        )
        .route(
            "/api/admin/skills/proposals/{id}/reject",
            post(reject_proposal_handler),
        )
        .route(
            "/api/admin/skills/{name}",
            get(get_handler).put(update_body_handler),
        )
        .route(
            "/api/admin/skills/{name}/versions",
            get(list_versions_handler),
        )
        .route("/api/admin/skills/{name}/promote", post(promote_handler))
        .route(
            "/api/admin/skills/{name}/rollback",
            post(rollback_skill_handler),
        )
        .route(
            "/api/admin/skills/{name}/eval-suite",
            axum::routing::put(save_skill_eval_suite),
        )
        .route("/api/admin/skills/{name}/evaluate", post(evaluate_skill))
        .route("/api/admin/skills/{name}/archive", post(archive_handler))
}
