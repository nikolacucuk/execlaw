//! Integration tests for chain-approval bridge on the existing
//! `/api/admin/approvals` feed + respond endpoint.

use axum::body::{self, Body};
use axum::http::{Method, Request, StatusCode, header};
use execlaw_core::conversation::{
    ConversationKind, ConversationRow, ConversationStore, Modality, Phase,
};
use execlaw_core::db::{Database, DbConfig};
use execlaw_core::ids::{ConversationId, EventSeq, IdempotencyKey, TurnSeq};
use execlaw_core::migrations::MigrationRunner;
use execlaw_plugin_host::{HookRegistry, PluginHost};
use execlaw_server::{AppState, EventBus, JwtSigner, RefreshStore, ServerConfig};
use serde_json::json;
use std::sync::Arc;
use tower::ServiceExt;

fn build_app(stage_root: std::path::PathBuf) -> (axum::Router, AppState) {
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
        plugin_host: PluginHost::new(db.clone(), HookRegistry::new(), stage_root),
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
        memory_extract: execlaw_server::memory_extract_runtime::MemoryExtractionSink::noop(),
        sidecar_supervisor: None,
        host_transports: execlaw_server::transport_registry::HostTransportRegistry::new(),
        skill_capture: execlaw_skills::AutoCaptureSink::noop(),
        reuse_update: execlaw_skills::ReuseUpdateSink::noop(),
        optimizer_worker: None,
        automation_bus: execlaw_server::automation_bus::AutomationBus::stub(db),
        automation_agent_pool: execlaw_server::automation_agent::AutomationsAgentPool::new(
            Arc::new(execlaw_server::automation_agent::StubAgentInvoker::err(
                "test pool: no LLM",
            )),
        ),
        data_dir: std::env::temp_dir().join(format!("execlaw-test-{}", uuid::Uuid::new_v4())),
        inference_metrics: execlaw_server::inference_metrics::InferenceMetrics::new(),
        login_limiter: execlaw_server::auth_rate_limit::LoginRateLimiter::new(),
    };
    (execlaw_server::routes::build_router(state.clone()), state)
}

async fn setup_get_token(app: &axum::Router) -> String {
    let body = serde_json::to_vec(&json!({
        "username": "tester",
        "admin_password": "hunter2-longer",
        "display_name": "Tester",
    }))
    .unwrap();
    let req = Request::builder()
        .method(Method::POST)
        .uri("/api/setup")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let bytes = body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    v["access_token"].as_str().unwrap().to_owned()
}

fn seed_conversation(db: &Database, conversation_id: &ConversationId) {
    let row = ConversationRow {
        conversation_id: conversation_id.clone(),
        kind: ConversationKind::ControllerDM,
        last_seq: EventSeq(11),
        phase: Phase::Idle,
        controller_id: Some("controller".into()),
        trust_class: "Controller".into(),
        snapshot_blob: None,
        snapshot_seq: None,
        lease_owner: None,
        lease_expires: None,
        modality: Modality::Text,
        display_name: None,
        display_name_source: "auto".into(),
        is_pinned: false,
        is_ephemeral: false,
        ephemeral_expires_at: None,
        last_activity_at: 0,
        context_window_policy: None,
    };
    ConversationStore::new(db).upsert(&row).unwrap();
}

fn seed_chain_pending(
    db: &Database,
    approval_id: &str,
    conversation_id: &ConversationId,
    run_seq: i64,
    objective: &str,
    effect_kind: &str,
) {
    let now = chrono::Utc::now().timestamp();
    let plan_id = format!("plan-{}", uuid::Uuid::new_v4());
    let run_id = format!("run-{}", uuid::Uuid::new_v4());
    let effect_step = json!({
        "step_index": 0,
        "label": "effect",
        "effect_kind": effect_kind,
        "payload": {"text": "hello"},
        "resource_preconditions": [],
        "compensation": null,
        "idempotency_key_override": null
    });
    let plan_json = serde_json::to_vec(&json!({
        "objective": objective,
        "constraints": [],
        "steps": [effect_step.clone()]
    }))
    .unwrap();
    let effect_hash = execlaw_core::tool::tool_schema_hash(&json!([effect_step]));

    db.with_conn(|c| {
        c.execute(
            "INSERT INTO state_chain_plans \
             (id, conversation_id, objective, constraints_json, plan_json, has_external_effects, created_by_trust, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, 1, 'Controller', ?6, ?6)",
            rusqlite::params![
                plan_id,
                conversation_id.as_str(),
                objective,
                "[]",
                plan_json,
                now
            ],
        )?;
        c.execute(
            "INSERT INTO state_chain_runs \
             (id, plan_id, conversation_id, run_seq, status, approval_id, next_step_index, created_at, updated_at, approval_effect_hash) \
             VALUES (?1, ?2, ?3, ?4, 'awaiting_approval', ?5, 0, ?6, ?6, ?7)",
            rusqlite::params![run_id, plan_id, conversation_id.as_str(), run_seq, approval_id, now, effect_hash],
        )?;
        Ok(())
    })
    .unwrap();
}

async fn get_pending(app: &axum::Router, token: &str) -> (StatusCode, serde_json::Value) {
    let req = Request::builder()
        .method(Method::GET)
        .uri("/api/admin/approvals")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or_default();
    (status, v)
}

async fn respond(
    app: &axum::Router,
    approval_id: &str,
    verb: &str,
    approval_token: &str,
    user_token: &str,
) -> (StatusCode, serde_json::Value) {
    let req = Request::builder()
        .method(Method::POST)
        .uri(format!("/api/admin/approvals/{approval_id}/respond"))
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, format!("Bearer {user_token}"))
        .body(Body::from(
            serde_json::to_vec(&json!({"verb": verb, "approval_token": approval_token})).unwrap(),
        ))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or_default();
    (status, v)
}

#[tokio::test]
async fn chain_pending_approvals_show_in_existing_feed() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, state) = build_app(tmp.path().to_path_buf());
    let token = setup_get_token(&app).await;

    let cid = ConversationId::from("chain-feed-1");
    seed_conversation(&state.db, &cid);
    seed_chain_pending(
        &state.db,
        "chain-appr-feed-1",
        &cid,
        7,
        "Ship report to operator",
        "transport.send",
    );

    let (status, body) = get_pending(&app, &token).await;
    assert_eq!(status, StatusCode::OK);
    let approvals = body["approvals"].as_array().unwrap();
    let item = approvals
        .iter()
        .find(|a| a["approval_id"] == "chain-appr-feed-1")
        .expect("chain approval should be present in feed");
    assert_eq!(item["sender_principal_id"], "tool-chain");
    assert_eq!(item["kind"], "effectful_chain");
    assert!(item["approval_token"].as_str().is_some());
    assert!(
        item["original_text"]
            .as_str()
            .unwrap()
            .contains("Tool-chain execution awaiting approval")
    );
}

#[tokio::test]
async fn chain_approval_approve_via_http_completes_run_and_enqueues_outbox() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, state) = build_app(tmp.path().to_path_buf());
    let user_token = setup_get_token(&app).await;

    let cid = ConversationId::from("chain-feed-2");
    seed_conversation(&state.db, &cid);
    seed_chain_pending(
        &state.db,
        "chain-appr-approve-1",
        &cid,
        3,
        "Send operator digest",
        "transport.send",
    );

    let (_, pending) = get_pending(&app, &user_token).await;
    let token = pending["approvals"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["approval_id"] == "chain-appr-approve-1")
        .unwrap()["approval_token"]
        .as_str()
        .unwrap()
        .to_owned();
    let effect_hash: String = state.db.with_conn(|connection| {
            Ok(connection.query_row(
                "SELECT approval_effect_hash FROM state_chain_runs WHERE approval_id = 'chain-appr-approve-1'",
                [],
                |row| row.get(0),
            )?)
        }).unwrap();
    let wrong_principal_token = execlaw_server::approvals::issue_approval_token(
        &state.signer,
        "chain-appr-approve-1",
        &cid,
        "effectful_chain",
        Some("different-controller".into()),
        Some(effect_hash),
    );
    let (denied_status, _) = respond(
        &app,
        "chain-appr-approve-1",
        "approve",
        &wrong_principal_token,
        &user_token,
    )
    .await;
    assert_eq!(denied_status, StatusCode::FORBIDDEN);
    let still_waiting: String = state
        .db
        .with_conn(|connection| {
            Ok(connection.query_row(
                "SELECT status FROM state_chain_runs WHERE approval_id = 'chain-appr-approve-1'",
                [],
                |row| row.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(still_waiting, "awaiting_approval");
    let (status, body) =
        respond(&app, "chain-appr-approve-1", "approve", &token, &user_token).await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(body["outcome"], "completed");

    let run_status: String = state
        .db
        .with_conn(|c| {
            Ok(c.query_row(
                "SELECT status FROM state_chain_runs WHERE approval_id = ?1",
                rusqlite::params!["chain-appr-approve-1"],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(run_status, "completed");

    let (duplicate_status, _) =
        respond(&app, "chain-appr-approve-1", "approve", &token, &user_token).await;
    assert_eq!(duplicate_status, StatusCode::NOT_FOUND);

    let outbox_count: i64 = state
        .db
        .with_conn(|c| Ok(c.query_row("SELECT COUNT(*) FROM state_outbox", [], |r| r.get(0))?))
        .unwrap();
    assert_eq!(outbox_count, 1);

    let key: String = state
        .db
        .with_conn(|c| {
            Ok(c.query_row(
                "SELECT outbox_idempotency_key FROM state_chain_run_steps WHERE step_index = 0",
                [],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    let expected = IdempotencyKey::mint(&cid, TurnSeq(3), 0);
    assert_eq!(key, expected.as_str());
}

#[tokio::test]
async fn chain_approval_reject_via_http_marks_denied_and_skips_outbox() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, state) = build_app(tmp.path().to_path_buf());
    let user_token = setup_get_token(&app).await;

    let cid = ConversationId::from("chain-feed-3");
    seed_conversation(&state.db, &cid);
    seed_chain_pending(
        &state.db,
        "chain-appr-reject-1",
        &cid,
        5,
        "Do effect later",
        "transport.send",
    );

    let (_, pending) = get_pending(&app, &user_token).await;
    let token = pending["approvals"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["approval_id"] == "chain-appr-reject-1")
        .unwrap()["approval_token"]
        .as_str()
        .unwrap()
        .to_owned();
    let (status, body) = respond(&app, "chain-appr-reject-1", "reject", &token, &user_token).await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(body["outcome"], "denied");

    let run_status: String = state
        .db
        .with_conn(|c| {
            Ok(c.query_row(
                "SELECT status FROM state_chain_runs WHERE approval_id = ?1",
                rusqlite::params!["chain-appr-reject-1"],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(run_status, "denied");

    let outbox_count: i64 = state
        .db
        .with_conn(|c| Ok(c.query_row("SELECT COUNT(*) FROM state_outbox", [], |r| r.get(0))?))
        .unwrap();
    assert_eq!(outbox_count, 0);
}

#[tokio::test]
async fn chain_approval_unsupported_verb_returns_400_with_expected_error_payload() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, state) = build_app(tmp.path().to_path_buf());
    let user_token = setup_get_token(&app).await;

    let cid = ConversationId::from("chain-feed-4");
    seed_conversation(&state.db, &cid);
    seed_chain_pending(
        &state.db,
        "chain-appr-unsupported-1",
        &cid,
        9,
        "Send digest eventually",
        "transport.send",
    );

    let (_, pending) = get_pending(&app, &user_token).await;
    let token = pending["approvals"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["approval_id"] == "chain-appr-unsupported-1")
        .unwrap()["approval_token"]
        .as_str()
        .unwrap()
        .to_owned();
    let (status, body) = respond(
        &app,
        "chain-appr-unsupported-1",
        "trust",
        &token,
        &user_token,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body={body}");
    assert_eq!(body["error"]["code"], "unsupported_verb");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("not supported for chain approvals")
    );

    let run_status: String = state
        .db
        .with_conn(|c| {
            Ok(c.query_row(
                "SELECT status FROM state_chain_runs WHERE approval_id = ?1",
                rusqlite::params!["chain-appr-unsupported-1"],
                |r| r.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(run_status, "awaiting_approval");
}

#[tokio::test]
async fn signed_chain_approval_cannot_authorize_changed_plan_arguments() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, state) = build_app(tmp.path().to_path_buf());
    let user_token = setup_get_token(&app).await;
    let cid = ConversationId::from("chain-effect-binding");
    seed_conversation(&state.db, &cid);
    seed_chain_pending(
        &state.db,
        "chain-appr-bound-1",
        &cid,
        13,
        "Send report",
        "transport.send",
    );

    let (_, pending) = get_pending(&app, &user_token).await;
    let token = pending["approvals"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["approval_id"] == "chain-appr-bound-1")
        .unwrap()["approval_token"]
        .as_str()
        .unwrap()
        .to_owned();
    state
        .db
        .with_conn(|connection| {
            let plan_id: String = connection.query_row(
                "SELECT plan_id FROM state_chain_runs WHERE approval_id = 'chain-appr-bound-1'",
                [],
                |row| row.get(0),
            )?;
            let mut plan: serde_json::Value =
                serde_json::from_slice(&connection.query_row::<Vec<u8>, _, _>(
                    "SELECT plan_json FROM state_chain_plans WHERE id = ?1",
                    [&plan_id],
                    |row| row.get(0),
                )?)
                .unwrap();
            plan["steps"][0]["payload"]["text"] = json!("changed after approval");
            connection.execute(
                "UPDATE state_chain_plans SET plan_json = ?1 WHERE id = ?2",
                rusqlite::params![serde_json::to_vec(&plan).unwrap(), plan_id],
            )?;
            Ok(())
        })
        .unwrap();

    let (status, body) = respond(&app, "chain-appr-bound-1", "approve", &token, &user_token).await;
    assert_eq!(status, StatusCode::CONFLICT, "body={body}");
    assert_eq!(body["error"]["code"], "approval_effect_mismatch");
    let (run_status, outbox_count): (String, i64) = state
        .db
        .with_conn(|connection| {
            Ok((
                connection.query_row(
                    "SELECT status FROM state_chain_runs WHERE approval_id = 'chain-appr-bound-1'",
                    [],
                    |row| row.get(0),
                )?,
                connection.query_row("SELECT COUNT(*) FROM state_outbox", [], |row| row.get(0))?,
            ))
        })
        .unwrap();
    assert_eq!(run_status, "failed");
    assert_eq!(outbox_count, 0);
}

#[tokio::test]
async fn expired_chain_approval_token_cannot_resolve_pending_run() {
    use execlaw_policy::sideband::ApprovalClaims;
    use jsonwebtoken::{Algorithm, Header, Validation, decode, encode};

    let tmp = tempfile::tempdir().unwrap();
    let (app, state) = build_app(tmp.path().to_path_buf());
    let user_token = setup_get_token(&app).await;
    let cid = ConversationId::from("chain-expired-token");
    seed_conversation(&state.db, &cid);
    seed_chain_pending(
        &state.db,
        "chain-appr-expired-1",
        &cid,
        15,
        "Send after approval",
        "transport.send",
    );
    let (_, feed) = get_pending(&app, &user_token).await;
    let valid_token = feed["approvals"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["approval_id"] == "chain-appr-expired-1")
        .unwrap()["approval_token"]
        .as_str()
        .unwrap();

    let mut validation = Validation::new(Algorithm::EdDSA);
    validation.validate_exp = false;
    let mut claims =
        decode::<ApprovalClaims>(valid_token, state.signer.decoding_key(), &validation)
            .unwrap()
            .claims;
    claims.iat = chrono::Utc::now().timestamp() - 120;
    claims.exp = chrono::Utc::now().timestamp() - 60;
    let expired = encode(
        &Header::new(Algorithm::EdDSA),
        &claims,
        state.signer.encoding_key(),
    )
    .unwrap();
    let (status, body) = respond(
        &app,
        "chain-appr-expired-1",
        "approve",
        &expired,
        &user_token,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "body={body}");
    let run_status: String = state
        .db
        .with_conn(|connection| {
            Ok(connection.query_row(
                "SELECT status FROM state_chain_runs WHERE approval_id = 'chain-appr-expired-1'",
                [],
                |row| row.get(0),
            )?)
        })
        .unwrap();
    assert_eq!(run_status, "awaiting_approval");
}
