//! Versioned public contract consumed by headless and terminal clients.

use axum::{Json, Router, routing::get};
use serde::Serialize;
use utoipa::ToSchema;

pub const CLIENT_CONTRACT_VERSION: u32 = 1;

#[derive(Debug, Serialize, ToSchema)]
pub struct ClientContract {
    pub version: u32,
    pub minimum_client_version: u32,
    pub api_schema: String,
    pub event_cursor_unit: String,
    pub features: Vec<String>,
}

#[utoipa::path(
    get,
    path = "/api/client-contract",
    responses((status = 200, description = "Versioned headless/editor client contract", body = ClientContract)),
    tag = "clients"
)]
pub async fn contract() -> Json<ClientContract> {
    Json(ClientContract {
        version: CLIENT_CONTRACT_VERSION,
        minimum_client_version: 1,
        api_schema: "/api/openapi.json".into(),
        event_cursor_unit: "conversation event sequence; exclusive before cursor".into(),
        features: vec![
            "chat_sessions".into(),
            "message_cursors".into(),
            "artifacts".into(),
            "approvals".into(),
            "cancel".into(),
            "durable_turn_controls".into(),
            "run_forks".into(),
            "workspace_inspection".into(),
            "workspace_checkpoints".into(),
            "workspace_diff_preview".into(),
            "workspace_diff_apply".into(),
            "workspace_restore".into(),
            "editor_execute_commands".into(),
        ],
    })
}

pub fn router() -> Router<crate::state::AppState> {
    Router::new().route("/api/client-contract", get(contract))
}
