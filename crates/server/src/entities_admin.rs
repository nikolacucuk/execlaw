//! Controller-only entity resolution and reversible merge review API.

use crate::{auth_extract::AuthedUser, state::AppState};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use execlaw_core::entities::EntityStore;
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

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateEntityRequest {
    pub kind: String,
    pub canonical_label: String,
}
#[derive(Debug, Deserialize, ToSchema)]
pub struct AddAliasRequest {
    pub label: String,
    pub valid_from: i64,
    pub valid_to: Option<i64>,
    pub evidence_ref: String,
    #[serde(default)]
    pub authoritative: bool,
}
#[derive(Debug, Deserialize, ToSchema)]
pub struct ProposeMergeRequest {
    pub left_entity_id: String,
    pub right_entity_id: String,
    pub evidence_ref: String,
}
#[derive(Debug, Deserialize, ToSchema)]
pub struct DecisionRequest {
    pub reason: String,
}
#[derive(Debug, Deserialize)]
pub struct CandidateQuery {
    pub label: String,
    pub at: Option<i64>,
}
#[derive(Debug, Deserialize)]
pub struct MergeListQuery {
    pub status: Option<String>,
}

#[utoipa::path(get,path="/api/admin/entity-merges",params(("status"=Option<String>,Query)),responses((status=200,description="Entity merge proposals and review decisions")),security(("bearer_jwt"=[])))]
pub async fn list_merges(
    State(state): State<AppState>,
    user: AuthedUser,
    Query(query): Query<MergeListQuery>,
) -> Response {
    if let Err(response) = require_controller(&user) {
        return response;
    }
    match EntityStore::new(&state.db).proposals(query.status.as_deref()) {
        Ok(proposals) => Json(serde_json::json!({"proposals":proposals})).into_response(),
        Err(error) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error":error.to_string()})),
        )
            .into_response(),
    }
}

#[utoipa::path(post,path="/api/admin/entities",request_body=CreateEntityRequest,responses((status=201,description="Entity created")),security(("bearer_jwt"=[])))]
pub async fn create(
    State(state): State<AppState>,
    user: AuthedUser,
    Json(request): Json<CreateEntityRequest>,
) -> Response {
    if let Err(response) = require_controller(&user) {
        return response;
    }
    match EntityStore::new(&state.db).create(
        &request.kind,
        &request.canonical_label,
        chrono::Utc::now().timestamp(),
    ) {
        Ok(entity_id) => (
            StatusCode::CREATED,
            Json(serde_json::json!({"entity_id":entity_id})),
        )
            .into_response(),
        Err(error) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error":error.to_string()})),
        )
            .into_response(),
    }
}

#[utoipa::path(get,path="/api/admin/entities/candidates",params(("label"=String,Query),("at"=Option<i64>,Query)),responses((status=200,description="Matching entities with ambiguity preserved")),security(("bearer_jwt"=[])))]
pub async fn candidates(
    State(state): State<AppState>,
    user: AuthedUser,
    Query(query): Query<CandidateQuery>,
) -> Response {
    if let Err(response) = require_controller(&user) {
        return response;
    }
    let matches = match EntityStore::new(&state.db).candidates(
        &query.label,
        query.at.unwrap_or_else(|| chrono::Utc::now().timestamp()),
    ) {
        Ok(matches) => matches,
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error":error.to_string()})),
            )
                .into_response();
        }
    };
    Json(serde_json::json!({"ambiguous":matches.len()>1,"candidates":matches})).into_response()
}

#[utoipa::path(post,path="/api/admin/entities/{id}/aliases",params(("id"=String,Path)),request_body=AddAliasRequest,responses((status=201,description="Evidence-backed alias added")),security(("bearer_jwt"=[])))]
pub async fn add_alias(
    State(state): State<AppState>,
    user: AuthedUser,
    Path(id): Path<String>,
    Json(request): Json<AddAliasRequest>,
) -> Response {
    if let Err(response) = require_controller(&user) {
        return response;
    }
    match EntityStore::new(&state.db).add_alias(
        &id,
        &request.label,
        request.valid_from,
        request.valid_to,
        &request.evidence_ref,
        request.authoritative,
    ) {
        Ok(alias_id) => (
            StatusCode::CREATED,
            Json(serde_json::json!({"alias_id":alias_id})),
        )
            .into_response(),
        Err(error) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error":error.to_string()})),
        )
            .into_response(),
    }
}

#[utoipa::path(post,path="/api/admin/entity-merges",request_body=ProposeMergeRequest,responses((status=201,description="Merge proposal created")),security(("bearer_jwt"=[])))]
pub async fn propose_merge(
    State(state): State<AppState>,
    user: AuthedUser,
    Json(request): Json<ProposeMergeRequest>,
) -> Response {
    if let Err(response) = require_controller(&user) {
        return response;
    }
    match EntityStore::new(&state.db).propose_merge(
        &request.left_entity_id,
        &request.right_entity_id,
        &request.evidence_ref,
        &user.user_id,
        chrono::Utc::now().timestamp(),
    ) {
        Ok(proposal_id) => (
            StatusCode::CREATED,
            Json(serde_json::json!({"proposal_id":proposal_id})),
        )
            .into_response(),
        Err(error) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error":error.to_string()})),
        )
            .into_response(),
    }
}

#[utoipa::path(post,path="/api/admin/entity-merges/{id}/accept",params(("id"=String,Path)),request_body=DecisionRequest,responses((status=204,description="Merge accepted")),security(("bearer_jwt"=[])))]
pub async fn accept_merge(
    State(state): State<AppState>,
    user: AuthedUser,
    Path(id): Path<String>,
    Json(request): Json<DecisionRequest>,
) -> Response {
    if let Err(response) = require_controller(&user) {
        return response;
    }
    match EntityStore::new(&state.db).accept_merge(
        &id,
        &user.user_id,
        &request.reason,
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

#[utoipa::path(post,path="/api/admin/entity-merges/{id}/reject",params(("id"=String,Path)),request_body=DecisionRequest,responses((status=204,description="Merge rejected")),security(("bearer_jwt"=[])))]
pub async fn reject_merge(
    State(state): State<AppState>,
    user: AuthedUser,
    Path(id): Path<String>,
    Json(request): Json<DecisionRequest>,
) -> Response {
    if let Err(response) = require_controller(&user) {
        return response;
    }
    match EntityStore::new(&state.db).reject_merge(
        &id,
        &user.user_id,
        &request.reason,
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

#[utoipa::path(post,path="/api/admin/entity-merges/{id}/reverse",params(("id"=String,Path)),request_body=DecisionRequest,responses((status=204,description="Merge reversed")),security(("bearer_jwt"=[])))]
pub async fn reverse_merge(
    State(state): State<AppState>,
    user: AuthedUser,
    Path(id): Path<String>,
    Json(request): Json<DecisionRequest>,
) -> Response {
    if let Err(response) = require_controller(&user) {
        return response;
    }
    match EntityStore::new(&state.db).reverse_merge(
        &id,
        &user.user_id,
        &request.reason,
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
        .route("/api/admin/entities", post(create))
        .route("/api/admin/entities/candidates", get(candidates))
        .route("/api/admin/entities/{id}/aliases", post(add_alias))
        .route(
            "/api/admin/entity-merges",
            get(list_merges).post(propose_merge),
        )
        .route("/api/admin/entity-merges/{id}/accept", post(accept_merge))
        .route("/api/admin/entity-merges/{id}/reject", post(reject_merge))
        .route("/api/admin/entity-merges/{id}/reverse", post(reverse_merge))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn entity_resolution_is_controller_only() {
        let user = |role| AuthedUser {
            user_id: "user".into(),
            session_id: None,
            username: "user".into(),
            display_name: "User".into(),
            email: None,
            role,
            last_login_at: None,
        };
        assert!(require_controller(&user(UserRole::Controller)).is_ok());
        assert_eq!(
            require_controller(&user(UserRole::Viewer))
                .unwrap_err()
                .status(),
            StatusCode::FORBIDDEN
        );
    }
}
