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
use execlaw_core::agents::{AgentError, AgentStore, AgentUpsert};
use execlaw_core::users::UserRole;
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
        }
    }
}
#[derive(Debug, Deserialize, ToSchema)]
pub struct AgentRequest {
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
}
fn draft_mode() -> String { "draft".into() }
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
        .route(
            "/api/admin/agents/{id}",
            get(get_one).put(update).delete(remove),
        )
        .route("/api/admin/agents/{id}/pause", post(pause))
        .route("/api/admin/agents/{id}/resume", post(resume))
        .route("/api/admin/agents/{id}/messages", post(message))
        .route("/api/admin/agents/{id}/runs", get(runs))
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
    let a = AgentStore::new(&s.db)
        .upsert(
            &AgentUpsert {
                id: None,
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
    Ok(Json(a.into()))
}
async fn update(
    State(s): State<AppState>,
    u: AuthedUser,
    Path(id): Path<String>,
    Json(r): Json<AgentRequest>,
) -> Result<Json<AgentView>, ApiError> {
    controller(&u)?;
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
    Ok(Json(
        AgentStore::new(&s.db)
            .enqueue(
                &id,
                Some("controller"),
                content,
                chrono::Utc::now().timestamp(),
            )
            .map_err(map)?,
    ))
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
