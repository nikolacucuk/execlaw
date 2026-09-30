//! Controller surface for capability-scoped private endpoint approvals.

use crate::auth_extract::AuthedUser;
use crate::routes::ApiError;
use crate::state::AppState;
use axum::Router;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::Json;
use axum::routing::get;
use execlaw_core::audit::AuditStore;
use execlaw_core::local_endpoint_policy::{EndpointApprovalKind, EndpointApprovalScope};
use execlaw_core::users::UserRole;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct EndpointApprovalView {
    pub scope: String,
    pub kind: String,
    pub value: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct EndpointApprovalList {
    pub approvals: Vec<EndpointApprovalView>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct EndpointApprovalWrite {
    pub scope: String,
    pub kind: String,
    pub value: String,
}

#[derive(Debug, Deserialize)]
pub struct EndpointApprovalQuery {
    pub scope: String,
    pub kind: String,
    pub value: String,
}

fn require_controller(user: &AuthedUser) -> Result<(), ApiError> {
    if user.role == UserRole::Controller {
        Ok(())
    } else {
        Err(ApiError {
            status: StatusCode::FORBIDDEN,
            code: "controller_required",
            message: "Controller role required".into(),
        })
    }
}

fn parse_scope(value: &str) -> Result<EndpointApprovalScope, ApiError> {
    EndpointApprovalScope::parse(value).ok_or_else(|| ApiError {
        status: StatusCode::BAD_REQUEST,
        code: "endpoint_approval_scope_invalid",
        message: "scope must be local_inference or private_integration".into(),
    })
}

fn parse_kind(value: &str) -> Result<EndpointApprovalKind, ApiError> {
    EndpointApprovalKind::parse(value).ok_or_else(|| ApiError {
        status: StatusCode::BAD_REQUEST,
        code: "endpoint_approval_kind_invalid",
        message: "kind must be cidr or dns_name".into(),
    })
}

#[utoipa::path(
    get,
    path = "/api/admin/network/endpoint-approvals",
    responses(
        (status = 200, description = "Capability-scoped endpoint approvals", body = EndpointApprovalList),
        (status = 403, description = "Controller role required")
    ),
    security(("bearer_jwt" = [])),
    tag = "network"
)]
pub async fn list(
    State(state): State<AppState>,
    user: AuthedUser,
) -> Result<Json<EndpointApprovalList>, ApiError> {
    require_controller(&user)?;
    let store = execlaw_core::local_endpoint_policy::LocalEndpointPolicyStore::new(&state.db);
    let mut approvals = Vec::new();
    for scope in [
        EndpointApprovalScope::LocalInference,
        EndpointApprovalScope::PrivateIntegration,
    ] {
        let rows = store.approvals_for(scope).map_err(|error| ApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "endpoint_approvals_read_failed",
            message: error.to_string(),
        })?;
        approvals.extend(rows.cidrs.into_iter().map(|value| EndpointApprovalView {
            scope: scope.as_str().to_owned(),
            kind: EndpointApprovalKind::Cidr.as_str().to_owned(),
            value,
        }));
        approvals.extend(
            rows.dns_names
                .into_iter()
                .map(|value| EndpointApprovalView {
                    scope: scope.as_str().to_owned(),
                    kind: EndpointApprovalKind::DnsName.as_str().to_owned(),
                    value,
                }),
        );
    }
    Ok(Json(EndpointApprovalList { approvals }))
}

#[utoipa::path(
    post,
    path = "/api/admin/network/endpoint-approvals",
    request_body = EndpointApprovalWrite,
    responses(
        (status = 201, description = "Endpoint approval recorded", body = EndpointApprovalView),
        (status = 400, description = "Invalid scope, kind, or value"),
        (status = 403, description = "Controller role required")
    ),
    security(("bearer_jwt" = [])),
    tag = "network"
)]
pub async fn create(
    State(state): State<AppState>,
    user: AuthedUser,
    Json(request): Json<EndpointApprovalWrite>,
) -> Result<(StatusCode, Json<EndpointApprovalView>), ApiError> {
    require_controller(&user)?;
    let scope = parse_scope(&request.scope)?;
    let kind = parse_kind(&request.kind)?;
    validate_value(kind, &request.value)?;
    let now = chrono::Utc::now().timestamp();
    execlaw_core::local_endpoint_policy::LocalEndpointPolicyStore::new(&state.db)
        .approve_for(scope, kind, &request.value, now)
        .map_err(|error| ApiError {
            status: StatusCode::BAD_REQUEST,
            code: "endpoint_approval_invalid",
            message: error.to_string(),
        })?;
    let view = EndpointApprovalView {
        scope: scope.as_str().to_owned(),
        kind: kind.as_str().to_owned(),
        value: request.value.trim().to_owned(),
    };
    let _ = AuditStore::new(&state.db).insert(
        &user.user_id,
        "config_local_endpoint_approvals",
        &format!("{}:{}:{}", view.scope, view.kind, view.value),
        None,
        Some(&serde_json::json!({"scope":view.scope,"kind":view.kind,"value":view.value})),
    );
    Ok((StatusCode::CREATED, Json(view)))
}

#[utoipa::path(
    delete,
    path = "/api/admin/network/endpoint-approvals",
    params(
        ("scope" = String, Query, description = "local_inference or private_integration"),
        ("kind" = String, Query, description = "cidr or dns_name"),
        ("value" = String, Query, description = "Approved CIDR or exact DNS name"),
    ),
    responses(
        (status = 200, description = "Endpoint approval revoked"),
        (status = 403, description = "Controller role required"),
        (status = 404, description = "Endpoint approval not found")
    ),
    security(("bearer_jwt" = [])),
    tag = "network"
)]
pub async fn revoke(
    State(state): State<AppState>,
    user: AuthedUser,
    Query(query): Query<EndpointApprovalQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_controller(&user)?;
    let scope = parse_scope(&query.scope)?;
    let kind = parse_kind(&query.kind)?;
    let removed = execlaw_core::local_endpoint_policy::LocalEndpointPolicyStore::new(&state.db)
        .revoke_for(scope, kind, &query.value)
        .map_err(|error| ApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "endpoint_approval_revoke_failed",
            message: error.to_string(),
        })?;
    if !removed {
        return Err(ApiError {
            status: StatusCode::NOT_FOUND,
            code: "endpoint_approval_not_found",
            message: "Endpoint approval not found".into(),
        });
    }
    let _ = AuditStore::new(&state.db).insert(
        &user.user_id,
        "config_local_endpoint_approvals",
        &format!("{}:{}:{}", query.scope, query.kind, query.value),
        Some(&serde_json::json!({"scope":query.scope,"kind":query.kind,"value":query.value})),
        None,
    );
    Ok(Json(serde_json::json!({"revoked": true})))
}

fn validate_value(kind: EndpointApprovalKind, value: &str) -> Result<(), ApiError> {
    let valid = match kind {
        EndpointApprovalKind::Cidr => value.parse::<ipnet::IpNet>().is_ok(),
        EndpointApprovalKind::DnsName => {
            let name = value.trim_end_matches('.');
            !name.is_empty()
                && name.len() <= 253
                && name.split('.').all(|label| {
                    !label.is_empty()
                        && label.len() <= 63
                        && label
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                        && label.as_bytes()[0].is_ascii_alphanumeric()
                        && label.as_bytes()[label.len() - 1].is_ascii_alphanumeric()
                })
        }
    };
    if valid {
        Ok(())
    } else {
        Err(ApiError {
            status: StatusCode::BAD_REQUEST,
            code: "endpoint_approval_value_invalid",
            message: match kind {
                EndpointApprovalKind::Cidr => "value must be a valid IP CIDR".into(),
                EndpointApprovalKind::DnsName => "value must be an exact DNS name".into(),
            },
        })
    }
}

pub fn router() -> Router<AppState> {
    Router::new().route(
        "/api/admin/network/endpoint-approvals",
        get(list).post(create).delete(revoke),
    )
}
