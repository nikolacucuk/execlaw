//! Controller API for always-on child-agent definitions and runs.

use crate::auth_extract::AuthedUser;
use crate::routes::ApiError;
use crate::state::AppState;
use axum::{
    Router,
    extract::{Path, Query, State},
    response::Json,
    routing::{get, post},
};
use execlaw_core::agent_contract::{
    AgentEvent, AgentPreviewDecision, AgentTriggerSpec, preview_events,
};
use execlaw_core::agent_ownership::{AgentOwnership, AgentOwnershipStore};
use execlaw_core::agents::{AgentError, AgentStore, AgentUpsert};
use execlaw_core::audit::AuditStore;
use execlaw_core::reply_drafts::{ReplyDraft, ReplyDraftStore};
use execlaw_core::users::UserRole;
use execlaw_inference_api::{ChatMessage, ChatRequest, ModelId};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Debug, Serialize, ToSchema)]
pub struct AgentView {
    pub id: String,
    pub name: String,
    pub role_prompt: String,
    pub model: Option<String>,
    pub backend_purpose: String,
    pub tools: Vec<String>,
    pub trust_policy: serde_json::Value,
    pub interval_secs: u32,
    pub token_budget: u32,
    pub max_runtime_secs: u32,
    pub concurrency_limit: u32,
    pub enabled: bool,
    pub paused: bool,
    pub next_run_at: Option<i64>,
    pub last_run_at: Option<i64>,
    pub last_run_status: Option<String>,
    pub last_error: Option<String>,
    pub trigger: serde_json::Value,
    pub reply_mode: String,
    pub definition_version: u32,
    pub schedule_next_at: Option<i64>,
}
impl From<execlaw_core::agents::AgentRow> for AgentView {
    fn from(a: execlaw_core::agents::AgentRow) -> Self {
        Self {
            id: a.id,
            name: a.name,
            role_prompt: a.role_prompt,
            model: a.model,
            backend_purpose: a.backend_purpose,
            tools: a.tools,
            trust_policy: a.trust_policy,
            interval_secs: a.interval_secs,
            token_budget: a.token_budget,
            max_runtime_secs: a.max_runtime_secs,
            concurrency_limit: a.concurrency_limit,
            enabled: a.enabled,
            paused: a.paused,
            next_run_at: a.next_run_at,
            last_run_at: a.last_run_at,
            last_run_status: a.last_run_status,
            last_error: a.last_error,
            trigger: a.trigger,
            reply_mode: a.reply_mode,
            definition_version: a.definition_version,
            schedule_next_at: a.schedule_next_at,
        }
    }
}
#[derive(Debug, Deserialize, ToSchema)]
pub struct AgentRequest {
    pub id: Option<String>,
    pub name: String,
    pub role_prompt: String,
    pub model: Option<String>,
    #[serde(default = "standard")]
    pub backend_purpose: String,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub trust_policy: serde_json::Value,
    #[serde(default = "default_interval")]
    pub interval_secs: u32,
    #[serde(default = "default_tokens")]
    pub token_budget: u32,
    #[serde(default = "default_runtime")]
    pub max_runtime_secs: u32,
    #[serde(default = "one")]
    pub concurrency_limit: u32,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default)]
    pub trigger: serde_json::Value,
    #[serde(default = "draft_mode")]
    pub reply_mode: String,
    #[serde(default)]
    #[schema(value_type = Option<serde_json::Value>)]
    pub completion_contract: Option<execlaw_core::runs::RunCompletionContractDraft>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct AgentMarkdownRequest {
    pub markdown: String,
}
fn draft_mode() -> String {
    "draft".into()
}
fn standard() -> String {
    "standard".into()
}
fn default_interval() -> u32 {
    300
}
fn default_tokens() -> u32 {
    1024
}
fn default_runtime() -> u32 {
    300
}
fn one() -> u32 {
    1
}
fn yes() -> bool {
    true
}
#[derive(Debug, Deserialize)]
pub struct Limit {
    pub limit: Option<u32>,
}
fn controller(user: &AuthedUser) -> Result<(), ApiError> {
    if user.role == UserRole::Controller {
        Ok(())
    } else {
        Err(ApiError {
            status: axum::http::StatusCode::FORBIDDEN,
            code: "controller_required",
            message: "Controller role required".into(),
        })
    }
}
fn map(e: AgentError) -> ApiError {
    ApiError {
        status: axum::http::StatusCode::BAD_REQUEST,
        code: "agent_error",
        message: e.to_string(),
    }
}
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/admin/agents", get(list).post(create))
        .route("/api/admin/agents/preview", post(preview))
        .route("/api/admin/agents/reply-drafts", get(reply_draft_inbox))
        .route(
            "/api/admin/agents/reply-drafts/{id}",
            axum::routing::put(edit_reply_draft),
        )
        .route(
            "/api/admin/agents/reply-drafts/{id}/reject",
            post(reject_reply_draft),
        )
        .route("/api/admin/agents/ownership", get(get_ownership))
        .route("/api/admin/agents/ownership/takeover", post(takeover))
        .route("/api/admin/agents/ownership/handback", post(handback))
        .route("/api/admin/agents/import-markdown", post(import_markdown))
        .route(
            "/api/admin/agents/{id}",
            get(get_one).put(update).delete(remove),
        )
        .route("/api/admin/agents/{id}/pause", post(pause))
        .route("/api/admin/agents/{id}/resume", post(resume))
        .route("/api/admin/agents/{id}/messages", post(message))
        .route("/api/admin/agents/{id}/runs", get(runs))
        .route("/api/admin/agents/{id}/schedule-fires", get(schedule_fires))
        .route(
            "/api/admin/agents/{id}/completion-contract",
            get(get_agent_completion_contract).put(set_agent_completion_contract),
        )
        .route(
            "/api/admin/agents/{id}/runs/{run_id}/completion/criteria/{criterion_id}",
            axum::routing::put(record_agent_criterion),
        )
        .route(
            "/api/admin/agents/{id}/runs/{run_id}/completion/artifacts/{artifact_id}",
            axum::routing::put(record_agent_artifact),
        )
        .route(
            "/api/admin/agents/{id}/runs/{run_id}/completion/delivery",
            post(confirm_agent_delivery),
        )
}

#[derive(Debug, Deserialize)]
struct AgentPreviewRequest {
    trigger: serde_json::Value,
    events: Vec<AgentEvent>,
    #[serde(default)]
    expected_event_ids: Vec<String>,
    #[serde(default)]
    expected_outcomes: std::collections::HashMap<String, String>,
    #[serde(default)]
    run_model: bool,
    agent_id: Option<String>,
}

#[derive(Debug, Serialize)]
struct AgentModelPreview {
    event_id: String,
    outcome_kind: Option<String>,
    error: Option<String>,
    latency_ms: u128,
}

#[derive(Debug, Serialize)]
struct AgentPreviewResponse {
    decisions: Vec<AgentPreviewDecision>,
    matched: usize,
    false_positives: usize,
    missed: usize,
    effect_count: u32,
    model_results: Vec<AgentModelPreview>,
    outcome_mismatches: usize,
    missing_drafts: usize,
    irrelevant_count: usize,
    max_latency_ms: u128,
}

async fn preview(
    State(state): State<AppState>,
    user: AuthedUser,
    Json(request): Json<AgentPreviewRequest>,
) -> Result<Json<AgentPreviewResponse>, ApiError> {
    controller(&user)?;
    if request.events.len() > 1000 {
        return Err(map(AgentError::Invalid(
            "preview accepts at most 1000 events".into(),
        )));
    }
    let trigger = AgentTriggerSpec::from_value(&request.trigger)
        .map_err(|error| map(AgentError::Invalid(error)))?;
    let decisions = preview_events(&trigger, &request.events)
        .map_err(|error| map(AgentError::Invalid(error)))?;
    let expected = request
        .expected_event_ids
        .into_iter()
        .collect::<std::collections::HashSet<_>>();
    let matched = decisions.iter().filter(|decision| decision.matched).count();
    let false_positives = decisions
        .iter()
        .filter(|decision| {
            decision.matched && !expected.is_empty() && !expected.contains(&decision.event_id)
        })
        .count();
    let missed = decisions
        .iter()
        .filter(|decision| !decision.matched && expected.contains(&decision.event_id))
        .count();
    let mut model_results = Vec::new();
    if request.run_model {
        let agent_id = request.agent_id.as_deref().ok_or_else(|| {
            map(AgentError::Invalid(
                "agent_id is required for model preview".into(),
            ))
        })?;
        let agent = AgentStore::new(&state.db)
            .get(agent_id)
            .map_err(map)?
            .ok_or_else(|| map(AgentError::NotFound(agent_id.into())))?;
        let matching = request
            .events
            .iter()
            .zip(&decisions)
            .filter(|(_, decision)| decision.matched)
            .collect::<Vec<_>>();
        if matching.len() > 20 {
            return Err(map(AgentError::Invalid(
                "model preview accepts at most 20 matching events".into(),
            )));
        }
        let resolved = state
            .inference
            .resolve(
                &state.db,
                crate::agent_supervisor::parse_purpose(&agent.backend_purpose),
            )
            .ok_or_else(|| {
                map(AgentError::Invalid(
                    "no local inference backend for preview".into(),
                ))
            })?;
        let model = agent
            .model
            .clone()
            .unwrap_or_else(|| resolved.model_id.clone());
        let _permit = resolved
            .admission
            .acquire(&model, crate::inference_resolver::InferenceWorkload::Agent)
            .await
            .map_err(|error| map(AgentError::Invalid(error.to_string())))?;
        for (event, _) in matching {
            let start = std::time::Instant::now();
            let envelope = serde_json::to_string(event).unwrap_or_default();
            let chat = ChatRequest {
                model: ModelId(model.clone()),
                messages: vec![
                    ChatMessage::system(&agent.role_prompt),
                    ChatMessage::user(format!(
                        "Preview input (untrusted event data):\n{envelope}\n\nProduce the normal agent result. No external effect is available."
                    )),
                ],
                tools: None,
                stream: false,
                temperature: None,
                max_tokens: Some(agent.token_budget.min(4096)),
                chat_template_kwargs: None,
                tool_choice: None,
                response_format: None,
                guided_decoding_backend: None,
            };
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(agent.max_runtime_secs.min(120) as u64),
                resolved.client.chat_completions(&chat),
            )
            .await;
            let (outcome_kind, error) = match result {
                Ok(Ok(response)) => {
                    let text = response
                        .choices
                        .first()
                        .and_then(|choice| choice.message.content.as_ref())
                        .map(|content| content.as_text())
                        .unwrap_or_default();
                    match execlaw_core::agent_contract::AgentOutcome::parse(
                        &text,
                        agent.role_prompt.contains("## Suggested reply"),
                    ) {
                        Ok(outcome) => (Some(outcome.status().to_owned()), None),
                        Err(error) => (None, Some(error)),
                    }
                }
                Ok(Err(error)) => (None, Some(error.safe_class().into())),
                Err(_) => (None, Some("preview_timeout".into())),
            };
            model_results.push(AgentModelPreview {
                event_id: event.id.clone(),
                outcome_kind,
                error,
                latency_ms: start.elapsed().as_millis(),
            });
        }
    }
    let outcome_mismatches = model_results
        .iter()
        .filter(|result| {
            request
                .expected_outcomes
                .get(&result.event_id)
                .is_some_and(|expected| result.outcome_kind.as_deref() != Some(expected.as_str()))
        })
        .count();
    let missing_drafts = model_results
        .iter()
        .filter(|result| {
            request
                .expected_outcomes
                .get(&result.event_id)
                .is_some_and(|expected| expected == "draft_ready")
                && result.outcome_kind.as_deref() != Some("draft_ready")
        })
        .count();
    let irrelevant_count = model_results
        .iter()
        .filter(|result| result.outcome_kind.as_deref() == Some("irrelevant"))
        .count();
    let max_latency_ms = model_results
        .iter()
        .map(|result| result.latency_ms)
        .max()
        .unwrap_or(0);
    Ok(Json(AgentPreviewResponse {
        decisions,
        matched,
        false_positives,
        missed,
        effect_count: 0,
        model_results,
        outcome_mismatches,
        missing_drafts,
        irrelevant_count,
        max_latency_ms,
    }))
}

async fn reply_draft_inbox(
    State(state): State<AppState>,
    user: AuthedUser,
) -> Result<Json<Vec<ReplyDraft>>, ApiError> {
    controller(&user)?;
    ReplyDraftStore::new(&state.db)
        .inbox(100)
        .map(Json)
        .map_err(|error| map(AgentError::Db(error)))
}

#[derive(Debug, Deserialize)]
struct DraftEditRequest {
    revision: u32,
    text: String,
}

async fn edit_reply_draft(
    State(state): State<AppState>,
    user: AuthedUser,
    Path(id): Path<String>,
    Json(request): Json<DraftEditRequest>,
) -> Result<Json<ReplyDraft>, ApiError> {
    controller(&user)?;
    ReplyDraftStore::new(&state.db)
        .edit(&id, request.revision, &request.text)
        .map(Json)
        .map_err(|error| map(AgentError::Db(error)))
}

#[derive(Debug, Deserialize)]
struct DraftRejectRequest {
    revision: u32,
}

async fn reject_reply_draft(
    State(state): State<AppState>,
    user: AuthedUser,
    Path(id): Path<String>,
    Json(request): Json<DraftRejectRequest>,
) -> Result<Json<bool>, ApiError> {
    controller(&user)?;
    ReplyDraftStore::new(&state.db)
        .reject(
            &id,
            request.revision,
            &user.user_id,
            chrono::Utc::now().timestamp(),
        )
        .map_err(|error| map(AgentError::Db(error)))?;
    Ok(Json(true))
}

#[derive(Debug, Deserialize)]
struct OwnershipScope {
    conversation_id: String,
    channel: String,
    recipient: String,
}

async fn get_ownership(
    State(state): State<AppState>,
    user: AuthedUser,
    Query(scope): Query<OwnershipScope>,
) -> Result<Json<Option<AgentOwnership>>, ApiError> {
    controller(&user)?;
    AgentOwnershipStore::new(&state.db)
        .get(&scope.conversation_id, &scope.channel, &scope.recipient)
        .map(Json)
        .map_err(|error| map(AgentError::Db(error)))
}

async fn takeover(
    State(state): State<AppState>,
    user: AuthedUser,
    Json(scope): Json<OwnershipScope>,
) -> Result<Json<AgentOwnership>, ApiError> {
    controller(&user)?;
    AgentOwnershipStore::new(&state.db)
        .takeover(
            &scope.conversation_id,
            &scope.channel,
            &scope.recipient,
            chrono::Utc::now().timestamp(),
        )
        .map(Json)
        .map_err(|error| map(AgentError::Db(error)))
}

#[derive(Debug, Deserialize)]
struct HandbackRequest {
    conversation_id: String,
    channel: String,
    recipient: String,
    generation: u64,
}

async fn handback(
    State(state): State<AppState>,
    user: AuthedUser,
    Json(request): Json<HandbackRequest>,
) -> Result<Json<bool>, ApiError> {
    controller(&user)?;
    AgentOwnershipStore::new(&state.db)
        .handback(
            &request.conversation_id,
            &request.channel,
            &request.recipient,
            request.generation,
        )
        .map_err(|error| map(AgentError::Db(error)))?;
    Ok(Json(true))
}
async fn list(State(s): State<AppState>, _: AuthedUser) -> Result<Json<Vec<AgentView>>, ApiError> {
    Ok(Json(
        AgentStore::new(&s.db)
            .list()
            .map_err(map)?
            .into_iter()
            .map(Into::into)
            .collect(),
    ))
}
async fn get_one(
    State(s): State<AppState>,
    _: AuthedUser,
    Path(id): Path<String>,
) -> Result<Json<AgentView>, ApiError> {
    AgentStore::new(&s.db)
        .get(&id)
        .map_err(map)?
        .map(|a| Json(a.into()))
        .ok_or_else(|| map(AgentError::NotFound(id)))
}
async fn create(
    State(s): State<AppState>,
    u: AuthedUser,
    Json(r): Json<AgentRequest>,
) -> Result<Json<AgentView>, ApiError> {
    controller(&u)?;
    let completion_contract = r.completion_contract.clone();
    let a = AgentStore::new(&s.db)
        .upsert(
            &AgentUpsert {
                id: r.id,
                name: r.name,
                role_prompt: r.role_prompt,
                model: r.model,
                backend_purpose: r.backend_purpose,
                tools: r.tools,
                trust_policy: r.trust_policy,
                interval_secs: r.interval_secs,
                token_budget: r.token_budget,
                max_runtime_secs: r.max_runtime_secs,
                concurrency_limit: r.concurrency_limit,
                enabled: r.enabled,
                trigger: r.trigger,
                reply_mode: r.reply_mode,
            },
            chrono::Utc::now().timestamp(),
        )
        .map_err(map)?;
    if let Some(contract) = completion_contract.as_ref() {
        AgentStore::new(&s.db)
            .set_completion_contract(&a.id, Some(contract), chrono::Utc::now().timestamp())
            .map_err(map)?;
    }
    Ok(Json(a.into()))
}

async fn import_markdown(
    State(s): State<AppState>,
    u: AuthedUser,
    Json(r): Json<AgentMarkdownRequest>,
) -> Result<Json<AgentView>, ApiError> {
    controller(&u)?;
    let (frontmatter, role_prompt) = split_frontmatter(&r.markdown)?;
    let name = frontmatter
        .get("name")
        .cloned()
        .unwrap_or_else(|| "Imported agent".into());
    let id = name.to_ascii_lowercase().replace([' ', '-', '.'], "_");
    let trigger = markdown_trigger(&frontmatter);
    let agent = AgentStore::new(&s.db)
        .upsert(
            &AgentUpsert {
                id: Some(id.clone()),
                name,
                role_prompt,
                model: None,
                backend_purpose: "standard".into(),
                tools: markdown_tools(&frontmatter)?,
                trust_policy: serde_json::json!({}),
                interval_secs: 300,
                token_budget: 1200,
                max_runtime_secs: 120,
                concurrency_limit: 1,
                enabled: true,
                trigger,
                reply_mode: "draft".into(),
            },
            chrono::Utc::now().timestamp(),
        )
        .map_err(map)?;
    Ok(Json(agent.into()))
}

fn markdown_trigger(frontmatter: &std::collections::HashMap<String, String>) -> serde_json::Value {
    let mut trigger = serde_json::Map::new();
    for key in ["channel", "event_only", "group_only", "observer"] {
        if let Some(value) = frontmatter.get(key) {
            let value = if matches!(key, "event_only" | "group_only" | "observer") {
                serde_json::Value::Bool(value.eq_ignore_ascii_case("true"))
            } else {
                serde_json::Value::String(value.clone())
            };
            trigger.insert(key.to_owned(), value);
        }
    }
    if let Some(keywords) = frontmatter.get("keywords") {
        let keywords = comma_separated_values(keywords);
        if !keywords.is_empty() {
            trigger.insert("keywords".to_owned(), serde_json::json!(keywords));
        }
    }
    if let Some(group_ids) = frontmatter.get("group_ids") {
        trigger.insert(
            "group_ids".into(),
            serde_json::json!(comma_separated_values(group_ids)),
        );
    }
    if let Some(priority) = frontmatter
        .get("priority")
        .and_then(|value| value.parse::<i32>().ok())
    {
        trigger.insert("priority".into(), serde_json::json!(priority));
    }
    if let Some(group_titles) = frontmatter.get("group_titles") {
        let group_titles = semicolon_separated_values(group_titles);
        if !group_titles.is_empty() {
            trigger.insert("group_titles".to_owned(), serde_json::json!(group_titles));
        }
    }
    if let Some(cron) = frontmatter.get("schedule_cron") {
        let quiet_hours = match (frontmatter.get("quiet_start"), frontmatter.get("quiet_end")) {
            (Some(start), Some(end)) => Some(serde_json::json!({"start":start,"end":end})),
            _ => None,
        };
        trigger.insert("schedule".into(), serde_json::json!({
            "cron": cron,
            "timezone": frontmatter.get("timezone").map(String::as_str).unwrap_or("UTC"),
            "overlap": frontmatter.get("overlap").map(String::as_str).unwrap_or("skip"),
            "catchup_secs": frontmatter.get("catchup_secs").and_then(|value| value.parse::<u32>().ok()).unwrap_or(3600),
            "quiet_hours": quiet_hours,
            "target_conversation_id": frontmatter.get("target_conversation_id"),
        }));
    }
    serde_json::Value::Object(trigger)
}

fn markdown_tools(
    frontmatter: &std::collections::HashMap<String, String>,
) -> Result<Vec<String>, ApiError> {
    let tools = frontmatter
        .get("tools")
        .map(|value| comma_separated_values(value))
        .unwrap_or_default();
    if tools
        .iter()
        .any(|tool| !matches!(tool.as_str(), "read" | "search"))
    {
        return Err(ApiError {
            status: axum::http::StatusCode::BAD_REQUEST,
            code: "unsupported_agent_tool",
            message: "always-on agents support scoped read and search only".into(),
        });
    }
    Ok(tools)
}

fn comma_separated_values(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn semicolon_separated_values(value: &str) -> Vec<String> {
    value
        .split(';')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn split_frontmatter(
    markdown: &str,
) -> Result<(std::collections::HashMap<String, String>, String), ApiError> {
    let mut lines = markdown.lines();
    if lines.next() != Some("---") {
        return Err(ApiError {
            status: axum::http::StatusCode::BAD_REQUEST,
            code: "invalid_agent_markdown",
            message: "agent Markdown must start with YAML frontmatter".into(),
        });
    }
    let mut frontmatter = std::collections::HashMap::new();
    let mut body = Vec::new();
    let mut in_frontmatter = true;
    let mut list_key: Option<String> = None;
    for line in lines {
        if in_frontmatter && line == "---" {
            in_frontmatter = false;
        } else if in_frontmatter {
            if let Some(value) = line.trim().strip_prefix("- ") {
                if let Some(key) = list_key.as_ref() {
                    frontmatter
                        .entry(key.clone())
                        .and_modify(|current: &mut String| {
                            if !current.is_empty() {
                                current.push(',');
                            }
                            current.push_str(value.trim());
                        })
                        .or_insert_with(|| value.trim().to_owned());
                    continue;
                }
            }
            if let Some((key, value)) = line.split_once(':') {
                list_key = if value.trim().is_empty() {
                    Some(key.trim().to_owned())
                } else {
                    None
                };
                frontmatter.insert(
                    key.trim().to_owned(),
                    value.trim().trim_matches('"').to_owned(),
                );
            }
        } else {
            body.push(line);
        }
    }
    if in_frontmatter || body.iter().all(|line| line.trim().is_empty()) {
        return Err(ApiError {
            status: axum::http::StatusCode::BAD_REQUEST,
            code: "invalid_agent_markdown",
            message: "agent Markdown has no role body".into(),
        });
    }
    Ok((frontmatter, body.join("\n").trim().to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_trigger_preserves_event_scoped_camper_configuration() {
        let (frontmatter, _) = split_frontmatter(
            "---\nname: camper_wha\nchannel: whatsapp\nevent_only: true\ngroup_only: true\nkeywords: camper, camper van, motorhome, camping\ngroup_titles: 1th Sept 2026, Luka Villa, Montenegro\n---\nDraft replies.",
        )
        .unwrap();

        assert_eq!(
            markdown_trigger(&frontmatter),
            serde_json::json!({
                "channel": "whatsapp",
                "event_only": true,
                "group_only": true,
                "keywords": ["camper", "camper van", "motorhome", "camping"],
                "group_titles": ["1th Sept 2026, Luka Villa, Montenegro"],
            })
        );
    }

    #[test]
    fn markdown_import_reads_scoped_tools_and_calendar_policy() {
        let (frontmatter, _) = split_frontmatter("---\nname: morning\ntools:\n  - read\n  - search\nschedule_cron: 0 8 * * *\ntimezone: America/Vancouver\noverlap: buffer_one\ncatchup_secs: 1800\nquiet_start: 22:00\nquiet_end: 07:00\n---\nReport pending questions.").unwrap();
        assert_eq!(
            markdown_tools(&frontmatter).unwrap(),
            vec!["read", "search"]
        );
        let trigger = markdown_trigger(&frontmatter);
        assert_eq!(trigger["schedule"]["timezone"], "America/Vancouver");
        assert_eq!(trigger["schedule"]["overlap"], "buffer_one");
        execlaw_core::agent_contract::AgentTriggerSpec::from_value(&trigger).unwrap();
    }
}
async fn update(
    State(s): State<AppState>,
    u: AuthedUser,
    Path(id): Path<String>,
    Json(r): Json<AgentRequest>,
) -> Result<Json<AgentView>, ApiError> {
    controller(&u)?;
    let completion_contract = r.completion_contract.clone();
    let a = AgentStore::new(&s.db)
        .upsert(
            &AgentUpsert {
                id: Some(id),
                name: r.name,
                role_prompt: r.role_prompt,
                model: r.model,
                backend_purpose: r.backend_purpose,
                tools: r.tools,
                trust_policy: r.trust_policy,
                interval_secs: r.interval_secs,
                token_budget: r.token_budget,
                max_runtime_secs: r.max_runtime_secs,
                concurrency_limit: r.concurrency_limit,
                enabled: r.enabled,
                trigger: r.trigger,
                reply_mode: r.reply_mode,
            },
            chrono::Utc::now().timestamp(),
        )
        .map_err(map)?;
    if let Some(contract) = completion_contract.as_ref() {
        AgentStore::new(&s.db)
            .set_completion_contract(&a.id, Some(contract), chrono::Utc::now().timestamp())
            .map_err(map)?;
    }
    Ok(Json(a.into()))
}
async fn remove(
    State(s): State<AppState>,
    u: AuthedUser,
    Path(id): Path<String>,
) -> Result<Json<bool>, ApiError> {
    controller(&u)?;
    Ok(Json(AgentStore::new(&s.db).delete(&id).map_err(map)?))
}
async fn pause(
    State(s): State<AppState>,
    u: AuthedUser,
    Path(id): Path<String>,
) -> Result<Json<bool>, ApiError> {
    controller(&u)?;
    AgentStore::new(&s.db)
        .set_state(&id, None, Some(true), None)
        .map_err(map)?;
    Ok(Json(true))
}
async fn resume(
    State(s): State<AppState>,
    u: AuthedUser,
    Path(id): Path<String>,
) -> Result<Json<bool>, ApiError> {
    controller(&u)?;
    AgentStore::new(&s.db)
        .set_state(&id, None, Some(false), None)
        .map_err(map)?;
    Ok(Json(true))
}
async fn message(
    State(s): State<AppState>,
    u: AuthedUser,
    Path(id): Path<String>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<String>, ApiError> {
    controller(&u)?;
    let content = body.get("content").and_then(|v| v.as_str()).unwrap_or("");
    let message_id = AgentStore::new(&s.db)
        .enqueue(
            &id,
            Some("controller"),
            content,
            chrono::Utc::now().timestamp(),
        )
        .map_err(map)?;
    crate::agent_supervisor::AgentSupervisor::kick_global();
    Ok(Json(message_id))
}
async fn runs(
    State(s): State<AppState>,
    _: AuthedUser,
    Path(id): Path<String>,
    Query(q): Query<Limit>,
) -> Result<Json<Vec<execlaw_core::agents::AgentRunRow>>, ApiError> {
    Ok(Json(
        AgentStore::new(&s.db)
            .runs(&id, q.limit.unwrap_or(50).min(200))
            .map_err(map)?,
    ))
}

async fn schedule_fires(
    State(state): State<AppState>,
    user: AuthedUser,
    Path(id): Path<String>,
) -> Result<Json<Vec<execlaw_core::agents::AgentScheduleFireRow>>, ApiError> {
    controller(&user)?;
    AgentStore::new(&state.db)
        .schedule_fires(&id, 50)
        .map(Json)
        .map_err(map)
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct AgentCriterionEvidenceRequest {
    #[schema(value_type = String)]
    pub status: execlaw_core::runs::VerificationStatus,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
    pub detail: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct AgentArtifactEvidenceRequest {
    pub present: bool,
    pub evidence_ref: Option<String>,
    pub detail: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct AgentDeliveryEvidenceRequest {
    pub evidence_ref: String,
}

#[utoipa::path(
    get,
    path = "/api/admin/agents/{id}/completion-contract",
    params(("id" = String, Path, description = "Agent definition")),
    responses((status = 200, description = "Default acceptance requirements for future agent runs", body = serde_json::Value)),
    security(("bearer_jwt" = [])),
    tag = "agents"
)]
pub async fn get_agent_completion_contract(
    State(state): State<AppState>,
    user: AuthedUser,
    Path(agent_id): Path<String>,
) -> Result<Json<Option<execlaw_core::runs::RunCompletionContractDraft>>, ApiError> {
    controller(&user)?;
    Ok(Json(
        AgentStore::new(&state.db)
            .completion_contract(&agent_id)
            .map_err(map)?,
    ))
}

#[utoipa::path(
    put,
    path = "/api/admin/agents/{id}/completion-contract",
    params(("id" = String, Path, description = "Agent definition")),
    request_body = serde_json::Value,
    responses((status = 200, description = "Stored agent-run acceptance requirements", body = serde_json::Value)),
    security(("bearer_jwt" = [])),
    tag = "agents"
)]
pub async fn set_agent_completion_contract(
    State(state): State<AppState>,
    user: AuthedUser,
    Path(agent_id): Path<String>,
    Json(contract): Json<Option<execlaw_core::runs::RunCompletionContractDraft>>,
) -> Result<Json<Option<execlaw_core::runs::RunCompletionContractDraft>>, ApiError> {
    controller(&user)?;
    let store = AgentStore::new(&state.db);
    store
        .set_completion_contract(&agent_id, contract.as_ref(), chrono::Utc::now().timestamp())
        .map_err(map)?;
    Ok(Json(store.completion_contract(&agent_id).map_err(map)?))
}

fn require_agent_run(store: &AgentStore, agent_id: &str, run_id: &str) -> Result<(), ApiError> {
    if store.run_belongs_to_agent(agent_id, run_id).map_err(map)? {
        Ok(())
    } else {
        Err(ApiError {
            status: axum::http::StatusCode::NOT_FOUND,
            code: "agent_run_not_found",
            message: "agent run not found".into(),
        })
    }
}

#[utoipa::path(
    put,
    path = "/api/admin/agents/{id}/runs/{run_id}/completion/criteria/{criterion_id}",
    params(("id" = String, Path), ("run_id" = String, Path), ("criterion_id" = String, Path)),
    request_body = AgentCriterionEvidenceRequest,
    responses((status = 200, description = "Updated agent-run completion report", body = serde_json::Value)),
    security(("bearer_jwt" = [])),
    tag = "agents"
)]
pub async fn record_agent_criterion(
    State(state): State<AppState>,
    user: AuthedUser,
    Path((agent_id, run_id, criterion_id)): Path<(String, String, String)>,
    Json(request): Json<AgentCriterionEvidenceRequest>,
) -> Result<Json<execlaw_core::runs::RunCompletionReport>, ApiError> {
    controller(&user)?;
    let store = AgentStore::new(&state.db);
    require_agent_run(&store, &agent_id, &run_id)?;
    let mut evidence_refs = request.evidence_refs;
    if request.status == execlaw_core::runs::VerificationStatus::Passed {
        execlaw_core::runs::validate_reference_list(&evidence_refs)
            .map_err(|error| map(AgentError::Invalid(error.to_string())))?;
        if evidence_refs.is_empty() {
            return Err(map(AgentError::Invalid(
                "a passing manual result needs submitted evidence".into(),
            )));
        }
        let audit = serde_json::json!({
            "status":"passed",
            "submitted_evidence_refs":&evidence_refs,
            "detail":&request.detail,
        });
        let id = AuditStore::new(&state.db)
            .insert(
                &user.user_id,
                "agent_run_completion_verification",
                &format!("{run_id}/{criterion_id}"),
                None,
                Some(&audit),
            )
            .map_err(|error| map(AgentError::Db(error)))?;
        evidence_refs.push(format!("attestation:{id}"));
    }
    store
        .record_completion_verification(
            &run_id,
            &execlaw_core::runs::CriterionVerification {
                criterion_id,
                status: request.status,
                evidence_refs,
                detail: request.detail,
                verified_at: chrono::Utc::now().timestamp(),
            },
        )
        .map_err(map)?;
    store
        .completion_report(&run_id)
        .map_err(map)?
        .map(Json)
        .ok_or_else(|| map(AgentError::NotFound(run_id)))
}

#[utoipa::path(
    put,
    path = "/api/admin/agents/{id}/runs/{run_id}/completion/artifacts/{artifact_id}",
    params(("id" = String, Path), ("run_id" = String, Path), ("artifact_id" = String, Path)),
    request_body = AgentArtifactEvidenceRequest,
    responses((status = 200, description = "Updated agent-run completion report", body = serde_json::Value)),
    security(("bearer_jwt" = [])),
    tag = "agents"
)]
pub async fn record_agent_artifact(
    State(state): State<AppState>,
    user: AuthedUser,
    Path((agent_id, run_id, artifact_id)): Path<(String, String, String)>,
    Json(request): Json<AgentArtifactEvidenceRequest>,
) -> Result<Json<execlaw_core::runs::RunCompletionReport>, ApiError> {
    controller(&user)?;
    let store = AgentStore::new(&state.db);
    require_agent_run(&store, &agent_id, &run_id)?;
    store
        .record_artifact_verification(
            &run_id,
            &execlaw_core::runs::ArtifactVerification {
                artifact_id,
                present: request.present,
                evidence_ref: request.evidence_ref,
                detail: request.detail,
                checked_at: chrono::Utc::now().timestamp(),
            },
        )
        .map_err(map)?;
    store
        .completion_report(&run_id)
        .map_err(map)?
        .map(Json)
        .ok_or_else(|| map(AgentError::NotFound(run_id)))
}

#[utoipa::path(
    post,
    path = "/api/admin/agents/{id}/runs/{run_id}/completion/delivery",
    params(("id" = String, Path), ("run_id" = String, Path)),
    request_body = AgentDeliveryEvidenceRequest,
    responses((status = 200, description = "Updated agent-run completion report", body = serde_json::Value)),
    security(("bearer_jwt" = [])),
    tag = "agents"
)]
pub async fn confirm_agent_delivery(
    State(state): State<AppState>,
    user: AuthedUser,
    Path((agent_id, run_id)): Path<(String, String)>,
    Json(request): Json<AgentDeliveryEvidenceRequest>,
) -> Result<Json<execlaw_core::runs::RunCompletionReport>, ApiError> {
    controller(&user)?;
    let store = AgentStore::new(&state.db);
    require_agent_run(&store, &agent_id, &run_id)?;
    let submitted = request.evidence_ref.trim();
    if submitted.is_empty() || submitted.len() > 512 {
        return Err(map(AgentError::Invalid(
            "delivery confirmation needs bounded submitted evidence".into(),
        )));
    }
    let audit = serde_json::json!({"status":"confirmed","submitted_evidence_ref":submitted});
    let id = AuditStore::new(&state.db)
        .insert(
            &user.user_id,
            "agent_run_completion_delivery",
            &run_id,
            None,
            Some(&audit),
        )
        .map_err(|error| map(AgentError::Db(error)))?;
    let evidence_ref = format!("attestation:{id}");
    store
        .confirm_completion_delivery(&run_id, &evidence_ref, chrono::Utc::now().timestamp())
        .map_err(map)?;
    store
        .completion_report(&run_id)
        .map_err(map)?
        .map(Json)
        .ok_or_else(|| map(AgentError::NotFound(run_id)))
}
