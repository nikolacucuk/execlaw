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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn advertised_contract_matches_pinned_schema_and_feature_surface() {
        let Json(contract) = contract().await;
        let value = serde_json::to_value(contract).unwrap();
        let schema: serde_json::Value =
            serde_json::from_str(include_str!("../../../spec/client-contract-v1.schema.json"))
                .unwrap();
        let validator = jsonschema::validator_for(&schema).unwrap();
        assert!(validator.is_valid(&value));
        for required in [
            "chat_sessions",
            "message_cursors",
            "artifacts",
            "approvals",
            "cancel",
            "durable_turn_controls",
            "workspace_checkpoints",
            "workspace_diff_preview",
            "workspace_diff_apply",
            "workspace_restore",
            "editor_execute_commands",
        ] {
            assert!(
                value["features"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|feature| feature == required),
                "contract omits required v1 feature {required}"
            );
        }
        assert_eq!(value["minimum_client_version"], 1);
    }
}
