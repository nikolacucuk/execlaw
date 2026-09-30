//! Controller API for durable chat steering, queueing, pause, and cancel.

use crate::auth_extract::AuthedUser;
use crate::routes::ApiError;
use crate::state::AppState;
use axum::{
    Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::Json,
    routing::post,
};
use execlaw_core::{
    turn_controls::{TurnControlKind, TurnControlRecord, TurnControlStatus, TurnControlStore},
    users::UserRole,
};
use serde::Deserialize;
use utoipa::ToSchema;

#[derive(Debug, Deserialize, ToSchema)]
pub struct SubmitTurnControl {
    pub kind: String,
    #[serde(default)]
    pub text: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct TurnControlCursor {
    #[serde(default)]
    pub after_created_at: i64,
}

pub fn router() -> Router<AppState> {
    Router::new().route(
        "/api/chats/{conversation_id}/controls",
        post(submit).get(list),
    )
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

#[utoipa::path(
    post,
    path = "/api/chats/{conversation_id}/controls",
    params(
        ("conversation_id" = String, Path, description = "Conversation control target"),
        ("Idempotency-Key" = String, Header, description = "Conversation-scoped durable control request identifier"),
    ),
    request_body = SubmitTurnControl,
    responses((status = 200, description = "Durable turn control accepted", body = serde_json::Value)),
    security(("bearer_jwt" = [])),
    tag = "chats"
)]
pub async fn submit(
    State(state): State<AppState>,
    user: AuthedUser,
    Path(conversation_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<SubmitTurnControl>,
) -> Result<Json<TurnControlRecord>, ApiError> {
    require_controller(&user)?;
    let kind = TurnControlKind::parse(&request.kind).ok_or_else(|| ApiError {
        status: StatusCode::BAD_REQUEST,
        code: "turn_control_kind_invalid",
        message: "kind must be queue_next_turn, steer, pause, resume, or cancel".into(),
    })?;
    let idempotency_key = headers
        .get("Idempotency-Key")
        .and_then(|value| value.to_str().ok())
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 128
                && value.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
        })
        .ok_or_else(|| ApiError {
            status: StatusCode::BAD_REQUEST,
            code: "turn_control_idempotency_required",
            message: "a visible-ASCII Idempotency-Key of 1 to 128 bytes is required".into(),
        })?;
    let active = state.turn_cancel.active_turn(&conversation_id);
    let live_runner = active
        .as_ref()
        .and_then(|turn| Some((turn.turn_id.as_deref()?, turn.group_id.as_deref()?)));
    let payload = match kind {
        TurnControlKind::QueueNextTurn | TurnControlKind::Steer => {
            let text = request.text.as_deref().unwrap_or_default();
            if text.trim().is_empty() || text.len() > 16 * 1024 {
                return Err(ApiError {
                    status: StatusCode::BAD_REQUEST,
                    code: "control_text_invalid",
                    message: "control text must contain 1 to 16384 bytes".into(),
                });
            }
            serde_json::json!({"text": text})
        }
        TurnControlKind::Pause | TurnControlKind::Resume | TurnControlKind::Cancel => {
            serde_json::json!({})
        }
    };
    if matches!(
        kind,
        TurnControlKind::Steer | TurnControlKind::Pause | TurnControlKind::Resume
    ) && live_runner.is_none()
    {
        return Err(ApiError {
            status: StatusCode::CONFLICT,
            code: "runner_control_unavailable",
            message: "this operation requires an active runner-mediated turn".into(),
        });
    }
    let turn_id = live_runner.map(|(turn_id, _)| turn_id);
    let store = TurnControlStore::new(&state.db);
    let (record, created) = store
        .enqueue_idempotent(
            &conversation_id,
            turn_id,
            kind,
            &payload,
            idempotency_key,
            chrono::Utc::now().timestamp(),
        )
        .map_err(map_store)?;
    if !created && record.status != TurnControlStatus::Accepted {
        return Ok(Json(record));
    }
    if !created && kind != TurnControlKind::QueueNextTurn && record.turn_id.as_deref() != turn_id {
        return Err(ApiError {
            status: StatusCode::CONFLICT,
            code: "turn_control_target_changed",
            message: "the original active turn is no longer available for this accepted control"
                .into(),
        });
    }
    if kind == TurnControlKind::QueueNextTurn {
        let text = payload["text"].as_str().unwrap_or_default();
        let seq = match crate::chats::append_queued_turn_message(
            &state,
            &execlaw_core::ConversationId::from(conversation_id.as_str()),
            text,
            &record.control_id,
        ) {
            Ok(seq) => seq,
            Err(error) => {
                if let Some(applied) = store
                    .get(&record.control_id)
                    .map_err(map_store)?
                    .filter(|control| control.status == TurnControlStatus::Applied)
                {
                    return Ok(Json(applied));
                }
                return Err(ApiError {
                    status: StatusCode::INTERNAL_SERVER_ERROR,
                    code: "queued_message_persist_failed",
                    message: error,
                });
            }
        };
        let applied = store
            .get(&record.control_id)
            .map_err(map_store)?
            .ok_or_else(|| ApiError {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                code: "queued_control_missing",
                message: "queued control disappeared after event commit".into(),
            })?;
        state
            .events
            .publish(crate::events::UiEvent::ChatMessageInbound {
                conversation_id: conversation_id.clone(),
                seq,
                text: text.to_owned(),
                sender: Some("controller".into()),
            });
        return Ok(Json(applied));
    }
    if kind == TurnControlKind::Cancel && live_runner.is_none() {
        if active.is_none() {
            return Err(ApiError {
                status: StatusCode::CONFLICT,
                code: "no_active_turn",
                message: "there is no active turn to cancel".into(),
            });
        }
        state.turn_cancel.cancel(&conversation_id);
        let acknowledged = store
            .transition(
                &record.control_id,
                TurnControlStatus::Acknowledged,
                Some(&serde_json::json!({"source":"local cancellation flag armed"})),
                chrono::Utc::now().timestamp(),
            )
            .map_err(map_store)?;
        return Ok(Json(acknowledged));
    }
    let Some((turn_id, group_id)) = live_runner else {
        return Err(ApiError {
            status: StatusCode::CONFLICT,
            code: "runner_control_unavailable",
            message: "active runner turn ended before dispatch".into(),
        });
    };
    let supervisor = state.runner_supervisor.as_ref().ok_or_else(|| ApiError {
        status: StatusCode::SERVICE_UNAVAILABLE,
        code: "runner_unavailable",
        message: "runner supervisor is unavailable".into(),
    })?;
    if !store
        .claim_delivery(&record.control_id, chrono::Utc::now().timestamp())
        .map_err(map_store)?
    {
        return store
            .get(&record.control_id)
            .map_err(map_store)?
            .map(Json)
            .ok_or_else(|| ApiError {
                status: StatusCode::NOT_FOUND,
                code: "turn_control_not_found",
                message: "turn control not found".into(),
            });
    }
    let sent = match kind {
        TurnControlKind::Steer => {
            supervisor
                .steer_turn(
                    group_id,
                    turn_id,
                    &record.control_id,
                    payload["text"].as_str().unwrap_or_default(),
                )
                .await
        }
        TurnControlKind::Pause => {
            supervisor
                .pause_turn(group_id, turn_id, &record.control_id)
                .await
        }
        TurnControlKind::Resume => {
            supervisor
                .resume_turn(group_id, turn_id, &record.control_id)
                .await
        }
        TurnControlKind::Cancel => {
            state.turn_cancel.cancel(&conversation_id);
            supervisor
                .cancel_turn_with_control(group_id, turn_id, &record.control_id)
                .await
        }
        TurnControlKind::QueueNextTurn => unreachable!(),
    };
    if !sent {
        let _ = store.transition(
            &record.control_id,
            TurnControlStatus::Failed,
            Some(&serde_json::json!({"reason":"runner delivery failed"})),
            chrono::Utc::now().timestamp(),
        );
        return Err(ApiError {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "runner_control_delivery_failed",
            message: "control was persisted but could not be delivered to the active runner".into(),
        });
    }
    let current = store
        .get(&record.control_id)
        .map_err(map_store)?
        .ok_or_else(|| ApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "turn_control_missing",
            message: "control disappeared after runner delivery".into(),
        })?;
    let current = if current.status == TurnControlStatus::Accepted {
        store
            .transition(
                &record.control_id,
                TurnControlStatus::Delivered,
                None,
                chrono::Utc::now().timestamp(),
            )
            .map_err(map_store)?
    } else {
        current
    };
    Ok(Json(current))
}

#[utoipa::path(
    get,
    path = "/api/chats/{conversation_id}/controls",
    params(("conversation_id" = String, Path, description = "Conversation control history"), ("after_created_at" = i64, Query, description = "Reconnect cursor")),
    responses((status = 200, description = "Durable turn control history", body = [serde_json::Value])),
    security(("bearer_jwt" = [])),
    tag = "chats"
)]
pub async fn list(
    State(state): State<AppState>,
    user: AuthedUser,
    Path(conversation_id): Path<String>,
    Query(cursor): Query<TurnControlCursor>,
) -> Result<Json<Vec<TurnControlRecord>>, ApiError> {
    require_controller(&user)?;
    TurnControlStore::new(&state.db)
        .list(&conversation_id, cursor.after_created_at, 200)
        .map(Json)
        .map_err(map_store)
}

fn map_store(error: execlaw_core::turn_controls::TurnControlError) -> ApiError {
    if matches!(
        &error,
        execlaw_core::turn_controls::TurnControlError::IdempotencyConflict
    ) {
        return ApiError {
            status: StatusCode::CONFLICT,
            code: "turn_control_idempotency_conflict",
            message: error.to_string(),
        };
    }
    let invalid = matches!(
        &error,
        execlaw_core::turn_controls::TurnControlError::Transition(_)
    );
    ApiError {
        status: if invalid {
            StatusCode::BAD_REQUEST
        } else {
            StatusCode::INTERNAL_SERVER_ERROR
        },
        code: "turn_control_error",
        message: error.to_string(),
    }
}

/// Finish queue-next-turn controls that were accepted before a process restart.
/// Their user event and `applied` transition are normally atomic; the event scan
/// also reconciles rows created by older non-atomic server versions.
pub fn recover_pending_queued_controls(state: &AppState) -> Result<usize, String> {
    use execlaw_core::events::EventKind;
    use execlaw_core::turn_controls::{TurnControlStatus, TurnControlStore};
    use std::collections::HashMap;

    let controls = TurnControlStore::new(&state.db)
        .pending_queue_controls(500)
        .map_err(|error| format!("list queued turn controls: {error}"))?;
    if controls.is_empty() {
        return Ok(0);
    }
    let event_log = crate::chats::event_log(state);
    let mut events_by_conversation = HashMap::new();
    let mut recovered = 0usize;
    for control in controls {
        let events = if let Some(events) = events_by_conversation.get(&control.conversation_id) {
            events
        } else {
            let events = event_log
                .replay_since(
                    &execlaw_core::ConversationId::from(control.conversation_id.clone()),
                    execlaw_core::EventSeq(0),
                )
                .map_err(|error| format!("verify queued-message event chain: {error}"))?;
            events_by_conversation.insert(control.conversation_id.clone(), events);
            events_by_conversation
                .get(&control.conversation_id)
                .ok_or_else(|| "queued-message event cache entry disappeared".to_owned())?
        };
        let existing_seq = events
            .iter()
            .filter(|event| event.kind == EventKind::UserMsg)
            .find_map(|event| {
                let payload = event.decode_payload::<serde_json::Value>().ok()?;
                (payload
                    .get("queued_control_id")
                    .and_then(serde_json::Value::as_str)
                    == Some(control.control_id.as_str()))
                .then_some(event.seq.0)
            });
        if let Some(event_seq) = existing_seq {
            TurnControlStore::new(&state.db)
                .transition(
                    &control.control_id,
                    TurnControlStatus::Applied,
                    Some(&serde_json::json!({"event_seq":event_seq,"recovered":true})),
                    chrono::Utc::now().timestamp(),
                )
                .map_err(|error| {
                    format!("reconcile queued control {}: {error}", control.control_id)
                })?;
            recovered = recovered.saturating_add(1);
            continue;
        }
        let text = control
            .payload
            .get("text")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| format!("queued control {} has no text", control.control_id))?;
        crate::chats::append_queued_turn_message(
            state,
            &execlaw_core::ConversationId::from(control.conversation_id.clone()),
            text,
            &control.control_id,
        )
        .map_err(|error| format!("recover queued control {}: {error}", control.control_id))?;
        recovered = recovered.saturating_add(1);
    }
    Ok(recovered)
}
