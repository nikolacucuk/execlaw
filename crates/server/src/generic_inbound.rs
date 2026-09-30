//! Channel-agnostic inbound routing entry point.
//!
//! Every transport plugin's frame decoder hands a decoded
//! [`InboundMessage`] here. The host owns:
//!
//!   1. Trust admit (mint principal if new, refresh if existing).
//!   2. Group binding lookup / mint.
//!   3. Conversation resolve / mint.
//!   4. Auto-rename via `apply_auto_display_name`.
//!   5. Cold-contact gate (UnknownPending senders → approval flow).
//!   6. Group-address classifier (skip dispatch for unaddressed
//!      group messages, persist them anyway).
//!   7. Turn dispatch via `dispatch_external_turn`.
//!
//! This is the channel-agnostic equivalent of
//! `signal_inbound::route_group_inbound` /
//! `route_inbound_message`. Phase B (signal plugin migration)
//! will fold those into this single router; for now both paths
//! coexist behind the new Rhai binding.

use crate::state::AppState;
use execlaw_core::agent_contract::{AgentEvent, AgentTriggerSpec};
use execlaw_core::agent_ownership::AgentOwnershipStore;
use execlaw_core::agents::{AgentSourceEvent, trigger_is_event_only};
use execlaw_core::conversation::ConversationStore;
use execlaw_core::ids::{ConversationId, PrincipalId};
use execlaw_core::principal::{Identifier, PrincipalStore, TrustLevel as CoreTrustLevel};
use execlaw_core::principal_groups::{GroupKey, PrincipalGroupStore};
use execlaw_core::reply_drafts::ReplyDraftStore;
use execlaw_core::transport_bindings::TransportBindingStore;
use execlaw_core::transport_conversations::{ConversationResolver, ResolveInput};
use execlaw_policy::trust::TrustLevel;
use execlaw_script::{HostCapError, InboundMessage, RouteOutcome};

fn is_generic_sender_name(name: &str) -> bool {
    matches!(
        name.trim().to_ascii_lowercase().as_str(),
        "you" | "me" | "self"
    )
}

/// Generic inbound routing — no Signal-specific code.
pub async fn route_inbound(
    state: &AppState,
    msg: InboundMessage,
) -> Result<RouteOutcome, HostCapError> {
    let now = chrono::Utc::now().timestamp();
    let channel = msg.channel.as_str();
    let plugin_id = format!("plugin-{channel}"); // ConversationResolver routing key
    tracing::info!(
        target: "generic_inbound",
        channel,
        native_id = %msg.native_id,
        group_id = ?msg.group_id,
        "inbound message received",
    );

    // 1. Resolve / mint the sender's principal via the shared
    //    admit helper. Same shape `signal_inbound` uses today.
    let hint_pid = PrincipalId::from(format!("pri_{channel}_{native}", native = msg.native_id));
    let (sender, _flat_trust) = crate::principal_admit::admit_external_principal(
        &state.db,
        &state.plugin_host,
        channel,
        &msg.native_id,
        hint_pid.as_str(),
    )
    .await
    .map_err(|e| HostCapError::new(format!("admit principal: {e}")))?;

    // Refresh last_seen on the principal row regardless of whether
    // admit minted it or returned an existing one.
    {
        let mut updated = sender.clone();
        updated.last_seen = Some(now);
        if let Some(metadata) = updated.metadata.as_object_mut() {
            metadata.insert(
                "native_id".to_owned(),
                serde_json::Value::String(msg.native_id.clone()),
            );
            if let Some(display_name) = msg
                .display_name
                .as_deref()
                .filter(|name| !name.trim().is_empty() && !is_generic_sender_name(name))
            {
                let current_is_generic = metadata
                    .get("display_name")
                    .and_then(|value| value.as_str())
                    .is_some_and(is_generic_sender_name);
                if !metadata.contains_key("display_name") || current_is_generic {
                    metadata.insert(
                        "display_name".to_owned(),
                        serde_json::Value::String(display_name.to_owned()),
                    );
                }
            }
        }
        let _ = PrincipalStore::new(&state.db).upsert(&updated);
    }

    // 2. Branch on group vs DM. The two shapes are similar enough
    //    that one function handles both.
    let (mut cid, principal_group_id) = if let Some(gid) = msg.group_id.as_deref() {
        resolve_group(
            state,
            channel,
            gid,
            &plugin_id,
            now,
            msg.reuse_conversation,
            msg.conversation_scope.as_deref(),
        )
        .await?
    } else {
        resolve_dm(
            state,
            channel,
            &msg.native_id,
            &sender,
            &plugin_id,
            now,
            msg.reuse_conversation,
            msg.conversation_scope.as_deref(),
        )
        .await?
    };

    if let Some(scope) = msg.conversation_scope.as_deref() {
        if should_merge_scoped_conversation(scope) {
            cid = merge_scoped_conversation_if_needed(state, &plugin_id, scope, &cid, now)?;
        } else {
            let binding_store = TransportBindingStore::new(&state.db);
            if let Ok(Some(group_id)) =
                PrincipalGroupStore::new(&state.db).principal_group_id_for(cid.as_str())
            {
                let mixed = binding_store
                    .bindings_for_group_any_channel(&group_id)
                    .map(|bindings| bindings.iter().any(|binding| binding.channel != channel))
                    .unwrap_or(false);
                if mixed {
                    if let Some(fresh) =
                        execlaw_core::transport_conversations::TransportConversationStore::new(
                            &state.db,
                        )
                        .force_rotate_current(&plugin_id, scope, scope, now)
                        .map_err(|e| {
                            HostCapError::new(format!("dedicated conversation rotate: {e}"))
                        })?
                    {
                        cid = fresh;
                    }
                }
            }
        }
    }

    // 3. Conversation row + binding.
    crate::chats::ensure_conversation_for(&state.db, &cid);
    let display_name_for_seed = if msg.group_id.is_some() {
        msg.group_name.as_deref()
    } else {
        msg.display_name.as_deref()
    };
    crate::chats::apply_auto_display_name(&state.db, &cid, display_name_for_seed);
    if msg
        .conversation_scope
        .as_deref()
        .is_some_and(|scope| scope.ends_with("-dedicated"))
    {
        let label = if channel == "whatsapp" {
            "WhatsApp"
        } else {
            "Signal"
        };
        let _ = ConversationStore::new(&state.db).set_display_name(&cid, Some(label));
    }
    let pg_store = PrincipalGroupStore::new(&state.db);
    pg_store
        .bind_conversation(cid.as_str(), &principal_group_id)
        .map_err(|e| HostCapError::new(format!("bind conversation: {e}")))?;
    if let Err(error) = crate::message_archive::archive_inbound(state, &msg, &cid, &sender) {
        tracing::error!(
            target: "message_archive",
            channel,
            error,
            "failed to archive inbound transport message"
        );
    }
    tracing::info!(
        target: "generic_inbound",
        channel,
        conversation_id = %cid,
        principal_group_id = %principal_group_id,
        "inbound message mapped to conversation",
    );

    // 3b. For groups, opportunistically grow the principal_group's
    // member list as senders appear. `resolve_group` mints with
    // empty `principals: &[]` because the transport plugins
    // (Signal / WhatsApp / Slack) only know the *current* sender
    // + the group JID on each inbound — none of them include the
    // full roster in their per-message wire format. Without this
    // step the member table stays empty forever, and
    // `should_dispatch_to_agent`'s eligibility gate
    // (`members.len() < 2`) returns `EligibilityBypass` on every
    // turn — the classifier never runs. That's the root cause of
    // "the agent keeps barging into group conversations."
    //
    // We seed two principal_ids per group inbound:
    //   * the observed sender (so subsequent messages from the
    //     same person reuse the row and we accumulate distinct
    //     senders over time)
    //   * the controller (always implicitly in the group — the
    //     operator owns the bridged WhatsApp/Signal/Slack identity
    //     — but never sends inbound there, so otherwise wouldn't
    //     get added)
    //
    // Together these bring `members.len()` to ≥2 as soon as the
    // first non-Controller speaks, which is exactly when the
    // addressing classifier becomes useful.
    if msg.group_id.is_some() {
        if let Err(e) = pg_store.add_member(&principal_group_id, &sender.id, now) {
            tracing::warn!(
                target: "generic_inbound",
                error = %e,
                group_id = %principal_group_id,
                principal_id = %sender.id.as_str(),
                "could not add sender to group membership; addressing classifier may bypass on this turn",
            );
        }
        match crate::routes::controller_principal_id(&state.db) {
            Ok(controller_pid) => {
                if let Err(e) = pg_store.add_member(&principal_group_id, &controller_pid, now) {
                    tracing::warn!(
                        target: "generic_inbound",
                        error = %e,
                        group_id = %principal_group_id,
                        "could not add controller to group membership",
                    );
                }
            }
            Err(e) => {
                // Fresh install before bootstrap finishes can land
                // here. Not fatal — the next inbound after
                // bootstrap will succeed.
                tracing::debug!(
                    target: "generic_inbound",
                    error = ?e,
                    "controller_principal_id unavailable; skipping controller-membership seed",
                );
            }
        }
    }

    // 4. Trust gate.
    let trust_tag = sender.trust_level.class_tag();
    let trust_flat = TrustLevel::parse(trust_tag).unwrap_or(TrustLevel::UnknownPending);

    if trust_flat == TrustLevel::Blocked {
        return Ok(RouteOutcome::Blocked);
    }

    if trust_flat == TrustLevel::UnknownPending {
        crate::chats::handle_cold_contact_for_inbound(state, &cid, &sender, &msg.text, channel)
            .await
            .map_err(|e| HostCapError::new(format!("cold-contact handler: {e}")))?;
        return Ok(RouteOutcome::ColdContact);
    }

    // 5. Inbound image attachments. Fetch + persist BEFORE the
    // group-addressing check so:
    //   (a) silent-commit paths (unaddressed group messages) still
    //       carry the attachment refs through to the SPA's bubble —
    //       operator sees the photo even though no turn ran;
    //   (b) the agent dispatch path has the same `attachment_ids`
    //       handle the web composer's `+` flow produces.
    //
    // Non-image attachments (PDFs, audio, video, etc.) are skipped
    // by `persist_inbound_attachments`; vision models can't see
    // them and a follow-up PR will add per-kind preprocessors
    // (whisper for audio, text extraction for PDFs).
    //
    // Failure to fetch any single attachment doesn't fail the
    // turn — the helper logs at WARN and continues.
    let attachment_ids: Vec<String> =
        crate::chats::persist_inbound_attachments(state, &cid, channel, &msg.attachments).await;

    if msg.is_self_message || !msg.agent_handling_enabled {
        let source_seq = crate::chats::commit_inbound_user_msg_silently(
            state,
            &cid,
            sender.id.as_str(),
            &msg.text,
            channel,
            msg.group_id.as_deref().unwrap_or(&msg.native_id),
            attachment_ids,
        )
        .await
        .map_err(|error| {
            HostCapError::new(format!(
                "persist inbound message with agent handling disabled: {error}"
            ))
        })?;
        ReplyDraftStore::new(&state.db)
            .stale_after_inbound(
                cid.as_str(),
                channel,
                reply_recipient(&msg.native_id, msg.group_id.as_deref()),
                source_seq,
                now,
            )
            .map_err(|error| HostCapError::new(format!("stale earlier drafts: {error}")))?;
        return Ok(RouteOutcome::GroupNotAddressed);
    }

    let recipient = reply_recipient(&msg.native_id, msg.group_id.as_deref());
    let ownership_store = AgentOwnershipStore::new(&state.db);
    let owner = ownership_store
        .get(cid.as_str(), channel, recipient)
        .map_err(|error| HostCapError::new(format!("read agent owner: {error}")))?;
    if owner
        .as_ref()
        .is_some_and(|owner| owner.owner_kind == "controller")
    {
        let source_seq = crate::chats::commit_inbound_user_msg_silently(
            state,
            &cid,
            sender.id.as_str(),
            &msg.text,
            channel,
            recipient,
            attachment_ids,
        )
        .await
        .map_err(|error| HostCapError::new(format!("persist Controller-owned inbound: {error}")))?;
        ReplyDraftStore::new(&state.db)
            .stale_after_inbound(cid.as_str(), channel, recipient, source_seq, now)
            .map_err(|error| HostCapError::new(format!("stale earlier drafts: {error}")))?;
        return Ok(RouteOutcome::ControllerOwned);
    }

    let mut triggered_agents = matching_triggered_agents(state, channel, &msg)
        .map_err(|error| HostCapError::new(format!("match triggered agents: {error}")))?;
    let lead_id = if let Some(owner) = owner.as_ref() {
        triggered_agents
            .iter()
            .find(|agent| Some(agent.id.as_str()) == owner.agent_id.as_deref())
            .map(|agent| agent.id.clone())
    } else {
        triggered_agents
            .iter()
            .find(|agent| {
                !agent
                    .trigger
                    .get("observer")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false)
            })
            .map(|agent| agent.id.clone())
    };
    triggered_agents.retain(|agent| {
        Some(agent.id.as_str()) == lead_id.as_deref()
            || agent
                .trigger
                .get("observer")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
    });
    if triggered_agents.iter().any(|agent| {
        Some(agent.id.as_str()) == lead_id.as_deref() && trigger_is_event_only(&agent.trigger)
    }) {
        // A specialist owns this inbound. Persist it for the conversation UI
        // before waking the worker, and avoid a second reply from the main turn.
        let source_seq = crate::chats::commit_inbound_user_msg_silently(
            state,
            &cid,
            sender.id.as_str(),
            &msg.text,
            channel,
            msg.group_id.as_deref().unwrap_or(&msg.native_id),
            attachment_ids,
        )
        .await
        .map_err(|error| HostCapError::new(format!("persist specialist inbound: {error}")))?;
        if let Some(lead_id) = lead_id.as_deref() {
            let assigned = ownership_store
                .assign_agent(cid.as_str(), channel, recipient, lead_id, now)
                .map_err(|error| HostCapError::new(format!("assign agent owner: {error}")))?;
            if assigned.agent_id.as_deref() != Some(lead_id) {
                return Err(HostCapError::new(
                    "agent ownership changed during inbound routing",
                ));
            }
        }
        ReplyDraftStore::new(&state.db)
            .stale_after_inbound(
                cid.as_str(),
                channel,
                reply_recipient(&msg.native_id, msg.group_id.as_deref()),
                source_seq,
                now,
            )
            .map_err(|error| HostCapError::new(format!("stale earlier drafts: {error}")))?;
        enqueue_triggered_agents(
            state,
            channel,
            &cid,
            &msg,
            &triggered_agents,
            Some(source_seq),
        )
        .map_err(|error| HostCapError::new(format!("enqueue triggered agents: {error}")))?;
        return Ok(RouteOutcome::SpecialistQueued);
    }

    // 6. Group address filter + group-context resolution. For DMs
    // we leave `group_context = None` and dispatch directly. For
    // groups we consult the addressing classifier; on Skip we
    // silent-commit; on Dispatch we build the per-turn group
    // context (name + member count + addressed reason) so the
    // agent's system prompt knows it's in a group AND why this
    // message reached it.
    //
    // 2026-05-15 — image attachments shortcut the classifier: a
    // group member sending the agent a photo is almost always
    // intentionally addressing the agent (text-only banter
    // doesn't normally include media), and image-only messages
    // have empty text the classifier would otherwise filter out
    // every time.
    let has_image_attachment = !attachment_ids.is_empty();
    let group_context: Option<crate::chats::GroupTurnContext> = if msg.group_id.is_some() {
        if has_image_attachment {
            // Skip the classifier; treat as addressed via the
            // attachment signal.
            crate::chats::resolve_group_turn_context(
                state,
                &cid,
                crate::group_addressing::AddressedReason::AttachmentDirected,
            )
        } else {
            let decision = if channel == "whatsapp" && msg.conversation_scope.is_some() {
                crate::group_addressing::DispatchDecision::Dispatch(
                    crate::group_addressing::AddressedReason::TransportMention,
                )
            } else {
                crate::group_addressing::should_dispatch_to_agent(
                    state,
                    &cid,
                    &msg.text,
                    msg.mention_of_self,
                )
                .await
            };
            match decision {
                crate::group_addressing::DispatchDecision::Skip => {
                    // Persist for context; skip dispatch.
                    match crate::chats::commit_inbound_user_msg_silently(
                        state,
                        &cid,
                        sender.id.as_str(),
                        &msg.text,
                        channel,
                        msg.group_id.as_deref().unwrap_or(&msg.native_id),
                        attachment_ids.clone(),
                    )
                    .await
                    {
                        Err(error) => tracing::warn!(
                            target: "generic_inbound",
                            error = %error,
                            conversation_id = %cid.as_str(),
                            "silent commit of unaddressed group message failed",
                        ),
                        Ok(source_seq) => {
                            ReplyDraftStore::new(&state.db)
                                .stale_after_inbound(
                                    cid.as_str(),
                                    channel,
                                    recipient,
                                    source_seq,
                                    now,
                                )
                                .map_err(|error| {
                                    HostCapError::new(format!("stale earlier drafts: {error}"))
                                })?;
                            enqueue_triggered_agents(
                                state,
                                channel,
                                &cid,
                                &msg,
                                &triggered_agents,
                                Some(source_seq),
                            )
                            .map_err(|e| {
                                HostCapError::new(format!("enqueue triggered agents: {e}"))
                            })?;
                        }
                    }
                    return Ok(RouteOutcome::GroupNotAddressed);
                }
                crate::group_addressing::DispatchDecision::Dispatch(reason) => {
                    // The classifier already ran the eligibility +
                    // members lookups internally. The resolver here
                    // re-runs the cheap members/name lookups to build
                    // the per-turn context — duplicate cost is small
                    // (two SQLite reads on indexed tables) and keeping
                    // the resolver self-contained simplifies the
                    // approval-replay paths that don't have a verdict
                    // to thread.
                    crate::chats::resolve_group_turn_context(state, &cid, reason)
                }
            }
        }
    } else {
        None
    };

    enqueue_triggered_agents(state, channel, &cid, &msg, &triggered_agents, None)
        .map_err(|e| HostCapError::new(format!("enqueue triggered agents: {e}")))?;

    // 7. Dispatch the turn through the standard pipeline.
    crate::chats::dispatch_external_turn(
        state,
        &cid,
        &sender,
        trust_flat,
        &msg.text,
        Some(channel),
        Some(msg.group_id.as_deref().unwrap_or(&msg.native_id)),
        group_context,
        attachment_ids,
    )
    .await
    .map_err(|e| HostCapError::new(format!("dispatch_external_turn: {e}")))?;
    if let Ok(seq) = crate::chats::event_log(state).last_seq(&cid) {
        ReplyDraftStore::new(&state.db)
            .stale_after_inbound(
                cid.as_str(),
                channel,
                reply_recipient(&msg.native_id, msg.group_id.as_deref()),
                seq.0,
                now,
            )
            .map_err(|error| HostCapError::new(format!("stale earlier drafts: {error}")))?;
    }
    Ok(RouteOutcome::Dispatched)
}

fn should_merge_scoped_conversation(scope: &str) -> bool {
    !scope.ends_with("-dedicated")
}

fn should_use_controller_thread(is_controller: bool, conversation_scope: Option<&str>) -> bool {
    is_controller && !conversation_scope.is_some_and(|scope| scope.ends_with("-dedicated"))
}

fn merge_scoped_conversation_if_needed(
    state: &AppState,
    plugin_id: &str,
    scope: &str,
    current_cid: &ConversationId,
    now: i64,
) -> Result<ConversationId, HostCapError> {
    let summaries = execlaw_core::conversation::ConversationStore::new(&state.db)
        .list_thread_summaries()
        .map_err(|e| HostCapError::new(format!("list active conversations: {e}")))?;
    let target = summaries
        .into_iter()
        .filter(|summary| {
            !summary.is_pinned
                && !summary.is_ephemeral
                && !summary
                    .conversation_id
                    .as_str()
                    .starts_with("controller-thread:")
        })
        .max_by_key(|summary| summary.last_activity_at);
    let Some(target) = target else {
        return Ok(current_cid.clone());
    };

    // WhatsApp is intentionally a shared operator thread: once a normal
    // chat exists, every WhatsApp contact/group scope follows the newest
    // normal conversation. Keep the current mapping when it is already
    // the selected target; otherwise move only this scope so the next
    // inbound lands in the latest chat without rewriting event history.
    if target.conversation_id == *current_cid {
        return Ok(current_cid.clone());
    }

    let store = execlaw_core::transport_conversations::TransportConversationStore::new(&state.db);
    let moved = store
        .retarget_current(plugin_id, scope, scope, &target.conversation_id, now)
        .map_err(|e| HostCapError::new(format!("retarget scoped conversation: {e}")))?;
    if moved {
        tracing::info!(
            target: "generic_inbound",
            from = %current_cid,
            to = %target.conversation_id,
            scope,
            "merged scoped transport into active conversation"
        );
        Ok(target.conversation_id)
    } else {
        Ok(current_cid.clone())
    }
}

fn enqueue_triggered_agents(
    state: &AppState,
    channel: &str,
    conversation_id: &ConversationId,
    msg: &InboundMessage,
    agents: &[execlaw_core::agents::AgentRow],
    source_seq: Option<i64>,
) -> Result<(), String> {
    let store = execlaw_core::agents::AgentStore::new(&state.db);
    let now = chrono::Utc::now().timestamp();
    let mut queued = false;
    for agent in agents {
        let source_event_id = msg
            .source_event_id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let recipient = reply_recipient(&msg.native_id, msg.group_id.as_deref());
        let envelope = serde_json::json!({
            "channel": channel,
            "conversation_id": conversation_id.as_str(),
            "recipient": recipient,
            "source_event_id": source_event_id.clone(),
            "source_occurred_at": msg.timestamp_ms.map(|timestamp| timestamp / 1000),
            "source_event_seq": source_seq,
            "definition_version": agent.definition_version,
            "display_name": msg.display_name,
            "text": msg.text,
            "group_id": msg.group_id,
            "group_name": msg.group_name,
        });
        let source_kind = format!("transport:{channel}");
        let content = envelope.to_string();
        let event = AgentSourceEvent {
            source_kind: &source_kind,
            source_event_id: &source_event_id,
            occurred_at: msg
                .timestamp_ms
                .map(|timestamp| timestamp / 1000)
                .unwrap_or(now),
            conversation_id: conversation_id.as_str(),
            recipient,
            content: &content,
        };
        let (_, inserted) = store
            .enqueue_triggered_event(&agent.id, &event, now)
            .map_err(|e| e.to_string())?;
        queued |= inserted;
    }
    if queued {
        crate::agent_supervisor::AgentSupervisor::kick_global();
    }
    Ok(())
}

fn matching_triggered_agents(
    state: &AppState,
    channel: &str,
    msg: &InboundMessage,
) -> Result<Vec<execlaw_core::agents::AgentRow>, String> {
    let agents = execlaw_core::agents::AgentStore::new(&state.db)
        .list()
        .map_err(|error| error.to_string())?;
    let mut matches = agents
        .into_iter()
        .filter(|agent| {
            agent.enabled
                && !agent.paused
                && trigger_matches(
                    &agent.trigger,
                    channel,
                    msg.group_id.as_deref(),
                    msg.group_name.as_deref(),
                    &msg.text,
                )
        })
        .collect::<Vec<_>>();
    matches.sort_by(|left, right| {
        let left_priority = AgentTriggerSpec::from_value(&left.trigger)
            .map(|trigger| trigger.priority)
            .unwrap_or(0);
        let right_priority = AgentTriggerSpec::from_value(&right.trigger)
            .map(|trigger| trigger.priority)
            .unwrap_or(0);
        right_priority
            .cmp(&left_priority)
            .then_with(|| left.id.cmp(&right.id))
    });
    Ok(matches)
}

fn reply_recipient<'a>(native_id: &'a str, group_id: Option<&'a str>) -> &'a str {
    group_id.unwrap_or(native_id)
}

fn trigger_matches(
    trigger: &serde_json::Value,
    channel: &str,
    group_id: Option<&str>,
    group_name: Option<&str>,
    text: &str,
) -> bool {
    let Ok(trigger) = AgentTriggerSpec::from_value(trigger) else {
        return false;
    };
    let event = AgentEvent {
        source: channel.to_owned(),
        id: String::new(),
        channel: channel.to_owned(),
        recipient: group_id.unwrap_or_default().to_owned(),
        group_id: group_id.map(str::to_owned),
        group_name: group_name.map(str::to_owned),
        text: text.to_owned(),
        occurred_at: 0,
    };
    trigger.match_reason(&event).ok().flatten().is_some()
}

async fn resolve_group(
    state: &AppState,
    channel: &str,
    group_id: &str,
    plugin_id: &str,
    now: i64,
    reuse_conversation: bool,
    conversation_scope: Option<&str>,
) -> Result<(ConversationId, String), HostCapError> {
    let binding_store = TransportBindingStore::new(&state.db);
    let pg_store = PrincipalGroupStore::new(&state.db);
    let group_pg_id = match binding_store
        .lookup_principal_group(channel, group_id)
        .map_err(|e| HostCapError::new(format!("group binding lookup: {e}")))?
    {
        Some(pg_id) => pg_id,
        None => {
            let pg = pg_store
                .resolve(
                    &GroupKey {
                        channel,
                        native_group_id: Some(group_id),
                        principals: &[],
                        includes_controller: true,
                    },
                    now,
                )
                .map_err(|e| HostCapError::new(format!("group principal_group mint: {e}")))?;
            let inserted = binding_store
                .insert_binding(channel, group_id, &pg.group_id, true, now)
                .map_err(|e| HostCapError::new(format!("group binding insert: {e}")))?;
            if !inserted {
                binding_store
                    .lookup_principal_group(channel, group_id)
                    .map_err(|e| HostCapError::new(format!("group binding re-lookup: {e}")))?
                    .ok_or_else(|| HostCapError::new("group binding vanished after insert race"))?
            } else {
                pg.group_id
            }
        }
    };
    let resolver = ConversationResolver::new(&state.db);
    // A transport-wide scope intentionally overrides the sender/group key.
    // WhatsApp supplies `conversation_scope = "whatsapp"`, so every direct
    // contact and group shares one current operator conversation while the
    // principal-group binding still preserves the real reply destination.
    let resolver_handle = conversation_scope.unwrap_or(group_id);
    let resolver_principal = conversation_scope.unwrap_or(group_id);
    let outcome = resolver
        .resolve_or_mint(&ResolveInput {
            plugin_id,
            transport_handle: resolver_handle,
            principal_id: resolver_principal,
            is_controller: false,
            idle_timeout_ms: if reuse_conversation {
                None
            } else {
                Some(30 * 60 * 1000)
            },
            now,
        })
        .map_err(|e| HostCapError::new(format!("group conversation resolve: {e}")))?;
    Ok((outcome.conversation_id().clone(), group_pg_id))
}

async fn resolve_dm(
    state: &AppState,
    channel: &str,
    native_id: &str,
    sender: &execlaw_core::principal::Principal,
    plugin_id: &str,
    now: i64,
    reuse_conversation: bool,
    conversation_scope: Option<&str>,
) -> Result<(ConversationId, String), HostCapError> {
    let binding_store = TransportBindingStore::new(&state.db);
    let pg_store = PrincipalGroupStore::new(&state.db);
    let principal_group_id = match binding_store
        .lookup_principal_group(channel, native_id)
        .map_err(|e| HostCapError::new(format!("binding lookup: {e}")))?
    {
        Some(pg_id) => pg_id,
        None => {
            let pid_array = [sender.id.clone()];
            let pg = pg_store
                .resolve(
                    &GroupKey {
                        channel,
                        native_group_id: None,
                        principals: &pid_array,
                        includes_controller: matches!(
                            sender.trust_level,
                            CoreTrustLevel::Controller
                        ),
                    },
                    now,
                )
                .map_err(|e| HostCapError::new(format!("principal_group mint: {e}")))?;
            let _ = binding_store
                .insert_binding(channel, native_id, &pg.group_id, false, now)
                .map_err(|e| HostCapError::new(format!("binding insert: {e}")))?;
            // Identifier upsert so future lookups by handle resolve.
            let mut updated = sender.clone();
            let ident = Identifier {
                transport: channel.to_owned(),
                handle: native_id.to_owned(),
            };
            if !updated
                .identifiers
                .iter()
                .any(|i| i.transport == ident.transport && i.handle == ident.handle)
            {
                updated.identifiers.push(ident);
                let _ = PrincipalStore::new(&state.db).upsert(&updated);
            }
            pg.group_id
        }
    };
    let is_controller = should_use_controller_thread(
        matches!(sender.trust_level, CoreTrustLevel::Controller),
        conversation_scope,
    );
    let resolver = ConversationResolver::new(&state.db);
    // Keep the resolver key transport-scoped rather than contact-scoped
    // when the plugin declares a shared operator conversation.
    let resolver_handle = conversation_scope.unwrap_or(native_id);
    let resolver_principal = if is_controller {
        sender.id.as_str()
    } else {
        conversation_scope.unwrap_or(sender.id.as_str())
    };
    let outcome = resolver
        .resolve_or_mint(&ResolveInput {
            plugin_id,
            transport_handle: resolver_handle,
            principal_id: resolver_principal,
            is_controller,
            idle_timeout_ms: if reuse_conversation {
                None
            } else {
                Some(30 * 60 * 1000)
            },
            now,
        })
        .map_err(|e| HostCapError::new(format!("conversation resolve: {e}")))?;
    Ok((outcome.conversation_id().clone(), principal_group_id))
}

#[cfg(test)]
mod tests {
    use super::{matching_triggered_agents, reply_recipient, trigger_matches};
    use execlaw_core::agents::{AgentStore, AgentUpsert, trigger_is_event_only};
    use execlaw_script::InboundMessage;
    use serde_json::json;

    #[test]
    fn group_agent_drafts_target_group_not_sender() {
        assert_eq!(
            reply_recipient("sender-phone", Some("group@g.us")),
            "group@g.us"
        );
        assert_eq!(reply_recipient("sender-phone", None), "sender-phone");
    }

    #[test]
    fn matching_event_only_specialist_owns_camper_group_inbound() {
        let state = crate::routes::test_app_state();
        AgentStore::new(&state.db)
            .upsert(&AgentUpsert {
                id: Some("camper_wha".into()),
                name: "camper_wha".into(),
                role_prompt: "Draft a reply".into(),
                model: None,
                backend_purpose: "standard".into(),
                tools: Vec::new(),
                trust_policy: json!({}),
                interval_secs: 300,
                token_budget: 1200,
                max_runtime_secs: 120,
                concurrency_limit: 1,
                enabled: true,
                trigger: json!({"channel":"whatsapp","group_only":true,"event_only":true,"keywords":["camper"]}),
                reply_mode: "draft".into(),
            }, 10)
            .unwrap();
        let inbound = InboundMessage {
            channel: "whatsapp".into(),
            source_event_id: Some("test-message".into()),
            native_id: "sender".into(),
            display_name: None,
            group_id: Some("group@g.us".into()),
            group_name: None,
            text: "Is the camper available?".into(),
            timestamp_ms: None,
            attachments: Vec::new(),
            mention_of_self: None,
            reuse_conversation: true,
            conversation_scope: Some("whatsapp".into()),
            agent_handling_enabled: true,
            is_self_message: false,
        };
        let matches = matching_triggered_agents(&state, "whatsapp", &inbound).unwrap();
        assert_eq!(matches.len(), 1);
        assert!(trigger_is_event_only(&matches[0].trigger));
    }

    #[test]
    fn trigger_matches_channel_and_keyword_case_insensitively() {
        let trigger = json!({
            "channel": "whatsapp",
            "keywords": ["camper", "camper van", "motorhome"]
        });
        assert!(trigger_matches(
            &trigger,
            "WhatsApp",
            None,
            None,
            "Do you rent a CAMPER van?"
        ));
        assert!(!trigger_matches(
            &trigger,
            "signal",
            None,
            None,
            "Do you rent a camper?"
        ));
        assert!(!trigger_matches(
            &trigger,
            "whatsapp",
            None,
            None,
            "Can you help with a boat?"
        ));
    }

    #[test]
    fn channel_only_trigger_matches_without_keywords() {
        let trigger = json!({"channel": "whatsapp"});
        assert!(trigger_matches(&trigger, "whatsapp", None, None, "hello"));
        assert!(!trigger_matches(&trigger, "signal", None, None, "hello"));
    }

    #[test]
    fn group_only_trigger_ignores_matching_direct_messages() {
        let trigger = json!({
            "channel": "whatsapp",
            "group_only": true,
            "keywords": ["camper"]
        });

        assert!(!trigger_matches(
            &trigger,
            "whatsapp",
            None,
            None,
            "Camper available?"
        ));
        assert!(trigger_matches(
            &trigger,
            "whatsapp",
            Some("group-123"),
            None,
            "Camper available?"
        ));
    }

    #[test]
    fn group_only_trigger_still_requires_channel_and_keyword() {
        let trigger = json!({
            "channel": "whatsapp",
            "group_only": true,
            "keywords": ["camper"]
        });

        assert!(!trigger_matches(
            &trigger,
            "signal",
            Some("group-123"),
            None,
            "Camper available?"
        ));
        assert!(!trigger_matches(
            &trigger,
            "whatsapp",
            Some("group-123"),
            None,
            "What time is dinner?"
        ));
    }

    #[test]
    fn trigger_matches_keywords_in_group_title() {
        let trigger = json!({
            "channel": "whatsapp",
            "group_only": true,
            "keywords": ["camper"]
        });

        assert!(trigger_matches(
            &trigger,
            "whatsapp",
            Some("group-123"),
            Some("1th Sept 2026, Luka Villa, Camper Montenegro"),
            "I'm at the beach haha"
        ));
    }

    #[test]
    fn explicit_group_title_also_requires_relevant_text() {
        let trigger = json!({
            "channel": "whatsapp",
            "group_only": true,
            "group_titles": ["1th Sept 2026, Luka Villa, Montenegro"],
            "keywords": ["camper"]
        });

        assert!(trigger_matches(
            &trigger,
            "whatsapp",
            Some("group-123"),
            Some("1th Sept 2026, Luka Villa, Montenegro"),
            "Is the camper available?"
        ));
        assert!(!trigger_matches(
            &trigger,
            "whatsapp",
            Some("group-123"),
            Some("1th Sept 2026, Luka Villa, Montenegro"),
            "I'm at the beach haha"
        ));
        assert!(!trigger_matches(
            &trigger,
            "whatsapp",
            Some("group-456"),
            Some("Another Montenegro group"),
            "Is the camper available?"
        ));
    }

    #[test]
    fn dedicated_transport_scopes_do_not_merge_into_latest_chat() {
        assert!(!super::should_merge_scoped_conversation("signal-dedicated"));
        assert!(!super::should_merge_scoped_conversation(
            "whatsapp-dedicated"
        ));
        assert!(super::should_merge_scoped_conversation("signal"));
        assert!(super::should_merge_scoped_conversation("whatsapp"));
    }

    #[test]
    fn dedicated_transport_scopes_do_not_use_controller_thread() {
        assert!(!super::should_use_controller_thread(
            true,
            Some("whatsapp-dedicated")
        ));
        assert!(!super::should_use_controller_thread(
            true,
            Some("signal-dedicated")
        ));
        assert!(super::should_use_controller_thread(true, Some("whatsapp")));
        assert!(super::should_use_controller_thread(true, None));
        assert!(!super::should_use_controller_thread(false, None));
    }
}
