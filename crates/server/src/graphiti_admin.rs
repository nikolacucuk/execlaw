//! Admin API for Graphiti connectivity checks.
//!
//! Routes are auth-gated and intended for the Settings / diagnostics UI,
//! so operators can validate Graphiti without driving a model turn.

use crate::auth_extract::AuthedUser;
use crate::graphiti_tool::{
    CONFIG_API_KEY_REF, CONFIG_BASE_URL, invoke_graphiti, validate_base_url,
};
use crate::state::AppState;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use execlaw_core::config::{ConfigKv, ConfigTable};
use execlaw_core::tool::ToolOutcome;
use execlaw_core::vault_row::VaultRowStore;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use utoipa::ToSchema;

#[derive(Debug, Serialize, ToSchema)]
pub struct GraphitiHealthResponse {
    pub ok: bool,
    pub status: String,
    pub details: Value,
}

#[derive(Debug, Deserialize, Serialize, ToSchema)]
pub struct GraphitiTestCallRequest {
    #[schema(value_type = serde_json::Value)]
    pub args: Value,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct GraphitiTestCallResponse {
    pub ok: bool,
    #[schema(value_type = serde_json::Value)]
    pub outcome: Value,
}

#[derive(Debug, Deserialize, Serialize, ToSchema)]
pub struct GraphitiConfigRequest {
    pub base_url: String,
    pub api_key_vault_ref: Option<String>,
    #[serde(default, skip_serializing)]
    pub api_key: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct GraphitiConfigResponse {
    pub base_url: Option<String>,
    pub api_key_vault_ref: Option<String>,
    pub credential_configured: bool,
}

fn config_response(db: &execlaw_core::Database) -> Result<GraphitiConfigResponse, String> {
    let config = ConfigKv::new(db, ConfigTable::RuntimeSettings);
    let base_url = config
        .get(CONFIG_BASE_URL)
        .map_err(|error| error.to_string())?
        .filter(|value| !value.is_empty());
    let api_key_vault_ref = config
        .get(CONFIG_API_KEY_REF)
        .map_err(|error| error.to_string())?
        .filter(|value| !value.is_empty());
    let credential_configured = match api_key_vault_ref.as_deref() {
        Some(secret_ref) => VaultRowStore::new(db)
            .get(None, secret_ref)
            .map_err(|error| error.to_string())?
            .is_some(),
        None => false,
    };
    Ok(GraphitiConfigResponse {
        base_url,
        api_key_vault_ref,
        credential_configured,
    })
}

#[utoipa::path(
    get,
    path = "/api/admin/graphiti/config",
    responses((status = 200, description = "Graphiti endpoint and credential-reference configuration", body = GraphitiConfigResponse)),
    security(("bearer_jwt" = [])),
    tag = "graphiti"
)]
pub async fn get_config_handler(
    State(state): State<AppState>,
    _user: AuthedUser,
) -> Result<Json<GraphitiConfigResponse>, (StatusCode, Json<Value>)> {
    config_response(&state.db).map(Json).map_err(|message| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error": {"code": "config_read_failed", "message": message}})),
        )
    })
}

#[utoipa::path(
    put,
    path = "/api/admin/graphiti/config",
    request_body = GraphitiConfigRequest,
    responses((status = 200, description = "Saved Graphiti configuration", body = GraphitiConfigResponse)),
    security(("bearer_jwt" = [])),
    tag = "graphiti"
)]
pub async fn put_config_handler(
    State(state): State<AppState>,
    _user: AuthedUser,
    Json(req): Json<GraphitiConfigRequest>,
) -> Result<Json<GraphitiConfigResponse>, (StatusCode, Json<Value>)> {
    let base_url = validate_base_url(&req.base_url).map_err(|message| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": {"code": "invalid_endpoint", "message": message}})),
        )
    })?;
    crate::local_endpoint_policy::checked_client(
        &state.db,
        "graphiti",
        base_url.as_str(),
        |builder| builder.timeout(std::time::Duration::from_secs(30)),
    )
    .map_err(|message| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": {"code": "endpoint_denied", "message": message}})),
        )
    })?;

    let secret_ref = req
        .api_key_vault_ref
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if req.api_key.is_some() && secret_ref.is_none() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(
                json!({"error": {"code": "credential_ref_required", "message": "api_key_vault_ref is required when api_key is supplied"}}),
            ),
        ));
    }
    if let (Some(secret_ref), Some(api_key)) = (secret_ref, req.api_key.as_deref()) {
        if api_key.is_empty() {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(
                    json!({"error": {"code": "credential_empty", "message": "api_key must not be empty"}}),
                ),
            ));
        }
        VaultRowStore::new(&state.db)
            .put(None, secret_ref, api_key.as_bytes(), chrono::Utc::now().timestamp())
            .map_err(|error| (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": {"code": "vault_write_failed", "message": error.to_string()}})),
            ))?;
    }
    let config = ConfigKv::new(&state.db, ConfigTable::RuntimeSettings);
    config
        .set(CONFIG_BASE_URL, base_url.as_str())
        .map_err(|error| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(
                    json!({"error": {"code": "config_write_failed", "message": error.to_string()}}),
                ),
            )
        })?;
    config
        .set(CONFIG_API_KEY_REF, secret_ref.unwrap_or_default())
        .map_err(|error| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(
                    json!({"error": {"code": "config_write_failed", "message": error.to_string()}}),
                ),
            )
        })?;
    get_config_handler(State(state), _user).await
}

fn outcome_to_http(outcome: ToolOutcome) -> Result<Value, (StatusCode, Json<Value>)> {
    match outcome {
        ToolOutcome::Ok(v) => Ok(v),
        ToolOutcome::Denied { reason } => Err((
            StatusCode::FORBIDDEN,
            Json(json!({"error": {"code": "graphiti_denied", "message": reason}})),
        )),
        ToolOutcome::Err { code, message } => Err((
            StatusCode::BAD_GATEWAY,
            Json(json!({"error": {"code": code, "message": message}})),
        )),
    }
}

#[utoipa::path(
    get,
    path = "/api/admin/graphiti/health",
    responses((status = 200, description = "Graphiti connectivity status", body = GraphitiHealthResponse)),
    security(("bearer_jwt" = [])),
    tag = "graphiti"
)]
pub async fn health_handler(
    State(state): State<AppState>,
    _user: AuthedUser,
) -> Result<Json<GraphitiHealthResponse>, (StatusCode, Json<Value>)> {
    let out = invoke_graphiti(json!({ "action": "status" }), &state.db).await;
    let details = outcome_to_http(out)?;
    Ok(Json(GraphitiHealthResponse {
        ok: true,
        status: "reachable".to_owned(),
        details,
    }))
}

#[utoipa::path(
    post,
    path = "/api/admin/graphiti/test-call",
    request_body = GraphitiTestCallRequest,
    responses((status = 200, description = "Graphiti test call result", body = GraphitiTestCallResponse)),
    security(("bearer_jwt" = [])),
    tag = "graphiti"
)]
pub async fn test_call_handler(
    State(state): State<AppState>,
    _user: AuthedUser,
    Json(req): Json<GraphitiTestCallRequest>,
) -> Result<Json<GraphitiTestCallResponse>, (StatusCode, Json<Value>)> {
    let out = invoke_graphiti(req.args, &state.db).await;
    let outcome = outcome_to_http(out)?;
    Ok(Json(GraphitiTestCallResponse { ok: true, outcome }))
}

pub fn graphiti_admin_router() -> Router<AppState> {
    Router::new()
        .route(
            "/api/admin/graphiti/config",
            get(get_config_handler).put(put_config_handler),
        )
        .route("/api/admin/graphiti/health", get(health_handler))
        .route("/api/admin/graphiti/test-call", post(test_call_handler))
}
