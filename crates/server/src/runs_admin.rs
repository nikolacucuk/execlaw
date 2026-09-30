//! Controller endpoints for durable run acceptance contracts and evidence.

use crate::auth_extract::AuthedUser;
use crate::routes::ApiError;
use crate::state::AppState;
use axum::{
    Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::Json,
    routing::{get, post, put},
};
use execlaw_core::audit::AuditStore;
use execlaw_core::{runs::*, users::UserRole};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct CompletionContractRequest {
    pub acceptance_criteria: Vec<AcceptanceCriterion>,
    #[serde(default)]
    pub required_artifacts: Vec<RequiredRunArtifact>,
    #[serde(default)]
    pub delivery_required: bool,
}

#[derive(Debug, Deserialize)]
pub struct VerificationRequest {
    pub criterion_id: String,
    pub status: VerificationStatus,
    #[serde(default)]
    pub evidence_refs: Vec<String>,
    pub detail: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ArtifactCheckRequest {
    pub artifact_id: String,
    pub present: bool,
    pub evidence_ref: Option<String>,
    pub detail: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct DeliveryConfirmationRequest {
    pub evidence_ref: String,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnknownOutboxResolution {
    AuthorizeRetry,
    ConfirmDelivered,
}

#[derive(Debug, Deserialize)]
pub struct UnknownOutboxResolutionRequest {
    pub resolution: UnknownOutboxResolution,
    pub evidence_ref: String,
}

#[derive(Debug, Deserialize)]
pub struct CompactionEvidenceCursor {
    #[serde(default)]
    pub after: Option<i64>,
    #[serde(default = "default_compaction_evidence_limit")]
    pub limit: usize,
}

fn default_compaction_evidence_limit() -> usize {
    100
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/admin/runs", get(list_runs))
        .route("/api/admin/runs/child-tasks", get(list_child_tasks))
        .route(
            "/api/admin/conversations/{conversation_id}/compactions/{receipt_id}",
            get(get_compaction_evidence),
        )
        .route("/api/admin/runs/{run_id}/completion", get(get_completion))
        .route("/api/admin/runs/{run_id}/trace", get(get_trace))
        .route("/api/admin/runs/{run_id}/fork", post(fork_run))
        .route(
            "/api/admin/runs/{run_id}/completion-contract",
            put(set_completion_contract),
        )
        .route(
            "/api/admin/runs/{run_id}/completion-verifications",
            post(record_verification),
        )
        .route(
            "/api/admin/runs/{run_id}/completion-artifacts",
            post(record_artifact_check),
        )
        .route(
            "/api/admin/runs/{run_id}/delivery-confirmation",
            post(confirm_delivery),
        )
        .route("/api/admin/outbox/unknown", get(list_unknown_outbox))
        .route(
            "/api/admin/outbox/{outbox_id}/resolution",
            post(resolve_unknown_outbox),
        )
}

async fn list_unknown_outbox(
    State(state): State<AppState>,
    user: AuthedUser,
) -> Result<Json<Vec<execlaw_core::outbox::UnknownOutboxEffect>>, ApiError> {
    controller(&user)?;
    let effects = execlaw_core::outbox::OutboxStore::new(&state.db)
        .unknown_effects(100)
        .map_err(|error| ApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "outbox_reconciliation_list_failed",
            message: error.to_string(),
        })?;
    Ok(Json(effects))
}

async fn resolve_unknown_outbox(
    State(state): State<AppState>,
    user: AuthedUser,
    Path(outbox_id): Path<i64>,
    Json(request): Json<UnknownOutboxResolutionRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    controller(&user)?;
    let evidence_ref = request.evidence_ref.trim();
    if evidence_ref.is_empty()
        || evidence_ref.len() > 512
        || evidence_ref.chars().any(char::is_control)
    {
        return Err(ApiError {
            status: StatusCode::BAD_REQUEST,
            code: "outbox_resolution_evidence_invalid",
            message: "resolution evidence must be a bounded, non-empty reference".into(),
        });
    }
    let store = execlaw_core::outbox::OutboxStore::new(&state.db);
    let is_unknown = store
        .unknown_effect(outbox_id)
        .map_err(|error| ApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "outbox_reconciliation_lookup_failed",
            message: error.to_string(),
        })?
        .is_some();
    if !is_unknown {
        return Err(ApiError {
            status: StatusCode::NOT_FOUND,
            code: "outbox_unknown_not_found",
            message: "unknown outbox effect not found".into(),
        });
    }
    match request.resolution {
        UnknownOutboxResolution::AuthorizeRetry => store
            .authorize_unknown_retry(outbox_id, &user.user_id, evidence_ref)
            .map_err(|error| ApiError {
                status: StatusCode::CONFLICT,
                code: "outbox_retry_resolution_failed",
                message: error.to_string(),
            })?,
        UnknownOutboxResolution::ConfirmDelivered => store
            .confirm_unknown_delivered(outbox_id, &user.user_id, evidence_ref)
            .map_err(|error| ApiError {
                status: StatusCode::CONFLICT,
                code: "outbox_delivery_confirmation_failed",
                message: error.to_string(),
            })?,
    }
    Ok(Json(serde_json::json!({
        "outbox_id": outbox_id,
        "status": match request.resolution {
            UnknownOutboxResolution::AuthorizeRetry => "pending",
            UnknownOutboxResolution::ConfirmDelivered => "delivered",
        }
    })))
}

#[derive(Debug, serde::Serialize)]
pub struct CompactionEvidenceResponse {
    pub receipt: execlaw_core::harness::CompactionReceipt,
    pub source_events: Vec<execlaw_core::EventRecord>,
    pub next_after: Option<i64>,
    pub integrity_verified: bool,
}

async fn get_compaction_evidence(
    State(state): State<AppState>,
    user: AuthedUser,
    Path((conversation_id, receipt_id)): Path<(String, String)>,
    Query(cursor): Query<CompactionEvidenceCursor>,
) -> Result<Json<CompactionEvidenceResponse>, ApiError> {
    controller(&user)?;
    let receipt = execlaw_core::harness::HarnessStore::new(&state.db)
        .compaction_receipt(&receipt_id)
        .map_err(|error| ApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "compaction_receipt_error",
            message: error.to_string(),
        })?
        .filter(|receipt| receipt.conversation_id == conversation_id)
        .ok_or_else(|| ApiError {
            status: StatusCode::NOT_FOUND,
            code: "compaction_receipt_not_found",
            message: "compaction receipt not found".into(),
        })?;
    let first_after = receipt.source_start_seq.saturating_sub(1);
    let after = cursor.after.unwrap_or(first_after);
    if after < first_after || after > receipt.source_end_seq {
        return Err(ApiError {
            status: StatusCode::BAD_REQUEST,
            code: "invalid_compaction_cursor",
            message: "cursor must fall within the receipt source range".into(),
        });
    }
    let page_limit = cursor.limit.clamp(1, 100);
    let mut event_log = execlaw_core::EventLog::new(&state.db);
    let integrity_verified = state.event_log_hmac_key.is_some();
    if let Some(key) = &state.event_log_hmac_key {
        event_log = event_log.with_hmac_key((**key).clone());
    }
    let candidates = event_log
        .replay_since(
            &execlaw_core::ConversationId::from(conversation_id),
            execlaw_core::EventSeq(after),
        )
        .map_err(|error| ApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "compaction_evidence_error",
            message: error.to_string(),
        })?
        .into_iter()
        .filter(|event| event.seq.0 <= receipt.source_end_seq)
        .collect::<Vec<_>>();
    const MAX_EVIDENCE_PAGE_BYTES: usize = 1024 * 1024;
    let mut source_events = Vec::new();
    let mut page_bytes = 0usize;
    let mut has_more = false;
    for event in candidates {
        if source_events.len() >= page_limit {
            has_more = true;
            break;
        }
        let event_bytes = serde_json::to_vec(&event)
            .map_err(|error| ApiError {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                code: "compaction_evidence_error",
                message: error.to_string(),
            })?
            .len();
        if page_bytes.saturating_add(event_bytes) > MAX_EVIDENCE_PAGE_BYTES {
            if source_events.is_empty() {
                return Err(ApiError {
                    status: StatusCode::PAYLOAD_TOO_LARGE,
                    code: "compaction_event_too_large",
                    message: "a source event exceeds the evidence page byte limit".into(),
                });
            }
            has_more = true;
            break;
        }
        page_bytes += event_bytes;
        source_events.push(event);
    }
    let next_after = has_more
        .then(|| source_events.last().map(|event| event.seq.0))
        .flatten();
    Ok(Json(CompactionEvidenceResponse {
        receipt,
        source_events,
        next_after,
        integrity_verified,
    }))
}

#[derive(Debug, Deserialize)]
pub struct TraceCursor {
    #[serde(default)]
    pub after: i64,
    #[serde(default = "default_trace_limit")]
    pub limit: usize,
}

fn default_trace_limit() -> usize {
    100
}

#[derive(Debug, serde::Serialize)]
pub struct TracePage {
    pub run: RunInspectorSummary,
    pub asset_loadout: Option<execlaw_core::memory_assets::TurnAssetLoadoutReceipt>,
    pub steps: Vec<RunStepInspector>,
    pub children: Vec<RunInspectorSummary>,
    pub child_tasks: Vec<ChildTaskInspector>,
    pub retries: Vec<ToolRetryInspector>,
    pub delivery: Vec<execlaw_core::outbox::ConversationDeliveryEvent>,
    pub events: Vec<execlaw_core::harness::RunTraceEvent>,
    pub next_cursor: i64,
    pub cursor_reset: bool,
    pub recovery_action: Option<RecoveryActionView>,
}

#[derive(Debug, serde::Serialize)]
pub struct ChildTaskInspector {
    pub child_run_id: String,
    pub parent_run_id: String,
    pub task: serde_json::Value,
    pub task_hash: String,
    pub trust_ceiling: serde_json::Value,
    pub budget_tokens: u32,
    pub tokens_used: Option<u32>,
    pub budget_time_ms: u64,
    pub time_used_ms: Option<u64>,
    pub budget_retries: u32,
    pub retries_used: Option<u32>,
    pub budget_effects: u32,
    pub effects_used: Option<u32>,
    pub dependencies: Vec<String>,
    pub result_artifact_id: Option<String>,
    pub status: &'static str,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Deserialize)]
pub struct ChildTaskListQuery {
    #[serde(default = "default_child_task_limit")]
    pub limit: usize,
}

fn default_child_task_limit() -> usize {
    50
}

#[utoipa::path(
    get,
    path = "/api/admin/runs/child-tasks",
    params(("limit" = Option<usize>, Query, description = "Maximum child-task rows")),
    responses((status = 200, description = "Recent delegated child-task tree", body = [serde_json::Value])),
    tag = "runs"
)]
pub async fn list_child_tasks(
    State(state): State<AppState>,
    user: AuthedUser,
    Query(query): Query<ChildTaskListQuery>,
) -> Result<Json<Vec<ChildTaskInspector>>, ApiError> {
    controller(&user)?;
    let tasks = RunStore::new(&state.db)
        .list_recent_child_tasks(query.limit)
        .map_err(map)?
        .into_iter()
        .map(|task| ChildTaskInspector {
            child_run_id: task.child_run_id,
            parent_run_id: task.parent_run_id,
            task: task.task,
            task_hash: task.task_hash,
            trust_ceiling: task.trust_ceiling,
            budget_tokens: task.budget_tokens,
            tokens_used: task.tokens_used,
            budget_time_ms: task.budget_time_ms,
            time_used_ms: task.time_used_ms,
            budget_retries: task.budget_retries,
            retries_used: task.retries_used,
            budget_effects: task.budget_effects,
            effects_used: task.effects_used,
            dependencies: task.dependencies,
            result_artifact_id: task.result_artifact_id,
            status: task.status.as_str(),
            created_at: task.created_at,
            updated_at: task.updated_at,
        })
        .collect();
    Ok(Json(tasks))
}

#[utoipa::path(
    post,
    path = "/api/admin/runs/{run_id}/fork",
    params(("run_id" = String, Path, description = "Source run to fork")),
    responses((status = 201, description = "Fresh durable child run", body = serde_json::Value)),
    tag = "runs"
)]
pub async fn fork_run(
    State(state): State<AppState>,
    user: AuthedUser,
    Path(run_id): Path<String>,
) -> Result<(StatusCode, Json<RunInspectorSummary>), ApiError> {
    controller(&user)?;
    let run = RunStore::new(&state.db)
        .fork_run(&run_id, chrono::Utc::now().timestamp())
        .map_err(map)?;
    if let Err(error) =
        crate::workspace_coding::clone_workspace_for_run(&state, &run_id, &run.run_id)
    {
        let _ = RunStore::new(&state.db).cancel_run(&run.run_id, chrono::Utc::now().timestamp());
        return Err(error);
    }
    Ok((StatusCode::CREATED, Json(run_summary(run))))
}

#[derive(Debug, serde::Serialize)]
pub struct RecoveryActionView {
    pub action: &'static str,
    pub step: Option<RecoveryStepView>,
    pub cursor: Option<i64>,
}

#[derive(Debug, serde::Serialize)]
pub struct RecoveryStepView {
    pub step_id: String,
    pub kind: &'static str,
    pub status: &'static str,
    pub lease_expires_at: Option<i64>,
}

fn recovery_action_view(action: NextSafeAction) -> RecoveryActionView {
    fn step_view(step: execlaw_core::runs::RunStepRecord) -> RecoveryStepView {
        RecoveryStepView {
            step_id: step.step_id,
            kind: step.kind.as_str(),
            status: step.status.as_str(),
            lease_expires_at: step.lease_expires_at,
        }
    }
    match action {
        NextSafeAction::Claim(step) => RecoveryActionView {
            action: "claim",
            step: Some(step_view(step)),
            cursor: None,
        },
        NextSafeAction::ReclaimExpired(step) => RecoveryActionView {
            action: "reclaim_expired",
            step: Some(step_view(step)),
            cursor: None,
        },
        NextSafeAction::WaitForLease(step) => RecoveryActionView {
            action: "wait_for_lease",
            step: Some(step_view(step)),
            cursor: None,
        },
        NextSafeAction::WaitForApproval(step) => RecoveryActionView {
            action: "wait_for_approval",
            step: Some(step_view(step)),
            cursor: None,
        },
        NextSafeAction::Wait(step) => RecoveryActionView {
            action: "wait",
            step: Some(step_view(step)),
            cursor: None,
        },
        NextSafeAction::AdvanceCursor(step) => RecoveryActionView {
            action: "advance_cursor",
            step: Some(step_view(step)),
            cursor: None,
        },
        NextSafeAction::CompleteRun { cursor } => RecoveryActionView {
            action: "complete_run",
            step: None,
            cursor: Some(cursor),
        },
        NextSafeAction::RunCompleted => RecoveryActionView {
            action: "run_completed",
            step: None,
            cursor: None,
        },
        NextSafeAction::RunFailed => RecoveryActionView {
            action: "run_failed",
            step: None,
            cursor: None,
        },
        NextSafeAction::RunCancelled => RecoveryActionView {
            action: "run_cancelled",
            step: None,
            cursor: None,
        },
    }
}

#[derive(Debug, serde::Serialize)]
pub struct ToolRetryInspector {
    pub step_id: String,
    pub tool_name: String,
    pub attempts_used: u32,
    pub retry_budget_total: u32,
    pub next_retry_at_ms: Option<i64>,
    pub status: String,
    pub input_schema_hash: Option<String>,
    pub result_schema_hash: Option<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct RunStepInspector {
    pub step_id: String,
    pub ordinal: i64,
    pub kind: &'static str,
    pub status: &'static str,
    pub attempt: i64,
    pub input_hash: String,
    pub approval_id: Option<String>,
    pub started_at: Option<i64>,
    pub completed_at: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct RunListQuery {
    pub before_started_at: Option<i64>,
    pub before_run_id: Option<String>,
    #[serde(default = "default_run_list_limit")]
    pub limit: usize,
}

fn default_run_list_limit() -> usize {
    50
}

async fn list_runs(
    State(state): State<AppState>,
    user: AuthedUser,
    Query(query): Query<RunListQuery>,
) -> Result<Json<Vec<RunInspectorSummary>>, ApiError> {
    controller(&user)?;
    let runs = RunStore::new(&state.db)
        .list_recent(
            query.before_started_at,
            query.before_run_id.as_deref(),
            query.limit,
        )
        .map_err(map)?;
    Ok(Json(runs.into_iter().map(run_summary).collect()))
}

#[derive(Debug, serde::Serialize)]
pub struct RunInspectorSummary {
    pub run_id: String,
    pub conversation_id: String,
    pub parent_run_id: Option<String>,
    pub status: &'static str,
    pub cursor: i64,
    pub input_event_seq: i64,
    pub started_at: i64,
    pub updated_at: i64,
    pub deadline_at: Option<i64>,
}

async fn get_trace(
    State(state): State<AppState>,
    user: AuthedUser,
    Path(run_id): Path<String>,
    Query(cursor): Query<TraceCursor>,
) -> Result<Json<TracePage>, ApiError> {
    controller(&user)?;
    let run_store = RunStore::new(&state.db);
    let run = run_store.get_run(&run_id).map_err(map)?.ok_or_else(|| {
        map(execlaw_core::runs::RunStoreError::NotFound {
            entity: "run",
            id: run_id.clone(),
        })
    })?;
    let asset_loadout = execlaw_core::memory_assets::MemoryAssetStore::new(&state.db)
        .turn_loadout(run.conversation_id.as_str(), run.input_event_seq.0)
        .map_err(|error| ApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "run_memory_loadout_error",
            message: error.to_string(),
        })?;
    let recovery_action = matches!(
        run.status,
        RunStatus::Pending | RunStatus::Running | RunStatus::Waiting
    )
    .then(|| run_store.next_safe_action(&run_id, chrono::Utc::now().timestamp()))
    .transpose()
    .map_err(map)?
    .map(recovery_action_view);
    let harness_store = execlaw_core::harness::HarnessStore::new(&state.db);
    let cursor_reset = harness_store
        .trace_cursor_requires_reload(&run_id, cursor.after)
        .map_err(|error| ApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "run_trace_error",
            message: error.to_string(),
        })?;
    let after = if cursor_reset { 0 } else { cursor.after.max(0) };
    let events = harness_store
        .trace_events_after(&run_id, after, cursor.limit)
        .map_err(|error| ApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "run_trace_error",
            message: error.to_string(),
        })?;
    let steps = RunStore::new(&state.db)
        .list_steps(&run_id)
        .map_err(map)?
        .into_iter()
        .map(|step| RunStepInspector {
            step_id: step.step_id,
            ordinal: step.ordinal,
            kind: step.kind.as_str(),
            status: step.status.as_str(),
            attempt: step.attempt,
            input_hash: step.input_hash,
            approval_id: step.approval_id,
            started_at: step.started_at,
            completed_at: step.completed_at,
        })
        .collect();
    let children = RunStore::new(&state.db)
        .list_children(&run_id)
        .map_err(map)?
        .into_iter()
        .map(run_summary)
        .collect();
    let child_tasks = RunStore::new(&state.db)
        .list_child_tasks(&run_id)
        .map_err(map)?
        .into_iter()
        .map(|task| ChildTaskInspector {
            child_run_id: task.child_run_id,
            parent_run_id: task.parent_run_id,
            task: task.task,
            task_hash: task.task_hash,
            trust_ceiling: task.trust_ceiling,
            budget_tokens: task.budget_tokens,
            tokens_used: task.tokens_used,
            budget_time_ms: task.budget_time_ms,
            time_used_ms: task.time_used_ms,
            budget_retries: task.budget_retries,
            retries_used: task.retries_used,
            budget_effects: task.budget_effects,
            effects_used: task.effects_used,
            dependencies: task.dependencies,
            result_artifact_id: task.result_artifact_id,
            status: task.status.as_str(),
            created_at: task.created_at,
            updated_at: task.updated_at,
        })
        .collect();
    let retries = execlaw_core::tool_execution::ToolExecutionStore::new(&state.db)
        .list_run_invocations(&run_id)
        .map_err(|error| ApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "run_retry_trace_error",
            message: error.to_string(),
        })?
        .into_iter()
        .map(|invocation| ToolRetryInspector {
            step_id: invocation.step_id,
            tool_name: invocation.tool_name,
            attempts_used: invocation.attempts_used,
            retry_budget_total: invocation.retry_budget_total,
            next_retry_at_ms: invocation.next_retry_at_ms,
            status: invocation.status,
            input_schema_hash: invocation.input_schema_hash,
            result_schema_hash: invocation.result_schema_hash,
        })
        .collect();
    let delivery = execlaw_core::outbox::OutboxStore::new(&state.db)
        .run_delivery_timeline(&run_id, 500)
        .map_err(|error| ApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "run_delivery_trace_error",
            message: error.to_string(),
        })?;
    let next_cursor = events.last().map(|event| event.cursor).unwrap_or(after);
    Ok(Json(TracePage {
        run: run_summary(run),
        asset_loadout,
        steps,
        children,
        child_tasks,
        retries,
        delivery,
        events,
        next_cursor,
        cursor_reset,
        recovery_action,
    }))
}

fn run_summary(run: RunRecord) -> RunInspectorSummary {
    RunInspectorSummary {
        run_id: run.run_id,
        conversation_id: run.conversation_id.to_string(),
        parent_run_id: run.parent_run_id,
        status: run.status.as_str(),
        cursor: run.cursor,
        input_event_seq: run.input_event_seq.0,
        started_at: run.started_at,
        updated_at: run.updated_at,
        deadline_at: run.deadline_at,
    }
}

fn controller(user: &AuthedUser) -> Result<(), ApiError> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request};
    use execlaw_core::{
        ids::{ConversationId, EventSeq, IdempotencyKey, TurnSeq},
        outbox::{OutboxRow, OutboxStatus, OutboxStore},
        users::{UserRole, UserRow, UserStore},
    };
    use tower::ServiceExt;

    fn controller_token(state: &AppState, user_id: &str, role: UserRole) -> String {
        let now = chrono::Utc::now().timestamp();
        UserStore::new(&state.db)
            .insert(&UserRow {
                user_id: user_id.into(),
                username: user_id.into(),
                display_name: user_id.into(),
                email: None,
                password_hash: "unused-test-hash".into(),
                role,
                created_at: now,
                last_login_at: None,
            })
            .unwrap();
        let session_id = format!("{user_id}-session");
        state
            .refresh_store
            .issue(user_id, &session_id, 3600)
            .unwrap();
        state
            .signer
            .issue_access_token(user_id, &session_id, 3600)
            .unwrap()
    }

    fn make_unknown_effect(state: &AppState, ord: u32) -> i64 {
        let conversation_id = ConversationId::from("outbox-resolution-test");
        let store = OutboxStore::new(&state.db);
        let id = store
            .enqueue(&OutboxRow {
                id: None,
                idempotency_key: IdempotencyKey::mint(&conversation_id, TurnSeq(1), ord),
                conversation_id,
                effect_kind: "transport.send".into(),
                payload: vec![0x01, 0x02],
                status: OutboxStatus::Pending,
                attempts: 0,
                next_attempt_at: None,
                last_error: None,
                enqueued_seq: EventSeq(1),
            })
            .unwrap();
        assert!(store.claim_with_lease(id, "test-relay", 100, 30).unwrap());
        store
            .mark_unknown(id, "remote acknowledgement was lost")
            .unwrap();
        id
    }

    #[tokio::test]
    async fn controller_can_resolve_unknown_outbox_and_resolution_records_actor() {
        let state = crate::routes::test_app_state();
        let controller = controller_token(&state, "outbox-controller", UserRole::Controller);
        let operator = controller_token(&state, "outbox-operator", UserRole::Operator);
        let id = make_unknown_effect(&state, 1);
        let app = crate::routes::build_router(state.clone());

        let request = Request::builder()
            .method("GET")
            .uri("/api/admin/outbox/unknown")
            .header("authorization", format!("Bearer {controller}"))
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let list: serde_json::Value = serde_json::from_slice(
            &axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(list.as_array().unwrap().len(), 1);
        assert_eq!(list[0]["id"], id);
        assert!(list[0].get("payload").is_none());

        let request = Request::builder()
            .method("POST")
            .uri(format!("/api/admin/outbox/{id}/resolution"))
            .header("authorization", format!("Bearer {controller}"))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({
                    "resolution":"authorize_retry",
                    "evidence_ref":"operator verified the remote status"
                })
                .to_string(),
            ))
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let timeline = OutboxStore::new(&state.db).delivery_timeline(id).unwrap();
        assert_eq!(timeline.last().unwrap().transition, "retry_authorized");
        assert_eq!(
            timeline.last().unwrap().actor.as_deref(),
            Some("outbox-controller")
        );

        let confirmed_id = make_unknown_effect(&state, 3);
        let request = Request::builder()
            .method("POST")
            .uri(format!("/api/admin/outbox/{confirmed_id}/resolution"))
            .header("authorization", format!("Bearer {controller}"))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({
                    "resolution":"confirm_delivered",
                    "evidence_ref":"sink-receipt-verified-3"
                })
                .to_string(),
            ))
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let timeline = OutboxStore::new(&state.db)
            .delivery_timeline(confirmed_id)
            .unwrap();
        assert_eq!(
            timeline.last().unwrap().transition,
            "operator_confirmed_delivered"
        );
        assert_eq!(
            timeline.last().unwrap().actor.as_deref(),
            Some("outbox-controller")
        );

        let denied_id = make_unknown_effect(&state, 2);
        let request = Request::builder()
            .method("POST")
            .uri(format!("/api/admin/outbox/{denied_id}/resolution"))
            .header("authorization", format!("Bearer {operator}"))
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({
                    "resolution":"authorize_retry",
                    "evidence_ref":"operator attempted retry"
                })
                .to_string(),
            ))
            .unwrap();
        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            OutboxStore::new(&state.db)
                .unknown_effect(denied_id)
                .unwrap()
                .unwrap()
                .id,
            denied_id
        );
    }

    #[tokio::test]
    async fn run_trace_returns_metadata_only_asset_loadout_receipt() {
        let state = crate::routes::test_app_state();
        let token = controller_token(&state, "loadout-trace-controller", UserRole::Controller);
        let conversation_id = ConversationId::from("loadout-trace");
        crate::chats::ensure_conversation_for(&state.db, &conversation_id);
        state
            .db
            .with_conn(|connection| {
                connection.execute(
                    "INSERT INTO state_events \
                     (conversation_id, seq, kind, payload, committed_at, actor) \
                     VALUES (?1, 1, 'user_msg', X'00', 100, 'controller')",
                    [conversation_id.as_str()],
                )?;
                Ok(())
            })
            .unwrap();
        let run_id = execlaw_core::runs::RunStore::new(&state.db)
            .create_run(&execlaw_core::runs::NewRun {
                conversation_id: conversation_id.clone(),
                parent_run_id: None,
                input_event_seq: EventSeq(1),
                started_at: 100,
                deadline_at: None,
            })
            .unwrap();
        let receipt = execlaw_core::memory_assets::TurnAssetLoadoutReceipt {
            agent_scope: "default".into(),
            conversation_trust_class: "Controller".into(),
            readable_trust_classes: vec!["Controller".into()],
            readable_owner_scopes: vec!["global".into(), "controller".into()],
            resolved_at: 101,
            retrieval_query_sha256: None,
            assets: vec![execlaw_core::memory_assets::TurnAssetLoadoutEntry {
                asset_id: "asset-1".into(),
                name: "Release notes".into(),
                asset_type: execlaw_core::memory_assets::AssetType::Memory,
                version: 2,
                source_hash: Some("sha256:source".into()),
                owner_scope: "controller".into(),
                visibility: execlaw_core::memory_assets::AssetVisibility::Private,
                trust_floor: "Controller".into(),
                status: "active".into(),
                expires_at: None,
                binding_agent_scope: "default".into(),
                binding_mode: execlaw_core::memory_assets::InjectionMode::Hot,
                binding_priority: 80,
                binding_max_chars: 240,
                injected_chars: 128,
                admission_reasons: vec!["trust_floor_readable".into()],
            }],
            retrieved_assets: Vec::new(),
        };
        execlaw_core::memory_assets::MemoryAssetStore::new(&state.db)
            .record_turn_loadout(conversation_id.as_str(), 1, &receipt)
            .unwrap();

        let request = Request::builder()
            .method("GET")
            .uri(format!("/api/admin/runs/{run_id}/trace?after=0&limit=10"))
            .header("authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();
        let response = crate::routes::build_router(state)
            .oneshot(request)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let value: serde_json::Value = serde_json::from_slice(
            &axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(value["asset_loadout"]["assets"][0]["asset_id"], "asset-1");
        assert_eq!(value["asset_loadout"]["assets"][0]["injected_chars"], 128);
        assert!(
            value["asset_loadout"]["assets"][0]
                .get("content_ref")
                .is_none()
        );
    }
}

fn map(error: execlaw_core::runs::RunStoreError) -> ApiError {
    let not_found = matches!(&error, execlaw_core::runs::RunStoreError::NotFound { .. });
    ApiError {
        status: if not_found {
            StatusCode::NOT_FOUND
        } else {
            StatusCode::BAD_REQUEST
        },
        code: if not_found {
            "run_completion_not_found"
        } else {
            "run_completion_error"
        },
        message: error.to_string(),
    }
}

async fn get_completion(
    State(state): State<AppState>,
    user: AuthedUser,
    Path(run_id): Path<String>,
) -> Result<Json<RunCompletionReport>, ApiError> {
    controller(&user)?;
    RunStore::new(&state.db)
        .completion_report(&run_id)
        .map_err(map)?
        .map(Json)
        .ok_or_else(|| {
            map(execlaw_core::runs::RunStoreError::NotFound {
                entity: "completion contract",
                id: run_id,
            })
        })
}

async fn set_completion_contract(
    State(state): State<AppState>,
    user: AuthedUser,
    Path(run_id): Path<String>,
    Json(request): Json<CompletionContractRequest>,
) -> Result<Json<RunCompletionReport>, ApiError> {
    controller(&user)?;
    let store = RunStore::new(&state.db);
    store
        .set_completion_contract(&RunCompletionContract {
            run_id: run_id.clone(),
            acceptance_criteria: request.acceptance_criteria,
            required_artifacts: request.required_artifacts,
            delivery_required: request.delivery_required,
            created_at: chrono::Utc::now().timestamp(),
        })
        .map_err(map)?;
    store
        .completion_report(&run_id)
        .map_err(map)?
        .map(Json)
        .ok_or_else(|| ApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "run_completion_missing",
            message: "completion contract disappeared after write".into(),
        })
}

async fn record_verification(
    State(state): State<AppState>,
    user: AuthedUser,
    Path(run_id): Path<String>,
    Json(request): Json<VerificationRequest>,
) -> Result<Json<RunCompletionReport>, ApiError> {
    controller(&user)?;
    let store = RunStore::new(&state.db);
    let mut evidence_refs = request.evidence_refs;
    if request.status == VerificationStatus::Passed {
        validate_reference_list(&evidence_refs).map_err(map)?;
        if evidence_refs.is_empty() {
            return Err(map(RunStoreError::Corrupt(
                "a passing manual result needs submitted evidence".into(),
            )));
        }
        let audit = serde_json::json!({
            "status":"passed",
            "submitted_evidence_refs":&evidence_refs,
            "detail":&request.detail,
        });
        let id = AuditStore::new(&state.db)
            .insert(
                &user.user_id,
                "run_completion_verification",
                &format!("{run_id}/{}", request.criterion_id),
                None,
                Some(&audit),
            )
            .map_err(|error| map(RunStoreError::Db(error)))?;
        evidence_refs.push(format!("attestation:{id}"));
    }
    store
        .record_completion_verification(
            &run_id,
            &CriterionVerification {
                criterion_id: request.criterion_id,
                status: request.status,
                evidence_refs,
                detail: request.detail,
                verified_at: chrono::Utc::now().timestamp(),
            },
        )
        .map_err(map)?;
    get_report(&store, &run_id)
}

async fn record_artifact_check(
    State(state): State<AppState>,
    user: AuthedUser,
    Path(run_id): Path<String>,
    Json(request): Json<ArtifactCheckRequest>,
) -> Result<Json<RunCompletionReport>, ApiError> {
    controller(&user)?;
    let store = RunStore::new(&state.db);
    store
        .record_artifact_verification(
            &run_id,
            &ArtifactVerification {
                artifact_id: request.artifact_id,
                present: request.present,
                evidence_ref: request.evidence_ref,
                detail: request.detail,
                checked_at: chrono::Utc::now().timestamp(),
            },
        )
        .map_err(map)?;
    get_report(&store, &run_id)
}

async fn confirm_delivery(
    State(state): State<AppState>,
    user: AuthedUser,
    Path(run_id): Path<String>,
    Json(request): Json<DeliveryConfirmationRequest>,
) -> Result<Json<RunCompletionReport>, ApiError> {
    controller(&user)?;
    let store = RunStore::new(&state.db);
    let evidence_ref = if request.evidence_ref.starts_with("outbox:") {
        request.evidence_ref
    } else {
        let submitted = request.evidence_ref.trim();
        if submitted.is_empty() || submitted.len() > 512 {
            return Err(map(RunStoreError::Corrupt(
                "delivery confirmation needs bounded submitted evidence".into(),
            )));
        }
        let audit = serde_json::json!({"status":"confirmed","submitted_evidence_ref":submitted});
        let id = AuditStore::new(&state.db)
            .insert(
                &user.user_id,
                "run_completion_delivery",
                &run_id,
                None,
                Some(&audit),
            )
            .map_err(|error| map(RunStoreError::Db(error)))?;
        format!("attestation:{id}")
    };
    store
        .confirm_run_delivery(&run_id, &evidence_ref, chrono::Utc::now().timestamp())
        .map_err(map)?;
    get_report(&store, &run_id)
}

fn get_report(store: &RunStore<'_>, run_id: &str) -> Result<Json<RunCompletionReport>, ApiError> {
    store
        .completion_report(run_id)
        .map_err(map)?
        .map(Json)
        .ok_or_else(|| {
            map(execlaw_core::runs::RunStoreError::NotFound {
                entity: "completion contract",
                id: run_id.to_owned(),
            })
        })
}
