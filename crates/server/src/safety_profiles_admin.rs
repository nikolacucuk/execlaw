//! Controller-visible task safety profiles.

use crate::auth_extract::AuthedUser;
use crate::routes::ApiError;
use crate::state::AppState;
use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::Json;
use axum::routing::{get, put};
use execlaw_core::safety_profiles::{SafetyProfile, SafetyProfileId, SafetyProfileStore};
use execlaw_core::tool_access::ToolAccessStore;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SafetyPermissionSummary {
    pub filesystem: String,
    pub process: String,
    pub network: String,
    pub secrets: String,
    pub destinations: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SafetyProfileView {
    pub profile: SafetyProfile,
    pub supported: bool,
    pub unsupported_reason: Option<String>,
    pub permissions: SafetyPermissionSummary,
    pub effective_tools: Vec<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SafetyProfileListResponse {
    pub profiles: Vec<SafetyProfileView>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct ApprovedToolsRequest {
    pub tool_names: Vec<String>,
}

#[utoipa::path(
    get,
    path = "/api/admin/safety-profiles",
    responses((status = 200, description = "Safety profiles and effective permissions", body = SafetyProfileListResponse)),
    security(("bearer_jwt" = [])),
    tag = "tools"
)]
pub async fn list_profiles(
    State(state): State<AppState>,
    _user: AuthedUser,
) -> Result<Json<SafetyProfileListResponse>, ApiError> {
    let profiles = SafetyProfileStore::new(&state.db)
        .list()
        .map_err(|error| ApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "safety_profile_storage_error",
            message: error.to_string(),
        })?;
    let tools = ToolAccessStore::new(&state.db)
        .list_all()
        .map_err(ApiError::from)?;
    let live_tools = tools
        .iter()
        .filter(|tool| {
            tool.enabled
                && tool.removed_at.is_none()
                && tool
                    .allowed_classes
                    .iter()
                    .any(|class| class == "Controller")
        })
        .map(|tool| tool.tool_name.clone())
        .collect::<Vec<_>>();
    let mut views = Vec::with_capacity(profiles.len());
    for profile in profiles {
        let (supported, unsupported_reason) = profile_support(&state, &profile).await;
        let effective_tools = if profile.profile_id == SafetyProfileId::ApprovedIntegration {
            profile
                .approved_tools
                .iter()
                .filter(|name| live_tools.iter().any(|live| live == *name))
                .cloned()
                .collect()
        } else {
            live_tools
                .iter()
                .filter(|name| {
                    let Some(registered) = state.plugin_host.registry().tool(name) else {
                        return false;
                    };
                    !registered.required_capabilities.is_empty()
                        && profile
                            .capabilities
                            .allows_declared_capability("workspace.read")
                        && registered
                            .required_capabilities
                            .iter()
                            .all(|cap| profile.capabilities.allows_declared_capability(cap))
                })
                .cloned()
                .collect()
        };
        let capabilities = profile.capabilities.clone();
        views.push(SafetyProfileView {
            profile,
            supported,
            unsupported_reason,
            permissions: SafetyPermissionSummary {
                filesystem: match (capabilities.workspace_read, capabilities.workspace_write) {
                    (true, true) => "isolated workspace read and write".into(),
                    (true, false) => "workspace read only".into(),
                    _ => "no workspace access".into(),
                },
                process: if capabilities.workspace_process {
                    "bounded workspace container only".into()
                } else {
                    "denied".into()
                },
                network: if capabilities.approved_network {
                    "approved integration tools only".into()
                } else {
                    "denied".into()
                },
                secrets: if capabilities.brokered_secret_use {
                    "vault-brokered integration access; values stay outside model context".into()
                } else {
                    "denied".into()
                },
                destinations: if capabilities.approved_destinations {
                    "approved integration tool allowlist only".into()
                } else {
                    "denied".into()
                },
            },
            effective_tools,
        });
    }
    Ok(Json(SafetyProfileListResponse { profiles: views }))
}

#[utoipa::path(
    put,
    path = "/api/admin/safety-profiles/{profile_id}/approved-tools",
    request_body = ApprovedToolsRequest,
    params(("profile_id" = String, Path, description = "Stable safety profile ID")),
    responses(
        (status = 200, description = "Updated integration allowlist", body = SafetyProfile),
        (status = 400, description = "Invalid profile or tool list"),
        (status = 403, description = "Controller role required"),
        (status = 404, description = "Tool is not currently registered and enabled")
    ),
    security(("bearer_jwt" = [])),
    tag = "tools"
)]
pub async fn set_approved_tools(
    State(state): State<AppState>,
    user: AuthedUser,
    Path(profile_id): Path<String>,
    Json(request): Json<ApprovedToolsRequest>,
) -> Result<Json<SafetyProfile>, ApiError> {
    require_controller(&state, &user)?;
    let profile_id = SafetyProfileId::parse(&profile_id).ok_or_else(|| ApiError {
        status: StatusCode::BAD_REQUEST,
        code: "safety_profile_invalid",
        message: "unknown safety profile".into(),
    })?;
    let rows = ToolAccessStore::new(&state.db)
        .list_all()
        .map_err(ApiError::from)?;
    for name in &request.tool_names {
        let Some(row) = rows.iter().find(|row| row.tool_name == *name) else {
            return Err(ApiError {
                status: StatusCode::NOT_FOUND,
                code: "approved_tool_unavailable",
                message: format!("tool '{name}' is not registered and enabled"),
            });
        };
        if !row.enabled
            || row.removed_at.is_some()
            || !row
                .allowed_classes
                .iter()
                .any(|class| class == "Controller")
        {
            return Err(ApiError {
                status: StatusCode::NOT_FOUND,
                code: "approved_tool_unavailable",
                message: format!("tool '{name}' is not enabled and allowed for this Controller"),
            });
        }
    }
    let store = SafetyProfileStore::new(&state.db);
    let prior = store.get(profile_id).map_err(|error| ApiError {
        status: StatusCode::BAD_REQUEST,
        code: "safety_profile_invalid",
        message: error.to_string(),
    })?;
    let profile = store
        .set_approved_tools(profile_id, &request.tool_names, &user.user_id, unix_now())
        .map_err(|error| ApiError {
            status: StatusCode::BAD_REQUEST,
            code: "safety_profile_invalid",
            message: error.to_string(),
        })?;
    let _ = execlaw_core::audit::AuditStore::new(&state.db).insert(
        &user.user_id,
        "config_safety_profiles",
        profile_id.as_str(),
        Some(&serde_json::json!({"revision": prior.revision, "approved_tools": prior.approved_tools})),
        Some(&serde_json::json!({"revision": profile.revision, "approved_tools": profile.approved_tools})),
    );
    Ok(Json(profile))
}

pub async fn profile_support(state: &AppState, profile: &SafetyProfile) -> (bool, Option<String>) {
    if profile.profile_id != SafetyProfileId::WorkspaceEdit {
        return (true, None);
    }
    if !matches!(std::env::consts::OS, "linux" | "windows" | "macos") {
        return (
            false,
            Some("workspace container enforcement is unsupported on this operating system".into()),
        );
    }
    let config = match execlaw_core::workspaces::WorkspaceStore::new(&state.db).execution_config() {
        Ok(config) => config,
        Err(error) => {
            return (
                false,
                Some(format!("workspace enforcement status unavailable: {error}")),
            );
        }
    };
    if config.image_reference.is_none() {
        return (
            false,
            Some(
                "configure an approved workspace toolchain image before using this profile".into(),
            ),
        );
    }
    let docker = match bollard::Docker::connect_with_local_defaults() {
        Ok(docker) => docker,
        Err(_) => {
            return (
                false,
                Some(
                    "the local container service is unavailable for workspace process isolation"
                        .into(),
                ),
            );
        }
    };
    match docker.ping().await {
        Ok(_) => (true, None),
        Err(_) => (
            false,
            Some(
                "the local container service is unavailable for workspace process isolation".into(),
            ),
        ),
    }
}

fn require_controller(state: &AppState, user: &AuthedUser) -> Result<(), ApiError> {
    use execlaw_core::users::{UserRole, UserStore};
    let role = UserStore::new(&state.db)
        .get_by_id(&user.user_id)
        .map_err(ApiError::from)?
        .map(|row| row.role);
    if role == Some(UserRole::Controller) {
        Ok(())
    } else {
        Err(ApiError {
            status: StatusCode::FORBIDDEN,
            code: "controller_only",
            message: "only a Controller can manage task safety profiles".into(),
        })
    }
}

fn unix_now() -> i64 {
    chrono::Utc::now().timestamp()
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/admin/safety-profiles", get(list_profiles))
        .route(
            "/api/admin/safety-profiles/{profile_id}/approved-tools",
            put(set_approved_tools),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes::{build_router, test_app_state};
    use axum::body::{self, Body};
    use axum::http::{Method, Request, header};
    use execlaw_core::tool_access::{ToolAccessSeed, ToolAccessStore, ToolSource};
    use tower::ServiceExt;

    async fn controller_token(app: &axum::Router) -> String {
        let request = Request::builder()
            .method(Method::POST)
            .uri("/api/setup")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::json!({
                    "username":"profile-controller",
                    "admin_password":"safety-profile-test-password",
                    "display_name":"Controller"
                })
                .to_string(),
            ))
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        let bytes = body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()["access_token"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    #[tokio::test]
    async fn controller_can_view_profiles_and_approve_a_registered_integration() {
        let state = test_app_state();
        ToolAccessStore::new(&state.db)
            .upsert_seen(
                &ToolAccessSeed {
                    tool_name: "calendar.create_event".into(),
                    source: ToolSource::Plugin,
                    source_id: Some("calendar".into()),
                    description: Some("Create one calendar event".into()),
                    input_schema: None,
                    default_allowed_classes: vec!["Controller".into()],
                },
                1,
            )
            .unwrap();
        let app = build_router(state);
        let token = controller_token(&app).await;
        let list = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::GET)
                    .uri("/api/admin/safety-profiles")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(list.status(), StatusCode::OK);
        let list_bytes = body::to_bytes(list.into_body(), usize::MAX).await.unwrap();
        let listed: serde_json::Value = serde_json::from_slice(&list_bytes).unwrap();
        assert_eq!(listed["profiles"].as_array().unwrap().len(), 3);
        assert_eq!(
            listed["profiles"][0]["profile"]["display_name"],
            "Approved integrations"
        );

        let update = app
            .oneshot(
                Request::builder()
                    .method(Method::PUT)
                    .uri("/api/admin/safety-profiles/approved_integration/approved-tools")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(
                        serde_json::json!({"tool_names":["calendar.create_event"]}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(update.status(), StatusCode::OK);
        let bytes = body::to_bytes(update.into_body(), usize::MAX)
            .await
            .unwrap();
        let profile: SafetyProfile = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(profile.revision, 2);
        assert_eq!(profile.approved_tools, vec!["calendar.create_event"]);
    }
}
