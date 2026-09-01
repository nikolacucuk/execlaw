//! End-to-end integration test for the `plugins/web-scraper` script plugin.
//!
//! Scope:
//! - Build an install ZIP from the real plugin files in-tree.
//! - POST `/api/admin/plugins/install`.
//! - POST `/api/admin/plugins/web-scraper/test` and assert a structured
//!   response shape (works even when no sidecar is running in test harness).

use axum::body::{self, Body};
use axum::http::{Method, Request, StatusCode, header};
use execlaw_core::db::{Database, DbConfig};
use execlaw_core::migrations::MigrationRunner;
use execlaw_plugin_host::{HookRegistry, PluginHost};
use execlaw_server::{AppState, EventBus, JwtSigner, RefreshStore, ServerConfig};
use std::io::{Cursor, Write};
use std::path::PathBuf;
use std::sync::Arc;
use tower::ServiceExt;
use zip::ZipWriter;
use zip::write::SimpleFileOptions;

const PLUGIN_ID: &str = "web-scraper";

fn build_zip(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut buf = Cursor::new(Vec::new());
    {
        let mut zw = ZipWriter::new(&mut buf);
        let opts = SimpleFileOptions::default();
        for (name, bytes) in files {
            zw.start_file::<_, ()>(*name, opts).unwrap();
            zw.write_all(bytes).unwrap();
        }
        zw.finish().unwrap();
    }
    buf.into_inner()
}

fn build_app(stage_root: PathBuf) -> (axum::Router, AppState) {
    let db_config = DbConfig::in_memory_unencrypted();
    let db = Database::open(&db_config).unwrap();
    MigrationRunner::new(&db).apply_all().unwrap();
    let events = EventBus::new();
    let state = AppState {
        db: db.clone(),
        db_config: Arc::new(db_config),
        config: Arc::new(ServerConfig::default()),
        signer: Arc::new(JwtSigner::generate("execlaw-test".into())),
        refresh_store: Arc::new(RefreshStore::new(db.clone())),
        events: events.clone(),
        event_log_hmac_key: Some(Arc::new(b"execlaw-test-hmac-key-32-bytes!!".to_vec())),
        inference: Arc::new(execlaw_server::inference_resolver::InferenceResolver::new(
            None,
        )),
        plugin_host: PluginHost::with_script_engine(
            db.clone(),
            HookRegistry::new(),
            stage_root,
            execlaw_script::ScriptEngine::with_loopback_allowed_for_tests(),
        ),
        webauthn: None,
        mcp_host: execlaw_server::mcp_host::McpHost::new(db.clone()),
        backend_supervisor: None,
        voice_sessions: execlaw_server::voice_session::VoiceSessionRegistry::new(events.clone()),
        voice_runtime: execlaw_server::voice_runtime::VoiceRuntime::new(
            events,
            Arc::new(|| {
                Box::new(execlaw_voice_pipeline::traits::MockStt::new(
                    Vec::new(),
                    String::new(),
                ))
            }),
            Arc::new(|| {
                (
                    Box::new(execlaw_voice_pipeline::traits::MockTts::default())
                        as Box<dyn execlaw_voice_pipeline::traits::TtsClient>,
                    None,
                )
            }),
        ),
        turn_cancel: execlaw_server::turn_cancel::TurnCancellationRegistry::new(),
        runner_supervisor: None,
        research_supervisor: None,
        sidecar_supervisor: None,
        host_transports: execlaw_server::transport_registry::HostTransportRegistry::new(),
        skill_capture: execlaw_skills::AutoCaptureSink::noop(),
        reuse_update: execlaw_skills::ReuseUpdateSink::noop(),
        optimizer_worker: None,
        automation_bus: execlaw_server::automation_bus::AutomationBus::stub(db),
        automation_agent_pool: execlaw_server::automation_agent::AutomationsAgentPool::new(
            std::sync::Arc::new(execlaw_server::automation_agent::StubAgentInvoker::err(
                "test pool: no LLM",
            )),
        ),
        data_dir: std::env::temp_dir().join(format!("execlaw-test-{}", uuid::Uuid::new_v4())),
        inference_metrics: execlaw_server::inference_metrics::InferenceMetrics::new(),
        login_limiter: execlaw_server::auth_rate_limit::LoginRateLimiter::new(),
    };

    let caps =
        execlaw_server::host_caps_impl::AppStateHostCapabilities::new(state.clone()).into_arc();
    let _ = state.plugin_host.attach_host_capabilities(caps);

    (execlaw_server::routes::build_router(state.clone()), state)
}

async fn post_zip(app: axum::Router, bytes: Vec<u8>) -> (StatusCode, serde_json::Value) {
    let req = Request::builder()
        .method(Method::POST)
        .uri("/api/admin/plugins/install")
        .header(header::CONTENT_TYPE, "application/zip")
        .body(Body::from(bytes))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    let status = resp.status();
    let body_bytes = body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let value: serde_json::Value =
        serde_json::from_slice(&body_bytes).unwrap_or(serde_json::Value::Null);
    (status, value)
}

async fn post_json(
    app: axum::Router,
    uri: &str,
    payload: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let req = Request::builder()
        .method(Method::POST)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(payload.to_string()))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    let status = resp.status();
    let body_bytes = body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let value: serde_json::Value =
        serde_json::from_slice(&body_bytes).unwrap_or(serde_json::Value::Null);
    (status, value)
}

async fn post_json_auth(
    app: axum::Router,
    uri: &str,
    payload: serde_json::Value,
    bearer: &str,
) -> (StatusCode, serde_json::Value) {
    let req = Request::builder()
        .method(Method::POST)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
        .body(Body::from(payload.to_string()))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    let status = resp.status();
    let body_bytes = body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let value: serde_json::Value =
        serde_json::from_slice(&body_bytes).unwrap_or(serde_json::Value::Null);
    (status, value)
}

async fn setup_and_get_access_token(app: &axum::Router) -> String {
    let (status, body) = post_json(
        app.clone(),
        "/api/setup",
        serde_json::json!({
            "username": "wsadmin",
            "admin_password": "hunter2-longer",
            "display_name": "Web Scraper Admin",
            "email": "wsadmin@example.com"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "setup body: {body}");
    body["access_token"]
        .as_str()
        .expect("setup returns access_token")
        .to_owned()
}

fn load_web_scraper_zip_from_workspace() -> Vec<u8> {
    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let plugin_root = workspace_root.join("plugins").join("web-scraper");

    let manifest = std::fs::read(plugin_root.join("plugin.toml")).expect("plugin.toml exists");
    let rhai = std::fs::read(plugin_root.join("main.rhai")).expect("main.rhai exists");
    let schema_fetch =
        std::fs::read(plugin_root.join("schemas/fetch_page.json")).expect("fetch schema exists");
    let schema_extract =
        std::fs::read(plugin_root.join("schemas/extract.json")).expect("extract schema exists");
    let schema_follow = std::fs::read(plugin_root.join("schemas/follow_links.json"))
        .expect("follow links schema exists");
    let schema_session = std::fs::read(plugin_root.join("schemas/session_close.json"))
        .expect("session close schema exists");
    let panel_js = std::fs::read(plugin_root.join("ui/panel.js")).expect("ui/panel.js exists");

    build_zip(&[
        ("plugin.toml", &manifest),
        ("main.rhai", &rhai),
        ("schemas/fetch_page.json", &schema_fetch),
        ("schemas/extract.json", &schema_extract),
        ("schemas/follow_links.json", &schema_follow),
        ("schemas/session_close.json", &schema_session),
        ("ui/panel.js", &panel_js),
    ])
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn web_scraper_plugin_install_and_admin_test_route() {
    let stage_dir = tempfile::tempdir().unwrap();
    let (app, _state) = build_app(stage_dir.path().to_path_buf());
    let token = setup_and_get_access_token(&app).await;

    let zip = load_web_scraper_zip_from_workspace();
    let (status, body) = post_zip(app.clone(), zip).await;
    assert_eq!(status, StatusCode::OK, "install body: {body}");
    assert_eq!(body["plugin_id"], PLUGIN_ID);

    let (status, test_body) = post_json_auth(
        app,
        "/api/admin/plugins/web-scraper/test",
        serde_json::json!({}),
        &token,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "admin test body: {test_body}");

    // In test harness there is no sidecar supervisor, so route should
    // still return a structured payload rather than throwing.
    assert_eq!(test_body["sidecar_healthy"], false);
    assert!(test_body.get("config").is_some());
}
