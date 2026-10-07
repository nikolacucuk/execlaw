//! Controller-only API for reviewing and correcting persistent preferences.

use crate::{auth_extract::AuthedUser, state::AppState};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use execlaw_core::preferences::{PreferenceAuthority, PreferenceStore};
use execlaw_core::users::UserRole;
use serde::Deserialize;
use utoipa::ToSchema;

fn require_controller(user: &AuthedUser) -> Result<(), Response> {
    if user.role == UserRole::Controller {
        Ok(())
    } else {
        Err((
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"error":"controller only"})),
        )
            .into_response())
    }
}

fn controller_id(state: &AppState) -> Result<String, Response> {
    crate::routes::controller_principal_id(&state.db)
        .map(|id| id.as_str().to_owned())
        .map_err(|_| {
            (
                StatusCode::CONFLICT,
                Json(serde_json::json!({"error":"controller identity is not configured"})),
            )
                .into_response()
        })
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct PreferenceListQuery {
    pub scope: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SetPreferenceRequest {
    pub scope: String,
    pub preference_key: String,
    pub value: serde_json::Value,
    pub expires_at: Option<i64>,
}

#[utoipa::path(get, path="/api/admin/preferences", params(("scope" = Option<String>, Query)), responses((status=200, description="Current and proposed preferences")), security(("bearer_auth"=[])))]
pub async fn list(
    State(state): State<AppState>,
    user: AuthedUser,
    Query(query): Query<PreferenceListQuery>,
) -> Response {
    if let Err(response) = require_controller(&user) {
        return response;
    }
    let owner = match controller_id(&state) {
        Ok(id) => id,
        Err(response) => return response,
    };
    match PreferenceStore::new(&state.db).list(&owner, query.scope.as_deref()) {
        Ok(preferences) => Json(serde_json::json!({"preferences":preferences})).into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error":error.to_string()})),
        )
            .into_response(),
    }
}

#[utoipa::path(post, path="/api/admin/preferences", request_body=SetPreferenceRequest, responses((status=200, description="Preference saved")), security(("bearer_auth"=[])))]
pub async fn set(
    State(state): State<AppState>,
    user: AuthedUser,
    Json(request): Json<SetPreferenceRequest>,
) -> Response {
    if let Err(response) = require_controller(&user) {
        return response;
    }
    let owner = match controller_id(&state) {
        Ok(id) => id,
        Err(response) => return response,
    };
    let now = chrono::Utc::now().timestamp();
    let evidence = format!("controller:{}:{now}", user.user_id);
    match PreferenceStore::new(&state.db).set_explicit(
        PreferenceAuthority::Operator,
        &owner,
        &request.scope,
        &request.preference_key,
        &request.value,
        &evidence,
        request.expires_at,
        now,
    ) {
        Ok(id) => (
            StatusCode::OK,
            Json(serde_json::json!({"preference_id":id})),
        )
            .into_response(),
        Err(error) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error":error.to_string()})),
        )
            .into_response(),
    }
}

fn owned_preference(state: &AppState, owner: &str, id: &str) -> Result<(), Response> {
    match PreferenceStore::new(&state.db).get(id) {
        Ok(Some(preference)) if preference.owner_principal_id == owner => Ok(()),
        Ok(_) => Err((
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error":"preference not found"})),
        )
            .into_response()),
        Err(error) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error":error.to_string()})),
        )
            .into_response()),
    }
}

#[utoipa::path(post, path="/api/admin/preferences/{id}/approve", params(("id" = String, Path)), responses((status=204, description="Inference approved")), security(("bearer_auth"=[])))]
pub async fn approve(
    State(state): State<AppState>,
    user: AuthedUser,
    Path(id): Path<String>,
) -> Response {
    if let Err(response) = require_controller(&user) {
        return response;
    }
    let owner = match controller_id(&state) {
        Ok(id) => id,
        Err(response) => return response,
    };
    if let Err(response) = owned_preference(&state, &owner, &id) {
        return response;
    }
    match PreferenceStore::new(&state.db).approve_inference(
        PreferenceAuthority::Operator,
        &id,
        chrono::Utc::now().timestamp(),
    ) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({"error":error.to_string()})),
        )
            .into_response(),
    }
}

#[utoipa::path(post, path="/api/admin/preferences/{id}/reject", params(("id" = String, Path)), responses((status=204, description="Inference rejected")), security(("bearer_auth"=[])))]
pub async fn reject(
    State(state): State<AppState>,
    user: AuthedUser,
    Path(id): Path<String>,
) -> Response {
    if let Err(response) = require_controller(&user) {
        return response;
    }
    let owner = match controller_id(&state) {
        Ok(id) => id,
        Err(response) => return response,
    };
    if let Err(response) = owned_preference(&state, &owner, &id) {
        return response;
    }
    match PreferenceStore::new(&state.db).reject_inference(
        PreferenceAuthority::Operator,
        &id,
        chrono::Utc::now().timestamp(),
    ) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({"error":error.to_string()})),
        )
            .into_response(),
    }
}

#[utoipa::path(delete, path="/api/admin/preferences/{id}", params(("id" = String, Path)), responses((status=204, description="Preference retracted")), security(("bearer_auth"=[])))]
pub async fn remove(
    State(state): State<AppState>,
    user: AuthedUser,
    Path(id): Path<String>,
) -> Response {
    if let Err(response) = require_controller(&user) {
        return response;
    }
    let owner = match controller_id(&state) {
        Ok(id) => id,
        Err(response) => return response,
    };
    if let Err(response) = owned_preference(&state, &owner, &id) {
        return response;
    }
    match PreferenceStore::new(&state.db).remove(
        PreferenceAuthority::Operator,
        &id,
        chrono::Utc::now().timestamp(),
    ) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({"error":error.to_string()})),
        )
            .into_response(),
    }
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/admin/preferences", get(list).post(set))
        .route("/api/admin/preferences/{id}/approve", post(approve))
        .route("/api/admin/preferences/{id}/reject", post(reject))
        .route("/api/admin/preferences/{id}", delete(remove))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(role: UserRole) -> AuthedUser {
        AuthedUser {
            user_id: "user-1".into(),
            session_id: None,
            username: "user".into(),
            display_name: "User".into(),
            email: None,
            role,
            last_login_at: None,
        }
    }

    #[test]
    fn preference_changes_are_controller_only() {
        assert!(require_controller(&user(UserRole::Controller)).is_ok());
        for role in [UserRole::Operator, UserRole::Viewer] {
            assert_eq!(
                require_controller(&user(role)).unwrap_err().status(),
                StatusCode::FORBIDDEN
            );
        }
    }
}
