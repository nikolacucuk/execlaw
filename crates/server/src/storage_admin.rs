//! Bounded operator-initiated SQLite maintenance.

use crate::auth_extract::AuthedUser;
use crate::routes::ApiError;
use crate::state::AppState;
use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::Json;
use axum::routing::post;
use execlaw_core::db::WalCheckpointProgress;
use serde::Serialize;
use utoipa::ToSchema;

#[derive(Debug, Serialize, ToSchema)]
pub struct WalMaintenanceResponse {
    pub progress: WalCheckpointProgress,
    pub wal_bytes_after: u64,
    pub action: String,
}

#[utoipa::path(
    post,
    path = "/api/admin/storage/checkpoint",
    responses((status = 200, description = "Passive WAL checkpoint progress", body = WalMaintenanceResponse), (status = 403, description = "Controller role required")),
    security(("bearer_jwt" = [])),
    tag = "diagnostics"
)]
pub async fn checkpoint(
    State(state): State<AppState>,
    user: AuthedUser,
) -> Result<Json<WalMaintenanceResponse>, ApiError> {
    let current_role = execlaw_core::users::UserStore::new(&state.db)
        .get_by_id(&user.user_id)
        .map_err(ApiError::from)?
        .map(|row| row.role);
    if current_role != Some(execlaw_core::users::UserRole::Controller) {
        return Err(ApiError {
            status: StatusCode::FORBIDDEN,
            code: "controller_required",
            message: "Controller role required".into(),
        });
    }
    let db = state.db.clone();
    let executor = db.clone();
    let progress = executor
        .run_blocking(move || db.checkpoint_passive())
        .await
        .map_err(ApiError::from)?
        .map_err(ApiError::from)?;
    let wal_bytes_after = state.db.file_sizes().wal_bytes;
    Ok(Json(WalMaintenanceResponse {
        progress,
        wal_bytes_after,
        action: "Passive checkpointed available frames; uncheckpointed frames remain in the WAL for a later retry.".into(),
    }))
}

pub fn router() -> Router<AppState> {
    Router::new().route("/api/admin/storage/checkpoint", post(checkpoint))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth_extract::AuthedUser;
    use execlaw_core::users::{UserRole, UserRow, UserStore};

    fn user(role: UserRole) -> AuthedUser {
        AuthedUser {
            user_id: "storage-test-user".into(),
            session_id: None,
            username: "storage-test".into(),
            display_name: "Storage Test".into(),
            email: None,
            role,
            last_login_at: None,
        }
    }

    fn insert_user(state: &AppState, user_id: &str, role: UserRole) {
        UserStore::new(&state.db)
            .insert(&UserRow {
                user_id: user_id.into(),
                username: user_id.into(),
                display_name: user_id.into(),
                email: None,
                password_hash: "unused-test-password-hash".into(),
                role,
                created_at: 1,
                last_login_at: None,
            })
            .unwrap();
    }

    #[tokio::test]
    async fn passive_checkpoint_is_controller_only_and_reports_progress() {
        let state = crate::routes::test_app_state();
        insert_user(&state, "storage-test-user", UserRole::Controller);
        let response = checkpoint(State(state.clone()), user(UserRole::Controller))
            .await
            .unwrap();
        assert!(response.0.progress.frames_in_wal >= -1);

        insert_user(&state, "storage-test-viewer", UserRole::Viewer);
        let mut viewer = user(UserRole::Viewer);
        viewer.user_id = "storage-test-viewer".into();
        let denied = checkpoint(State(state), viewer).await.unwrap_err();
        assert_eq!(denied.status, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn stale_controller_claim_cannot_checkpoint_after_role_demotion() {
        let state = crate::routes::test_app_state();
        insert_user(&state, "storage-test-user", UserRole::Controller);
        state
            .db
            .with_conn(|connection| {
                connection.execute(
                    "UPDATE users SET role = 'viewer' WHERE user_id = ?1",
                    ["storage-test-user"],
                )?;
                Ok(())
            })
            .unwrap();

        let denied = checkpoint(State(state), user(UserRole::Controller))
            .await
            .unwrap_err();
        assert_eq!(denied.status, StatusCode::FORBIDDEN);
    }
}
