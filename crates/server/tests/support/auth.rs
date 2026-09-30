//! A real Controller session for integration tests that exercise private routes.

use axum::body::Body;
use axum::http::{Request, header};
use axum::middleware::{Next, from_fn};
use execlaw_core::users::{UserRole, UserRow, UserStore};
use execlaw_server::AppState;

pub fn authenticated_router(state: AppState) -> axum::Router {
    let user_id = "integration-controller";
    let session_id = "integration-session";
    UserStore::new(&state.db)
        .insert(&UserRow {
            user_id: user_id.into(),
            username: "integration-controller".into(),
            display_name: "Integration Controller".into(),
            email: None,
            password_hash: "unused-in-integration-test".into(),
            role: UserRole::Controller,
            created_at: 0,
            last_login_at: None,
        })
        .expect("seed integration Controller");
    let token = state
        .signer
        .issue_access_token(user_id, session_id, 600)
        .expect("mint integration access token");
    state
        .refresh_store
        .issue(user_id, session_id, state.config.refresh_token_ttl_secs)
        .expect("persist integration session");
    let bearer = format!("Bearer {token}");
    execlaw_server::routes::build_router(state).layer(from_fn(
        move |mut request: Request<Body>, next: Next| {
            let bearer = bearer.clone();
            async move {
                let path = request.uri().path();
                if path.starts_with("/api/admin/")
                    || path.starts_with("/api/chats/")
                    || path == "/api/stream"
                {
                    request.headers_mut().insert(
                        header::AUTHORIZATION,
                        bearer.parse().expect("valid integration bearer header"),
                    );
                }
                next.run(request).await
            }
        },
    ))
}
