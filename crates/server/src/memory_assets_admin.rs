use crate::auth_extract::AuthedUser;
use crate::state::AppState;
use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::{
    Router,
    routing::{get, put},
};
use execlaw_core::memory_assertions::{
    MemoryAssertionError, MemoryAssertionEvidenceView, MemoryAssertionReviewRecord,
    MemoryAssertionStore, MemoryEvidenceRecord,
};
use execlaw_core::memory_assets::{
    AssetBinding, InjectionMode, MemoryAsset, MemoryAssetStore, MemoryRetrievalConfig,
};
use execlaw_core::users::UserRole;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

fn controller(user: &AuthedUser) -> Result<(), axum::response::Response> {
    if user.role == UserRole::Controller {
        Ok(())
    } else {
        Err((
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"error": "controller only"})),
        )
            .into_response())
    }
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct BindAssetRequest {
    pub agent_scope: String,
    pub injection_mode: String,
    pub priority: i64,
    pub max_chars: i64,
}

#[derive(Debug, Deserialize)]
pub struct AgentScopeQuery {
    pub agent_scope: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct MemoryRetrievalConfigRequest {
    pub embedding_model_id: String,
    #[serde(default = "default_memory_reranker_version")]
    pub reranker_version: String,
}

fn default_memory_reranker_version() -> String {
    execlaw_core::memory_assets::MEMORY_RERANKER_VERSION.to_owned()
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct EmbeddingRebuildRequest {
    #[serde(default = "default_embedding_rebuild_batch")]
    pub limit: u32,
}

fn default_embedding_rebuild_batch() -> u32 {
    32
}

#[derive(Debug, serde::Serialize, ToSchema)]
pub struct AgentScopeView {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct MemoryRetrievalConfigView {
    pub embedding_model_id: String,
    pub reranker_version: String,
    pub updated_at: i64,
}

impl From<MemoryRetrievalConfig> for MemoryRetrievalConfigView {
    fn from(config: MemoryRetrievalConfig) -> Self {
        Self {
            embedding_model_id: config.embedding_model_id,
            reranker_version: config.reranker_version,
            updated_at: config.updated_at,
        }
    }
}

#[derive(Debug, serde::Serialize, ToSchema)]
pub struct MemoryAssetAdminView {
    pub asset_id: String,
    pub asset_type: String,
    pub name: String,
    pub description: String,
    pub owner_scope: String,
    pub visibility: String,
    pub trust_floor: String,
    pub status: String,
    pub version: i64,
    pub source_hash: Option<String>,
    pub expires_at: Option<i64>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct MemoryEvidenceAdminView {
    pub evidence_id: String,
    pub conversation_id: String,
    pub event_seq: i64,
    pub payload_path: String,
    pub quote_hash: String,
    pub evidence_kind: String,
    pub created_at: i64,
}

impl From<MemoryEvidenceRecord> for MemoryEvidenceAdminView {
    fn from(record: MemoryEvidenceRecord) -> Self {
        Self {
            evidence_id: record.evidence_id,
            conversation_id: record.conversation_id,
            event_seq: record.event_seq,
            payload_path: record.payload_path,
            quote_hash: record.quote_hash,
            evidence_kind: record.evidence_kind,
            created_at: record.created_at,
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct MemoryAssertionReviewAdminView {
    pub decision: String,
    pub conversation_id: String,
    pub event_seq: i64,
    pub reviewer_id: String,
    pub reason: String,
    pub created_at: i64,
}

impl From<MemoryAssertionReviewRecord> for MemoryAssertionReviewAdminView {
    fn from(record: MemoryAssertionReviewRecord) -> Self {
        Self {
            decision: record.decision,
            conversation_id: record.conversation_id,
            event_seq: record.event_seq,
            reviewer_id: record.reviewer_id,
            reason: record.reason,
            created_at: record.created_at,
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct MemoryAssertionAdminView {
    pub assertion_id: String,
    pub scope: String,
    pub trust_class: String,
    pub kind: String,
    pub subject: String,
    pub predicate: String,
    pub object: serde_json::Value,
    pub confidence: f64,
    pub status: String,
    pub observed_from: i64,
    pub observed_to: Option<i64>,
    pub valid_from: i64,
    pub valid_to: Option<i64>,
    pub supersedes_id: Option<String>,
    pub extraction_run_id: String,
    pub created_event_seq: i64,
    pub created_at: i64,
    pub evidence: Vec<MemoryEvidenceAdminView>,
    pub evidence_total: usize,
    pub review: Option<MemoryAssertionReviewAdminView>,
}

impl From<MemoryAssertionEvidenceView> for MemoryAssertionAdminView {
    fn from(view: MemoryAssertionEvidenceView) -> Self {
        let assertion = view.assertion;
        let label =
            |value: serde_json::Value| value.as_str().map(str::to_owned).unwrap_or_default();
        Self {
            assertion_id: assertion.assertion_id,
            scope: assertion.scope,
            trust_class: assertion.trust_class,
            kind: label(serde_json::to_value(assertion.kind).unwrap_or_default()),
            subject: assertion.subject,
            predicate: assertion.predicate,
            object: assertion.object,
            confidence: assertion.confidence,
            status: label(serde_json::to_value(assertion.status).unwrap_or_default()),
            observed_from: assertion.observed_from,
            observed_to: assertion.observed_to,
            valid_from: assertion.valid_from,
            valid_to: assertion.valid_to,
            supersedes_id: assertion.supersedes_id,
            extraction_run_id: assertion.extraction_run_id,
            created_event_seq: assertion.created_event_seq.0,
            created_at: assertion.created_at,
            evidence: view.evidence.into_iter().map(Into::into).collect(),
            evidence_total: view.evidence_total,
            review: view.review.map(Into::into),
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct MemoryEvidenceSourceView {
    pub evidence_id: String,
    pub conversation_id: String,
    pub event_seq: i64,
    pub payload_path: String,
    pub quote_hash: String,
    pub evidence_kind: String,
    pub source_quote: String,
    pub source_quote_bytes: usize,
    pub truncated: bool,
    pub integrity_verified: bool,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct RetractMemoryAssertionRequest {
    pub reason: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct ForgetMemorySourceRequest {
    pub conversation_id: String,
    pub event_seq: i64,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CorrectMemoryAssertionRequest {
    pub replacement: serde_json::Value,
    pub reason: String,
}

impl From<MemoryAsset> for MemoryAssetAdminView {
    fn from(asset: MemoryAsset) -> Self {
        fn serialized_label<T: serde::Serialize>(value: T) -> String {
            serde_json::to_value(value)
                .ok()
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_default()
        }
        Self {
            asset_id: asset.asset_id,
            asset_type: serialized_label(asset.asset_type),
            name: asset.name,
            description: asset.description,
            owner_scope: asset.owner_scope,
            visibility: serialized_label(asset.visibility),
            trust_floor: asset.trust_floor,
            status: asset.status,
            version: asset.version,
            source_hash: asset.source_hash,
            expires_at: asset.expires_at,
        }
    }
}

#[derive(Debug, serde::Serialize, ToSchema)]
pub struct MemoryAssetBindingView {
    pub asset_id: String,
    pub agent_scope: String,
    pub injection_mode: String,
    pub priority: i64,
    pub max_chars: i64,
    pub created_at: i64,
}

impl From<AssetBinding> for MemoryAssetBindingView {
    fn from(binding: AssetBinding) -> Self {
        Self {
            asset_id: binding.asset_id,
            agent_scope: binding.agent_scope,
            injection_mode: match binding.injection_mode {
                InjectionMode::Hot => "hot",
                InjectionMode::Discoverable => "discoverable",
                InjectionMode::ToolOnly => "tool_only",
            }
            .to_owned(),
            priority: binding.priority,
            max_chars: binding.max_chars,
            created_at: binding.created_at,
        }
    }
}

#[derive(Debug, serde::Serialize, ToSchema)]
pub struct MemoryAssetsAdminResponse {
    pub assets: Vec<MemoryAssetAdminView>,
    pub bindings: Vec<MemoryAssetBindingView>,
    pub agent_scopes: Vec<AgentScopeView>,
    pub assertions: Vec<MemoryAssertionAdminView>,
}

#[utoipa::path(
    get,
    path = "/api/admin/memory-assets",
    params(("agent_scope" = Option<String>, Query, description = "default or a configured agent id")),
    responses((status = 200, description = "Registered asset metadata and default-agent bindings", body = MemoryAssetsAdminResponse), (status = 403, description = "Controller role required")),
    security(("bearer_jwt" = [])),
    tag = "memory"
)]
pub async fn list(
    State(state): State<AppState>,
    user: AuthedUser,
    Query(query): Query<AgentScopeQuery>,
) -> impl IntoResponse {
    if let Err(response) = controller(&user) {
        return response;
    }
    let agent_scope = query.agent_scope.as_deref().unwrap_or("default");
    if !agent_scope_exists(&state, agent_scope) {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "agent scope not found"})),
        )
            .into_response();
    }
    let store = MemoryAssetStore::new(&state.db);
    let assets = match store.list(200) {
        Ok(assets) => assets.into_iter().map(MemoryAssetAdminView::from).collect(),
        Err(error) => return failure(error),
    };
    let bindings = match store.list_loadout(agent_scope, 200) {
        Ok(bindings) => bindings
            .into_iter()
            .map(MemoryAssetBindingView::from)
            .collect(),
        Err(error) => return failure(error),
    };
    let mut agent_scopes = vec![AgentScopeView {
        id: "default".into(),
        name: "Default chat agent".into(),
    }];
    agent_scopes.push(AgentScopeView {
        id: "research".into(),
        name: "Deep research".into(),
    });
    match execlaw_core::agents::AgentStore::new(&state.db).list() {
        Ok(agents) => agent_scopes.extend(agents.into_iter().map(|agent| AgentScopeView {
            id: agent.id,
            name: agent.name,
        })),
        Err(error) => return failure(error),
    }
    match execlaw_core::routines::RoutineStore::new(&state.db).list_all() {
        Ok(routines) => agent_scopes.extend(routines.into_iter().map(|routine| AgentScopeView {
            id: format!("routine:{}", routine.id),
            name: format!("Routine: {}", routine.name),
        })),
        Err(error) => return failure(error),
    }
    let assertions = match MemoryAssertionStore::new(&state.db).list_recent_with_evidence(100) {
        Ok(assertions) => assertions.into_iter().map(Into::into).collect(),
        Err(error) => return failure(error),
    };
    (
        StatusCode::OK,
        Json(MemoryAssetsAdminResponse {
            assets,
            bindings,
            agent_scopes,
            assertions,
        }),
    )
        .into_response()
}

#[utoipa::path(
    put,
    path = "/api/admin/memory-assets/{asset_id}/binding",
    params(("asset_id" = String, Path, description = "Asset to bind")),
    request_body = BindAssetRequest,
    responses((status = 200, description = "Asset binding saved"), (status = 400, description = "Invalid binding"), (status = 403, description = "Controller role required"), (status = 404, description = "Asset not found")),
    security(("bearer_jwt" = [])),
    tag = "memory"
)]
pub async fn bind(
    State(state): State<AppState>,
    user: AuthedUser,
    Path(asset_id): Path<String>,
    Json(request): Json<BindAssetRequest>,
) -> impl IntoResponse {
    if let Err(response) = controller(&user) {
        return response;
    }
    if !agent_scope_exists(&state, &request.agent_scope)
        || request.max_chars <= 0
        || request.max_chars > 2048
        || !(-1000..=1000).contains(&request.priority)
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "invalid memory asset binding"})),
        )
            .into_response();
    }
    let injection_mode = match request.injection_mode.as_str() {
        "hot" => InjectionMode::Hot,
        "discoverable" => InjectionMode::Discoverable,
        "tool_only" => InjectionMode::ToolOnly,
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "invalid injection mode"})),
            )
                .into_response();
        }
    };
    match MemoryAssetStore::new(&state.db).bind(
        &asset_id,
        &request.agent_scope,
        injection_mode,
        request.priority,
        request.max_chars,
        chrono::Utc::now().timestamp(),
    ) {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({"bound": true}))).into_response(),
        Err(execlaw_core::memory_assets::MemoryAssetError::NotFound(_)) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "memory asset not found"})),
        )
            .into_response(),
        Err(error) => failure(error),
    }
}

#[utoipa::path(
    delete,
    path = "/api/admin/memory-assets/{asset_id}/binding",
    params(("asset_id" = String, Path, description = "Asset to unbind")),
    responses((status = 200, description = "Asset binding removed"), (status = 403, description = "Controller role required")),
    security(("bearer_jwt" = [])),
    tag = "memory"
)]
pub async fn unbind(
    State(state): State<AppState>,
    user: AuthedUser,
    Path(asset_id): Path<String>,
    Query(query): Query<AgentScopeQuery>,
) -> impl IntoResponse {
    if let Err(response) = controller(&user) {
        return response;
    }
    let agent_scope = query.agent_scope.as_deref().unwrap_or("default");
    if !agent_scope_exists(&state, agent_scope) {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "agent scope not found"})),
        )
            .into_response();
    }
    match MemoryAssetStore::new(&state.db).unbind(&asset_id, agent_scope) {
        Ok(removed) => (
            StatusCode::OK,
            Json(serde_json::json!({"unbound": removed})),
        )
            .into_response(),
        Err(error) => failure(error),
    }
}

fn agent_scope_exists(state: &AppState, agent_scope: &str) -> bool {
    if matches!(agent_scope, "default" | "research") {
        return true;
    }
    if let Some(routine_id) = agent_scope.strip_prefix("routine:") {
        return execlaw_core::routines::RoutineStore::new(&state.db)
            .get(routine_id)
            .ok()
            .flatten()
            .is_some();
    }
    execlaw_core::agents::AgentStore::new(&state.db)
        .get(agent_scope)
        .ok()
        .flatten()
        .is_some()
}

fn failure(error: impl std::fmt::Display) -> axum::response::Response {
    tracing::warn!(%error, "memory asset binding request failed");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(serde_json::json!({"error": "memory asset operation failed"})),
    )
        .into_response()
}

fn evidence_source_quote(
    state: &AppState,
    evidence: &MemoryEvidenceRecord,
) -> Result<(String, bool), axum::response::Response> {
    use execlaw_core::events::EventLog;
    use execlaw_core::ids::{ConversationId, EventSeq};
    use sha2::{Digest, Sha256};

    let conversation = ConversationId::from(evidence.conversation_id.as_str());
    let mut log = EventLog::new(&state.db);
    let integrity_verified = state.event_log_hmac_key.is_some();
    if let Some(key) = state.event_log_hmac_key.as_ref() {
        log = log.with_hmac_key((**key).clone());
    }
    let events = log
        .replay_since(&conversation, EventSeq(evidence.event_seq.saturating_sub(1)))
        .map_err(|error| {
            tracing::warn!(conversation_id = %conversation, %error, "memory source event integrity check failed");
            (StatusCode::CONFLICT, Json(serde_json::json!({"error": "source event integrity failed"}))).into_response()
        })?;
    let event = events
        .iter()
        .find(|event| event.seq.0 == evidence.event_seq)
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": "source event not found"})),
            )
                .into_response()
        })?;
    let payload: serde_json::Value = event.decode_payload().map_err(|_| {
        (
            StatusCode::CONFLICT,
            Json(serde_json::json!({"error": "source payload could not be verified"})),
        )
            .into_response()
    })?;
    let quote = execlaw_core::memory_assertions::evidence_quote(&payload, &evidence.payload_path)
        .ok_or_else(|| {
        (
            StatusCode::CONFLICT,
            Json(serde_json::json!({"error": "source span was not found"})),
        )
            .into_response()
    })?;
    if hex::encode(Sha256::digest(quote.as_bytes())) != evidence.quote_hash {
        return Err((
            StatusCode::CONFLICT,
            Json(serde_json::json!({"error": "source quote hash mismatch"})),
        )
            .into_response());
    }
    Ok((quote, integrity_verified))
}

fn review_log(state: &AppState) -> execlaw_core::events::EventLog<'_> {
    let mut log = execlaw_core::events::EventLog::new(&state.db);
    if let Some(key) = state.event_log_hmac_key.as_ref() {
        log = log.with_hmac_key((**key).clone());
    }
    log
}

fn review_source(
    state: &AppState,
    assertion_id: &str,
) -> Result<MemoryEvidenceRecord, axum::response::Response> {
    let store = MemoryAssertionStore::new(&state.db);
    let evidence = store
        .first_evidence(assertion_id)
        .map_err(failure)?
        .ok_or_else(|| {
            (
                StatusCode::CONFLICT,
                Json(serde_json::json!({"error": "assertion has no source evidence"})),
            )
                .into_response()
        })?;
    evidence_source_quote(state, &evidence)?;
    Ok(evidence)
}

pub async fn delete_asset(
    State(state): State<AppState>,
    user: AuthedUser,
    Path(asset_id): Path<String>,
) -> impl IntoResponse {
    if let Err(response) = controller(&user) {
        return response;
    }
    match MemoryAssetStore::new(&state.db).delete(
        &asset_id,
        &user.user_id,
        chrono::Utc::now().timestamp(),
    ) {
        Ok(deleted) => (
            if deleted {
                StatusCode::OK
            } else {
                StatusCode::NOT_FOUND
            },
            Json(serde_json::json!({"asset_id": asset_id, "deleted": deleted})),
        )
            .into_response(),
        Err(error) => failure(error),
    }
}

#[utoipa::path(
    post,
    path = "/api/admin/memory-assertions/forget-source",
    request_body = ForgetMemorySourceRequest,
    responses(
        (status = 200, description = "Memory assertions and evidence derived from the source event were hidden"),
        (status = 400, description = "Conversation ID and positive event sequence are required"),
        (status = 403, description = "Controller role required"),
        (status = 404, description = "Source event not found")
    ),
    security(("bearer_jwt" = [])),
    tag = "memory"
)]
pub async fn forget_memory_source(
    State(state): State<AppState>,
    user: AuthedUser,
    Json(request): Json<ForgetMemorySourceRequest>,
) -> impl IntoResponse {
    if let Err(response) = controller(&user) {
        return response;
    }
    if request.conversation_id.trim().is_empty() || request.event_seq <= 0 {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error":"conversation_id and a positive event_seq are required"})),
        )
            .into_response();
    }
    match MemoryAssertionStore::new(&state.db).tombstone_source_event(
        &request.conversation_id,
        request.event_seq,
        &user.user_id,
        chrono::Utc::now().timestamp(),
    ) {
        Ok(report) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "conversation_id": request.conversation_id,
                "event_seq": request.event_seq,
                "assertions_hidden": report.assertions_hidden,
                "evidence_hidden": report.evidence_hidden,
            })),
        )
            .into_response(),
        Err(MemoryAssertionError::NotFound(_)) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error":"source event not found"})),
        )
            .into_response(),
        Err(error) => failure(error),
    }
}

pub async fn evidence_source(
    State(state): State<AppState>,
    user: AuthedUser,
    Path((assertion_id, evidence_id)): Path<(String, String)>,
) -> impl IntoResponse {
    if let Err(response) = controller(&user) {
        return response;
    }
    let evidence =
        match MemoryAssertionStore::new(&state.db).evidence_by_id(&assertion_id, &evidence_id) {
            Ok(Some(evidence)) => evidence,
            Ok(None) => {
                return (
                    StatusCode::NOT_FOUND,
                    Json(serde_json::json!({"error": "evidence not found"})),
                )
                    .into_response();
            }
            Err(error) => return failure(error),
        };
    let (quote, integrity_verified) = match evidence_source_quote(&state, &evidence) {
        Ok(verified) => verified,
        Err(response) => return response,
    };
    let bytes = quote.len();
    let mut end = bytes.min(2_048);
    while !quote.is_char_boundary(end) {
        end -= 1;
    }
    (
        StatusCode::OK,
        Json(MemoryEvidenceSourceView {
            evidence_id: evidence.evidence_id,
            conversation_id: evidence.conversation_id,
            event_seq: evidence.event_seq,
            payload_path: evidence.payload_path,
            quote_hash: evidence.quote_hash,
            evidence_kind: evidence.evidence_kind,
            source_quote: quote[..end].to_owned(),
            source_quote_bytes: bytes,
            truncated: end < bytes,
            integrity_verified,
        }),
    )
        .into_response()
}

#[utoipa::path(
    post,
    path = "/api/admin/memory-assertions/{assertion_id}/retract",
    params(("assertion_id" = String, Path)),
    request_body = RetractMemoryAssertionRequest,
    responses((status = 200, description = "Assertion retracted"), (status = 403, description = "Controller required"), (status = 409, description = "Evidence or review conflict")),
    security(("bearer_jwt" = [])),
    tag = "memory"
)]
pub async fn retract_assertion(
    State(state): State<AppState>,
    user: AuthedUser,
    Path(assertion_id): Path<String>,
    Json(request): Json<RetractMemoryAssertionRequest>,
) -> impl IntoResponse {
    if let Err(response) = controller(&user) {
        return response;
    }
    if request.reason.trim().is_empty() || request.reason.len() > 2_000 {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "bounded reason required"})),
        )
            .into_response();
    }
    let evidence = match review_source(&state, &assertion_id) {
        Ok(evidence) => evidence,
        Err(response) => return response,
    };
    let conversation = execlaw_core::ConversationId::from(evidence.conversation_id.as_str());
    let log = review_log(&state);
    let base = match log.last_seq(&conversation) {
        Ok(base) => base,
        Err(error) => return failure(error),
    };
    let pending = match execlaw_core::events::PendingEvent::encode(
        execlaw_core::events::EventKind::Other,
        &serde_json::json!({"kind":"memory_assertion_retracted","assertion_id":assertion_id,"reason":request.reason}),
        Some(user.user_id.clone()),
    ) {
        Ok(pending) => pending,
        Err(error) => return failure(error),
    };
    let now = chrono::Utc::now().timestamp();
    let committed =
        log.commit_turn_with_projection(&conversation, base, vec![pending], |tx, events| {
            let event = events.first().ok_or_else(|| {
                execlaw_core::DbError::Invariant("review event was not committed".into())
            })?;
            MemoryAssertionStore::insert_retraction_in_transaction(
                tx,
                &assertion_id,
                &user.user_id,
                &request.reason,
                event,
                now,
            )
            .map_err(|error| execlaw_core::DbError::Invariant(error.to_string()))
        });
    match committed {
        Ok(events) => (StatusCode::OK, Json(serde_json::json!({"assertion_id":assertion_id,"retracted":true,"review_event_seq":events.first().map(|event|event.seq.0)}))).into_response(),
        Err(error) => {
            tracing::warn!(%error, "memory retraction was not committed");
            (StatusCode::CONFLICT, Json(serde_json::json!({"error":"memory retraction conflict"}))).into_response()
        }
    }
}

pub async fn correct_assertion(
    State(state): State<AppState>,
    user: AuthedUser,
    Path(assertion_id): Path<String>,
    Json(request): Json<CorrectMemoryAssertionRequest>,
) -> impl IntoResponse {
    if let Err(response) = controller(&user) {
        return response;
    }
    if request.reason.trim().is_empty() || request.reason.len() > 2_000 {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "bounded reason required"})),
        )
            .into_response();
    }
    let store = MemoryAssertionStore::new(&state.db);
    let original = match store.get(&assertion_id) {
        Ok(Some(assertion)) => assertion,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error":"assertion not found"})),
            )
                .into_response();
        }
        Err(error) => return failure(error),
    };
    let evidence = match review_source(&state, &assertion_id) {
        Ok(evidence) => evidence,
        Err(response) => return response,
    };
    let conversation = execlaw_core::ConversationId::from(evidence.conversation_id.as_str());
    let log = review_log(&state);
    let base = match log.last_seq(&conversation) {
        Ok(base) => base,
        Err(error) => return failure(error),
    };
    let pending = match execlaw_core::events::PendingEvent::encode(
        execlaw_core::events::EventKind::Other,
        &serde_json::json!({"kind":"memory_assertion_corrected","assertion_id":assertion_id,"reason":request.reason,"replacement":request.replacement}),
        Some(user.user_id.clone()),
    ) {
        Ok(pending) => pending,
        Err(error) => return failure(error),
    };
    let now = chrono::Utc::now().timestamp();
    let mut replacement_id = None;
    let committed =
        log.commit_turn_with_projection(&conversation, base, vec![pending], |tx, events| {
            let event = events.first().ok_or_else(|| {
                execlaw_core::DbError::Invariant("review event was not committed".into())
            })?;
            replacement_id = Some(
                MemoryAssertionStore::insert_correction_in_transaction(
                    tx,
                    &original,
                    &request.replacement,
                    &user.user_id,
                    &request.reason,
                    event,
                    now,
                )
                .map_err(|error| execlaw_core::DbError::Invariant(error.to_string()))?,
            );
            Ok(())
        });
    match committed {
        Ok(events) => (StatusCode::OK, Json(serde_json::json!({"source_assertion_id":assertion_id,"replacement_assertion_id":replacement_id,"corrected":true,"review_event_seq":events.first().map(|event|event.seq.0)}))).into_response(),
        Err(error) => {
            tracing::warn!(%error, "memory correction was not committed");
            (StatusCode::CONFLICT, Json(serde_json::json!({"error":"memory correction conflict"}))).into_response()
        }
    }
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/api/admin/memory-assets", get(list))
        .route(
            "/api/admin/memory-assets/retrieval-config",
            get(get_retrieval_config).put(put_retrieval_config),
        )
        .route(
            "/api/admin/memory-assets/embeddings/rebuild",
            axum::routing::post(rebuild_embeddings),
        )
        .route(
            "/api/admin/memory-assertions/forget-source",
            axum::routing::post(forget_memory_source),
        )
        .route(
            "/api/admin/memory-assets/{asset_id}",
            axum::routing::delete(delete_asset),
        )
        .route(
            "/api/admin/memory-assets/{asset_id}/binding",
            put(bind).delete(unbind),
        )
        .route(
            "/api/admin/memory-assertions/{assertion_id}/evidence/{evidence_id}",
            get(evidence_source),
        )
        .route(
            "/api/admin/memory-assertions/{assertion_id}/retract",
            axum::routing::post(retract_assertion),
        )
        .route(
            "/api/admin/memory-assertions/{assertion_id}/correct",
            axum::routing::post(correct_assertion),
        )
}

#[utoipa::path(
    get,
    path = "/api/admin/memory-assets/retrieval-config",
    responses((status = 200, description = "Local memory retrieval model identities", body = Option<MemoryRetrievalConfigView>)),
    tag = "memory"
)]
pub async fn get_retrieval_config(
    State(state): State<AppState>,
    user: AuthedUser,
) -> impl IntoResponse {
    if let Err(response) = controller(&user) {
        return response;
    }
    match MemoryAssetStore::new(&state.db).retrieval_config() {
        Ok(config) => (
            StatusCode::OK,
            Json(serde_json::to_value(config).unwrap_or_default()),
        )
            .into_response(),
        Err(error) => failure(error),
    }
}

#[utoipa::path(
    put,
    path = "/api/admin/memory-assets/retrieval-config",
    request_body = MemoryRetrievalConfigRequest,
    responses((status = 200, description = "Saved local retrieval model identities", body = MemoryRetrievalConfigView)),
    tag = "memory"
)]
pub async fn put_retrieval_config(
    State(state): State<AppState>,
    user: AuthedUser,
    Json(request): Json<MemoryRetrievalConfigRequest>,
) -> impl IntoResponse {
    if let Err(response) = controller(&user) {
        return response;
    }
    let store = MemoryAssetStore::new(&state.db);
    match store.set_retrieval_config(
        &request.embedding_model_id,
        &request.reranker_version,
        chrono::Utc::now().timestamp(),
    ) {
        Ok(()) => match store.retrieval_config() {
            Ok(Some(config)) => (StatusCode::OK, Json(serde_json::to_value(config).unwrap_or_default())).into_response(),
            Ok(None) => failure("retrieval config disappeared after write"),
            Err(error) => failure(error),
        },
        Err(error) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error":"invalid memory retrieval configuration", "detail":error.to_string()})),
        ).into_response(),
    }
}

#[utoipa::path(
    post,
    path = "/api/admin/memory-assets/embeddings/rebuild",
    request_body = EmbeddingRebuildRequest,
    responses((status = 200, description = "Rebuilt a bounded batch of stale local embeddings", body = serde_json::Value)),
    tag = "memory"
)]
pub async fn rebuild_embeddings(
    State(state): State<AppState>,
    user: AuthedUser,
    Json(request): Json<EmbeddingRebuildRequest>,
) -> impl IntoResponse {
    if let Err(response) = controller(&user) {
        return response;
    }
    let store = MemoryAssetStore::new(&state.db);
    let config = match store.retrieval_config() {
        Ok(Some(config)) => config,
        Ok(None) => {
            return (
                StatusCode::CONFLICT,
                Json(serde_json::json!({"error":"configure a local embedding model first"})),
            )
                .into_response();
        }
        Err(error) => return failure(error),
    };
    let resolved = match state
        .inference
        .resolve(&state.db, execlaw_core::backends::BackendPurpose::Standard)
    {
        Some(resolved) => resolved.with_workload("background"),
        None => return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({"error":"configured local inference backend is unavailable"})),
        )
            .into_response(),
    };
    let index_id = embedding_index_id(
        &resolved.endpoint,
        &resolved.model_id,
        &config.embedding_model_id,
        resolved.client.engine,
    );
    let batch_limit = request.limit.clamp(1, 128) as usize;
    let candidates = match store.embedding_rebuild_candidates(&index_id, (batch_limit + 1) as u32) {
        Ok(mut candidates) => {
            candidates.truncate(batch_limit + 1);
            candidates
        }
        Err(error) => return failure(error),
    };
    let has_more = candidates.len() > batch_limit;
    let mut embedded = 0usize;
    for candidate in candidates.iter().take(batch_limit) {
        let vector = match resolved
            .client
            .embeddings(&config.embedding_model_id, &candidate.input_text)
            .await
        {
            Ok(vector) => vector,
            Err(error) => {
                tracing::warn!(asset_id = %candidate.asset_id, error_class = error.safe_class(), "local memory embedding failed");
                return (StatusCode::BAD_GATEWAY, Json(serde_json::json!({"error":"local embedding request failed", "embedded":embedded}))).into_response();
            }
        };
        if let Err(error) = store.upsert_embedding(
            &candidate.asset_id,
            &index_id,
            &vector,
            &candidate.source_hash,
            chrono::Utc::now().timestamp(),
        ) {
            return failure(error);
        }
        embedded += 1;
    }
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "embedding_model_id": config.embedding_model_id,
            "index_id": index_id,
            "embedded": embedded,
            "has_more": has_more,
            "effects_enabled": false
        })),
    )
        .into_response()
}

pub(crate) fn embedding_index_id(
    endpoint: &str,
    chat_model_id: &str,
    embedding_model_id: &str,
    engine: execlaw_inference_api::InferenceEngine,
) -> String {
    use sha2::{Digest, Sha256};
    let fingerprint = format!("{endpoint}\0{chat_model_id}\0{embedding_model_id}\0{engine:?}");
    format!(
        "{}:{}",
        embedding_model_id,
        hex::encode(Sha256::digest(fingerprint.as_bytes()))
    )
}

pub(crate) async fn embed_memory_query(
    state: &AppState,
    query: &str,
) -> Option<(String, Vec<f32>)> {
    if query.trim().is_empty() {
        return None;
    }
    let store = MemoryAssetStore::new(&state.db);
    let config = match store.retrieval_config() {
        Ok(Some(config)) => config,
        Ok(None) => return None,
        Err(error) => {
            tracing::warn!(%error, "memory retrieval config lookup failed; using lexical search");
            return None;
        }
    };
    let resolved = match state
        .inference
        .resolve(&state.db, execlaw_core::backends::BackendPurpose::Standard)
    {
        Some(resolved) => resolved.with_workload("background"),
        None => return None,
    };
    let index_id = embedding_index_id(
        &resolved.endpoint,
        &resolved.model_id,
        &config.embedding_model_id,
        resolved.client.engine,
    );
    match resolved
        .client
        .embeddings(&config.embedding_model_id, query)
        .await
    {
        Ok(vector) => Some((index_id, vector)),
        Err(error) => {
            use sha2::Digest;
            tracing::warn!(
                query_sha256 = %hex::encode(sha2::Sha256::digest(query.as_bytes())),
                error_class = error.safe_class(),
                "local memory query embedding failed; using lexical search"
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::Path;
    use execlaw_core::events::{EventKind, EventLog, PendingEvent};
    use execlaw_core::ids::{ConversationId, EventSeq};
    use execlaw_core::memory_assertions::{
        AssertionStatus, EvidenceKind, MemoryKind, NewMemoryAssertion, NewMemoryEvidence,
    };
    use execlaw_core::memory_assets::{AssetType, NewMemoryAsset};
    use sha2::{Digest, Sha256};

    fn user(role: UserRole) -> AuthedUser {
        AuthedUser {
            user_id: "user-1".into(),
            session_id: None,
            username: "operator".into(),
            display_name: "Operator".into(),
            email: None,
            role,
            last_login_at: None,
        }
    }

    fn seed_assertion(state: &AppState) {
        let conversation = ConversationId::from("memory-review-fixture");
        crate::chats::ensure_conversation_for(&state.db, &conversation);
        let key = state.event_log_hmac_key.as_ref().unwrap();
        EventLog::new(&state.db)
            .with_hmac_key((**key).clone())
            .commit_turn(
                &conversation,
                EventSeq(0),
                vec![
                    PendingEvent::encode(
                        EventKind::Other,
                        &serde_json::json!({"source":"memo"}),
                        Some("controller".into()),
                    )
                    .unwrap(),
                ],
            )
            .unwrap();
        let store = MemoryAssertionStore::new(&state.db);
        store
            .append(&NewMemoryAssertion {
                assertion_id: "assertion-1".into(),
                scope: "global".into(),
                trust_class: "Controller".into(),
                kind: MemoryKind::Semantic,
                subject: "project".into(),
                predicate: "status".into(),
                object: serde_json::json!("ready"),
                confidence: 0.9,
                status: AssertionStatus::Approved,
                observed_from: 1,
                observed_to: None,
                valid_from: 1,
                valid_to: None,
                supersedes_id: None,
                extraction_run_id: "fixture".into(),
                created_event_seq: EventSeq(1),
                created_at: 1,
            })
            .unwrap();
        store
            .add_evidence(&NewMemoryEvidence {
                evidence_id: "evidence-1".into(),
                assertion_id: "assertion-1".into(),
                conversation_id: conversation,
                event_seq: EventSeq(1),
                payload_path: "$.source".into(),
                quote_hash: hex::encode(Sha256::digest(b"memo")),
                evidence_kind: EvidenceKind::DirectQuote,
                created_at: 1,
            })
            .unwrap();
    }

    #[tokio::test]
    async fn forget_memory_source_is_controller_only_and_hides_evidence_lineage() {
        let state = crate::routes::test_app_state();
        seed_assertion(&state);
        let request = ForgetMemorySourceRequest {
            conversation_id: "memory-review-fixture".into(),
            event_seq: 1,
        };
        let denied = forget_memory_source(
            State(state.clone()),
            user(UserRole::Viewer),
            Json(ForgetMemorySourceRequest {
                conversation_id: request.conversation_id.clone(),
                event_seq: request.event_seq,
            }),
        )
        .await
        .into_response();
        assert_eq!(denied.status(), StatusCode::FORBIDDEN);

        let response = forget_memory_source(
            State(state.clone()),
            user(UserRole::Controller),
            Json(request),
        )
        .await
        .into_response();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            MemoryAssertionStore::new(&state.db)
                .get("assertion-1")
                .unwrap()
                .is_none()
        );
        assert!(
            MemoryAssertionStore::new(&state.db)
                .evidence_by_id("assertion-1", "evidence-1")
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn source_reveal_checks_event_and_quote_hash_before_returning_text() {
        let state = crate::routes::test_app_state();
        seed_assertion(&state);
        let response = list(
            State(state.clone()),
            user(UserRole::Controller),
            Query(AgentScopeQuery { agent_scope: None }),
        )
        .await
        .into_response();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let listed: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(listed["assertions"][0]["assertion_id"], "assertion-1");
        let response = evidence_source(
            State(state.clone()),
            user(UserRole::Controller),
            Path(("assertion-1".into(), "evidence-1".into())),
        )
        .await
        .into_response();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let source: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(source["source_quote"], "memo");
        assert_eq!(source["integrity_verified"], true);

        MemoryAssertionStore::new(&state.db)
            .add_evidence(&NewMemoryEvidence {
                evidence_id: "evidence-mismatched".into(),
                assertion_id: "assertion-1".into(),
                conversation_id: ConversationId::from("memory-review-fixture"),
                event_seq: EventSeq(1),
                payload_path: "$.source".into(),
                quote_hash: "0".repeat(64),
                evidence_kind: EvidenceKind::DirectQuote,
                created_at: 2,
            })
            .unwrap();
        let denied = evidence_source(
            State(state.clone()),
            user(UserRole::Controller),
            Path(("assertion-1".into(), "evidence-mismatched".into())),
        )
        .await
        .into_response();
        assert_eq!(denied.status(), StatusCode::CONFLICT);
        let denied = evidence_source(
            State(state),
            user(UserRole::Operator),
            Path(("assertion-1".into(), "evidence-1".into())),
        )
        .await
        .into_response();
        assert_eq!(denied.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn reviews_append_signed_event_with_projection_in_one_commit() {
        let state = crate::routes::test_app_state();
        seed_assertion(&state);
        let response = retract_assertion(
            State(state.clone()),
            user(UserRole::Controller),
            Path("assertion-1".into()),
            Json(RetractMemoryAssertionRequest {
                reason: "outdated".into(),
            }),
        )
        .await
        .into_response();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            MemoryAssertionStore::new(&state.db)
                .is_retracted("assertion-1")
                .unwrap()
        );
        let conversation = ConversationId::from("memory-review-fixture");
        let key = state.event_log_hmac_key.as_ref().unwrap();
        assert_eq!(
            EventLog::new(&state.db)
                .with_hmac_key((**key).clone())
                .replay_since(&conversation, EventSeq(0))
                .unwrap()
                .len(),
            2
        );

        let correction_state = crate::routes::test_app_state();
        seed_assertion(&correction_state);
        let response = correct_assertion(
            State(correction_state.clone()),
            user(UserRole::Controller),
            Path("assertion-1".into()),
            Json(CorrectMemoryAssertionRequest {
                replacement: serde_json::json!({"state":"revised"}),
                reason: "new evidence".into(),
            }),
        )
        .await
        .into_response();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let replacement_id = payload["replacement_assertion_id"].as_str().unwrap();
        let evidence = MemoryAssertionStore::new(&correction_state.db)
            .evidence_for(replacement_id, 1)
            .unwrap();
        assert_eq!(evidence[0].payload_path, "$.replacement");
        let response = evidence_source(
            State(correction_state),
            user(UserRole::Controller),
            Path((replacement_id.into(), evidence[0].evidence_id.clone())),
        )
        .await
        .into_response();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn controller_can_bind_and_unbind_while_operator_is_denied() {
        let state = crate::routes::test_app_state();
        MemoryAssetStore::new(&state.db)
            .create(NewMemoryAsset {
                asset_id: "asset-1",
                asset_type: AssetType::Memory,
                name: "Operator preference",
                description: "Preference",
                owner_scope: "global",
                visibility: execlaw_core::memory_assets::AssetVisibility::Private,
                trust_floor: "Controller",
                source_ref: None,
                content_ref: Some("English"),
                source_hash: Some("hash-1"),
                now_unix: 1,
            })
            .unwrap();

        let response = bind(
            State(state.clone()),
            user(UserRole::Controller),
            Path("asset-1".to_owned()),
            Json(BindAssetRequest {
                agent_scope: "default".into(),
                injection_mode: "hot".into(),
                priority: 10,
                max_chars: 128,
            }),
        )
        .await
        .into_response();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            MemoryAssetStore::new(&state.db)
                .list_loadout("default", 10)
                .unwrap()
                .len(),
            1
        );

        let denied = unbind(
            State(state.clone()),
            user(UserRole::Operator),
            Path("asset-1".to_owned()),
            Query(AgentScopeQuery { agent_scope: None }),
        )
        .await
        .into_response();
        assert_eq!(denied.status(), StatusCode::FORBIDDEN);

        let removed = unbind(
            State(state.clone()),
            user(UserRole::Controller),
            Path("asset-1".to_owned()),
            Query(AgentScopeQuery { agent_scope: None }),
        )
        .await
        .into_response();
        assert_eq!(removed.status(), StatusCode::OK);
        assert!(
            MemoryAssetStore::new(&state.db)
                .list_loadout("default", 10)
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn retrieval_model_configuration_is_controller_gated_and_rebuild_fails_closed() {
        let state = crate::routes::test_app_state();
        let missing = get_retrieval_config(State(state.clone()), user(UserRole::Controller))
            .await
            .into_response();
        assert_eq!(missing.status(), StatusCode::OK);

        let saved = put_retrieval_config(
            State(state.clone()),
            user(UserRole::Controller),
            Json(MemoryRetrievalConfigRequest {
                embedding_model_id: "local-embed-v1".into(),
                reranker_version: "local-hybrid-rrf-v1".into(),
            }),
        )
        .await
        .into_response();
        assert_eq!(saved.status(), StatusCode::OK);
        assert_eq!(
            MemoryAssetStore::new(&state.db)
                .retrieval_config()
                .unwrap()
                .unwrap()
                .embedding_model_id,
            "local-embed-v1"
        );

        let denied = put_retrieval_config(
            State(state.clone()),
            user(UserRole::Operator),
            Json(MemoryRetrievalConfigRequest {
                embedding_model_id: "untrusted-model".into(),
                reranker_version: "local-hybrid-rrf-v1".into(),
            }),
        )
        .await
        .into_response();
        assert_eq!(denied.status(), StatusCode::FORBIDDEN);

        let unavailable = rebuild_embeddings(
            State(state),
            user(UserRole::Controller),
            Json(EmbeddingRebuildRequest { limit: 8 }),
        )
        .await
        .into_response();
        assert_eq!(unavailable.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
