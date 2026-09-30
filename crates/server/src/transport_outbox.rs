//! Production adapter from durable transport effects to manifest-owned tools.

use crate::{chats, message_archive, state::AppState};
use async_trait::async_trait;
use execlaw_core::{
    ids::{ConversationId, EventSeq, IdempotencyKey, TurnSeq},
    outbox::{OutboxRow, OutboxStatus, OutboxStore},
};
use execlaw_outbox::{
    DeliverySafety, DispatchError, DispatchReceipt, Dispatcher, DispatcherRegistry, DrainConfig,
    WakeupDispatcher, run_drain_loop,
};
use serde::{Deserialize, Serialize};

const EFFECT_KIND: &str = "transport.send";
const ATTACHMENT_EFFECT_KIND: &str = "transport.send_attachments";

/// Identify a send/reply tool declared by an installed transport plugin.
pub fn transport_tool_channel(
    host: &execlaw_plugin_host::PluginHost,
    registry: &crate::transport_registry::HostTransportRegistry,
    tool_name: &str,
) -> Option<String> {
    let tool = host.registry().tool(tool_name)?;
    registry
        .channel_for_send_tool(&tool.plugin_id, tool_name)
        .map(ToOwned::to_owned)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TransportSendEffect {
    channel: String,
    recipient: String,
    text: String,
    model_seq: Option<i64>,
    archive_message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    owner: Option<OwnerFence>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OwnerFence {
    agent_id: String,
    generation: u64,
    scope_key: String,
}

/// Stable identity for one automatic specialist transport effect.
pub struct AgentSendIdentity<'a> {
    pub agent_id: &'a str,
    pub mailbox_scope: &'a [u8],
}

struct EnqueueContext<'a> {
    model_seq: Option<i64>,
    archive_message_id: Option<&'a str>,
    owner: Option<OwnerFence>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TransportAttachmentEffect {
    channel: String,
    recipient: String,
    text: String,
    attachments: Vec<String>,
}

fn transport_tool_call(
    effect: &TransportSendEffect,
    attachments: &[String],
) -> (String, serde_json::Value) {
    let operation = if attachments.is_empty() {
        "send_message"
    } else {
        "send_with_attachments"
    };
    let tool_name = format!("{}.{}", effect.channel, operation);
    let mut args = serde_json::json!({
        "to": effect.recipient,
        "text": effect.text,
    });
    if !attachments.is_empty() {
        args["attachments"] = serde_json::json!(attachments);
    }
    (tool_name, args)
}

/// Persist an outbound transport request under a key derived from the
/// committed assistant event. Replaying the bridge returns the same row.
pub fn enqueue_text(
    state: &AppState,
    conversation_id: &ConversationId,
    model_seq: i64,
    channel: &str,
    recipient: &str,
    _is_group: bool,
    text: &str,
    archive_message_id: &str,
) -> Result<(i64, bool), String> {
    if model_seq <= 0 || channel.is_empty() || recipient.is_empty() || text.trim().is_empty() {
        return Err("transport effect is missing its committed source or destination".into());
    }
    enqueue_with_key(
        state,
        conversation_id,
        IdempotencyKey::mint(conversation_id, TurnSeq(model_seq), 0),
        channel,
        recipient,
        text,
        EnqueueContext {
            model_seq: Some(model_seq),
            archive_message_id: Some(archive_message_id),
            owner: None,
        },
    )
}

/// Queue an automatic agent reply under a key derived from the persisted
/// mailbox batch so restarting the agent cannot send the same reply twice.
pub fn enqueue_agent_text(
    state: &AppState,
    identity: AgentSendIdentity<'_>,
    conversation_id: &ConversationId,
    channel: &str,
    recipient: &str,
    text: &str,
    archive_message_id: &str,
) -> Result<(i64, bool), String> {
    let ownership = execlaw_core::agent_ownership::AgentOwnershipStore::new(&state.db)
        .get(conversation_id.as_str(), channel, recipient)
        .map_err(|error| format!("read agent transport owner: {error}"))?
        .ok_or_else(|| "automatic agent transport has no owner".to_owned())?;
    if ownership.owner_kind != "agent" || ownership.agent_id.as_deref() != Some(identity.agent_id) {
        return Err("automatic agent lost transport ownership".into());
    }
    enqueue_with_key(
        state,
        conversation_id,
        IdempotencyKey::mint_scoped(conversation_id, identity.mailbox_scope, 0),
        channel,
        recipient,
        text,
        EnqueueContext {
            model_seq: None,
            archive_message_id: Some(archive_message_id),
            owner: Some(OwnerFence {
                agent_id: identity.agent_id.to_owned(),
                generation: ownership.generation,
                scope_key: execlaw_core::agent_ownership::scope_key(
                    conversation_id.as_str(),
                    channel,
                    recipient,
                ),
            }),
        },
    )
}

/// Stage a model-invoked transport send until the model's `tool_use` and
/// paired `tool_result` batch commits. `EventLog` releases rows staged for the
/// turn in that same transaction after it stores the pair.
pub fn stage_plugin_text(
    db: &execlaw_core::Database,
    conversation_id: &ConversationId,
    turn_seq: i64,
    ordinal: u32,
    channel: &str,
    recipient: &str,
    text: &str,
) -> Result<i64, String> {
    if turn_seq <= 0 || channel.is_empty() || recipient.is_empty() || text.trim().is_empty() {
        return Err("transport tool effect is missing its turn, destination, or text".into());
    }
    let effect = TransportSendEffect {
        channel: channel.to_owned(),
        recipient: recipient.to_owned(),
        text: text.to_owned(),
        model_seq: None,
        archive_message_id: None,
        owner: None,
    };
    let payload = rmp_serde::to_vec(&effect)
        .map_err(|error| format!("encode transport tool effect: {error}"))?;
    let row = OutboxRow {
        id: None,
        idempotency_key: IdempotencyKey::mint(conversation_id, TurnSeq(turn_seq), ordinal),
        conversation_id: conversation_id.clone(),
        effect_kind: EFFECT_KIND.into(),
        payload,
        status: OutboxStatus::Pending,
        attempts: 0,
        next_attempt_at: Some(i64::MAX),
        last_error: None,
        enqueued_seq: EventSeq(turn_seq),
    };
    OutboxStore::new(db)
        .enqueue_idempotent(&row)
        .map(|(id, _)| id)
        .map_err(|error| format!("stage transport tool effect: {error}"))
}

/// Stage a model-invoked attachment send under its tool-call ordinal. The
/// conversation event commit releases the row only after the paired tool
/// result is durable, just like a text transport send.
pub fn stage_plugin_attachments(
    db: &execlaw_core::Database,
    conversation_id: &ConversationId,
    turn_seq: i64,
    ordinal: u32,
    channel: &str,
    recipient: &str,
    text: &str,
    attachments: &[String],
) -> Result<i64, String> {
    if turn_seq <= 0
        || channel.is_empty()
        || recipient.is_empty()
        || text.trim().is_empty()
        || attachments.is_empty()
        || attachments.len() > 64
        || attachments.iter().any(|id| id.len() > 256)
        || attachments.iter().any(|id| id.trim().is_empty())
    {
        return Err(
            "transport attachment effect is missing its turn, destination, or attachment".into(),
        );
    }
    let effect = TransportAttachmentEffect {
        channel: channel.to_owned(),
        recipient: recipient.to_owned(),
        text: text.to_owned(),
        attachments: attachments.to_vec(),
    };
    let payload = rmp_serde::to_vec(&effect)
        .map_err(|error| format!("encode transport attachment effect: {error}"))?;
    let row = OutboxRow {
        id: None,
        idempotency_key: IdempotencyKey::mint(conversation_id, TurnSeq(turn_seq), ordinal),
        conversation_id: conversation_id.clone(),
        effect_kind: ATTACHMENT_EFFECT_KIND.into(),
        payload,
        status: OutboxStatus::Pending,
        attempts: 0,
        next_attempt_at: Some(i64::MAX),
        last_error: None,
        enqueued_seq: EventSeq(turn_seq),
    };
    OutboxStore::new(db)
        .enqueue_idempotent(&row)
        .map(|(id, _)| id)
        .map_err(|error| format!("stage transport attachment effect: {error}"))
}

/// Persist an attachment bridge created by a durable background task. Its
/// stable scoped key makes a repeated completion callback return the same
/// delivery record instead of creating a second transport send.
pub fn enqueue_task_attachments(
    db: &execlaw_core::Database,
    conversation_id: &ConversationId,
    source_seq: i64,
    task_scope: &[u8],
    channel: &str,
    recipient: &str,
    text: &str,
    attachments: &[String],
) -> Result<(i64, bool), String> {
    if source_seq <= 0
        || channel.is_empty()
        || recipient.is_empty()
        || text.trim().is_empty()
        || task_scope.is_empty()
        || attachments.is_empty()
        || attachments.len() > 64
        || attachments.iter().any(|id| id.len() > 256)
        || attachments.iter().any(|id| id.trim().is_empty())
    {
        return Err("task attachment effect is missing its source, scope, or destination".into());
    }
    let effect = TransportAttachmentEffect {
        channel: channel.to_owned(),
        recipient: recipient.to_owned(),
        text: text.to_owned(),
        attachments: attachments.to_vec(),
    };
    let payload = rmp_serde::to_vec(&effect)
        .map_err(|error| format!("encode task attachment effect: {error}"))?;
    let row = OutboxRow {
        id: None,
        idempotency_key: IdempotencyKey::mint_scoped(conversation_id, task_scope, 0),
        conversation_id: conversation_id.clone(),
        effect_kind: ATTACHMENT_EFFECT_KIND.into(),
        payload,
        status: OutboxStatus::Pending,
        attempts: 0,
        next_attempt_at: None,
        last_error: None,
        enqueued_seq: EventSeq(source_seq),
    };
    OutboxStore::new(db)
        .enqueue_idempotent(&row)
        .map_err(|error| format!("persist task attachment effect: {error}"))
}

fn enqueue_with_key(
    state: &AppState,
    conversation_id: &ConversationId,
    idempotency_key: IdempotencyKey,
    channel: &str,
    recipient: &str,
    text: &str,
    context: EnqueueContext<'_>,
) -> Result<(i64, bool), String> {
    let effect = TransportSendEffect {
        channel: channel.to_owned(),
        recipient: recipient.to_owned(),
        text: text.to_owned(),
        model_seq: context.model_seq,
        archive_message_id: context.archive_message_id.map(ToOwned::to_owned),
        owner: context.owner.clone(),
    };
    let payload =
        rmp_serde::to_vec(&effect).map_err(|error| format!("encode transport effect: {error}"))?;
    let row = OutboxRow {
        id: None,
        idempotency_key,
        conversation_id: conversation_id.clone(),
        effect_kind: EFFECT_KIND.into(),
        payload,
        status: OutboxStatus::Pending,
        attempts: 0,
        next_attempt_at: None,
        last_error: None,
        enqueued_seq: EventSeq(context.model_seq.unwrap_or_else(|| {
            execlaw_core::EventLog::new(&state.db)
                .last_seq(conversation_id)
                .map(|seq| seq.0)
                .unwrap_or(0)
                .max(1)
        })),
    };
    let store = OutboxStore::new(&state.db);
    let (id, created) = match context.owner.as_ref() {
        Some(owner) => store.enqueue_idempotent_for_agent(
            &row,
            &owner.scope_key,
            &owner.agent_id,
            owner.generation,
        ),
        None => store.enqueue_idempotent(&row),
    }
    .map_err(|error| format!("persist transport effect: {error}"))?;
    if created {
        if let Some(archive_message_id) = context.archive_message_id {
            if let Err(error) = message_archive::mark_outbound_status(
                state,
                conversation_id,
                channel,
                recipient,
                archive_message_id,
                "queued",
            ) {
                tracing::warn!(
                    conversation_id = %conversation_id.as_str(),
                    error = %error,
                    "transport outbox row is durable but the archive projection did not update"
                );
            }
        }
    }
    Ok((id, created))
}

/// Dispatch transport effects through the channel named by the persisted
/// effect. Since transport plugins do not yet expose sink deduplication or
/// status lookup contracts, an ambiguous plugin result is parked as unknown.
pub struct PluginTransportDispatcher {
    state: AppState,
    effect_kind: &'static str,
}

impl PluginTransportDispatcher {
    pub fn new(state: AppState) -> Self {
        Self {
            state,
            effect_kind: EFFECT_KIND,
        }
    }

    fn for_attachments(state: AppState) -> Self {
        Self {
            state,
            effect_kind: ATTACHMENT_EFFECT_KIND,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_transport_effect_payload_without_owner_still_decodes() {
        #[derive(Serialize)]
        struct OldEffect<'a> {
            channel: &'a str,
            recipient: &'a str,
            text: &'a str,
            model_seq: Option<i64>,
            archive_message_id: Option<&'a str>,
        }
        let bytes = rmp_serde::to_vec(&OldEffect {
            channel: "whatsapp",
            recipient: "group@g.us",
            text: "hello",
            model_seq: Some(7),
            archive_message_id: None,
        })
        .unwrap();
        let decoded: TransportSendEffect = rmp_serde::from_slice(&bytes).unwrap();
        assert!(decoded.owner.is_none());
        assert_eq!(decoded.recipient, "group@g.us");
    }

    fn fresh_db() -> execlaw_core::Database {
        let db =
            execlaw_core::Database::open(&execlaw_core::DbConfig::in_memory_unencrypted()).unwrap();
        execlaw_core::MigrationRunner::new(&db).apply_all().unwrap();
        db
    }

    #[test]
    fn attachment_send_is_durable_idempotent_and_waits_for_pair_commit() {
        let db = fresh_db();
        let conversation_id = ConversationId::from("attachment-outbox");
        let attachments = vec!["att-1".to_owned(), "att-2".to_owned()];
        let outbox_id = stage_plugin_attachments(
            &db,
            &conversation_id,
            12,
            3,
            "signal",
            "recipient",
            "two reports",
            &attachments,
        )
        .unwrap();
        let replayed = stage_plugin_attachments(
            &db,
            &conversation_id,
            12,
            3,
            "signal",
            "recipient",
            "two reports",
            &attachments,
        )
        .unwrap();
        assert_eq!(outbox_id, replayed);
        assert!(
            OutboxStore::new(&db)
                .ready_pending(chrono::Utc::now().timestamp(), 10)
                .unwrap()
                .is_empty(),
            "an attachment effect must not dispatch before its tool event pair commits"
        );
        let payload: Vec<u8> = db
            .with_conn(|connection| {
                Ok(connection.query_row(
                    "SELECT payload FROM state_outbox WHERE id=?1",
                    [outbox_id],
                    |row| row.get(0),
                )?)
            })
            .unwrap();
        let effect: TransportAttachmentEffect = rmp_serde::from_slice(&payload).unwrap();
        assert_eq!(effect.channel, "signal");
        assert_eq!(effect.recipient, "recipient");
        assert_eq!(effect.text, "two reports");
        assert_eq!(effect.attachments, attachments);
    }

    #[test]
    fn attachment_send_rejects_empty_or_blank_attachment_ids() {
        let db = fresh_db();
        let conversation_id = ConversationId::from("attachment-outbox-invalid");
        for attachments in [&[][..], &[" ".to_owned()][..]] {
            assert!(
                stage_plugin_attachments(
                    &db,
                    &conversation_id,
                    12,
                    3,
                    "signal",
                    "recipient",
                    "report",
                    attachments,
                )
                .is_err()
            );
        }
    }

    #[test]
    fn attachment_effect_dispatches_the_attachment_tool_with_all_ids() {
        let effect = TransportSendEffect {
            channel: "signal".into(),
            recipient: "group-id".into(),
            text: "reports".into(),
            model_seq: None,
            archive_message_id: None,
            owner: None,
        };
        let attachments = vec!["att-1".into(), "att-2".into()];
        let (tool_name, args) = transport_tool_call(&effect, &attachments);
        assert_eq!(tool_name, "signal.send_with_attachments");
        assert_eq!(args["to"], "group-id");
        assert_eq!(args["text"], "reports");
        assert_eq!(args["attachments"], serde_json::json!(attachments));
    }

    #[test]
    fn task_attachment_bridge_is_immediately_ready_and_idempotent() {
        let db = fresh_db();
        let conversation_id = ConversationId::from("research-attachment-outbox");
        let attachment = vec!["report.pdf".to_owned()];
        let first = enqueue_task_attachments(
            &db,
            &conversation_id,
            21,
            b"research-run:run-1",
            "signal",
            "recipient",
            "Research report",
            &attachment,
        )
        .unwrap();
        let retry = enqueue_task_attachments(
            &db,
            &conversation_id,
            21,
            b"research-run:run-1",
            "signal",
            "recipient",
            "Research report",
            &attachment,
        )
        .unwrap();
        assert_eq!(first.0, retry.0);
        assert!(first.1);
        assert!(!retry.1);
        let ready = OutboxStore::new(&db)
            .ready_pending(chrono::Utc::now().timestamp(), 10)
            .unwrap();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].id, Some(first.0));
        assert_eq!(ready[0].effect_kind, ATTACHMENT_EFFECT_KIND);
    }
}

/// Start the production relay for plugin-backed transport effects and durable
/// wakeups. Send `true` to the returned handle during graceful shutdown.
pub fn spawn(state: AppState) -> tokio::sync::watch::Sender<bool> {
    let db = state.db.clone();
    let mut registry = DispatcherRegistry::new();
    registry.register(std::sync::Arc::new(PluginTransportDispatcher::new(
        state.clone(),
    )));
    registry.register(std::sync::Arc::new(
        PluginTransportDispatcher::for_attachments(state.clone()),
    ));
    registry.register(std::sync::Arc::new(WakeupDispatcher::new(db.clone())));
    let (stop, receiver) = tokio::sync::watch::channel(false);
    tokio::spawn(run_drain_loop(
        db,
        std::sync::Arc::new(registry),
        DrainConfig::default(),
        receiver,
    ));
    stop
}

#[async_trait]
impl Dispatcher for PluginTransportDispatcher {
    fn effect_kind(&self) -> &'static str {
        self.effect_kind
    }

    fn delivery_safety(&self) -> DeliverySafety {
        DeliverySafety::NoGuarantee
    }

    async fn dispatch(&self, row: &OutboxRow) -> Result<DispatchReceipt, DispatchError> {
        let (effect, attachments) = if self.effect_kind == ATTACHMENT_EFFECT_KIND {
            let attachment_effect: TransportAttachmentEffect = rmp_serde::from_slice(&row.payload)
                .map_err(|error| {
                    DispatchError::NotAccepted(format!(
                        "invalid transport attachment effect payload: {error}"
                    ))
                })?;
            (
                TransportSendEffect {
                    channel: attachment_effect.channel,
                    recipient: attachment_effect.recipient,
                    text: attachment_effect.text,
                    model_seq: None,
                    archive_message_id: None,
                    owner: None,
                },
                attachment_effect.attachments,
            )
        } else {
            let effect: TransportSendEffect =
                rmp_serde::from_slice(&row.payload).map_err(|error| {
                    DispatchError::NotAccepted(format!("invalid transport effect payload: {error}"))
                })?;
            (effect, Vec::new())
        };
        let conversation_id = row.conversation_id.clone();
        if let Some(owner) = effect.owner.as_ref() {
            let current = execlaw_core::agent_ownership::AgentOwnershipStore::new(&self.state.db)
                .get(conversation_id.as_str(), &effect.channel, &effect.recipient)
                .map_err(|error| {
                    DispatchError::NotAccepted(format!("read agent owner: {error}"))
                })?;
            if !current.is_some_and(|current| {
                current.owner_kind == "agent"
                    && current.agent_id.as_deref() == Some(owner.agent_id.as_str())
                    && current.generation == owner.generation
            }) {
                return Err(DispatchError::NotAccepted(
                    "agent transport ownership changed before delivery".into(),
                ));
            }
        }
        let (tool_name, args) = transport_tool_call(&effect, &attachments);
        if let Some(policy) = execlaw_core::tool_access::ToolAccessStore::new(&self.state.db)
            .get(&tool_name)
            .map_err(|error| {
                DispatchError::NotAccepted(format!("read transport tool policy: {error}"))
            })?
        {
            if !policy.enabled
                || policy.removed_at.is_some()
                || !policy
                    .allowed_classes
                    .iter()
                    .any(|class| class == "Controller")
            {
                return Err(DispatchError::NotAccepted(
                    "transport tool is disabled or no longer authorized for Controller sends"
                        .into(),
                ));
            }
        }
        self.state
            .plugin_host
            .validate_tool_call(&tool_name, &args, &["*"], Some("Controller"))
            .map_err(DispatchError::NotAccepted)?;
        let result = self
            .state
            .plugin_host
            .call_tool(&tool_name, args, &["*"], Some("Controller"))
            .await;

        let result = match result {
            Ok(result)
                if result.get("isError").and_then(serde_json::Value::as_bool) != Some(true) =>
            {
                result
            }
            Ok(_) => {
                self.record_unknown(&conversation_id, &effect);
                return Err(DispatchError::OutcomeUnknown(format!(
                    "transport tool {tool_name} returned an error result"
                )));
            }
            Err(error) => {
                self.record_unknown(&conversation_id, &effect);
                return Err(DispatchError::OutcomeUnknown(format!(
                    "transport tool {tool_name} outcome is unknown: {error}"
                )));
            }
        };

        let status = chats::transport_delivery_status(&result);
        if let Err(error) = self.record_status(&conversation_id, &effect, status) {
            self.record_unknown(&conversation_id, &effect);
            return Err(DispatchError::OutcomeUnknown(format!(
                "transport accepted the effect but its receipt could not be recorded: {error}"
            )));
        }
        let receipt = result
            .get("external_receipt")
            .or_else(|| result.get("receipt"))
            .and_then(serde_json::Value::as_str)
            .filter(|value| value.len() <= 512)
            .map(ToOwned::to_owned);
        Ok(DispatchReceipt {
            external_receipt: receipt,
        })
    }
}

impl PluginTransportDispatcher {
    fn record_status(
        &self,
        conversation_id: &ConversationId,
        effect: &TransportSendEffect,
        status: &str,
    ) -> Result<(), String> {
        if let Some(model_seq) = effect.model_seq {
            chats::append_transport_review_decision(
                &self.state,
                conversation_id,
                model_seq,
                status,
            )?;
        }
        if let Some(archive_message_id) = effect.archive_message_id.as_deref() {
            let archive_status = if status == "accepted" || status == "queued" {
                "queued"
            } else {
                status
            };
            message_archive::mark_outbound_status(
                &self.state,
                conversation_id,
                &effect.channel,
                &effect.recipient,
                archive_message_id,
                archive_status,
            )?;
        }
        Ok(())
    }

    fn record_unknown(&self, conversation_id: &ConversationId, effect: &TransportSendEffect) {
        if let Some(model_seq) = effect.model_seq {
            let _ = chats::append_transport_review_decision(
                &self.state,
                conversation_id,
                model_seq,
                "unknown",
            );
        }
        if let Some(archive_message_id) = effect.archive_message_id.as_deref() {
            let _ = message_archive::mark_outbound_status(
                &self.state,
                conversation_id,
                &effect.channel,
                &effect.recipient,
                archive_message_id,
                "queued",
            );
        }
    }
}
