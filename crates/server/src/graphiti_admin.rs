//! Admin API for Graphiti connectivity checks.
//!
//! Routes are auth-gated and intended for the Settings / diagnostics UI,
//! so operators can validate Graphiti without driving a model turn.

use crate::auth_extract::AuthedUser;
use crate::graphiti_tool::invoke_graphiti;
use crate::state::AppState;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use execlaw_core::tool::ToolOutcome;
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
    State(_state): State<AppState>,
    _user: AuthedUser,
) -> Result<Json<GraphitiHealthResponse>, (StatusCode, Json<Value>)> {
    let out = invoke_graphiti(json!({ "action": "status" })).await;
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
    State(_state): State<AppState>,
    _user: AuthedUser,
    Json(req): Json<GraphitiTestCallRequest>,
) -> Result<Json<GraphitiTestCallResponse>, (StatusCode, Json<Value>)> {
    let out = invoke_graphiti(req.args).await;
    let outcome = outcome_to_http(out)?;
    Ok(Json(GraphitiTestCallResponse { ok: true, outcome }))
}

pub fn graphiti_admin_router() -> Router<AppState> {
    Router::new()
        .route("/api/admin/graphiti/health", get(health_handler))
        .route("/api/admin/graphiti/test-call", post(test_call_handler))
}
