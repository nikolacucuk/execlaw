//! Admin HTTP surface for Settings → Tools (Phase 8a).
//!
//! Two routes:
//!   * `GET  /api/admin/tools` — list every row in `config_tool_access`.
//!   * `PATCH /api/admin/tools/{tool_name}` — update enabled / allowed_classes.
//!
//! Controller-only (mirrors the deployments + users surfaces). Every
//! mutation goes through `AuditStore` so an operator change is
//! visible in the audit log.

use crate::auth_extract::AuthedUser;
use crate::routes::ApiError;
use crate::state::AppState;
use axum::Router;
use axum::extract::{Path as AxumPath, State};
use axum::http::StatusCode;
use axum::response::Json;
use axum::routing::{get, patch, post};
use execlaw_core::tool_access::{ToolAccessRow, ToolAccessStore};
use execlaw_policy::trust::TrustLevel;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// JSON shape of a tool row in the Settings UI. Same fields as the
/// core `ToolAccessRow` but `source` is rendered as a string for the
/// SPA's discriminator.
#[derive(Debug, Serialize, ToSchema)]
pub struct ToolView {
    pub tool_name: String,
    pub source: String,
    pub source_id: Option<String>,
    pub enabled: bool,
    pub allowed_classes: Vec<String>,
    pub description: Option<String>,
    /// Manifest trust floor when the tool is plugin-owned.
    pub trust_floor: Option<String>,
    pub first_seen_at: i64,
    pub last_seen_at: i64,
    pub removed_at: Option<i64>,
}

impl From<&ToolAccessRow> for ToolView {
    fn from(row: &ToolAccessRow) -> Self {
        Self {
            tool_name: row.tool_name.clone(),
            source: row.source.as_str().to_owned(),
            source_id: row.source_id.clone(),
            enabled: row.enabled,
            allowed_classes: row.allowed_classes.clone(),
            description: row.description.clone(),
            trust_floor: None,
            first_seen_at: row.first_seen_at,
            last_seen_at: row.last_seen_at,
            removed_at: row.removed_at,
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ToolListResponse {
    pub tools: Vec<ToolView>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateToolPolicyRequest {
    pub enabled: bool,
    /// Trust-class allowlist. Strings must be exact `TrustLevel`
    /// names — `Controller`, `Delegated`, `KnownTrusted`,
    /// `KnownLimited`, `UnknownPending`, `Blocked`. Unknown strings
    /// are rejected with 400 so a typo can't lock the operator out.
    pub allowed_classes: Vec<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SimulateToolPolicyRequest {
    pub enabled: bool,
    pub allowed_classes: Vec<String>,
    /// Optional proposed trust floor. Simulation does not mutate a manifest.
    pub trust_floor: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ToolPolicyRevisionListResponse {
    pub revisions: Vec<execlaw_core::tool_access::ToolAccessPolicyRevision>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ToolPolicySimulationResponse {
    pub tool_name: String,
    pub historical_decisions: usize,
    pub trust_class_fixtures: usize,
    pub report: execlaw_core::policy_simulation::PolicySimulationReport,
}

/// Cap on how many distinct classes can appear in one allowlist —
/// there are only six valid `TrustLevel` values so anything larger
/// is a malformed request.
const MAX_CLASSES: usize = 8;

#[utoipa::path(
    get,
    path = "/api/admin/tools",
    responses((status = 200, description = "Every registered tool", body = ToolListResponse)),
    security(("bearer_jwt" = [])),
    tag = "tools"
)]
pub async fn list_handler(
    State(state): State<AppState>,
    _user: AuthedUser,
) -> Result<Json<ToolListResponse>, ApiError> {
    let rows = ToolAccessStore::new(&state.db)
        .list_all()
        .map_err(ApiError::from)?;
    let mut tools = rows.iter().map(ToolView::from).collect::<Vec<_>>();
    for tool in &mut tools {
        tool.trust_floor = state
            .plugin_host
            .registry()
            .tool(&tool.tool_name)
            .and_then(|registered| registered.trust_floor.clone());
    }
    Ok(Json(ToolListResponse { tools }))
}

#[utoipa::path(
    patch,
    path = "/api/admin/tools/{tool_name}",
    request_body = UpdateToolPolicyRequest,
    params(("tool_name" = String, Path, description = "Canonical tool name")),
    responses(
        (status = 200, description = "Updated", body = ToolView),
        (status = 400, description = "Unknown trust-class string"),
        (status = 403, description = "Caller is not a Controller"),
        (status = 404, description = "Tool not registered"),
    ),
    security(("bearer_jwt" = [])),
    tag = "tools"
)]
pub async fn update_handler(
    State(state): State<AppState>,
    user: AuthedUser,
    AxumPath(tool_name): AxumPath<String>,
    Json(req): Json<UpdateToolPolicyRequest>,
) -> Result<Json<ToolView>, ApiError> {
    if !is_controller(&state, &user)? {
        return Err(ApiError {
            status: StatusCode::FORBIDDEN,
            code: "controller_only",
            message: "only a Controller can change tool access policy".into(),
        });
    }
    if req.allowed_classes.len() > MAX_CLASSES {
        return Err(ApiError {
            status: StatusCode::BAD_REQUEST,
            code: "too_many_classes",
            message: format!("allowed_classes capped at {MAX_CLASSES} entries"),
        });
    }
    for cls in &req.allowed_classes {
        if TrustLevel::parse(cls).is_none() {
            return Err(ApiError {
                status: StatusCode::BAD_REQUEST,
                code: "unknown_trust_class",
                message: format!("'{cls}' is not a valid trust class"),
            });
        }
    }

    let store = ToolAccessStore::new(&state.db);
    let prior = store
        .get(&tool_name)
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError {
            status: StatusCode::NOT_FOUND,
            code: "tool_not_found",
            message: format!("no tool registered as '{tool_name}'"),
        })?;
    let updated = store
        .set_policy_with_actor(
            &tool_name,
            req.enabled,
            &req.allowed_classes,
            &user.user_id,
            chrono::Utc::now().timestamp(),
            None,
        )
        .map_err(ApiError::from)?;
    if !updated {
        // Lost a race with a deletion — surface as 404 so the SPA
        // re-fetches the list.
        return Err(ApiError {
            status: StatusCode::NOT_FOUND,
            code: "tool_not_found",
            message: format!("'{tool_name}' was removed mid-update"),
        });
    }
    let after = store
        .get(&tool_name)
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "tool_lookup_after_update",
            message: "tool disappeared after update".into(),
        })?;

    let audit = execlaw_core::audit::AuditStore::new(&state.db);
    let _ = audit.insert(
        &user.user_id,
        "config_tool_access",
        &tool_name,
        Some(&serde_json::json!({
            "enabled": prior.enabled,
            "allowed_classes": prior.allowed_classes,
        })),
        Some(&serde_json::json!({
            "enabled": after.enabled,
            "allowed_classes": after.allowed_classes,
        })),
    );

    let mut view = ToolView::from(&after);
    view.trust_floor = state
        .plugin_host
        .registry()
        .tool(&tool_name)
        .and_then(|registered| registered.trust_floor.clone());
    Ok(Json(view))
}

#[utoipa::path(
    post,
    path = "/api/admin/tools/{tool_name}/simulate",
    request_body = SimulateToolPolicyRequest,
    params(("tool_name" = String, Path, description = "Canonical tool name")),
    responses((status = 200, description = "Non-executing policy impact preview", body = ToolPolicySimulationResponse)),
    security(("bearer_jwt" = [])),
    tag = "tools"
)]
pub async fn simulate_handler(
    State(state): State<AppState>,
    user: AuthedUser,
    AxumPath(tool_name): AxumPath<String>,
    Json(request): Json<SimulateToolPolicyRequest>,
) -> Result<Json<ToolPolicySimulationResponse>, ApiError> {
    if !is_controller(&state, &user)? {
        return Err(controller_only_error());
    }
    validate_class_list(&request.allowed_classes)?;
    if let Some(floor) = request.trust_floor.as_deref()
        && TrustLevel::parse(floor).is_none()
    {
        return Err(ApiError {
            status: StatusCode::BAD_REQUEST,
            code: "unknown_trust_floor",
            message: format!("'{floor}' is not a valid trust class"),
        });
    }
    let store = ToolAccessStore::new(&state.db);
    let current = store
        .get(&tool_name)
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError {
            status: StatusCode::NOT_FOUND,
            code: "tool_not_found",
            message: format!("no tool registered as '{tool_name}'"),
        })?;
    let current_floor = state
        .plugin_host
        .registry()
        .tool(&tool_name)
        .and_then(|tool| tool.trust_floor.clone());
    let candidate_floor = stricter_floor(current_floor.as_deref(), request.trust_floor.as_deref());
    let (sensitive, external_effect) = tool_risk(&state, &current);
    let mut decisions = execlaw_core::policy_simulation::ToolPolicyDecisionStore::new(&state.db)
        .list_for_tool(&tool_name, 2_000)
        .map_err(|error| ApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "policy_history_unavailable",
            message: error.to_string(),
        })?;

    for class in [
        "Controller",
        "Delegated",
        "KnownTrusted",
        "KnownLimited",
        "UnknownPending",
        "Blocked",
    ] {
        let current_allowed = current.enabled
            && current
                .allowed_classes
                .iter()
                .any(|allowed| allowed == class)
            && current_floor
                .as_deref()
                .is_none_or(|floor| trust_rank(class) >= trust_rank(floor));
        decisions.push(execlaw_core::policy_simulation::ToolPolicyDecision {
            decision_id: format!("fixture:{class}"),
            run_id: "policy-simulation-fixture".into(),
            conversation_id: "policy-simulation-fixture".into(),
            input_event_seq: 1,
            tool_name: tool_name.clone(),
            caller_trust: class.into(),
            trust_floor: current_floor.clone(),
            required_capabilities: Vec::new(),
            globally_enabled: current.enabled,
            allowed_classes: current.allowed_classes.clone(),
            profile_id: None,
            profile_revision: None,
            outcome: if current_allowed {
                execlaw_core::policy_simulation::PolicyDecisionOutcome::Allowed
            } else {
                execlaw_core::policy_simulation::PolicyDecisionOutcome::Denied
            },
            reason_code: if current_allowed {
                "fixture_currently_allowed"
            } else {
                "fixture_currently_denied"
            }
            .into(),
            sensitive,
            external_effect,
            approval_required: false,
            policy_revision: 0,
            decided_at: 0,
        });
    }
    let report = execlaw_core::policy_simulation::simulate_policy_change(
        &decisions,
        &execlaw_core::policy_simulation::CandidateToolPolicy {
            tool_name: tool_name.clone(),
            enabled: request.enabled,
            allowed_classes: request.allowed_classes,
            trust_floor: candidate_floor,
        },
    );
    Ok(Json(ToolPolicySimulationResponse {
        tool_name,
        historical_decisions: report.evaluated_decisions.saturating_sub(6),
        trust_class_fixtures: 6,
        report,
    }))
}

#[utoipa::path(
    get,
    path = "/api/admin/tools/{tool_name}/revisions",
    params(("tool_name" = String, Path, description = "Canonical tool name")),
    responses((status = 200, description = "Immutable policy revision history", body = ToolPolicyRevisionListResponse)),
    security(("bearer_jwt" = [])),
    tag = "tools"
)]
pub async fn revisions_handler(
    State(state): State<AppState>,
    user: AuthedUser,
    AxumPath(tool_name): AxumPath<String>,
) -> Result<Json<ToolPolicyRevisionListResponse>, ApiError> {
    if !is_controller(&state, &user)? {
        return Err(controller_only_error());
    }
    let revisions = ToolAccessStore::new(&state.db)
        .policy_revisions(&tool_name, 50)
        .map_err(ApiError::from)?;
    Ok(Json(ToolPolicyRevisionListResponse { revisions }))
}

#[utoipa::path(
    post,
    path = "/api/admin/tools/{tool_name}/revisions/{revision_id}/rollback",
    params(("tool_name" = String, Path, description = "Canonical tool name"), ("revision_id" = i64, Path, description = "Revision to restore")),
    responses((status = 200, description = "Previous policy restored as a new revision", body = ToolView)),
    security(("bearer_jwt" = [])),
    tag = "tools"
)]
pub async fn rollback_handler(
    State(state): State<AppState>,
    user: AuthedUser,
    AxumPath((tool_name, revision_id)): AxumPath<(String, i64)>,
) -> Result<Json<ToolView>, ApiError> {
    if !is_controller(&state, &user)? {
        return Err(controller_only_error());
    }
    let store = ToolAccessStore::new(&state.db);
    let prior = store
        .get(&tool_name)
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError {
            status: StatusCode::NOT_FOUND,
            code: "tool_not_found",
            message: format!("no tool registered as '{tool_name}'"),
        })?;
    if !store
        .rollback_policy_revision(
            &tool_name,
            revision_id,
            &user.user_id,
            chrono::Utc::now().timestamp(),
        )
        .map_err(ApiError::from)?
    {
        return Err(ApiError {
            status: StatusCode::NOT_FOUND,
            code: "policy_revision_not_found",
            message: "the requested revision does not belong to this tool".into(),
        });
    }
    let restored = store
        .get(&tool_name)
        .map_err(ApiError::from)?
        .ok_or_else(|| ApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "tool_lookup_after_rollback",
            message: "tool disappeared after policy rollback".into(),
        })?;
    let _ = execlaw_core::audit::AuditStore::new(&state.db).insert(
        &user.user_id,
        "config_tool_access",
        &tool_name,
        Some(&serde_json::json!({"enabled": prior.enabled,"allowed_classes": prior.allowed_classes})),
        Some(&serde_json::json!({"enabled": restored.enabled,"allowed_classes": restored.allowed_classes,"rollback_of": revision_id})),
    );
    let mut view = ToolView::from(&restored);
    view.trust_floor = state
        .plugin_host
        .registry()
        .tool(&tool_name)
        .and_then(|registered| registered.trust_floor.clone());
    Ok(Json(view))
}

fn validate_class_list(classes: &[String]) -> Result<(), ApiError> {
    if classes.len() > MAX_CLASSES {
        return Err(ApiError {
            status: StatusCode::BAD_REQUEST,
            code: "too_many_classes",
            message: format!("allowed_classes capped at {MAX_CLASSES} entries"),
        });
    }
    for class in classes {
        if TrustLevel::parse(class).is_none() {
            return Err(ApiError {
                status: StatusCode::BAD_REQUEST,
                code: "unknown_trust_class",
                message: format!("'{class}' is not a valid trust class"),
            });
        }
    }
    Ok(())
}

fn controller_only_error() -> ApiError {
    ApiError {
        status: StatusCode::FORBIDDEN,
        code: "controller_only",
        message: "only a Controller can change tool access policy".into(),
    }
}

fn trust_rank(class: &str) -> u8 {
    TrustLevel::parse(class).map_or(0, TrustLevel::rank)
}

fn stricter_floor(current: Option<&str>, candidate: Option<&str>) -> Option<String> {
    match (current, candidate) {
        (Some(current), Some(candidate)) if trust_rank(current) >= trust_rank(candidate) => {
            Some(current.to_owned())
        }
        (_, Some(candidate)) => Some(candidate.to_owned()),
        (Some(current), None) => Some(current.to_owned()),
        (None, None) => None,
    }
}

fn tool_risk(state: &AppState, row: &ToolAccessRow) -> (bool, bool) {
    if let Some(tool) = state.plugin_host.registry().builtin(&row.tool_name) {
        let descriptor = tool.descriptor();
        use execlaw_core::tool::Capability;
        let external = descriptor.capabilities.iter().any(|capability| {
            matches!(
                capability,
                Capability::WebFetch
                    | Capability::Search
                    | Capability::Notify
                    | Capability::SubagentSpawn
                    | Capability::ResearchSpawn
                    | Capability::AttachmentSend
                    | Capability::Transport
            )
        });
        (descriptor.sensitive, external)
    } else {
        let external = row.source != execlaw_core::tool_access::ToolSource::Builtin;
        // Plugin manifest effects remain unclassified until H065; project
        // integration calls conservatively as sensitive + external here.
        (external, external)
    }
}

fn is_controller(state: &AppState, user: &AuthedUser) -> Result<bool, ApiError> {
    use execlaw_core::users::{UserRole, UserStore};
    let row = UserStore::new(&state.db)
        .get_by_id(&user.user_id)
        .map_err(ApiError::from)?;
    Ok(matches!(row.map(|u| u.role), Some(UserRole::Controller)))
}

pub fn tools_admin_router() -> Router<AppState> {
    Router::new()
        .route("/api/admin/tools", get(list_handler))
        .route(
            "/api/admin/tools/{tool_name}/simulate",
            post(simulate_handler),
        )
        .route(
            "/api/admin/tools/{tool_name}/revisions",
            get(revisions_handler),
        )
        .route(
            "/api/admin/tools/{tool_name}/revisions/{revision_id}/rollback",
            post(rollback_handler),
        )
        .route("/api/admin/tools/{tool_name}", patch(update_handler))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes::{build_router, test_app_state};
    use axum::body::{self, Body};
    use axum::http::{Method, Request, header};
    use execlaw_core::tool_access::{ToolAccessSeed, ToolAccessStore, ToolSource};
    use tower::ServiceExt;

    async fn setup_controller_token(app: &axum::Router) -> String {
        let body = serde_json::to_vec(&serde_json::json!({
            "username": "ctrl",
            "admin_password": "hunter2-longer",
            "display_name": "Controller",
        }))
        .unwrap();
        let req = Request::builder()
            .method(Method::POST)
            .uri("/api/setup")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body))
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        let bytes = body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        v["access_token"].as_str().unwrap().to_owned()
    }

    fn seed(store: &ToolAccessStore<'_>, name: &str) {
        store
            .upsert_seen(
                &ToolAccessSeed {
                    tool_name: name.into(),
                    source: ToolSource::Builtin,
                    source_id: None,
                    description: Some(format!("desc-{name}")),
                    input_schema: None,
                    default_allowed_classes: vec!["Controller".into(), "KnownTrusted".into()],
                },
                100,
            )
            .unwrap();
    }

    #[tokio::test]
    async fn list_tools_returns_seeded_rows_when_authenticated() {
        let state = test_app_state();
        let store = ToolAccessStore::new(&state.db);
        seed(&store, "read_memory");
        seed(&store, "set_thread_name");
        let app = build_router(state);
        let tok = setup_controller_token(&app).await;
        let req = Request::builder()
            .method(Method::GET)
            .uri("/api/admin/tools")
            .header(header::AUTHORIZATION, format!("Bearer {tok}"))
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let tools = v["tools"].as_array().unwrap();
        assert!(tools.iter().any(|t| t["tool_name"] == "read_memory"));
        assert!(tools.iter().any(|t| t["tool_name"] == "set_thread_name"));
    }

    #[tokio::test]
    async fn list_tools_requires_auth() {
        let app = build_router(test_app_state());
        let req = Request::builder()
            .method(Method::GET)
            .uri("/api/admin/tools")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn update_rejects_unknown_trust_class() {
        let state = test_app_state();
        let store = ToolAccessStore::new(&state.db);
        seed(&store, "read_memory");
        let app = build_router(state);
        let tok = setup_controller_token(&app).await;
        let body = serde_json::json!({
            "enabled": true,
            "allowed_classes": ["Controller", "Hacker"],
        });
        let req = Request::builder()
            .method(Method::PATCH)
            .uri("/api/admin/tools/read_memory")
            .header(header::AUTHORIZATION, format!("Bearer {tok}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let bytes = body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["error"]["code"], "unknown_trust_class");
    }

    #[tokio::test]
    async fn update_returns_404_for_unregistered_tool() {
        let state = test_app_state();
        let app = build_router(state);
        let tok = setup_controller_token(&app).await;
        let body = serde_json::json!({"enabled": true, "allowed_classes": ["Controller"]});
        let req = Request::builder()
            .method(Method::PATCH)
            .uri("/api/admin/tools/never_seen_tool")
            .header(header::AUTHORIZATION, format!("Bearer {tok}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn update_persists_new_policy_for_controller_caller() {
        let state = test_app_state();
        let store = ToolAccessStore::new(&state.db);
        seed(&store, "read_memory");
        let app = build_router(state.clone());
        let tok = setup_controller_token(&app).await;
        let body = serde_json::json!({
            "enabled": false,
            "allowed_classes": ["Controller"],
        });
        let req = Request::builder()
            .method(Method::PATCH)
            .uri("/api/admin/tools/read_memory")
            .header(header::AUTHORIZATION, format!("Bearer {tok}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let row = ToolAccessStore::new(&state.db)
            .get("read_memory")
            .unwrap()
            .unwrap();
        assert!(!row.enabled);
        assert_eq!(row.allowed_classes, vec!["Controller"]);
    }

    #[tokio::test]
    async fn policy_simulation_covers_every_trust_class_without_mutating_policy_or_outbox() {
        let state = test_app_state();
        let store = ToolAccessStore::new(&state.db);
        seed(&store, "read_memory");
        let before = store.get("read_memory").unwrap().unwrap();
        let outbox_before: i64 = state
            .db
            .with_conn(|connection| {
                Ok(connection
                    .query_row("SELECT COUNT(*) FROM state_outbox", [], |row| row.get(0))?)
            })
            .unwrap();
        let app = build_router(state.clone());
        let token = setup_controller_token(&app).await;
        let body = serde_json::json!({
            "enabled": true,
            "allowed_classes": ["Controller", "Delegated", "KnownTrusted", "KnownLimited"],
            "trust_floor": "KnownLimited"
        });
        let request = Request::builder()
            .method(Method::POST)
            .uri("/api/admin/tools/read_memory/simulate")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body: serde_json::Value = body::to_bytes(response.into_body(), usize::MAX)
            .await
            .map(|bytes| serde_json::from_slice(&bytes).unwrap())
            .unwrap();
        assert_eq!(body["trust_class_fixtures"], 6);
        assert_eq!(body["report"]["evaluated_decisions"], 6);
        assert!(
            body["report"]["newly_allowed"]
                .as_array()
                .unwrap()
                .iter()
                .any(|change| change["caller_trust"] == "KnownLimited")
        );
        assert_eq!(store.get("read_memory").unwrap().unwrap(), before);
        let outbox_after: i64 = state
            .db
            .with_conn(|connection| {
                Ok(connection
                    .query_row("SELECT COUNT(*) FROM state_outbox", [], |row| row.get(0))?)
            })
            .unwrap();
        assert_eq!(outbox_after, outbox_before);
    }

    #[tokio::test]
    async fn rollback_restores_policy_as_a_fresh_revision_without_touching_approval_tokens() {
        let state = test_app_state();
        let store = ToolAccessStore::new(&state.db);
        seed(&store, "read_memory");
        store
            .set_policy_with_actor(
                "read_memory",
                true,
                &["Controller".into()],
                "controller-1",
                101,
                None,
            )
            .unwrap();
        let app = build_router(state.clone());
        let token = setup_controller_token(&app).await;

        let patch_policy = |enabled: bool, allowed: &[&str]| serde_json::json!({"enabled": enabled, "allowed_classes": allowed});
        for (enabled, classes) in [
            (true, vec!["Controller", "KnownTrusted"]),
            (false, vec!["Controller"]),
        ] {
            let request = Request::builder()
                .method(Method::PATCH)
                .uri("/api/admin/tools/read_memory")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(patch_policy(enabled, &classes).to_string()))
                .unwrap();
            let response = app.clone().oneshot(request).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
        }
        let revisions = store.policy_revisions("read_memory", 10).unwrap();
        assert_eq!(revisions.len(), 4);
        let restore_id = revisions[1].revision_id;
        let latest_id = revisions[0].revision_id;
        let expired_claim = execlaw_policy::sideband::ApprovalClaims {
            iss: state.signer.issuer().to_owned(),
            jti: "expired-approval".into(),
            conversation_id: "read_memory-policy".into(),
            reason: execlaw_policy::sideband::ApprovalReason::SensitiveToolCall,
            tool_call_id: None,
            principal_id: None,
            effect_hash: None,
            iat: chrono::Utc::now().timestamp() - 100,
            exp: chrono::Utc::now().timestamp() - 60,
        };
        let expired_token = jsonwebtoken::encode(
            &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::EdDSA),
            &expired_claim,
            state.signer.encoding_key(),
        )
        .unwrap();

        let request = Request::builder()
            .method(Method::POST)
            .uri(format!(
                "/api/admin/tools/read_memory/revisions/{restore_id}/rollback"
            ))
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let row = store.get("read_memory").unwrap().unwrap();
        assert!(row.enabled);
        assert_eq!(row.allowed_classes, vec!["Controller", "KnownTrusted"]);
        let revisions = store.policy_revisions("read_memory", 10).unwrap();
        assert!(revisions[0].revision_id > latest_id);
        assert_eq!(revisions[0].rollback_of, Some(restore_id));
        assert!(
            crate::approvals::verify_approval_token(
                &state.signer,
                &expired_token,
                "expired-approval"
            )
            .is_err(),
            "policy rollback must not make an expired approval token valid again"
        );
    }
}
