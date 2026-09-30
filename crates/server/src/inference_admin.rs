//! Admin HTTP surface for the inference observability page (M5).
//!
//! Single endpoint:
//!
//!   `GET /api/admin/inference/metrics` — bounded per-consumer and
//!   per-phase latency distributions plus numeric context-size estimates.
//!   Backs the `/admin/inference` SPA page.
//!
//! Read-only; no mutations. Anyone with controller auth can hit it.

use crate::auth_extract::AuthedUser;
use crate::inference_metrics::MetricsSnapshot;
use crate::routes::ApiError;
use crate::state::AppState;
use axum::Router;
use axum::extract::{Query, State};
use axum::response::Json;
use axum::routing::get;
use execlaw_core::users::UserRole;
use serde::Deserialize;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/admin/inference/metrics", get(metrics))
        .route(
            "/api/admin/inference/capability-profile/current",
            get(get_current_capability_profile),
        )
        .route(
            "/api/admin/inference/capability-profile",
            get(get_capability_profile),
        )
}

pub async fn get_current_capability_profile(
    State(state): State<AppState>,
    user: AuthedUser,
) -> Result<Json<Option<execlaw_core::harness::ModelCapabilityProfile>>, ApiError> {
    require_controller(&user)?;
    let Some(resolved) = state
        .inference
        .resolve(&state.db, execlaw_core::backends::BackendPurpose::Standard)
    else {
        return Ok(Json(None));
    };
    let Some(identity) = crate::inference_probe::current_model_identity(
        &state.db,
        execlaw_core::backends::BackendPurpose::Standard,
        &resolved.model_id,
    ) else {
        return Ok(Json(None));
    };
    execlaw_core::harness::HarnessStore::new(&state.db)
        .get_profile(&identity)
        .map(Json)
        .map_err(|error| ApiError {
            status: axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            code: "model_profile_error",
            message: error.to_string(),
        })
}

#[derive(Debug, Deserialize)]
pub struct CapabilityProfileQuery {
    pub model_id: String,
    pub quantization: String,
    pub chat_template: String,
    pub backend_version: String,
    pub parser_version: String,
}

fn require_controller(user: &AuthedUser) -> Result<(), ApiError> {
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

fn profile_identity(query: CapabilityProfileQuery) -> execlaw_core::harness::ModelIdentity {
    execlaw_core::harness::ModelIdentity {
        model_id: query.model_id,
        quantization: query.quantization,
        chat_template: query.chat_template,
        backend_version: query.backend_version,
        parser_version: query.parser_version,
    }
}

pub async fn get_capability_profile(
    State(state): State<AppState>,
    user: AuthedUser,
    Query(query): Query<CapabilityProfileQuery>,
) -> Result<Json<Option<execlaw_core::harness::ModelCapabilityProfile>>, ApiError> {
    require_controller(&user)?;
    execlaw_core::harness::HarnessStore::new(&state.db)
        .get_profile(&profile_identity(query))
        .map(Json)
        .map_err(|error| ApiError {
            status: axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            code: "model_profile_error",
            message: error.to_string(),
        })
}

#[utoipa::path(
    get,
    path = "/api/admin/inference/metrics",
    responses((status = 200, description = "Per-consumer inference call counters + p50/p95 latencies", body = MetricsSnapshot)),
    security(("bearer_jwt" = [])),
    tag = "inference"
)]
pub async fn metrics(State(state): State<AppState>) -> Result<Json<MetricsSnapshot>, ApiError> {
    Ok(Json(state.inference_metrics.snapshot()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inference_metrics::InferenceConsumer;
    use crate::routes::test_app_state;
    use axum::body::{self, Body};
    use axum::http::{Method, Request, StatusCode};
    use tower::ServiceExt;

    async fn observe_ok(state: &AppState, c: InferenceConsumer) {
        state
            .inference_metrics
            .observe::<_, &'static str, _>(c, async { Ok::<_, &'static str>("ok") })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn metrics_returns_empty_consumers_on_fresh_state() {
        let state = test_app_state();
        let app = router().with_state(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/admin/inference/metrics")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(v["consumers"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn metrics_reflects_observed_consumers() {
        let state = test_app_state();
        observe_ok(&state, InferenceConsumer::Chat).await;
        observe_ok(&state, InferenceConsumer::Automations).await;
        observe_ok(&state, InferenceConsumer::Automations).await;
        let app = router().with_state(state);
        let resp = app
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/admin/inference/metrics")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let bytes = body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let consumers = v["consumers"].as_array().unwrap();
        assert_eq!(consumers.len(), 2);
        let auto = consumers
            .iter()
            .find(|c| c["consumer"] == "automations")
            .unwrap();
        let chat = consumers.iter().find(|c| c["consumer"] == "chat").unwrap();
        assert_eq!(auto["total_calls"], 2);
        assert_eq!(chat["total_calls"], 1);
        assert!(v["phases"].is_array());
        assert!(v["contexts"].is_array());
    }
}
