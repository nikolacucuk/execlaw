//! Approval flow routes (Phase 3, §2.14).
//!
//! When a cold contact messages the agent, the chat route parks the
//! conversation (`AwaitingTrustDecision`) and writes a
//! `ColdContactArrived` event carrying an `approval_id`. The
//! controller responds to the sideband notification by hitting this
//! endpoint with a verb (`Trust` / `TrustLimited` / `Block` /
//! `IgnoreOnce`). The verb decides the principal's new `TrustLevel`,
//! a `TrustChanged` event lands in the log, and — on `Trust` /
//! `TrustLimited` — the original user message is replayed through
//! the normal turn path.
//!
//! The approval id is currently an opaque UUID the server minted on
//! the cold-contact path; Phase-7 hardening swaps it for an
//! EdDSA-signed JWT per §2.11.

use axum::Json;
use axum::extract::FromRequestParts;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::response::IntoResponse;
use axum::{
    Router,
    routing::{get, post},
};
use execlaw_core::conversation::{ConversationStore, Phase};
use execlaw_core::events::{EventKind, EventLog, PendingEvent};
use execlaw_core::ids::{ConversationId, EventSeq, PrincipalId};
use execlaw_core::memory_lifecycle::{PromotionProposal, PromotionStore};
use execlaw_core::principal::{PrincipalStore, TrustLevel as CoreTrustLevel};
use execlaw_core::users::UserRole;
use execlaw_policy::sideband::{ApprovalClaims, ApprovalReason, ApprovalVerb};
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use std::convert::Infallible;

use crate::auth::JwtSigner;
use crate::events::UiEvent;
use crate::state::AppState;

pub struct MaybeAuthedUser(Option<crate::auth_extract::AuthedUser>);

impl FromRequestParts<AppState> for MaybeAuthedUser {
    type Rejection = Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        Ok(Self(
            crate::auth_extract::AuthedUser::from_request_parts(parts, state)
                .await
                .ok(),
        ))
    }
}

/// Look up the conversation's originating-transport channel by
/// asking the host's transport-binding store + transport registry.
/// Used by the cold-contact and claim_as_me approval replays so
/// they don't hardcode which channel the inbound came in on
/// (previously this was always "signal", which broke the moment
/// other transports started seeing cold-contact flows).
///
/// Returns `None` when:
///   * The conversation isn't tied to a principal_group (web-only
///     chat with no transport);
///   * No bindings exist yet for that group;
///   * No installed plugin handles any of the bindings' channels
///     (e.g. operator uninstalled the transport between the cold-
///     contact arriving and the approval being processed).
///
/// Callers thread the result into `dispatch_external_turn`'s
/// `origin_channel` so the auto-bridge / typing indicator / system
/// prose all surface the right transport.
fn origin_channel_for_conversation(state: &AppState, cid: &ConversationId) -> Option<String> {
    use execlaw_core::principal_groups::PrincipalGroupStore;
    use execlaw_core::transport_bindings::TransportBindingStore;
    let pg_store = PrincipalGroupStore::new(&state.db);
    // 2026-05-13 — log DB errors loudly instead of `.ok()`-
    // swallowing them. Pre-rework a `principal_groups` or
    // `transport_bindings` decode failure silently degraded the
    // approval reply to "no transport bridged this message" with
    // no diagnostic, which is indistinguishable from a legitimately
    // web-only chat. The WARN now distinguishes the two.
    let pg_id = match pg_store.principal_group_id_for(cid.as_str()) {
        Ok(opt) => opt?,
        Err(e) => {
            tracing::warn!(
                target: "approvals",
                conversation_id = %cid.as_str(),
                error = %e,
                "principal_groups read failed — approval reply will not be bridged via transport. \
                 Likely BLOB column corruption; check Settings → Groups.",
            );
            return None;
        }
    };
    let binding_store = TransportBindingStore::new(&state.db);
    let bindings = match binding_store.bindings_for_group_any_channel(&pg_id) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(
                target: "approvals",
                principal_group_id = %pg_id,
                error = %e,
                "transport_bindings read failed — approval reply will not be bridged via transport.",
            );
            return None;
        }
    };
    state
        .host_transports
        .lookup_first_supported_binding(&bindings)
        .map(|r| r.channel)
}

/// Mint a signed approval-token JWT (§2.11). The token's `jti` is
/// the approval_id; verifying the token before honoring an approval
/// response prevents an attacker from forging an `/approvals/X/respond`
/// request with a guessed id.
pub fn issue_approval_token(
    signer: &JwtSigner,
    approval_id: &str,
    conversation_id: &ConversationId,
    reason: &str,
    principal_id: Option<String>,
    effect_hash: Option<String>,
) -> String {
    use jsonwebtoken::{Algorithm, Header, encode};

    let now = chrono::Utc::now().timestamp();
    let reason_enum = match reason {
        "cold_contact" => ApprovalReason::ColdContact,
        "rule_of_two_breach" => ApprovalReason::RuleOfTwoBreach,
        "sensitive_tool_call" => ApprovalReason::SensitiveToolCall,
        "ask_controller" => ApprovalReason::AskController,
        "anomaly_tripwire" => ApprovalReason::AnomalyTripwire,
        "effectful_chain" => ApprovalReason::EffectfulChain,
        _ => ApprovalReason::ColdContact,
    };
    let claims = ApprovalClaims {
        iss: signer.issuer().to_owned(),
        jti: approval_id.to_owned(),
        conversation_id: conversation_id.as_str().to_owned(),
        reason: reason_enum,
        tool_call_id: None,
        principal_id,
        effect_hash,
        iat: now,
        exp: now + 24 * 3600, // 24h window for the controller to respond
    };
    let header = Header::new(Algorithm::EdDSA);
    encode(&header, &claims, signer.encoding_key()).expect("JWT encode")
}

/// Verify a signed approval token. Returns the decoded claims if
/// the token is valid AND its `jti` matches the path-param
/// `approval_id`. Mismatch → caller can't authorize this approval.
pub fn verify_approval_token(
    signer: &JwtSigner,
    token: &str,
    expected_jti: &str,
) -> Result<ApprovalClaims, String> {
    use jsonwebtoken::{Algorithm, Validation, decode};

    let mut validation = Validation::new(Algorithm::EdDSA);
    validation.set_issuer(&[signer.issuer()]);
    validation.leeway = 5;

    let data = decode::<ApprovalClaims>(token, signer.decoding_key(), &validation)
        .map_err(|e| format!("approval token verification failed: {e}"))?;

    if data.claims.jti != expected_jti {
        return Err(format!(
            "approval token jti '{}' does not match path approval_id '{}'",
            data.claims.jti, expected_jti
        ));
    }
    Ok(data.claims)
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct ApprovalRequest {
    /// The verb the controller is responding with. See
    /// [`ApprovalVerb`] in `execlaw-policy::sideband`.
    #[schema(value_type = String)]
    pub verb: ApprovalVerb,
    /// Optional topic scopes for `TrustLimited`.
    #[serde(default)]
    pub allowed_topics: Vec<String>,
    /// Optional human-readable reason (saved on the principal for audit).
    #[serde(default)]
    pub reason: Option<String>,
    /// Signed approval-token JWT minted by the cold-contact path.
    /// Required when `EXECLAW_APPROVAL_TOKEN_REQUIRED` is set or
    /// when the controller's UI is the only thing that should be
    /// able to call this endpoint. Phase 3 accepts an empty token
    /// (back-compat) but logs a warning; Phase 7 hardening flips
    /// this to required.
    #[serde(default)]
    pub approval_token: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ApprovalResponse {
    pub approval_id: String,
    pub principal_id: String,
    pub conversation_id: String,
    pub new_trust_class: String,
    pub outcome: String,
}

/// `POST /api/admin/approvals/:id/respond`
#[utoipa::path(
    post,
    path = "/api/admin/approvals/{approval_id}/respond",
    params(
        ("approval_id" = String, Path, description = "Pending approval id"),
    ),
    responses(
        (status = 200, description = "Approval recorded; original action resumed (or dropped)"),
        (status = 401, description = "Missing or invalid signed approval token"),
        (status = 404, description = "Unknown approval id"),
    ),
    security(("bearer_jwt" = [])),
    tag = "approvals"
)]
pub async fn respond_handler(
    State(state): State<AppState>,
    Path(approval_id): Path<String>,
    user: MaybeAuthedUser,
    Json(req): Json<ApprovalRequest>,
) -> impl IntoResponse {
    // Verify the signed approval token if one is supplied. An
    // attacker who guesses the approval_id but doesn't have the
    // server's signing key can't forge a matching token (§2.11).
    let claims = if let Some(token) = &req.approval_token {
        match verify_approval_token(&state.signer, token, &approval_id) {
            Ok(claims) => Some(claims),
            Err(e) => {
                return (
                    StatusCode::UNAUTHORIZED,
                    Json(serde_json::json!({
                        "error": {
                            "code": "bad_approval_token",
                            "message": e,
                        }
                    })),
                )
                    .into_response();
            }
        }
    } else {
        None
    };

    // Chain approvals reuse this endpoint so the existing approvals
    // UI feed + action path can drive both cold-contact and chain-run
    // decisions.
    if find_cold_contact_event(&state, &approval_id).is_none() {
        return respond_chain_approval(state, approval_id, req, claims, user.0).await;
    }

    // Look up the ColdContactArrived event that minted this
    // approval_id. Phase 3 scans state_events for the matching row;
    // a dedicated `state_approvals` index lands as a Phase-5
    // hardening when the event volume warrants it.
    let Some((cid, sender_principal_id, original_text)) =
        find_cold_contact_event(&state, &approval_id)
    else {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": {
                    "code": "approval_not_found",
                    "message": "no cold-contact event matches this approval_id",
                }
            })),
        )
            .into_response();
    };

    // Apply the verb to the principal.
    let principals = PrincipalStore::new(&state.db);
    let pid = PrincipalId::from(sender_principal_id.clone());
    let now = chrono::Utc::now().timestamp();

    let (new_level, outcome): (CoreTrustLevel, &'static str) = match req.verb {
        ApprovalVerb::Trust => (
            CoreTrustLevel::KnownTrusted {
                resolvers: vec![],
                approved_by: PrincipalId::from("controller"),
                approved_at: now,
            },
            "trust",
        ),
        ApprovalVerb::TrustLimited => (
            CoreTrustLevel::KnownLimited {
                resolvers: vec![],
                allowed_topics: req.allowed_topics.clone(),
                allowed_tools: None,
            },
            "trust_limited",
        ),
        ApprovalVerb::Block => (
            CoreTrustLevel::Blocked {
                blocked_by: PrincipalId::from("controller"),
                blocked_at: now,
                reason: req.reason.clone(),
            },
            "block",
        ),
        ApprovalVerb::IgnoreOnce => {
            // Don't change the trust level; just clear the parked
            // state so future messages prompt again.
            let store = ConversationStore::new(&state.db);
            if let Ok(Some(mut row)) = store.get(&cid) {
                row.phase = Phase::Idle;
                let _ = store.upsert(&row);
            }
            state.events.publish(UiEvent::ApprovalResolved {
                approval_id: approval_id.clone(),
                conversation_id: cid.as_str().to_owned(),
            });
            return (
                StatusCode::OK,
                Json(serde_json::json!(ApprovalResponse {
                    approval_id,
                    principal_id: sender_principal_id,
                    conversation_id: cid.as_str().to_owned(),
                    new_trust_class: "UnknownPending".into(),
                    outcome: "ignore_once".into(),
                })),
            )
                .into_response();
        }
        ApprovalVerb::ClaimAsMe => {
            return claim_as_me(state, approval_id, &cid, &pid, original_text).await;
        }
        // The non-cold-contact verbs (Approve / Edit / Reject) land
        // with the Rule-of-Two + sensitive-tool-call flows in Phase 3+.
        other => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": {
                        "code": "unsupported_verb",
                        "message": format!(
                            "verb {:?} is not supported for cold-contact approvals; \
                             use Trust / TrustLimited / ClaimAsMe / Block / IgnoreOnce",
                            other
                        ),
                    }
                })),
            )
                .into_response();
        }
    };

    let new_class_tag = new_level.class_tag().to_owned();

    if let Err(e) = principals.set_trust(&pid, new_level) {
        return internal_error(&format!("set_trust: {e}"));
    }

    // Commit a TrustChanged event so replay captures the transition.
    let trust_payload = TrustChangedPayload {
        principal_id: sender_principal_id.clone(),
        new_class: new_class_tag.clone(),
        approval_id: approval_id.clone(),
        reason: req.reason.clone(),
    };
    let log = event_log(&state);
    let trust_event = match PendingEvent::encode(
        EventKind::TrustChanged,
        &trust_payload,
        Some("controller".into()),
    ) {
        Ok(e) => e,
        Err(e) => return internal_error(&format!("encode trust_changed: {e}")),
    };
    let base_seq = match log.last_seq(&cid) {
        Ok(s) => s,
        Err(e) => return internal_error(&format!("last_seq: {e}")),
    };
    if let Err(e) = log.commit_turn(&cid, base_seq, vec![trust_event]) {
        return internal_error(&format!("commit trust_changed: {e}"));
    }

    // On Trust / TrustLimited: broadcast the original message as a
    // ChatMessageInbound so the UI picks up the parked text. On
    // Block: no further messages.
    if matches!(outcome, "trust" | "trust_limited") {
        state.events.publish(UiEvent::ChatMessageInbound {
            conversation_id: cid.as_str().to_owned(),
            seq: log.last_seq(&cid).map(|s| s.0).unwrap_or(0),
            text: original_text.clone(),
            sender: Some(sender_principal_id.clone()),
        });
    }

    // Un-park the conversation on non-blocking verbs.
    if !matches!(outcome, "block") {
        let cstore = ConversationStore::new(&state.db);
        if let Ok(Some(mut row)) = cstore.get(&cid) {
            row.phase = Phase::Idle;
            let _ = cstore.upsert(&row);
        }
    }

    // Replay the queued message through the agent so the controller
    // doesn't have to ask the contact to re-send. Pre-fix, the
    // approval flipped trust + un-parked the conversation but no
    // turn ever ran — the contact would just see "you read this 5
    // minutes ago" with no reply. Now we dispatch_external_turn
    // with the freshly-promoted principal so the agent answers
    // what was actually asked.
    if matches!(outcome, "trust" | "trust_limited") {
        let promoted = match principals.get(&pid) {
            Ok(Some(p)) => p,
            _ => {
                return internal_error("post-trust principal lookup failed");
            }
        };
        let trust_flat = execlaw_policy::trust::TrustLevel::parse(promoted.trust_level.class_tag())
            .unwrap_or(execlaw_policy::trust::TrustLevel::UnknownPending);
        // Look up the actual originating channel from the
        // conversation's first transport binding. Previously this
        // was hardcoded to "signal" because Signal was the only
        // shipped transport with a cold-contact UX; now any
        // installed transport (sms, whatsapp, slack, ...) gets the
        // right origin channel without code changes here.
        let origin_channel = origin_channel_for_conversation(&state, &cid);
        // Post-approval replay: the controller has explicitly trusted
        // this contact, so the addressing question is moot — fall
        // through with `EligibilityBypass`. The conversation may
        // still be a group, in which case the resolver returns
        // Some(...) and the agent gets the same room-awareness it
        // would on a directly addressed inbound.
        let replay_group_ctx = crate::chats::resolve_group_turn_context(
            &state,
            &cid,
            crate::group_addressing::AddressedReason::EligibilityBypass,
        );
        if let Err(e) = crate::chats::dispatch_external_turn(
            &state,
            &cid,
            &promoted,
            trust_flat,
            &original_text,
            origin_channel.as_deref(),
            None,
            replay_group_ctx,
            // Approval-replay paths don't carry attachments — the
            // cold-contact's first message arrived before the
            // trust upgrade, attachments (if any) weren't fetched
            // back then, and re-fetching here would require
            // re-running the plugin's `fetch_attachment` from a
            // stored bridge_id that we don't currently persist.
            // Future: stash the bridge_ids on the parked event so
            // approval replay can re-hydrate images too.
            Vec::new(),
        )
        .await
        {
            tracing::warn!(
                target: "approvals",
                conversation_id = %cid.as_str(),
                error = %e,
                "post-approval replay through dispatch_external_turn failed; \
                 trust transition stands but the agent didn't run a turn — \
                 operator can ask the contact to re-send",
            );
        }
    }

    state.events.publish(UiEvent::ApprovalResolved {
        approval_id: approval_id.clone(),
        conversation_id: cid.as_str().to_owned(),
    });

    (
        StatusCode::OK,
        Json(serde_json::json!(ApprovalResponse {
            approval_id,
            principal_id: sender_principal_id,
            conversation_id: cid.as_str().to_owned(),
            new_trust_class: new_class_tag,
            outcome: outcome.into(),
        })),
    )
        .into_response()
}

async fn respond_chain_approval(
    state: AppState,
    approval_id: String,
    req: ApprovalRequest,
    claims: Option<ApprovalClaims>,
    user: Option<crate::auth_extract::AuthedUser>,
) -> axum::response::Response {
    let pending = match state.db.with_conn(|c| {
        let mut stmt = c.prepare(
            "SELECT approval_effect_hash, conversation_id FROM state_chain_runs \
                 WHERE approval_id = ?1 AND status = 'awaiting_approval' \
                 LIMIT 1",
        )?;
        Ok(stmt
            .query_row(rusqlite::params![approval_id.as_str()], |row| {
                Ok((row.get::<_, Option<String>>(0)?, row.get::<_, String>(1)?))
            })
            .optional()?)
    }) {
        Ok(Some((Some(hash), conversation_id))) => (hash, conversation_id),
        Ok(Some((None, _))) => {
            return (
                StatusCode::CONFLICT,
                Json(serde_json::json!({"error": {"code": "approval_effect_unbound", "message": "pending chain approval has no effect fingerprint"}})),
            ).into_response();
        }
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": {"code": "approval_not_found", "message": "no pending approval matches this approval_id"}})),
            ).into_response();
        }
        Err(error) => return internal_error(&format!("chain approval lookup: {error}")),
    };

    let Some(claims) = claims else {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": {"code": "approval_token_required", "message": "effect-bound chain approval requires its signed token"}})),
        ).into_response();
    };
    let Some(user) = user else {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": {"code": "controller_auth_required", "message": "chain effect approval requires an authenticated Controller"}})),
        ).into_response();
    };
    if user.role != UserRole::Controller
        || claims.principal_id.as_deref() != Some(user.user_id.as_str())
    {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"error": {"code": "approval_principal_mismatch", "message": "signed approval belongs to a different Controller"}})),
        ).into_response();
    }
    if claims.reason != ApprovalReason::EffectfulChain
        || claims.conversation_id != pending.1
        || claims.effect_hash.as_deref() != Some(pending.0.as_str())
    {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({"error": {"code": "approval_effect_mismatch", "message": "signed approval does not match the pending effect"}})),
        ).into_response();
    }

    let decision = match req.verb {
        ApprovalVerb::Approve => crate::tool_chain_tool::ChainApprovalDecision::Approve,
        ApprovalVerb::Reject => crate::tool_chain_tool::ChainApprovalDecision::Deny,
        other => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": {
                        "code": "unsupported_verb",
                        "message": format!(
                            "verb {:?} is not supported for chain approvals; use Approve or Reject",
                            other
                        ),
                    }
                })),
            )
                .into_response();
        }
    };

    let result = crate::tool_chain_tool::resolve_chain_approval_http(
        &state.db,
        &approval_id,
        decision,
        chrono::Utc::now().timestamp(),
    );
    let value = match result {
        Ok(v) => v,
        Err(e) if e == "approval_not_found" => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({
                    "error": {
                        "code": "approval_not_found",
                        "message": "no pending chain approval matches this approval_id",
                    }
                })),
            )
                .into_response();
        }
        Err(e) if e == "approval_effect_mismatch" => {
            return (
                StatusCode::CONFLICT,
                Json(serde_json::json!({"error": {"code": "approval_effect_mismatch", "message": "the pending plan changed after approval was issued"}})),
            )
                .into_response();
        }
        Err(e) => return internal_error(&format!("chain approval resolve: {e}")),
    };

    let conv_id = value
        .get("conversation_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_owned();
    if !conv_id.is_empty() {
        state.events.publish(UiEvent::ApprovalResolved {
            approval_id: approval_id.clone(),
            conversation_id: conv_id.clone(),
        });
    }

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "approval_id": approval_id,
            "principal_id": "tool-chain",
            "conversation_id": conv_id,
            "new_trust_class": "N/A",
            "outcome": value.get("status").cloned().unwrap_or(serde_json::Value::String("unknown".to_string())),
            "chain": value,
        })),
    )
        .into_response()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ColdContactReplayPayload {
    text: String,
    sender_principal_id: String,
    approval_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TrustChangedPayload {
    principal_id: String,
    new_class: String,
    approval_id: String,
    reason: Option<String>,
}

/// "This is me" — the controller is messaging the agent from a
/// not-yet-registered handle. Adds the cold-contact's
/// `(transport, handle)` to the controller's identifiers, then
/// calls `principal_admit::reconcile_against_my_identities` which
/// merges the stale UnknownPending principal into the controller,
/// rebinds the binding + conversation, and unsticks the
/// `awaiting_trust_decision` phase. After reconcile the queued
/// message is dispatched through the controller turn handler so
/// the agent answers immediately.
async fn claim_as_me(
    state: AppState,
    approval_id: String,
    cid: &ConversationId,
    stale_pid: &PrincipalId,
    original_text: String,
) -> axum::response::Response {
    let principals = PrincipalStore::new(&state.db);
    let now = chrono::Utc::now().timestamp();

    // 1. Pull the stale principal's identifiers. If the principal is
    // already gone, that means reconcile ran between the cold-contact
    // arriving and the operator clicking — the resolution we'd
    // perform here has already happened (or is moot). Idempotent
    // 200: log the situation, replay the queued message as a
    // Controller turn (the binding now points at the controller's
    // group), and return success so the SPA refreshes its list and
    // drops the phantom approval entry.
    let stale = match principals.get(stale_pid) {
        Ok(Some(p)) => Some(p),
        Ok(None) => {
            tracing::info!(
                target: "approvals::claim_as_me",
                approval_id = %approval_id,
                stale_pid = %stale_pid.as_str(),
                "cold-contact principal already reconciled away — replaying queued message only",
            );
            None
        }
        Err(e) => return internal_error(&format!("principal get: {e}")),
    };

    // 2. Resolve the controller principal id via the same accessor
    // `add_my_identifier` uses. Lazy-create when the controller's
    // explicit row doesn't exist yet (matches the existing My-
    // identities semantics).
    let controller_pid = match crate::routes::controller_principal_id(&state.db) {
        Ok(p) => p,
        Err(e) => return internal_error(&format!("controller principal: {}", e.message)),
    };
    let mut controller_row = match principals.get(&controller_pid) {
        Ok(Some(p)) => p,
        Ok(None) => execlaw_core::principal::Principal {
            id: controller_pid.clone(),
            identifiers: Vec::new(),
            trust_level: CoreTrustLevel::Controller,
            resolved_by: Vec::new(),
            metadata: serde_json::json!({}),
            first_seen: now,
            last_seen: Some(now),
            controller_notes: None,
        },
        Err(e) => return internal_error(&format!("controller principal lookup: {e}")),
    };

    // 3. Add every identifier from the stale principal to the
    // controller (deduped). The reconcile pass below will then
    // merge the stale principal away, rebind the conversation,
    // and flip the awaiting_trust_decision phase to idle.
    // When `stale` is None (already reconciled), this loop is a
    // no-op — the controller already owns the identifier.
    let mut added_any = false;
    if let Some(stale_row) = stale.as_ref() {
        for ident in &stale_row.identifiers {
            if !controller_row.identifiers.contains(ident) {
                controller_row.identifiers.push(ident.clone());
                added_any = true;
            }
        }
    }
    if added_any {
        controller_row.last_seen = Some(now);
        if let Err(e) = principals.upsert(&controller_row) {
            return internal_error(&format!("controller upsert: {e}"));
        }
    }

    // 4. Reconcile.
    let report = match crate::principal_admit::reconcile_against_my_identities(&state.db) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(
                target: "approvals::claim_as_me",
                error = %e,
                "reconcile after claim_as_me failed; controller now owns the identifier \
                 but the stale principal may still shadow it",
            );
            crate::principal_admit::ReconcileReport::default()
        }
    };
    tracing::info!(
        target: "approvals::claim_as_me",
        approval_id = %approval_id,
        merged = report.merged.len(),
        bindings_repointed = report.bindings_repointed,
        conversations_repointed = report.conversations_repointed,
        "claim_as_me reconcile complete",
    );

    // 5. Replay the queued message as the controller. After
    // reconcile the conversation is bound to the controller's
    // group; dispatch_external_turn with the controller principal
    // runs the same pipeline a fresh inbound from the controller
    // would.
    let controller_now = match principals.get(&controller_pid) {
        Ok(Some(p)) => p,
        _ => return internal_error("controller principal vanished mid-claim"),
    };
    let trust_flat = execlaw_policy::trust::TrustLevel::Controller;
    // Look up the actual origin channel; see
    // origin_channel_for_conversation for rationale.
    let origin_channel = origin_channel_for_conversation(&state, cid);
    // Same rationale as the post-approval replay: the controller
    // explicitly claimed this turn, so addressing is moot — but
    // resolve the group context anyway so a controller-claimed
    // turn in a Signal group still gets the room-awareness block.
    let claim_group_ctx = crate::chats::resolve_group_turn_context(
        &state,
        cid,
        crate::group_addressing::AddressedReason::EligibilityBypass,
    );
    if let Err(e) = crate::chats::dispatch_external_turn(
        &state,
        cid,
        &controller_now,
        trust_flat,
        &original_text,
        origin_channel.as_deref(),
        None,
        claim_group_ctx,
        // claim_as_me replay path doesn't carry attachments — see
        // the comment in the cold-contact branch above.
        Vec::new(),
    )
    .await
    {
        tracing::warn!(
            target: "approvals::claim_as_me",
            conversation_id = %cid.as_str(),
            error = %e,
            "dispatch_external_turn replay failed after claim_as_me; \
             trust transition stands but no turn ran",
        );
    }

    state.events.publish(UiEvent::ApprovalResolved {
        approval_id: approval_id.clone(),
        conversation_id: cid.as_str().to_owned(),
    });

    (
        StatusCode::OK,
        Json(serde_json::json!(ApprovalResponse {
            approval_id,
            principal_id: controller_pid.as_str().to_owned(),
            conversation_id: cid.as_str().to_owned(),
            new_trust_class: "Controller".into(),
            outcome: "claim_as_me".into(),
        })),
    )
        .into_response()
}

/// Scan conversations for a ColdContactArrived event matching this
/// approval_id. Returns (conversation_id, sender_principal_id, text).
///
/// This is a linear scan over state_events; fine for Phase 3 where
/// approvals are rare and short-lived. An index on
/// `(kind, approval_id)` lands as a hardening pass.
fn find_cold_contact_event(
    state: &AppState,
    approval_id: &str,
) -> Option<(ConversationId, String, String)> {
    // 2026-05-13 — log DB errors at WARN instead of `.ok()?`-
    // swallowing them. Pre-rework a `state_events` query failure
    // (lock contention, schema mismatch, HMAC reverification error)
    // silently degraded to "approval not found", which the caller
    // surfaces as a generic 404 — operators chasing a missing
    // approval had no signal that the underlying read failed.
    let db = &state.db;
    let conv_ids: Vec<String> = match db.with_conn(|c| {
        let mut stmt = c
            .prepare("SELECT DISTINCT conversation_id FROM state_events WHERE kind = 'cold_contact_arrived'")
            .map_err(execlaw_core::db::DbError::from)?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(execlaw_core::db::DbError::from)?;
        let out: Result<Vec<_>, _> = rows.collect();
        Ok(out?)
    }) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                target: "approvals::find_cold_contact_event",
                approval_id = %approval_id,
                error = %e,
                "state_events scan failed — caller will see 'approval not found'. \
                 Likely DB lock contention or schema drift.",
            );
            return None;
        }
    };

    let log = event_log(state);
    for id in conv_ids {
        let cid = ConversationId::from(id);
        let events = match log.replay_since(&cid, EventSeq(0)) {
            Ok(ev) => ev,
            Err(e) => {
                tracing::warn!(
                    target: "approvals::find_cold_contact_event",
                    approval_id = %approval_id,
                    conversation_id = %cid.as_str(),
                    error = %e,
                    "replay_since failed for one conversation — continuing scan.",
                );
                continue;
            }
        };
        for ev in events {
            if ev.kind != EventKind::ColdContactArrived {
                continue;
            }
            if let Ok(p) = ev.decode_payload::<ColdContactReplayPayload>() {
                if p.approval_id == approval_id {
                    return Some((cid, p.sender_principal_id, p.text));
                }
            }
        }
    }
    None
}

fn event_log(state: &AppState) -> EventLog<'_> {
    let log = EventLog::new(&state.db);
    match &state.event_log_hmac_key {
        Some(k) => log.with_hmac_key((**k).clone()),
        None => log,
    }
}

fn internal_error(msg: &str) -> axum::response::Response {
    tracing::error!("{msg}");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(serde_json::json!({
            "error": {"code": "internal", "message": msg}
        })),
    )
        .into_response()
}

/// `POST /api/admin/principals/:id/revoke` — controller-only path
/// to revoke trust on an existing principal without going through
/// the cold-contact flow. The principal's `TrustLevel` flips to
/// `Blocked`; future messages from them get 403 sender_blocked.
///
/// This is the explicit operator action for "I no longer trust X" —
/// distinct from `Block` via the approval flow (which targets a
/// specific cold-contact request). Use this for an *already
/// trusted* contact you want to remove.
#[utoipa::path(
    post,
    path = "/api/admin/principals/{principal_id}/revoke",
    params(
        ("principal_id" = String, Path, description = "Principal to flip to Blocked"),
    ),
    responses(
        (status = 200, description = "Trust revoked; principal now Blocked"),
        (status = 404, description = "Unknown principal id"),
    ),
    tag = "approvals"
)]
pub async fn revoke_handler(
    State(state): State<AppState>,
    Path(principal_id): Path<String>,
    Json(req): Json<RevokeRequest>,
) -> impl IntoResponse {
    let principals = PrincipalStore::new(&state.db);
    let pid = PrincipalId::from(principal_id.clone());
    let Ok(Some(_existing)) = principals.get(&pid) else {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": {
                    "code": "principal_not_found",
                    "message": format!("no principal with id '{principal_id}'"),
                }
            })),
        )
            .into_response();
    };

    let now = chrono::Utc::now().timestamp();
    let new_level = CoreTrustLevel::Blocked {
        blocked_by: PrincipalId::from("controller"),
        blocked_at: now,
        reason: req.reason.clone(),
    };
    if let Err(e) = principals.set_trust(&pid, new_level) {
        return internal_error(&format!("set_trust: {e}"));
    }

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "principal_id": principal_id,
            "new_trust_class": "Blocked",
            "outcome": "revoked",
        })),
    )
        .into_response()
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct RevokeRequest {
    #[serde(default)]
    pub reason: Option<String>,
}

/// `POST /api/admin/principals/:id/trust` — controller-only path to
/// change a principal's trust class out-of-band (i.e. not through the
/// cold-contact approval flow). Use this from Settings → Contacts to
/// promote a `KnownLimited` to `KnownTrusted`, demote a Trusted
/// principal to Limited (with optional topic scope), or Block them.
///
/// Distinct from `revoke` (which is the one-click "I'm done with this
/// person" path that always lands on Blocked): `trust` accepts a
/// target class so the operator can elevate as well as demote.
///
/// Restricted to the operator-mutable contact tiers — `Controller`,
/// `Delegated`, and `UnknownPending` cannot be set via this endpoint
/// because:
///   * `Controller` is the operator's own identity, minted at setup;
///     letting a SPA button demote it is a foot-gun with no upside.
///   * `Delegated` carries a capability scope + expiry that the
///     simple `{class}` body can't express; that flow needs its own
///     ceremony when we ship it.
///   * `UnknownPending` is a system state — going BACK to it from
///     an explicitly-trusted principal is incoherent.
///
/// Like `revoke_handler`, the trust transition mutates the principals
/// store directly without committing a `TrustChanged` event log entry
/// — that event variant is conversation-scoped and this path operates
/// on a principal in isolation. The trace span below is the audit
/// trail.
#[utoipa::path(
    post,
    path = "/api/admin/principals/{principal_id}/trust",
    params(
        ("principal_id" = String, Path, description = "Principal whose trust class to change"),
    ),
    responses(
        (status = 200, description = "Trust class updated"),
        (status = 400, description = "Target class is not operator-settable"),
        (status = 404, description = "Unknown principal id"),
    ),
    tag = "approvals"
)]
pub async fn set_trust_handler(
    State(state): State<AppState>,
    _user: crate::auth_extract::AuthedUser,
    Path(principal_id): Path<String>,
    Json(req): Json<SetTrustRequest>,
) -> impl IntoResponse {
    let principals = PrincipalStore::new(&state.db);
    let pid = PrincipalId::from(principal_id.clone());
    let Ok(Some(_existing)) = principals.get(&pid) else {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": {
                    "code": "principal_not_found",
                    "message": format!("no principal with id '{principal_id}'"),
                }
            })),
        )
            .into_response();
    };

    let now = chrono::Utc::now().timestamp();
    let new_level = match req.class.as_str() {
        "KnownTrusted" => CoreTrustLevel::KnownTrusted {
            resolvers: vec![],
            approved_by: PrincipalId::from("controller"),
            approved_at: now,
        },
        "KnownLimited" => CoreTrustLevel::KnownLimited {
            resolvers: vec![],
            allowed_topics: req.allowed_topics.clone().unwrap_or_default(),
            allowed_tools: None,
        },
        "Blocked" => CoreTrustLevel::Blocked {
            blocked_by: PrincipalId::from("controller"),
            blocked_at: now,
            reason: req.reason.clone(),
        },
        other => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": {
                        "code": "unsupported_class",
                        "message": format!(
                            "trust class '{other}' is not operator-settable; \
                             use one of: KnownTrusted, KnownLimited, Blocked"
                        ),
                    }
                })),
            )
                .into_response();
        }
    };

    let new_class_tag = new_level.class_tag().to_owned();
    if let Err(e) = principals.set_trust(&pid, new_level) {
        return internal_error(&format!("set_trust: {e}"));
    }

    tracing::info!(
        target: "approvals::set_trust",
        principal_id = %principal_id,
        new_class = %new_class_tag,
        reason = ?req.reason,
        "controller changed principal trust class",
    );

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "principal_id": principal_id,
            "new_trust_class": new_class_tag,
            "outcome": "trust_changed",
        })),
    )
        .into_response()
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct SetTrustRequest {
    /// Target trust class — must be one of `KnownTrusted`,
    /// `KnownLimited`, `Blocked`. Other values are rejected.
    pub class: String,
    /// Topic allowlist when `class == "KnownLimited"`. Ignored for
    /// other classes. Defaults to `[]` (no topic restrictions).
    #[serde(default)]
    pub allowed_topics: Option<Vec<String>>,
    /// Free-form operator note. Persisted on `Blocked` rows for
    /// future "why did I block this contact?" recall; logged on
    /// trace for the others.
    #[serde(default)]
    pub reason: Option<String>,
}

// ---------------------------------------------------------------------------
// List endpoints — read-only feeds for the SPA's settings pages.
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct PrincipalSummary {
    pub id: String,
    pub trust_class: String,
    pub display_name: Option<String>,
    pub first_seen: i64,
    pub last_seen: Option<i64>,
    pub identifiers: Vec<IdentifierSummary>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct IdentifierSummary {
    pub transport: String,
    pub handle: String,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct PrincipalListResponse {
    pub principals: Vec<PrincipalSummary>,
}

/// `GET /api/admin/principals` — every principal the system has seen,
/// ordered by first_seen ascending (Controller first since it was
/// minted at setup time).
#[utoipa::path(
    get,
    path = "/api/admin/principals",
    responses(
        (status = 200, description = "Principal summaries", body = PrincipalListResponse),
        (status = 401, description = "Missing or invalid Authorization header"),
    ),
    security(("bearer_jwt" = [])),
    tag = "approvals"
)]
pub async fn list_principals_handler(
    State(state): State<AppState>,
    _user: crate::auth_extract::AuthedUser,
) -> impl IntoResponse {
    let store = PrincipalStore::new(&state.db);
    let principals = match store.list_all() {
        Ok(p) => p,
        Err(e) => return internal_error(&format!("list_all: {e}")),
    };
    let summaries: Vec<PrincipalSummary> = principals
        .into_iter()
        .map(|p| PrincipalSummary {
            id: p.id.as_str().to_owned(),
            trust_class: p.trust_level.class_tag().to_owned(),
            display_name: p
                .metadata
                .get("display_name")
                .and_then(|v| v.as_str())
                .map(|s| s.to_owned()),
            first_seen: p.first_seen,
            last_seen: p.last_seen,
            identifiers: p
                .identifiers
                .into_iter()
                .map(|i| IdentifierSummary {
                    transport: i.transport,
                    handle: i.handle,
                })
                .collect(),
        })
        .collect();
    (
        StatusCode::OK,
        Json(serde_json::json!(PrincipalListResponse {
            principals: summaries
        })),
    )
        .into_response()
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct PendingApprovalSummary {
    pub kind: String,
    pub approval_id: String,
    pub conversation_id: String,
    pub sender_principal_id: String,
    pub original_text: String,
    pub scope: String,
    pub reason: String,
    /// A controller-readable description of the exact action being authorized.
    pub requested_action: String,
    /// Canonical, structured effect details derived from validated persisted
    /// arguments. The prose field remains for older clients only.
    #[serde(default)]
    pub canonical_actions: Vec<CanonicalActionPreview>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approval_token: Option<String>,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct CanonicalActionPreview {
    pub operation: String,
    pub target: Option<String>,
    pub changed_fields: Vec<CanonicalActionField>,
    pub reversible: bool,
    pub approval_scope: String,
}

#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct CanonicalActionField {
    pub name: String,
    pub value: String,
}

fn chain_effect_actions(plan_json: &[u8]) -> Vec<CanonicalActionPreview> {
    let Ok(plan) = serde_json::from_slice::<serde_json::Value>(plan_json) else {
        return Vec::new();
    };
    plan.get("steps")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter(|step| step.get("effect_kind").is_some_and(|kind| !kind.is_null()))
        .map(|step| {
            let operation = step
                .get("effect_kind")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown_effect")
                .to_owned();
            let payload = step.get("payload").and_then(serde_json::Value::as_object);
            let target = payload.and_then(|fields| {
                ["to", "target", "url", "path", "resource", "id"]
                    .iter()
                    .find_map(|key| fields.get(*key).map(|value| safe_action_value(key, value)))
            });
            let changed_fields = payload
                .into_iter()
                .flat_map(|fields| fields.iter())
                .map(|(name, value)| CanonicalActionField {
                    name: name.clone(),
                    value: safe_action_value(name, value),
                })
                .collect();
            CanonicalActionPreview {
                operation,
                target,
                changed_fields,
                reversible: false,
                approval_scope: "this exact persisted action".into(),
            }
        })
        .collect()
}

fn cold_contact_actions(principal_id: &str) -> Vec<CanonicalActionPreview> {
    [
        ("trust", "trust_principal", "KnownTrusted", true),
        (
            "limited trust",
            "trust_principal_with_topics",
            "KnownLimited",
            true,
        ),
        ("block", "block_principal", "Blocked", true),
        (
            "ignore once",
            "dismiss_pending_message",
            "UnknownPending",
            true,
        ),
        (
            "claim as me",
            "merge_into_controller_identity",
            "Controller",
            false,
        ),
    ]
    .into_iter()
    .map(
        |(choice, operation, trust, reversible)| CanonicalActionPreview {
            operation: operation.into(),
            target: Some(principal_id.into()),
            changed_fields: vec![
                CanonicalActionField {
                    name: "before_trust_class".into(),
                    value: "UnknownPending".into(),
                },
                CanonicalActionField {
                    name: "operator_choice".into(),
                    value: choice.into(),
                },
                CanonicalActionField {
                    name: "resulting_trust_class".into(),
                    value: trust.into(),
                },
                CanonicalActionField {
                    name: "parked_message".into(),
                    value: if matches!(choice, "trust" | "limited trust") {
                        "replayed after trust change".into()
                    } else if choice == "claim as me" {
                        "reconciled into Controller identity".into()
                    } else if choice == "block" {
                        "retained for audit; future messages denied".into()
                    } else {
                        "dismissed; future messages prompt again".into()
                    },
                },
            ],
            reversible,
            approval_scope: if choice == "limited trust" {
                "persistent trust grant limited by the topic scope entered by the Controller".into()
            } else if choice == "trust" || choice == "block" {
                "persistent principal trust change plus this parked message".into()
            } else {
                "this principal and parked message only".into()
            },
        },
    )
    .collect()
}

fn safe_action_value(name: &str, value: &serde_json::Value) -> String {
    let lower = name.to_ascii_lowercase();
    if ["secret", "token", "password", "api_key", "credential"]
        .iter()
        .any(|marker| lower.contains(marker))
    {
        return "[redacted]".into();
    }
    let rendered = match value {
        serde_json::Value::String(text) => text.clone(),
        _ => value.to_string(),
    };
    if !execlaw_policy::outbound::secret_indicators(&rendered).is_empty() {
        return "[redacted]".into();
    }
    if rendered.chars().count() > 512 {
        format!(
            "{}… [truncated]",
            rendered.chars().take(512).collect::<String>()
        )
    } else {
        rendered
    }
}

fn chain_effect_action(plan_json: &[u8]) -> String {
    let actions = chain_effect_actions(plan_json);
    serde_json::to_string_pretty(&actions).unwrap_or_else(|_| {
        "Effect details are unavailable; reject until the plan can be inspected.".into()
    })
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct PendingApprovalsResponse {
    pub approvals: Vec<PendingApprovalSummary>,
    pub memory_promotions: Vec<MemoryPromotionSummary>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct MemoryPromotionSummary {
    pub id: i64,
    pub scope: String,
    pub trust_class: String,
    pub key: String,
    pub from_tier: String,
    pub to_tier: String,
    pub reason: String,
    pub proposed_by: String,
    pub proposed_at: i64,
    pub evidence_ref: String,
}

impl From<PromotionProposal> for MemoryPromotionSummary {
    fn from(proposal: PromotionProposal) -> Self {
        let evidence_ref = format!(
            "memory://{}/{}/{}",
            proposal.scope, proposal.trust_class, proposal.key
        );
        Self {
            id: proposal.id,
            scope: proposal.scope,
            trust_class: proposal.trust_class,
            key: proposal.key,
            from_tier: serde_json::to_value(proposal.from_tier)
                .ok()
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_default(),
            to_tier: serde_json::to_value(proposal.to_tier)
                .ok()
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_default(),
            reason: serde_json::to_value(proposal.reason)
                .ok()
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_default(),
            proposed_by: serde_json::to_value(proposal.proposed_by)
                .ok()
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_default(),
            proposed_at: proposal.proposed_at,
            evidence_ref,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum MemoryPromotionDecisionRequest {
    Approve,
    Reject,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct MemoryPromotionDecisionBody {
    pub decision: MemoryPromotionDecisionRequest,
    #[serde(default)]
    pub note: Option<String>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct InformationDeclassificationBody {
    pub sha256: String,
    pub destination: String,
    pub scope: String,
}

#[utoipa::path(
    post,
    path = "/api/admin/information-labels/{subject_kind}/{subject_id}/declassify",
    params(
        ("subject_kind" = String, Path, description = "Host-owned information subject kind"),
        ("subject_id" = String, Path, description = "Host-owned information subject id")
    ),
    request_body = InformationDeclassificationBody,
    responses(
        (status = 200, description = "Destination-scoped declassification recorded"),
        (status = 403, description = "Caller is not a Controller"),
        (status = 404, description = "Information label not found")
    ),
    security(("bearer_jwt" = [])),
    tag = "approvals"
)]
pub async fn declassify_information_handler(
    State(state): State<AppState>,
    user: crate::auth_extract::AuthedUser,
    Path((subject_kind, subject_id)): Path<(String, String)>,
    Json(body): Json<InformationDeclassificationBody>,
) -> impl IntoResponse {
    if user.role != UserRole::Controller {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"error":{"code":"forbidden","message":"only Controllers can authorize information exports"}})),
        )
            .into_response();
    }
    let subject = execlaw_core::information_store::InformationSubject {
        kind: subject_kind,
        id: subject_id,
        sha256: body.sha256,
    };
    match execlaw_core::information_store::InformationLabelStore::new(&state.db).declassify(
        &subject,
        &user.user_id,
        &body.destination,
        &body.scope,
        chrono::Utc::now().timestamp(),
    ) {
        Ok(label) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "subject": subject,
                "destination": body.destination,
                "scope": body.scope,
                "actor_id": user.user_id,
                "authorized_at": label.declassification.as_ref().map(|record| record.authorized_at),
            })),
        )
            .into_response(),
        Err(execlaw_core::db::DbError::Invariant(message))
            if message == "information label does not exist" =>
        {
            (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error":{"code":"information_label_not_found","message":"information label not found"}})),
            )
                .into_response()
        }
        Err(error) => internal_error(&format!("information declassification: {error}")),
    }
}

#[utoipa::path(
    post,
    path = "/api/admin/memory-promotions/{proposal_id}/respond",
    params(("proposal_id" = i64, Path, description = "Pending memory promotion proposal")),
    request_body = MemoryPromotionDecisionBody,
    responses(
        (status = 200, description = "Memory tier proposal decided"),
        (status = 403, description = "Caller is not a Controller"),
        (status = 404, description = "Proposal not found"),
        (status = 409, description = "Proposal already decided")
    ),
    security(("bearer_jwt" = [])),
    tag = "approvals"
)]
pub async fn decide_memory_promotion_handler(
    State(state): State<AppState>,
    user: crate::auth_extract::AuthedUser,
    Path(proposal_id): Path<i64>,
    Json(body): Json<MemoryPromotionDecisionBody>,
) -> impl IntoResponse {
    if user.role != UserRole::Controller {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"error": {"code": "forbidden", "message": "only Controllers can decide memory promotions"}})),
        )
            .into_response();
    }
    let store = PromotionStore::new(&state.db);
    let now = chrono::Utc::now().timestamp();
    let result = match body.decision {
        MemoryPromotionDecisionRequest::Approve => {
            store.approve(proposal_id, now, body.note.as_deref())
        }
        MemoryPromotionDecisionRequest::Reject => {
            store.reject(proposal_id, now, body.note.as_deref())
        }
    };
    match result {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({"proposal_id": proposal_id, "decision": match body.decision { MemoryPromotionDecisionRequest::Approve => "approved", MemoryPromotionDecisionRequest::Reject => "rejected" }}))).into_response(),
        Err(execlaw_core::memory_lifecycle::LifecycleError::NotFound(_)) => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": {"code": "promotion_not_found", "message": "memory promotion proposal not found"}})),
        ).into_response(),
        Err(execlaw_core::memory_lifecycle::LifecycleError::AlreadyDecided(_)) => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({"error": {"code": "promotion_already_decided", "message": "memory promotion proposal was already decided"}})),
        ).into_response(),
        Err(execlaw_core::memory_lifecycle::LifecycleError::StaleTarget(_)) => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({"error": {"code": "promotion_stale", "message": "memory tier changed after this proposal was created; refresh the queue"}})),
        ).into_response(),
        Err(error) => internal_error(&format!("memory promotion decision: {error}")),
    }
}

/// `GET /api/admin/approvals` — every cold-contact arrival whose
/// sender is still `UnknownPending`. Linear scan via state_events;
/// acceptable while approval volume is low (Phase-3 scope).
#[utoipa::path(
    get,
    path = "/api/admin/approvals",
    responses(
        (status = 200, description = "Pending approvals", body = PendingApprovalsResponse),
        (status = 401, description = "Missing or invalid Authorization header"),
    ),
    security(("bearer_jwt" = [])),
    tag = "approvals"
)]
pub async fn list_pending_approvals_handler(
    State(state): State<AppState>,
    user: crate::auth_extract::AuthedUser,
) -> impl IntoResponse {
    let principals = PrincipalStore::new(&state.db);

    // Pull every distinct conversation that has at least one
    // cold_contact_arrived event.
    let conv_ids: Vec<String> = match state.db.with_conn(|c| {
        let mut stmt = c
            .prepare(
                "SELECT DISTINCT conversation_id FROM state_events \
                 WHERE kind = 'cold_contact_arrived'",
            )
            .map_err(execlaw_core::db::DbError::from)?;
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(execlaw_core::db::DbError::from)?;
        let out: Result<Vec<_>, _> = rows.collect();
        Ok(out?)
    }) {
        Ok(v) => v,
        Err(e) => return internal_error(&format!("conv scan: {e}")),
    };

    let log = event_log(&state);
    let mut approvals: Vec<PendingApprovalSummary> = Vec::new();
    for cid_str in conv_ids {
        let cid = ConversationId::from(cid_str.clone());
        let events = match log.replay_since(&cid, EventSeq(0)) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for ev in events {
            if ev.kind != EventKind::ColdContactArrived {
                continue;
            }
            let Ok(p) = ev.decode_payload::<ColdContactReplayPayload>() else {
                continue;
            };
            // Filter out approvals that are no longer actionable:
            //   * principal still exists AND is UnknownPending → pending
            //   * principal still exists AND is anything else → resolved
            //     (operator already approved/blocked, or trust changed
            //     via another path)
            //   * principal MISSING → reconciled away. Pre-fix this
            //     defaulted to `pending=true`, so a cold-contact whose
            //     principal got merged into the controller via the
            //     My-identities reconcile flow kept appearing as a
            //     phantom approval — clicking any verb errored
            //     because the principal was gone, leaving the
            //     operator stuck. The right answer is to drop it:
            //     a vanished principal IS the resolution.
            let principal_state = principals
                .get(&PrincipalId::from(p.sender_principal_id.clone()))
                .ok()
                .flatten();
            let still_pending = match principal_state {
                Some(principal) => {
                    matches!(principal.trust_level, CoreTrustLevel::UnknownPending { .. })
                }
                None => false,
            };
            if !still_pending {
                continue;
            }
            approvals.push(PendingApprovalSummary {
                kind: "cold_contact".into(),
                approval_id: p.approval_id,
                conversation_id: cid_str.clone(),
                sender_principal_id: p.sender_principal_id.clone(),
                original_text: p.text,
                scope: format!("Trust record for principal {}", p.sender_principal_id),
                reason: "A new sender has no approved trust level.".into(),
                requested_action: "Choose a trust decision. Trust or limited trust also replays the queued first message; Block prevents future messages; Ignore once only dismisses this message.".into(),
                canonical_actions: cold_contact_actions(&p.sender_principal_id),
                approval_token: None,
            });
        }
    }

    // Bridge phase-2 tool-chain approvals into the same feed so the
    // operator can act from one approvals surface.
    if user.role == UserRole::Controller {
        if let Ok(chain_rows) = state.db.with_conn(|c| {
            let mut stmt = c.prepare(
                "SELECT r.approval_id, r.conversation_id, r.plan_id, p.objective, \
                 p.plan_json, r.approval_effect_hash \
             FROM state_chain_runs r \
             JOIN state_chain_plans p ON p.id = r.plan_id \
             WHERE r.status = 'awaiting_approval' \
               AND r.approval_id IS NOT NULL",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, Vec<u8>>(4)?,
                    r.get::<_, Option<String>>(5)?,
                ))
            })?;
            let out: Result<Vec<(String, String, String, String, Vec<u8>, Option<String>)>, _> =
                rows.collect();
            Ok(out?)
        }) {
            for (approval_id, conversation_id, plan_id, objective, plan_json, effect_hash) in
                chain_rows
            {
                let cid = ConversationId::from(conversation_id.clone());
                let approval_token = effect_hash.map(|hash| {
                    issue_approval_token(
                        &state.signer,
                        &approval_id,
                        &cid,
                        "effectful_chain",
                        Some(user.user_id.clone()),
                        Some(hash),
                    )
                });
                let canonical_actions = chain_effect_actions(&plan_json);
                approvals.push(PendingApprovalSummary {
                    kind: "effectful_chain".into(),
                    approval_id,
                    conversation_id,
                    sender_principal_id: "tool-chain".to_string(),
                    original_text: format!("Tool-chain execution awaiting approval: {objective}"),
                    scope: format!("External effect set in saved plan {plan_id}"),
                    reason: "The plan contains external effects that require Controller approval."
                        .into(),
                    requested_action: chain_effect_action(&plan_json),
                    canonical_actions,
                    approval_token,
                });
            }
        }
    }

    let memory_promotions = if user.role == UserRole::Controller {
        match PromotionStore::new(&state.db).list_pending(100) {
            Ok(rows) => rows.into_iter().map(MemoryPromotionSummary::from).collect(),
            Err(error) => return internal_error(&format!("memory promotion scan: {error}")),
        }
    } else {
        Vec::new()
    };

    (
        StatusCode::OK,
        Json(serde_json::json!(PendingApprovalsResponse {
            approvals,
            memory_promotions
        })),
    )
        .into_response()
}

/// Sub-router mounted at `/api/admin/approvals/...` and
/// `/api/admin/principals/.../revoke`.
pub fn approvals_router() -> Router<AppState> {
    Router::new()
        .route("/api/admin/approvals", get(list_pending_approvals_handler))
        .route(
            "/api/admin/information-labels/{subject_kind}/{subject_id}/declassify",
            post(declassify_information_handler),
        )
        .route("/api/admin/principals", get(list_principals_handler))
        .route(
            "/api/admin/approvals/{approval_id}/respond",
            post(respond_handler),
        )
        .route(
            "/api/admin/memory-promotions/{proposal_id}/respond",
            post(decide_memory_promotion_handler),
        )
        .route(
            "/api/admin/principals/{principal_id}/revoke",
            post(revoke_handler),
        )
        .route(
            "/api/admin/principals/{principal_id}/trust",
            post(set_trust_handler),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes::{build_router, test_app_state};
    use axum::body::{self, Body};
    use axum::http::{Method, Request, header};
    use tower::ServiceExt;

    #[test]
    fn chain_approval_summary_contains_the_persisted_effect_action() {
        let plan = serde_json::json!({
            "steps": [
                {"label": "inspect", "effect_kind": null, "payload": {"path": "report.md"}},
                {"label": "send report", "effect_kind": "email.send", "payload": {"to": "operator@example.test", "subject": "Weekly report"}}
            ]
        });
        let actions = chain_effect_actions(&serde_json::to_vec(&plan).unwrap());
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].operation, "email.send");
        assert_eq!(actions[0].target.as_deref(), Some("operator@example.test"));
        assert!(
            actions[0]
                .changed_fields
                .iter()
                .any(|field| field.name == "to" && field.value == "operator@example.test")
        );
        assert!(!actions[0].reversible);
    }

    #[test]
    fn canonical_approval_fields_ignore_model_prose_and_redact_credential_arguments() {
        let plan = serde_json::json!({
            "objective": "Nothing will be sent",
            "steps": [{
                "effect_kind": "email.send",
                "payload": {
                    "to": "external@example.test",
                    "subject": "Report",
                    "api_key": "synthetic-secret-value"
                }
            }]
        });
        let actions = chain_effect_actions(&serde_json::to_vec(&plan).unwrap());
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].operation, "email.send");
        assert_eq!(actions[0].target.as_deref(), Some("external@example.test"));
        let key = actions[0]
            .changed_fields
            .iter()
            .find(|field| field.name == "api_key")
            .unwrap();
        assert_eq!(key.value, "[redacted]");
        assert!(
            !serde_json::to_string(&actions)
                .unwrap()
                .contains("synthetic-secret-value")
        );
    }

    #[test]
    fn cold_contact_approval_lists_persistent_trust_actions_and_their_scope() {
        let actions = cold_contact_actions("principal-1");
        assert_eq!(actions.len(), 5);
        assert!(
            actions
                .iter()
                .all(|action| action.target.as_deref() == Some("principal-1"))
        );
        let limited = actions
            .iter()
            .find(|action| action.operation == "trust_principal_with_topics")
            .unwrap();
        assert!(limited.approval_scope.contains("persistent trust grant"));
        assert!(
            limited.changed_fields.iter().any(|field| {
                field.name == "parked_message" && field.value.contains("replayed")
            })
        );
    }

    #[tokio::test]
    async fn only_controller_can_record_scoped_information_declassification() {
        let state = crate::routes::test_app_state();
        let subject = execlaw_core::information_store::InformationSubject {
            kind: "artifact".into(),
            id: "declassify-route-artifact".into(),
            sha256: "a".repeat(64),
        };
        let label = execlaw_core::information::InformationLabel::observed(
            execlaw_core::information::Sensitivity::Sensitive,
            Some("owner-1".into()),
            "Controller",
            "artifact",
            "artifact-1",
            std::iter::empty(),
        );
        execlaw_core::information_store::InformationLabelStore::new(&state.db)
            .observe(&subject, &label, "host:test", 1)
            .unwrap();
        let operator = crate::auth_extract::AuthedUser {
            user_id: "operator-1".into(),
            session_id: None,
            username: "operator".into(),
            display_name: "Operator".into(),
            email: None,
            role: UserRole::Operator,
            last_login_at: None,
        };
        let body = || InformationDeclassificationBody {
            sha256: subject.sha256.clone(),
            destination: "transport:signal:alice".into(),
            scope: "case-17".into(),
        };
        let denied = declassify_information_handler(
            State(state.clone()),
            operator,
            Path((subject.kind.clone(), subject.id.clone())),
            Json(body()),
        )
        .await
        .into_response();
        assert_eq!(denied.status(), StatusCode::FORBIDDEN);

        let controller = crate::auth_extract::AuthedUser {
            user_id: "controller-1".into(),
            session_id: None,
            username: "controller".into(),
            display_name: "Controller".into(),
            email: None,
            role: UserRole::Controller,
            last_login_at: None,
        };
        let accepted = declassify_information_handler(
            State(state.clone()),
            controller,
            Path((subject.kind.clone(), subject.id.clone())),
            Json(body()),
        )
        .await
        .into_response();
        assert_eq!(accepted.status(), StatusCode::OK);
        let audit: (String, String) = state
            .db
            .with_conn(|connection| {
                Ok(connection.query_row(
                    "SELECT actor_id,scope FROM state_information_label_events ORDER BY rowid DESC LIMIT 1",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )?)
            })
            .unwrap();
        assert_eq!(audit, ("controller-1".into(), "case-17".into()));
    }

    async fn setup_get_token(app: &axum::Router) -> String {
        let body = serde_json::to_vec(&serde_json::json!({
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

    async fn read_json(
        app: &axum::Router,
        token: Option<&str>,
        uri: &str,
    ) -> (StatusCode, serde_json::Value) {
        let mut req = Request::builder().method(Method::GET).uri(uri);
        if let Some(t) = token {
            req = req.header(header::AUTHORIZATION, format!("Bearer {t}"));
        }
        let resp = app
            .clone()
            .oneshot(req.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = resp.status();
        let bytes = body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let v: serde_json::Value =
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, v)
    }

    #[tokio::test]
    async fn list_principals_requires_auth() {
        let app = build_router(test_app_state());
        let (status, _) = read_json(&app, None, "/api/admin/principals").await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn list_principals_returns_empty_on_fresh_db() {
        let app = build_router(test_app_state());
        let token = setup_get_token(&app).await;
        let (status, body) = read_json(&app, Some(&token), "/api/admin/principals").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body["principals"].is_array());
        assert_eq!(body["principals"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn list_pending_approvals_requires_auth() {
        let app = build_router(test_app_state());
        let (status, _) = read_json(&app, None, "/api/admin/approvals").await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn list_pending_approvals_returns_empty_on_fresh_db() {
        let app = build_router(test_app_state());
        let token = setup_get_token(&app).await;
        let (status, body) = read_json(&app, Some(&token), "/api/admin/approvals").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["approvals"].as_array().unwrap().len(), 0);
        assert_eq!(body["memory_promotions"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn memory_promotion_is_listed_and_controller_can_reject_it() {
        use execlaw_core::memory::{MemoryEntry, MemoryStore, MemoryTier};
        use execlaw_core::memory_lifecycle::{
            PromotionDecision, PromotionReason, PromotionStore, ProposedBy,
        };

        let state = test_app_state();
        let app = build_router(state.clone());
        let token = setup_get_token(&app).await;
        MemoryStore::new(&state.db)
            .upsert(&MemoryEntry {
                scope: "global".into(),
                trust_class: "Controller".into(),
                key: "preferred_language".into(),
                value_blob: b"English".to_vec(),
                ttl_expires: None,
                updated_at: 100,
                tier: MemoryTier::Warm,
                hits: 4,
                last_used_at: Some(100),
                created_at: 100,
            })
            .unwrap();
        let proposal_id = PromotionStore::new(&state.db)
            .propose(
                "global",
                "Controller",
                "preferred_language",
                MemoryTier::Warm,
                MemoryTier::Hot,
                PromotionReason::Frequency,
                ProposedBy::Sweeper,
                100,
            )
            .unwrap();

        let (status, feed) = read_json(&app, Some(&token), "/api/admin/approvals").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(feed["memory_promotions"][0]["id"], proposal_id);
        assert_eq!(feed["memory_promotions"][0]["key"], "preferred_language");
        assert_eq!(
            feed["memory_promotions"][0]["evidence_ref"],
            "memory://global/Controller/preferred_language"
        );

        let operator = crate::auth_extract::AuthedUser {
            user_id: "operator-1".into(),
            session_id: None,
            username: "operator".into(),
            display_name: "Operator".into(),
            email: None,
            role: UserRole::Operator,
            last_login_at: None,
        };
        let operator_response = list_pending_approvals_handler(State(state.clone()), operator)
            .await
            .into_response();
        let operator_body = body::to_bytes(operator_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let operator_json: serde_json::Value = serde_json::from_slice(&operator_body).unwrap();
        assert!(
            operator_json["memory_promotions"]
                .as_array()
                .unwrap()
                .is_empty()
        );

        let operator = crate::auth_extract::AuthedUser {
            user_id: "operator-1".into(),
            session_id: None,
            username: "operator".into(),
            display_name: "Operator".into(),
            email: None,
            role: UserRole::Operator,
            last_login_at: None,
        };
        let denied = decide_memory_promotion_handler(
            State(state.clone()),
            operator,
            Path(proposal_id),
            Json(MemoryPromotionDecisionBody {
                decision: MemoryPromotionDecisionRequest::Approve,
                note: None,
            }),
        )
        .await
        .into_response();
        assert_eq!(denied.status(), StatusCode::FORBIDDEN);
        assert!(
            PromotionStore::new(&state.db)
                .get(proposal_id)
                .unwrap()
                .unwrap()
                .decided_at
                .is_none()
        );

        let (status, _) = post_json(
            &app,
            Some(&token),
            &format!("/api/admin/memory-promotions/{proposal_id}/respond"),
            serde_json::json!({"decision": "reject", "note": "not useful"}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let proposal = PromotionStore::new(&state.db)
            .get(proposal_id)
            .unwrap()
            .unwrap();
        assert!(proposal.decided_at.is_some());
        assert_eq!(proposal.decision, Some(PromotionDecision::Rejected));
    }

    #[tokio::test]
    async fn list_pending_filters_out_events_whose_principal_was_reconciled_away() {
        // Regression: when reconcile (My-identities flow) deleted a
        // stale UnknownPending principal, the cold_contact_arrived
        // events for it stayed in the immutable event log. The
        // pre-fix list filter defaulted "missing principal" to
        // pending=true, so these phantoms kept appearing in the
        // approvals list and the SPA's "This is me" / Trust /
        // Block clicks all errored because the principal was gone.
        // Post-fix: a vanished principal IS the resolution; drop
        // the event from the list.
        use execlaw_core::events::{EventKind, PendingEvent};
        let state = test_app_state();
        let app = build_router(state.clone());
        let token = setup_get_token(&app).await;

        // Seed a cold_contact_arrived event whose principal_id
        // points at a row that doesn't exist (simulates post-
        // reconcile state).
        let cid = ConversationId::from("conv-phantom".to_owned());
        let payload = ColdContactReplayPayload {
            text: "Good morning".to_owned(),
            sender_principal_id: "pri_signal_+15551234567".to_owned(),
            approval_id: "appr-phantom-1".to_owned(),
        };
        let pending = PendingEvent::encode(EventKind::ColdContactArrived, &payload, None).unwrap();
        let log = event_log(&state);
        log.commit_turn(&cid, EventSeq(0), vec![pending]).unwrap();

        // Verify the principal IS missing (no upsert; the test
        // simulates "reconcile already deleted it").
        let principals = PrincipalStore::new(&state.db);
        assert!(
            principals
                .get(&PrincipalId::from(payload.sender_principal_id.clone()))
                .unwrap()
                .is_none()
        );

        // Pending-list endpoint must NOT surface this event.
        let (status, body) = read_json(&app, Some(&token), "/api/admin/approvals").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body["approvals"].as_array().unwrap().len(),
            0,
            "phantom approval (principal deleted by reconcile) must not appear in pending list"
        );
    }

    #[tokio::test]
    async fn list_pending_keeps_events_whose_principal_is_unknown_pending() {
        // Counterpart to the phantom-filter test: when the principal
        // EXISTS and is genuinely UnknownPending, the cold-contact
        // event should appear in the pending list.
        use execlaw_core::events::{EventKind, PendingEvent};
        use execlaw_core::principal::{Principal, TrustLevel as CoreTrustLevel};

        let state = test_app_state();
        let app = build_router(state.clone());
        let token = setup_get_token(&app).await;

        let principals = PrincipalStore::new(&state.db);
        let pid = PrincipalId::from("pri_signal_+15559998888");
        principals
            .upsert(&Principal {
                id: pid.clone(),
                identifiers: Vec::new(),
                trust_level: CoreTrustLevel::UnknownPending {
                    first_seen: 0,
                    notification_event_seq: None,
                },
                resolved_by: Vec::new(),
                metadata: serde_json::json!({}),
                first_seen: 0,
                last_seen: None,
                controller_notes: None,
            })
            .unwrap();

        let cid = ConversationId::from("conv-real".to_owned());
        let payload = ColdContactReplayPayload {
            text: "hi can we chat".to_owned(),
            sender_principal_id: pid.as_str().to_owned(),
            approval_id: "appr-real-1".to_owned(),
        };
        let pending = PendingEvent::encode(EventKind::ColdContactArrived, &payload, None).unwrap();
        let log = event_log(&state);
        log.commit_turn(&cid, EventSeq(0), vec![pending]).unwrap();

        let (status, body) = read_json(&app, Some(&token), "/api/admin/approvals").await;
        assert_eq!(status, StatusCode::OK);
        let arr = body["approvals"].as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["approval_id"], "appr-real-1");
    }

    // ---- set_trust_handler ----------------------------------------

    async fn post_json(
        app: &axum::Router,
        token: Option<&str>,
        uri: &str,
        body_value: serde_json::Value,
    ) -> (StatusCode, serde_json::Value) {
        let mut req = Request::builder()
            .method(Method::POST)
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/json");
        if let Some(t) = token {
            req = req.header(header::AUTHORIZATION, format!("Bearer {t}"));
        }
        let resp = app
            .clone()
            .oneshot(
                req.body(Body::from(serde_json::to_vec(&body_value).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = resp.status();
        let bytes = body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let v: serde_json::Value =
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, v)
    }

    fn seed_principal(state: &AppState, id: &str, level: CoreTrustLevel) -> PrincipalId {
        use execlaw_core::principal::Principal;
        let pid = PrincipalId::from(id);
        PrincipalStore::new(&state.db)
            .upsert(&Principal {
                id: pid.clone(),
                identifiers: Vec::new(),
                trust_level: level,
                resolved_by: Vec::new(),
                metadata: serde_json::json!({}),
                first_seen: 0,
                last_seen: None,
                controller_notes: None,
            })
            .unwrap();
        pid
    }

    #[tokio::test]
    async fn set_trust_requires_auth() {
        let app = build_router(test_app_state());
        let (status, _) = post_json(
            &app,
            None,
            "/api/admin/principals/pri_x/trust",
            serde_json::json!({"class": "KnownTrusted"}),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn set_trust_404s_unknown_principal() {
        let state = test_app_state();
        let app = build_router(state.clone());
        let token = setup_get_token(&app).await;
        let (status, body) = post_json(
            &app,
            Some(&token),
            "/api/admin/principals/pri_does_not_exist/trust",
            serde_json::json!({"class": "KnownTrusted"}),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"]["code"], "principal_not_found");
    }

    #[tokio::test]
    async fn set_trust_elevates_limited_to_trusted() {
        // The primary user-facing path: a KnownLimited contact in
        // Settings → Contacts gets bumped to KnownTrusted.
        let state = test_app_state();
        let app = build_router(state.clone());
        let token = setup_get_token(&app).await;
        let pid = seed_principal(
            &state,
            "pri_signal_+15551111111",
            CoreTrustLevel::KnownLimited {
                resolvers: Vec::new(),
                allowed_topics: vec!["scheduling".into()],
                allowed_tools: None,
            },
        );

        let (status, body) = post_json(
            &app,
            Some(&token),
            &format!("/api/admin/principals/{}/trust", pid.as_str()),
            serde_json::json!({"class": "KnownTrusted"}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body: {body}");
        assert_eq!(body["new_trust_class"], "KnownTrusted");

        // Read-back: the store actually flipped.
        let principals = PrincipalStore::new(&state.db);
        let p = principals.get(&pid).unwrap().unwrap();
        assert_eq!(p.trust_level.class_tag(), "KnownTrusted");
    }

    #[tokio::test]
    async fn set_trust_demotes_to_limited_with_topic_scope() {
        let state = test_app_state();
        let app = build_router(state.clone());
        let token = setup_get_token(&app).await;
        let pid = seed_principal(
            &state,
            "pri_signal_+15552222222",
            CoreTrustLevel::KnownTrusted {
                resolvers: Vec::new(),
                approved_by: PrincipalId::from("controller"),
                approved_at: 0,
            },
        );

        let (status, body) = post_json(
            &app,
            Some(&token),
            &format!("/api/admin/principals/{}/trust", pid.as_str()),
            serde_json::json!({
                "class": "KnownLimited",
                "allowed_topics": ["scheduling", "logistics"],
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body: {body}");

        let principals = PrincipalStore::new(&state.db);
        let p = principals.get(&pid).unwrap().unwrap();
        match &p.trust_level {
            CoreTrustLevel::KnownLimited { allowed_topics, .. } => {
                assert_eq!(
                    allowed_topics,
                    &vec!["scheduling".to_owned(), "logistics".to_owned()],
                );
            }
            other => panic!("expected KnownLimited, got {:?}", other.class_tag()),
        }
    }

    #[tokio::test]
    async fn set_trust_blocks_with_reason() {
        let state = test_app_state();
        let app = build_router(state.clone());
        let token = setup_get_token(&app).await;
        let pid = seed_principal(
            &state,
            "pri_signal_+15553333333",
            CoreTrustLevel::KnownTrusted {
                resolvers: Vec::new(),
                approved_by: PrincipalId::from("controller"),
                approved_at: 0,
            },
        );

        let (status, _) = post_json(
            &app,
            Some(&token),
            &format!("/api/admin/principals/{}/trust", pid.as_str()),
            serde_json::json!({"class": "Blocked", "reason": "spam after vacation"}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let principals = PrincipalStore::new(&state.db);
        let p = principals.get(&pid).unwrap().unwrap();
        match &p.trust_level {
            CoreTrustLevel::Blocked { reason, .. } => {
                assert_eq!(reason.as_deref(), Some("spam after vacation"));
            }
            other => panic!("expected Blocked, got {:?}", other.class_tag()),
        }
    }

    #[tokio::test]
    async fn set_trust_rejects_non_settable_classes() {
        // Controller / Delegated / UnknownPending are NOT valid
        // targets — guard the operator against the obvious foot-gun.
        let state = test_app_state();
        let app = build_router(state.clone());
        let token = setup_get_token(&app).await;
        let pid = seed_principal(
            &state,
            "pri_signal_+15554444444",
            CoreTrustLevel::KnownLimited {
                resolvers: Vec::new(),
                allowed_topics: Vec::new(),
                allowed_tools: None,
            },
        );

        for forbidden in ["Controller", "Delegated", "UnknownPending", "Garbage"] {
            let (status, body) = post_json(
                &app,
                Some(&token),
                &format!("/api/admin/principals/{}/trust", pid.as_str()),
                serde_json::json!({"class": forbidden}),
            )
            .await;
            assert_eq!(
                status,
                StatusCode::BAD_REQUEST,
                "class {forbidden} must be rejected",
            );
            assert_eq!(body["error"]["code"], "unsupported_class");
        }

        // And the store is unchanged.
        let principals = PrincipalStore::new(&state.db);
        let p = principals.get(&pid).unwrap().unwrap();
        assert_eq!(p.trust_level.class_tag(), "KnownLimited");
    }
}
