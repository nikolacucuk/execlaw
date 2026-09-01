//! End-to-end integration test for plugin-shipped skills imported via
//! plugin ZIP install flow.
//!
//! Scope:
//! - Build install ZIPs from in-tree plugin files.
//! - POST `/api/admin/plugins/install` for both skill plugins.
//! - GET `/api/admin/skills` and assert plugin-shipped names are present.

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

const HUMANIZER_PLUGIN_ID: &str = "humanizer-skills";
const OBSIDIAN_PLUGIN_ID: &str = "obsidian-skills";

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
    let plugin_host = PluginHost::with_script_engine(
        db.clone(),
        HookRegistry::new(),
        stage_root,
        execlaw_script::ScriptEngine::with_loopback_allowed_for_tests(),
    );
    let skill_store = Arc::new(execlaw_skills::SkillStore::new(db.clone()));
    plugin_host.attach_skill_store(skill_store);

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
        plugin_host: plugin_host.clone(),
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

async fn get_json_auth(
    app: axum::Router,
    uri: &str,
    bearer: &str,
) -> (StatusCode, serde_json::Value) {
    let req = Request::builder()
        .method(Method::GET)
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
        .body(Body::empty())
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
            "username": "skillsadmin",
            "admin_password": "hunter2-longer",
            "display_name": "Skills Admin",
            "email": "skillsadmin@example.com"
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "setup body: {body}");
    body["access_token"]
        .as_str()
        .expect("setup returns access_token")
        .to_owned()
}

fn workspace_plugin_root(plugin_id: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("plugins")
        .join(plugin_id)
}

fn load_humanizer_plugin_zip_from_workspace() -> Vec<u8> {
    let plugin_root = workspace_plugin_root(HUMANIZER_PLUGIN_ID);
    let manifest = std::fs::read(plugin_root.join("plugin.toml")).expect("plugin.toml exists");
    let rhai = std::fs::read(plugin_root.join("main.rhai")).expect("main.rhai exists");
    let skill_md = std::fs::read(plugin_root.join("skills/humanizer.md")).expect("skill md exists");

    build_zip(&[
        ("plugin.toml", &manifest),
        ("main.rhai", &rhai),
        ("skills/humanizer.md", &skill_md),
    ])
}

fn load_obsidian_plugin_zip_from_workspace() -> Vec<u8> {
    let plugin_root = workspace_plugin_root(OBSIDIAN_PLUGIN_ID);
    let manifest = std::fs::read(plugin_root.join("plugin.toml")).expect("plugin.toml exists");
    let rhai = std::fs::read(plugin_root.join("main.rhai")).expect("main.rhai exists");
    let vault_workflow = std::fs::read(plugin_root.join("skills/vault-workflow.md"))
        .expect("vault-workflow skill exists");
    let atomic_notes =
        std::fs::read(plugin_root.join("skills/atomic-notes.md")).expect("atomic-notes exists");

    build_zip(&[
        ("plugin.toml", &manifest),
        ("main.rhai", &rhai),
        ("skills/vault-workflow.md", &vault_workflow),
        ("skills/atomic-notes.md", &atomic_notes),
    ])
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn installing_skill_plugin_zips_exposes_plugin_shipped_skills_in_admin_list() {
    let stage_dir = tempfile::tempdir().unwrap();
    let (app, _state) = build_app(stage_dir.path().to_path_buf());
    let token = setup_and_get_access_token(&app).await;

    let (status, body) = post_zip(app.clone(), load_humanizer_plugin_zip_from_workspace()).await;
    assert_eq!(status, StatusCode::OK, "install body: {body}");
    assert_eq!(body["plugin_id"], HUMANIZER_PLUGIN_ID);

    let (status, body) = post_zip(app.clone(), load_obsidian_plugin_zip_from_workspace()).await;
    assert_eq!(status, StatusCode::OK, "install body: {body}");
    assert_eq!(body["plugin_id"], OBSIDIAN_PLUGIN_ID);

    let (status, skills_body) = get_json_auth(app, "/api/admin/skills", &token).await;
    assert_eq!(status, StatusCode::OK, "skills body: {skills_body}");

    let skills = skills_body["skills"].as_array().expect("skills array");

    let has_humanizer = skills.iter().any(|s| {
        s["name"] == "humanizer-skills/humanizer"
            && s["registration_kind"] == "shipped"
            && s["owning_plugin_id"] == HUMANIZER_PLUGIN_ID
    });
    let has_obsidian_vault = skills.iter().any(|s| {
        s["name"] == "obsidian-skills/vault-workflow"
            && s["registration_kind"] == "shipped"
            && s["owning_plugin_id"] == OBSIDIAN_PLUGIN_ID
    });

    assert!(
        has_humanizer,
        "expected humanizer shipped skill in /api/admin/skills: {skills_body}"
    );
    assert!(
        has_obsidian_vault,
        "expected obsidian shipped skill in /api/admin/skills: {skills_body}"
    );
}
