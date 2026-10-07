//! Production adapter from durable transport effects to manifest-owned tools.

use crate::{chats, message_archive, state::AppState};
use async_trait::async_trait;
use execlaw_core::{
    ids::{AttachmentId, ConversationId, EventSeq, IdempotencyKey, TurnSeq},
    outbox::{OutboxRow, OutboxStatus, OutboxStore},
};
use execlaw_outbox::{
    DeliverySafety, DispatchError, DispatchReceipt, Dispatcher, DispatcherRegistry, DrainConfig,
    WakeupDispatcher, run_drain_loop,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Read;

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
struct LabeledTransportSendEffect {
    effect: TransportSendEffect,
    information_label: execlaw_core::information::InformationLabel,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LabeledTransportAttachmentEffect {
    effect: TransportAttachmentEffect,
    information_label: execlaw_core::information::InformationLabel,
}

fn decode_transport_send_effect(
    payload: &[u8],
) -> Result<
    (
        TransportSendEffect,
        Option<execlaw_core::information::InformationLabel>,
    ),
    String,
> {
    if let Ok(labeled) = rmp_serde::from_slice::<LabeledTransportSendEffect>(payload) {
        return Ok((labeled.effect, Some(labeled.information_label)));
    }
    Err("transport effect is missing its authority and information label".into())
}

fn decode_transport_attachment_effect(
    payload: &[u8],
) -> Result<
    (
        TransportAttachmentEffect,
        Option<execlaw_core::information::InformationLabel>,
    ),
    String,
> {
    if let Ok(labeled) = rmp_serde::from_slice::<LabeledTransportAttachmentEffect>(payload) {
        return Ok((labeled.effect, Some(labeled.information_label)));
    }
    Err("transport attachment effect is missing its authority and information label".into())
}

fn transport_information_label(
    db: &execlaw_core::Database,
    conversation_id: &ConversationId,
    channel: &str,
    recipient: &str,
    source_id: &str,
) -> execlaw_core::information::InformationLabel {
    let trust_class = execlaw_core::conversation::ConversationStore::new(db)
        .get(conversation_id)
        .ok()
        .flatten()
        .map(|conversation| conversation.trust_class)
        .unwrap_or_else(|| "Unknown".into());
    let destination = format!("transport:{channel}:{recipient}");
    let policy_revision = execlaw_core::tool_access::ToolAccessStore::new(db)
        .latest_policy_revision_id()
        .unwrap_or(0);
    let mut label = execlaw_core::information::InformationLabel::observed(
        execlaw_core::information::Sensitivity::Sensitive,
        Some(conversation_id.as_str().to_owned()),
        trust_class,
        "conversation_output",
        source_id,
        [destination],
    );
    if let Some(turn_seq) = source_id
        .strip_prefix("turn:")
        .and_then(|rest| rest.split(':').next())
        .and_then(|value| value.parse::<i64>().ok())
        && let Ok(Some(run)) = execlaw_core::runs::RunStore::new(db)
            .find_run_for_input(conversation_id, EventSeq(turn_seq))
        && let Ok(Some((run_subject, run_label))) =
            execlaw_core::information_store::InformationLabelStore::new(db)
                .latest_for_identity("run", &run.run_id)
        && let Some(combined) = execlaw_core::information::InformationLabel::combine(
            &[label.clone(), run_label],
            "transport_effect",
        )
    {
        let _ = run_subject;
        label = combined;
    }
    label.with_policy_revision(policy_revision)
}

fn transport_information_subject(
    source_id: &str,
    text: &str,
) -> execlaw_core::information_store::InformationSubject {
    execlaw_core::information_store::InformationSubject {
        kind: "transport_effect".into(),
        id: source_id.to_owned(),
        sha256: hex::encode(Sha256::digest(text.as_bytes())),
    }
}

fn persist_transport_information_label(
    db: &execlaw_core::Database,
    source_id: &str,
    text: &str,
    label: execlaw_core::information::InformationLabel,
) -> Result<execlaw_core::information::InformationLabel, String> {
    let store = execlaw_core::information_store::InformationLabelStore::new(db);
    let subject = transport_information_subject(source_id, text);
    if let Some(existing) = store
        .get(&subject)
        .map_err(|error| format!("read outbound information label: {error}"))?
    {
        if existing.authority_principal_id != label.authority_principal_id
            || existing.authority_fingerprint != label.authority_fingerprint
        {
            return Err("outbound information label is bound to different authority".into());
        }
        return Ok(existing);
    }
    store
        .observe(
            &subject,
            &label,
            "host:transport-outbox",
            chrono::Utc::now().timestamp(),
        )
        .map_err(|error| format!("persist outbound information label: {error}"))?;
    Ok(label)
}

fn load_transport_information_label(
    db: &execlaw_core::Database,
    text: &str,
    payload_label: Option<execlaw_core::information::InformationLabel>,
) -> Result<Option<execlaw_core::information::InformationLabel>, DispatchError> {
    let Some(payload_label) = payload_label else {
        return Ok(None);
    };
    let source_id = payload_label
        .provenance
        .first()
        .map(|entry| entry.source_id.as_str())
        .ok_or_else(|| {
            DispatchError::NotAccepted("outbound information provenance is missing".into())
        })?;
    let subject = transport_information_subject(source_id, text);
    execlaw_core::information_store::InformationLabelStore::new(db)
        .get(&subject)
        .map_err(|_| DispatchError::NotAccepted("outbound information label lookup failed".into()))
        .map(|stored| stored.or(Some(payload_label)))
}

fn originating_principal_authority(
    db: &execlaw_core::Database,
    conversation_id: &ConversationId,
    model_seq: i64,
) -> Result<Option<(String, String)>, String> {
    use execlaw_core::events::EventKind;
    let events = execlaw_core::EventLog::new(db)
        .replay_since(conversation_id, EventSeq(0))
        .map_err(|error| format!("read outbound source events: {error}"))?;
    let raw_id = events
        .iter()
        .filter(|event| event.seq.0 < model_seq && event.kind == EventKind::UserMsg)
        .rev()
        .find_map(|event| {
            event
                .decode_payload::<serde_json::Value>()
                .ok()?
                .get("sender_principal_id")?
                .as_str()
                .map(ToOwned::to_owned)
        });
    let Some(raw_id) = raw_id else {
        return Ok(None);
    };
    let principal_id = if raw_id == "controller" {
        crate::routes::controller_principal_id(db)
            .ok()
            .map(|id| id.as_str().to_owned())
    } else {
        Some(raw_id)
    };
    let Some(principal_id) = principal_id else {
        return Ok(None);
    };
    let principal_id = if principal_id == "controller" {
        crate::routes::controller_principal_id(db)
            .ok()
            .map(|id| id.as_str().to_owned())
    } else {
        Some(principal_id)
    };
    let Some(principal_id) = principal_id else {
        return Ok(None);
    };
    let principal = execlaw_core::principal::PrincipalStore::new(db)
        .get(&execlaw_core::ids::PrincipalId::from(principal_id.clone()))
        .map_err(|error| format!("read outbound source authority: {error}"))?;
    let Some(principal) = principal else {
        return Ok(None);
    };
    let snapshot = serde_json::to_vec(&principal.trust_level)
        .map_err(|error| format!("encode outbound source authority: {error}"))?;
    Ok(Some((principal_id, hex::encode(Sha256::digest(snapshot)))))
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
    authority: Option<(&str, &str)>,
) -> Result<i64, String> {
    if turn_seq <= 0 || channel.is_empty() || recipient.is_empty() || text.trim().is_empty() {
        return Err("transport tool effect is missing its turn, destination, or text".into());
    }
    let source_id = format!("turn:{turn_seq}:{ordinal}");
    let mut information_label =
        transport_information_label(db, conversation_id, channel, recipient, &source_id);
    if let Some((principal_id, fingerprint)) = authority {
        information_label = information_label.with_authority(principal_id, fingerprint);
    }
    information_label =
        persist_transport_information_label(db, &source_id, text, information_label)?;
    let effect = TransportSendEffect {
        channel: channel.to_owned(),
        recipient: recipient.to_owned(),
        text: text.to_owned(),
        model_seq: None,
        archive_message_id: None,
        owner: None,
    };
    let payload = rmp_serde::to_vec(&LabeledTransportSendEffect {
        effect,
        information_label,
    })
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
    stage_plugin_attachments_with_authority(
        db,
        conversation_id,
        turn_seq,
        ordinal,
        channel,
        recipient,
        text,
        attachments,
        None,
    )
}

pub fn stage_plugin_attachments_with_authority(
    db: &execlaw_core::Database,
    conversation_id: &ConversationId,
    turn_seq: i64,
    ordinal: u32,
    channel: &str,
    recipient: &str,
    text: &str,
    attachments: &[String],
    authority: Option<(&str, &str)>,
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
    let source_id = format!("turn:{turn_seq}:{ordinal}");
    let mut information_label =
        transport_information_label(db, conversation_id, channel, recipient, &source_id);
    if let Some((principal_id, fingerprint)) = authority {
        information_label = information_label.with_authority(principal_id, fingerprint);
    }
    information_label =
        persist_transport_information_label(db, &source_id, text, information_label)?;
    let effect = TransportAttachmentEffect {
        channel: channel.to_owned(),
        recipient: recipient.to_owned(),
        text: text.to_owned(),
        attachments: attachments.to_vec(),
    };
    let payload = rmp_serde::to_vec(&LabeledTransportAttachmentEffect {
        effect,
        information_label,
    })
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
    let information_label =
        transport_information_label(db, conversation_id, channel, recipient, "task_attachment");
    let information_label =
        persist_transport_information_label(db, "task_attachment", text, information_label)?;
    let effect = TransportAttachmentEffect {
        channel: channel.to_owned(),
        recipient: recipient.to_owned(),
        text: text.to_owned(),
        attachments: attachments.to_vec(),
    };
    let payload = rmp_serde::to_vec(&LabeledTransportAttachmentEffect {
        effect,
        information_label,
    })
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
    let source_id = context
        .model_seq
        .map(|seq| format!("event:{seq}"))
        .unwrap_or_else(|| "agent_output".into());
    let mut information_label =
        transport_information_label(&state.db, conversation_id, channel, recipient, &source_id);
    if let Some(model_seq) = context.model_seq {
        if let Some((principal_id, fingerprint)) =
            originating_principal_authority(&state.db, conversation_id, model_seq)?
        {
            information_label = information_label.with_authority(principal_id, fingerprint);
        }
    }
    information_label =
        persist_transport_information_label(&state.db, &source_id, text, information_label)?;
    let effect = TransportSendEffect {
        channel: channel.to_owned(),
        recipient: recipient.to_owned(),
        text: text.to_owned(),
        model_seq: context.model_seq,
        archive_message_id: context.archive_message_id.map(ToOwned::to_owned),
        owner: context.owner.clone(),
    };
    let payload = rmp_serde::to_vec(&LabeledTransportSendEffect {
        effect,
        information_label,
    })
    .map_err(|error| format!("encode transport effect: {error}"))?;
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
fn inspect_transport_attachment(
    state: &AppState,
    conversation_id: &ConversationId,
    attachment_id: &str,
    destination: &str,
) -> Result<(), DispatchError> {
    const MAX_SCAN_BYTES: u64 = 32 * 1024 * 1024;
    let store = execlaw_core::attachments::AttachmentStore::new(&state.db);
    let attachment = store
        .get(&AttachmentId::from(attachment_id.to_owned()))
        .map_err(|_| DispatchError::NotAccepted("attachment lookup failed before delivery".into()))?
        .ok_or_else(|| {
            DispatchError::NotAccepted("attachment is unavailable for outbound inspection".into())
        })?;
    if attachment.conversation_id.as_str() != conversation_id.as_str() {
        return Err(DispatchError::NotAccepted(
            "attachment is outside the sending conversation".into(),
        ));
    }
    let (path, expected_hash) = (attachment.path, attachment.sha256);
    let file = std::fs::File::open(&path).map_err(|_| {
        DispatchError::NotAccepted(
            "attachment bytes are unavailable for outbound inspection".into(),
        )
    })?;
    let mut bytes = Vec::new();
    file.take(MAX_SCAN_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| DispatchError::NotAccepted("attachment could not be inspected".into()))?;
    if bytes.len() as u64 > MAX_SCAN_BYTES {
        return Err(DispatchError::NotAccepted(
            "attachment exceeds the outbound inspection size limit".into(),
        ));
    }
    let actual_hash = hex::encode(Sha256::digest(&bytes));
    if actual_hash != expected_hash {
        return Err(DispatchError::NotAccepted(
            "attachment integrity check failed before delivery".into(),
        ));
    }
    let attachment_subject = execlaw_core::information_store::InformationSubject {
        kind: "attachment".into(),
        id: attachment_id.to_owned(),
        sha256: expected_hash,
    };
    let attachment_label = execlaw_core::information_store::InformationLabelStore::new(&state.db)
        .get(&attachment_subject)
        .map_err(|_| {
            DispatchError::NotAccepted("attachment information label lookup failed".into())
        })?
        .ok_or_else(|| {
            DispatchError::NotAccepted("attachment information label is unavailable".into())
        })?;
    let decision =
        execlaw_policy::outbound::inspect_outbound(&bytes, Some(&attachment_label), destination);
    if !decision.allowed {
        return Err(DispatchError::NotAccepted(format!(
            "outbound data check blocked attachment: {}",
            decision.findings.join(",")
        )));
    }
    Ok(())
}

fn validate_outbound_authority(
    db: &execlaw_core::Database,
    label: Option<&execlaw_core::information::InformationLabel>,
) -> Result<(), DispatchError> {
    let Some(label) = label else {
        return Err(DispatchError::NotAccepted(
            "outbound effect is missing its authority receipt".into(),
        ));
    };
    let current_policy_revision = execlaw_core::tool_access::ToolAccessStore::new(db)
        .latest_policy_revision_id()
        .map_err(|_| DispatchError::NotAccepted("outbound policy revision lookup failed".into()))?;
    if label.authority_policy_revision != Some(current_policy_revision) {
        return Err(DispatchError::NotAccepted(
            "outbound policy changed before delivery".into(),
        ));
    }
    let (Some(principal_id), Some(expected_fingerprint)) = (
        label.authority_principal_id.as_deref(),
        label.authority_fingerprint.as_deref(),
    ) else {
        if label.authority_principal_id.is_some() || label.authority_fingerprint.is_some() {
            return Err(DispatchError::NotAccepted(
                "outbound authority receipt is incomplete".into(),
            ));
        }
        return Ok(());
    };
    let principal = execlaw_core::principal::PrincipalStore::new(db)
        .get(&execlaw_core::ids::PrincipalId::from(
            principal_id.to_owned(),
        ))
        .map_err(|_| DispatchError::NotAccepted("outbound authority lookup failed".into()))?
        .ok_or_else(|| DispatchError::NotAccepted("outbound principal was removed".into()))?;
    let current = serde_json::to_vec(&principal.trust_level)
        .map(|bytes| hex::encode(Sha256::digest(bytes)))
        .map_err(|_| {
            DispatchError::NotAccepted("outbound authority could not be verified".into())
        })?;
    if current != expected_fingerprint
        || principal.trust_level.class_tag() != label.source_trust_class
    {
        return Err(DispatchError::NotAccepted(
            "outbound authority changed before delivery".into(),
        ));
    }
    match principal.trust_level {
        execlaw_core::principal::TrustLevel::Blocked { .. }
        | execlaw_core::principal::TrustLevel::UnknownPending { .. } => Err(
            DispatchError::NotAccepted("outbound principal is no longer trusted".into()),
        ),
        execlaw_core::principal::TrustLevel::Delegated {
            expires_at: Some(expires_at),
            ..
        } if expires_at <= chrono::Utc::now().timestamp() => Err(DispatchError::NotAccepted(
            "outbound delegated authority expired before delivery".into(),
        )),
        _ => Ok(()),
    }
}

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
    fn staged_model_send_carries_a_typed_non_model_editable_label() {
        let db = fresh_db();
        let conversation_id = ConversationId::from("labelled-transport-outbox");
        let id = stage_plugin_text(
            &db,
            &conversation_id,
            12,
            3,
            "signal",
            "recipient",
            "ordinary report",
            Some(("principal-1", "grant-fingerprint-1")),
        )
        .unwrap();
        let bytes: Vec<u8> = db
            .with_conn(|connection| {
                Ok(connection.query_row(
                    "SELECT payload FROM state_outbox WHERE id = ?1",
                    [id],
                    |row| row.get(0),
                )?)
            })
            .unwrap();
        let effect: LabeledTransportSendEffect = rmp_serde::from_slice(&bytes).unwrap();
        let label = effect.information_label;
        assert_eq!(
            label.sensitivity,
            execlaw_core::information::Sensitivity::Sensitive
        );
        assert!(
            label
                .allowed_destinations
                .contains("transport:signal:recipient")
        );
        assert_eq!(label.authority_principal_id.as_deref(), Some("principal-1"));
        assert_eq!(
            label.authority_fingerprint.as_deref(),
            Some("grant-fingerprint-1")
        );
    }

    #[test]
    fn staged_transport_secret_can_be_exported_only_after_scoped_controller_declassification() {
        let db = fresh_db();
        let conversation_id = ConversationId::from("declassified-transport-outbox");
        let text = "api_key=synthetic-secret-value";
        let id = stage_plugin_text(
            &db,
            &conversation_id,
            12,
            1,
            "signal",
            "recipient",
            text,
            None,
        )
        .unwrap();
        let bytes: Vec<u8> = db
            .with_conn(|connection| {
                Ok(connection.query_row(
                    "SELECT payload FROM state_outbox WHERE id=?1",
                    [id],
                    |row| row.get(0),
                )?)
            })
            .unwrap();
        let effect: LabeledTransportSendEffect = rmp_serde::from_slice(&bytes).unwrap();
        let destination = "transport:signal:recipient";
        assert!(
            !execlaw_policy::outbound::inspect_outbound(
                text.as_bytes(),
                Some(&effect.information_label),
                destination,
            )
            .allowed
        );
        let source_id = effect.information_label.provenance[0].source_id.clone();
        let subject = transport_information_subject(&source_id, text);
        execlaw_core::information_store::InformationLabelStore::new(&db)
            .declassify(&subject, "controller-1", destination, "case-17", 20)
            .unwrap();
        let current = load_transport_information_label(&db, text, Some(effect.information_label))
            .unwrap()
            .unwrap();
        assert!(
            execlaw_policy::outbound::inspect_outbound(
                text.as_bytes(),
                Some(&current),
                destination,
            )
            .allowed
        );
        assert!(
            !execlaw_policy::outbound::inspect_outbound(
                text.as_bytes(),
                Some(&current),
                "transport:signal:other-recipient",
            )
            .allowed
        );
    }

    #[test]
    fn outbound_attachment_scan_blocks_synthetic_credentials_at_the_sink_boundary() {
        use execlaw_core::attachments::{AttachmentRow, AttachmentStore};
        use execlaw_core::ids::AttachmentId;
        use sha2::{Digest, Sha256};

        let state = crate::routes::test_app_state();
        let conversation_id = ConversationId::from("outbound-scan-conversation");
        state
            .db
            .with_conn(|connection| {
                connection.execute(
                    "INSERT INTO state_conversations (conversation_id,kind,phase,trust_class,modality) VALUES (?1,'ControllerDM','idle','Controller','Text')",
                    [conversation_id.as_str()],
                )?;
                Ok(())
            })
            .unwrap();
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("report.txt");
        let bytes = b"api_key=synthetic-credential-value";
        std::fs::write(&path, bytes).unwrap();
        AttachmentStore::new(&state.db)
            .insert(&AttachmentRow {
                id: AttachmentId::from("outbound-secret-file"),
                conversation_id: conversation_id.clone(),
                mime_type: "text/plain".into(),
                path: path.to_string_lossy().into_owned(),
                sha256: hex::encode(Sha256::digest(bytes)),
                received_at: 1,
                filename: Some("report.txt".into()),
            })
            .unwrap();
        let result = inspect_transport_attachment(
            &state,
            &conversation_id,
            "outbound-secret-file",
            "transport:signal:recipient",
        );
        assert!(
            matches!(result, Err(DispatchError::NotAccepted(reason)) if reason.contains("credential_assignment"))
        );
    }

    #[test]
    fn controller_declassification_allows_an_attachment_only_for_its_approved_recipient() {
        use execlaw_core::attachments::{AttachmentRow, AttachmentStore};
        use execlaw_core::ids::AttachmentId;
        use sha2::{Digest, Sha256};

        let state = crate::routes::test_app_state();
        let conversation_id = ConversationId::from("approved-attachment-export");
        state
            .db
            .with_conn(|connection| {
                connection.execute(
                    "INSERT INTO state_conversations (conversation_id,kind,phase,trust_class,modality) VALUES (?1,'ControllerDM','idle','Controller','Text')",
                    [conversation_id.as_str()],
                )?;
                Ok(())
            })
            .unwrap();
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("report.txt");
        let bytes = b"api_key=synthetic-credential-value";
        std::fs::write(&path, bytes).unwrap();
        let hash = hex::encode(Sha256::digest(bytes));
        AttachmentStore::new(&state.db)
            .insert(&AttachmentRow {
                id: AttachmentId::from("approved-export-file"),
                conversation_id: conversation_id.clone(),
                mime_type: "text/plain".into(),
                path: path.to_string_lossy().into_owned(),
                sha256: hash.clone(),
                received_at: 1,
                filename: Some("report.txt".into()),
            })
            .unwrap();
        let subject = execlaw_core::information_store::InformationSubject {
            kind: "attachment".into(),
            id: "approved-export-file".into(),
            sha256: hash,
        };
        execlaw_core::information_store::InformationLabelStore::new(&state.db)
            .declassify(
                &subject,
                "controller-1",
                "transport:signal:alice",
                "case-22",
                2,
            )
            .unwrap();

        assert!(
            inspect_transport_attachment(
                &state,
                &conversation_id,
                "approved-export-file",
                "transport:signal:alice",
            )
            .is_ok()
        );
        assert!(
            inspect_transport_attachment(
                &state,
                &conversation_id,
                "approved-export-file",
                "transport:signal:bob",
            )
            .is_err()
        );
    }

    #[test]
    fn pending_transport_effect_is_rejected_after_principal_authority_changes() {
        use execlaw_core::ids::PrincipalId;
        use execlaw_core::information::{InformationLabel, Sensitivity};
        use execlaw_core::principal::{Principal, PrincipalStore, TrustLevel};

        let db = fresh_db();
        let principal_id = PrincipalId::from("outbound-authority-test");
        let original_trust = TrustLevel::KnownTrusted {
            resolvers: Vec::new(),
            approved_by: PrincipalId::from("controller"),
            approved_at: 1,
        };
        let principal = Principal {
            id: principal_id.clone(),
            identifiers: Vec::new(),
            trust_level: original_trust.clone(),
            resolved_by: Vec::new(),
            metadata: serde_json::json!({}),
            first_seen: 1,
            last_seen: Some(1),
            controller_notes: None,
        };
        let store = PrincipalStore::new(&db);
        store.upsert(&principal).unwrap();
        let fingerprint = hex::encode(Sha256::digest(serde_json::to_vec(&original_trust).unwrap()));
        let label = InformationLabel::observed(
            Sensitivity::Sensitive,
            Some("conversation-1".into()),
            "KnownTrusted",
            "conversation_output",
            "event-10",
            ["transport:signal:recipient".to_owned()],
        )
        .with_authority(principal_id.as_str(), fingerprint)
        .with_policy_revision(
            execlaw_core::tool_access::ToolAccessStore::new(&db)
                .latest_policy_revision_id()
                .unwrap(),
        );

        let mut revoked = principal;
        revoked.trust_level = TrustLevel::Blocked {
            blocked_by: PrincipalId::from("controller"),
            blocked_at: 2,
            reason: None,
        };
        store.upsert(&revoked).unwrap();
        assert!(matches!(
            validate_outbound_authority(&db, Some(&label)),
            Err(DispatchError::NotAccepted(reason)) if reason.contains("authority changed")
        ));
    }

    #[test]
    fn legacy_transport_payload_without_authority_is_rejected() {
        let payload = rmp_serde::to_vec(&TransportSendEffect {
            channel: "signal".into(),
            recipient: "recipient".into(),
            text: "queued before authority receipts".into(),
            model_seq: None,
            archive_message_id: None,
            owner: None,
        })
        .unwrap();
        assert!(decode_transport_send_effect(&payload).is_err());
    }

    #[test]
    fn outbound_effect_without_label_or_authority_is_rejected() {
        let db = fresh_db();
        assert!(matches!(
            validate_outbound_authority(&db, None),
            Err(DispatchError::NotAccepted(reason)) if reason.contains("missing its authority receipt")
        ));
    }

    #[test]
    fn queued_transport_effect_is_rejected_after_policy_revision_changes() {
        use execlaw_core::ids::PrincipalId;
        use execlaw_core::information::{InformationLabel, Sensitivity};
        use execlaw_core::principal::{Principal, PrincipalStore, TrustLevel};

        let db = fresh_db();
        let principal_id = PrincipalId::from("policy-revision-authority");
        let trust = TrustLevel::KnownTrusted {
            resolvers: Vec::new(),
            approved_by: PrincipalId::from("controller"),
            approved_at: 1,
        };
        PrincipalStore::new(&db)
            .upsert(&Principal {
                id: principal_id.clone(),
                identifiers: Vec::new(),
                trust_level: trust.clone(),
                resolved_by: Vec::new(),
                metadata: serde_json::json!({}),
                first_seen: 1,
                last_seen: Some(1),
                controller_notes: None,
            })
            .unwrap();
        let fingerprint = hex::encode(Sha256::digest(serde_json::to_vec(&trust).unwrap()));
        let revision = execlaw_core::tool_access::ToolAccessStore::new(&db)
            .latest_policy_revision_id()
            .unwrap();
        let label = InformationLabel::observed(
            Sensitivity::Sensitive,
            Some("conversation-1".into()),
            "KnownTrusted",
            "conversation_output",
            "event-10",
            ["transport:signal:recipient".to_owned()],
        )
        .with_authority(principal_id.as_str(), fingerprint)
        .with_policy_revision(revision);
        db.with_conn(|connection| {
            connection.execute(
                "INSERT INTO config_tool_access_policy_revisions \
                 (tool_name,enabled,allowed_classes_json,revised_by,revised_at) \
                 VALUES ('transport.send',1,'[\"KnownTrusted\"]','controller',2)",
                [],
            )?;
            Ok(())
        })
        .unwrap();

        assert!(matches!(
            validate_outbound_authority(&db, Some(&label)),
            Err(DispatchError::NotAccepted(reason)) if reason.contains("policy changed")
        ));
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
        let labeled: LabeledTransportAttachmentEffect = rmp_serde::from_slice(&payload).unwrap();
        assert!(
            labeled
                .information_label
                .allowed_destinations
                .contains("transport:signal:recipient")
        );
        let effect = labeled.effect;
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
pub struct OutboxDrainTask {
    pub stop: tokio::sync::watch::Sender<bool>,
    pub task: tokio::task::JoinHandle<()>,
}

pub fn spawn(state: AppState) -> OutboxDrainTask {
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
    let task = tokio::spawn(run_drain_loop(
        db,
        std::sync::Arc::new(registry),
        DrainConfig::default(),
        receiver,
    ));
    OutboxDrainTask { stop, task }
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
        let (effect, attachments, payload_information_label) =
            if self.effect_kind == ATTACHMENT_EFFECT_KIND {
                let (attachment_effect, information_label) =
                    decode_transport_attachment_effect(&row.payload)
                        .map_err(DispatchError::NotAccepted)?;
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
                    information_label,
                )
            } else {
                let (effect, information_label) = decode_transport_send_effect(&row.payload)
                    .map_err(DispatchError::NotAccepted)?;
                (effect, Vec::new(), information_label)
            };
        let information_label = load_transport_information_label(
            &self.state.db,
            &effect.text,
            payload_information_label,
        )?;
        validate_outbound_authority(&self.state.db, information_label.as_ref())?;
        let destination = format!("transport:{}:{}", effect.channel, effect.recipient);
        let text_check = execlaw_policy::outbound::inspect_outbound(
            effect.text.as_bytes(),
            information_label.as_ref(),
            &destination,
        );
        if !text_check.allowed {
            return Err(DispatchError::NotAccepted(format!(
                "outbound data check blocked transport effect: {}",
                text_check.findings.join(",")
            )));
        }
        for attachment_id in &attachments {
            inspect_transport_attachment(
                &self.state,
                &row.conversation_id,
                attachment_id,
                &destination,
            )?;
        }
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
