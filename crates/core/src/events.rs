//! Event log primitives (§2.3, §2.4).
//!
//! The `state_events` table is append-only. Reads replay events in order to
//! reconstruct conversation state. Turns commit atomically — see `commit_turn`
//! which enforces the `tool_use`/`tool_result` pairing invariant by
//! synthesizing cancellation results for any open tool_use without a
//! matching result.

use crate::db::{Database, DbError};
use crate::ids::{ConversationId, EventSeq};
use rmp_serde as rmps;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::fmt;

/// The set of event kinds a replay must be able to reconstruct.
///
/// This is **additive** — new kinds land without breaking existing consumers
/// because replay is expected to tolerate unknown kinds by skipping them
/// for state-reconstruction purposes while still carrying them forward in
/// forensic queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EventKind {
    UserMsg,
    ModelTurn,
    ToolUse,
    ToolResult,
    Interrupt,
    Resume,
    Approval,
    TransportReviewDecision,
    EffectCommitted,
    Wakeup,
    AlertFired,
    AlertRenotified,
    AlertAcked,
    AlertResolved,
    AlertSnoozed,
    IncidentOpened,
    IncidentClosed,
    ColdContactArrived,
    IdentityResolutionConflict,
    TrustChanged,
    VoiceSessionStarted,
    VoiceSessionEnded,
    AudioInChunk,
    VadSpeechStarted,
    VadSpeechEnded,
    SttPartial,
    SttFinal,
    TurnUserEnded,
    LlmToken,
    LlmResponseFinal,
    LlmCancelled,
    TtsFirstAudio,
    TtsAudioChunk,
    TtsEnded,
    InterruptStarted,
    InterruptRescinded,
    InterruptConfirmed,
    ResearchProgressUpdated,
    /// 2026-04-29 — generic card primitive for long-running operator-
    /// visible tasks. `CardOpened` introduces the card with a kind +
    /// initial state; `CardProgressed` updates progress / phase /
    /// details; `CardClosed` flips it to a terminal state and may
    /// reference an attachment. The card's persistent shape is the
    /// projection of its sequence of events; renderers (web SPA) and
    /// transports (Signal / WhatsApp / SMS / voice) compose the live
    /// view. See `crate::cards` for the helpers + tests.
    CardOpened,
    CardProgressed,
    CardClosed,
    /// 2026-04-29 — subagent lifecycle (synchronous in-turn child
    /// LLM call). The SPA's typing-indicator pill subscribes to
    /// these so the operator sees "subagent: <task>" while the
    /// parent is mid-turn waiting for the child.
    SubagentStarted,
    SubagentCompleted,
    /// 2026-05-02 — skill subsystem (Phase A). DB-backed,
    /// versioned, admin-authored / plugin-registered procedural
    /// knowledge. `SkillCreated` fires on the first version of a
    /// new skill; `SkillVersionAdded` on subsequent edits;
    /// `SkillPromoted` when a version transitions trial → stable;
    /// `SkillArchived` when the skill is retired; `SkillInvoked`
    /// when an agent loads the full body via `skills.view`;
    /// `SkillWriteRejected` when the secret scanner blocks a
    /// write. See `execlaw-skills` for emission sites.
    SkillCreated,
    SkillVersionAdded,
    SkillPromoted,
    SkillArchived,
    SkillInvoked,
    SkillWriteRejected,
    /// Escape hatch for future additions that predate this enum. The payload
    /// contains the original kind string.
    Other,
}

impl EventKind {
    /// Canonical wire form used in `state_events.kind`.
    pub fn as_str(&self) -> &'static str {
        match self {
            EventKind::UserMsg => "user_msg",
            EventKind::ModelTurn => "model_turn",
            EventKind::ToolUse => "tool_use",
            EventKind::ToolResult => "tool_result",
            EventKind::Interrupt => "interrupt",
            EventKind::Resume => "resume",
            EventKind::Approval => "approval",
            EventKind::TransportReviewDecision => "transport_review_decision",
            EventKind::EffectCommitted => "effect_committed",
            EventKind::Wakeup => "wakeup",
            EventKind::AlertFired => "alert_fired",
            EventKind::AlertRenotified => "alert_renotified",
            EventKind::AlertAcked => "alert_acked",
            EventKind::AlertResolved => "alert_resolved",
            EventKind::AlertSnoozed => "alert_snoozed",
            EventKind::IncidentOpened => "incident_opened",
            EventKind::IncidentClosed => "incident_closed",
            EventKind::ColdContactArrived => "cold_contact_arrived",
            EventKind::IdentityResolutionConflict => "identity_resolution_conflict",
            EventKind::TrustChanged => "trust_changed",
            EventKind::VoiceSessionStarted => "voice.session_started",
            EventKind::VoiceSessionEnded => "voice.session_ended",
            EventKind::AudioInChunk => "audio.in_chunk",
            EventKind::VadSpeechStarted => "vad.speech_started",
            EventKind::VadSpeechEnded => "vad.speech_ended",
            EventKind::SttPartial => "stt.partial",
            EventKind::SttFinal => "stt.final",
            EventKind::TurnUserEnded => "turn.user_ended",
            EventKind::LlmToken => "llm.token",
            EventKind::LlmResponseFinal => "llm.response_final",
            EventKind::LlmCancelled => "llm.cancelled",
            EventKind::TtsFirstAudio => "tts.first_audio",
            EventKind::TtsAudioChunk => "tts.audio_chunk",
            EventKind::TtsEnded => "tts.ended",
            EventKind::InterruptStarted => "interrupt.started",
            EventKind::InterruptRescinded => "interrupt.rescinded",
            EventKind::InterruptConfirmed => "interrupt.confirmed",
            EventKind::ResearchProgressUpdated => "research_progress_updated",
            EventKind::CardOpened => "card.opened",
            EventKind::CardProgressed => "card.progressed",
            EventKind::CardClosed => "card.closed",
            EventKind::SubagentStarted => "subagent.started",
            EventKind::SubagentCompleted => "subagent.completed",
            EventKind::SkillCreated => "skill.created",
            EventKind::SkillVersionAdded => "skill.version_added",
            EventKind::SkillPromoted => "skill.promoted",
            EventKind::SkillArchived => "skill.archived",
            EventKind::SkillInvoked => "skill.invoked",
            EventKind::SkillWriteRejected => "skill.write_rejected",
            EventKind::Other => "other",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "user_msg" => EventKind::UserMsg,
            "model_turn" => EventKind::ModelTurn,
            "tool_use" => EventKind::ToolUse,
            "tool_result" => EventKind::ToolResult,
            "interrupt" => EventKind::Interrupt,
            "resume" => EventKind::Resume,
            "approval" => EventKind::Approval,
            "transport_review_decision" => EventKind::TransportReviewDecision,
            "effect_committed" => EventKind::EffectCommitted,
            "wakeup" => EventKind::Wakeup,
            "alert_fired" => EventKind::AlertFired,
            "alert_renotified" => EventKind::AlertRenotified,
            "alert_acked" => EventKind::AlertAcked,
            "alert_resolved" => EventKind::AlertResolved,
            "alert_snoozed" => EventKind::AlertSnoozed,
            "incident_opened" => EventKind::IncidentOpened,
            "incident_closed" => EventKind::IncidentClosed,
            "cold_contact_arrived" => EventKind::ColdContactArrived,
            "identity_resolution_conflict" => EventKind::IdentityResolutionConflict,
            "trust_changed" => EventKind::TrustChanged,
            "voice.session_started" => EventKind::VoiceSessionStarted,
            "voice.session_ended" => EventKind::VoiceSessionEnded,
            "audio.in_chunk" => EventKind::AudioInChunk,
            "vad.speech_started" => EventKind::VadSpeechStarted,
            "vad.speech_ended" => EventKind::VadSpeechEnded,
            "stt.partial" => EventKind::SttPartial,
            "stt.final" => EventKind::SttFinal,
            "turn.user_ended" => EventKind::TurnUserEnded,
            "llm.token" => EventKind::LlmToken,
            "llm.response_final" => EventKind::LlmResponseFinal,
            "llm.cancelled" => EventKind::LlmCancelled,
            "tts.first_audio" => EventKind::TtsFirstAudio,
            "tts.audio_chunk" => EventKind::TtsAudioChunk,
            "tts.ended" => EventKind::TtsEnded,
            "interrupt.started" => EventKind::InterruptStarted,
            "interrupt.rescinded" => EventKind::InterruptRescinded,
            "interrupt.confirmed" => EventKind::InterruptConfirmed,
            "research_progress_updated" => EventKind::ResearchProgressUpdated,
            "card.opened" => EventKind::CardOpened,
            "card.progressed" => EventKind::CardProgressed,
            "card.closed" => EventKind::CardClosed,
            "subagent.started" => EventKind::SubagentStarted,
            "subagent.completed" => EventKind::SubagentCompleted,
            "skill.created" => EventKind::SkillCreated,
            "skill.version_added" => EventKind::SkillVersionAdded,
            "skill.promoted" => EventKind::SkillPromoted,
            "skill.archived" => EventKind::SkillArchived,
            "skill.invoked" => EventKind::SkillInvoked,
            "skill.write_rejected" => EventKind::SkillWriteRejected,
            _ => EventKind::Other,
        }
    }
}

impl fmt::Display for EventKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One row in `state_events`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventRecord {
    pub conversation_id: ConversationId,
    pub seq: EventSeq,
    pub kind: EventKind,
    pub payload: Vec<u8>, // MessagePack-encoded
    pub committed_at: i64,
    pub actor: Option<String>,
}

impl EventRecord {
    /// Build a new event record; the caller is responsible for passing a
    /// `seq` that is strictly greater than the conversation's current
    /// `last_seq`.
    pub fn new<P: Serialize>(
        conversation_id: ConversationId,
        seq: EventSeq,
        kind: EventKind,
        payload: &P,
        actor: Option<String>,
    ) -> Result<Self, DbError> {
        let payload = rmps::to_vec_named(payload)
            .map_err(|e| DbError::Serde(format!("encoding event payload: {e}")))?;
        Ok(Self {
            conversation_id,
            seq,
            kind,
            payload,
            committed_at: chrono::Utc::now().timestamp(),
            actor,
        })
    }

    /// Decode the MessagePack payload into a typed struct.
    pub fn decode_payload<P: for<'de> Deserialize<'de>>(&self) -> Result<P, DbError> {
        rmps::from_slice(&self.payload)
            .map_err(|e| DbError::Serde(format!("decoding event payload: {e}")))
    }
}

/// Minimal envelope for a `tool_use` event when we need to correlate with a
/// matching `tool_result` to enforce the pairing invariant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolUsePayload {
    /// Framework-assigned ordinal (the `tool_call_ordinal` used in the
    /// outbox idempotency key). Stable across retries.
    pub ordinal: u32,
    pub tool_name: String,
    pub args_json: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResultPayload {
    /// Must match an upstream `ToolUsePayload.ordinal`.
    pub ordinal: u32,
    /// `Ok(_)` on success, `Err(reason)` on failure or synthesized cancel.
    pub outcome: Result<serde_json::Value, String>,
}

/// The event log facade.
///
/// Legacy v1 rows retain their independent HMAC-SHA256 interpretation.
/// When a key is attached, new writes are v2: each tag authenticates the
/// unchanged v1 canonical row plus its key id and predecessor tag. Append and
/// commit update a separately signed terminal checkpoint in the same SQLite
/// transaction. Replay verifies the complete conversation chain before
/// returning any requested suffix.
///
/// When no key is attached (tests, first-run before vault is ready),
/// rows are written with `tag = NULL` and verification is skipped.
/// Production always attaches a key at server startup.
///
/// **Key rotation.** `EventLog` accepts a [`KeyRing`] of
/// `(key_id, bytes)` pairs. Append writes the ring's `current_id` into
/// the `state_events.key_id` column; replay reads that id and verifies
/// each row under the corresponding key. To rotate, register the new
/// key with `KeyRing::add` and call `KeyRing::set_current` — old rows
/// continue to verify under their original key, new rows pick up the
/// new one. The single-key `with_hmac_key(k)` helper is retained as a
/// thin wrapper that builds a one-entry ring.
pub struct EventLog<'db> {
    db: &'db Database,
    key_ring: Option<KeyRing>,
}

/// Outcome of [`EventLog::backfill_null_tags`].
///
/// `signed` is the count of rows whose `tag` was NULL and that we
/// successfully filled in under the ring's current key.
/// `null_remaining` is what's left after the pass — should be `0` on
/// success. Once that holds across the fleet, the operator can flip
/// `state_events.tag` to NOT NULL via a follow-up migration.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BackfillReport {
    pub signed: usize,
    pub skipped: usize,
    pub null_remaining: usize,
}

/// Row shape pulled from `state_events` during back-fill: the six
/// columns that feed canonical-bytes signing (conv_id, seq, kind,
/// payload, committed_at, actor). Aliased so clippy doesn't trip on
/// the tuple width and so the SELECT + the destructure stay in sync.
type NullTagRow = (String, i64, String, Vec<u8>, i64, Option<String>);

#[derive(Debug, Clone)]
struct IntegrityRow {
    event: EventRecord,
    tag: Option<Vec<u8>>,
    key_id: i64,
    integrity_version: i64,
    prev_tag: Option<Vec<u8>>,
}

#[derive(Debug, Clone)]
struct IntegrityHead {
    chain_start_seq: i64,
    head_seq: i64,
    head_tag: Vec<u8>,
    genesis_key_id: i64,
    key_id: i64,
    checkpoint_tag: Vec<u8>,
}

/// Multi-key HMAC ring used by `EventLog` for signing + verifying.
///
/// `current_id` is what every new append signs under. Verification
/// looks up each row's `key_id` column to fish out the right key.
#[derive(Debug, Clone, Default)]
pub struct KeyRing {
    keys: std::collections::HashMap<i64, Vec<u8>>,
    current_id: i64,
}

impl KeyRing {
    /// Build a ring starting with one key. The id is the operator's
    /// choice; Phase-1 single-key deployments used `0`.
    pub fn single(id: i64, key: Vec<u8>) -> Self {
        let mut keys = std::collections::HashMap::new();
        keys.insert(id, key);
        Self {
            keys,
            current_id: id,
        }
    }

    /// Register an additional key without changing `current_id`. Used
    /// when bringing an old key forward for verification only.
    pub fn add(&mut self, id: i64, key: Vec<u8>) {
        self.keys.insert(id, key);
    }

    /// Promote `id` to the current signing key. Errors if the id was
    /// never registered.
    pub fn set_current(&mut self, id: i64) -> Result<(), DbError> {
        if !self.keys.contains_key(&id) {
            return Err(DbError::Config(format!(
                "key_ring: key_id {id} is not registered"
            )));
        }
        self.current_id = id;
        Ok(())
    }

    /// Convenience: register a new key AND set it as current. Returns
    /// the previous `current_id` so the caller can persist it.
    pub fn rotate(&mut self, new_id: i64, new_key: Vec<u8>) -> i64 {
        let prev = self.current_id;
        self.keys.insert(new_id, new_key);
        self.current_id = new_id;
        prev
    }

    pub fn current_id(&self) -> i64 {
        self.current_id
    }

    pub fn registered_ids(&self) -> Vec<i64> {
        let mut ids: Vec<i64> = self.keys.keys().copied().collect();
        ids.sort_unstable();
        ids
    }

    fn current_key(&self) -> Option<&[u8]> {
        self.keys.get(&self.current_id).map(|v| v.as_slice())
    }

    fn key_for(&self, id: i64) -> Option<&[u8]> {
        self.keys.get(&id).map(|v| v.as_slice())
    }
}

impl<'db> EventLog<'db> {
    pub fn new(db: &'db Database) -> Self {
        Self { db, key_ring: None }
    }

    /// Attach a single HMAC key — convenience wrapper that builds a
    /// one-entry [`KeyRing`] under id `0`. Phase-1 single-key callers
    /// keep working unchanged.
    pub fn with_hmac_key(self, key: Vec<u8>) -> Self {
        self.with_key_ring(KeyRing::single(0, key))
    }

    /// Attach a [`KeyRing`]. Append signs with `ring.current_id`;
    /// replay verifies each row against `ring[row.key_id]`.
    pub fn with_key_ring(mut self, ring: KeyRing) -> Self {
        self.key_ring = Some(ring);
        self
    }

    fn fixed_tag(bytes: &[u8], context: &str) -> Result<[u8; 32], DbError> {
        bytes.try_into().map_err(|_| {
            DbError::TamperDetected(format!("{context} has malformed tag (len {})", bytes.len()))
        })
    }

    fn v1_canonical(ev: &EventRecord) -> Vec<u8> {
        crate::event_hmac::canonical_bytes(
            ev.conversation_id.as_str(),
            ev.seq.0,
            ev.kind.as_str(),
            ev.committed_at,
            ev.actor.as_deref(),
            &ev.payload,
        )
    }

    fn legacy_genesis_canonical(
        conversation_id: &ConversationId,
        chain_start_seq: i64,
        rows: &[IntegrityRow],
    ) -> Vec<u8> {
        let legacy_rows = rows
            .iter()
            .filter(|row| row.integrity_version == 1)
            .map(|row| {
                let mut canonical = Self::v1_canonical(&row.event);
                canonical.extend_from_slice(&row.key_id.to_le_bytes());
                match row.tag.as_deref() {
                    Some(tag) => {
                        canonical.extend_from_slice(&(tag.len() as u64).to_le_bytes());
                        canonical.extend_from_slice(tag);
                    }
                    None => canonical.extend_from_slice(&0u64.to_le_bytes()),
                }
                canonical
            })
            .collect::<Vec<_>>();
        crate::event_hmac::canonical_genesis(
            conversation_id.as_str(),
            chain_start_seq,
            &legacy_rows,
        )
    }

    fn load_integrity_rows(
        conn: &rusqlite::Connection,
        conversation_id: &ConversationId,
    ) -> Result<Vec<IntegrityRow>, DbError> {
        let mut stmt = conn.prepare_cached(
            "SELECT seq, kind, payload, committed_at, actor, tag, key_id, \
                    integrity_version, prev_tag \
             FROM state_events WHERE conversation_id = ?1 ORDER BY seq ASC",
        )?;
        let rows = stmt
            .query_map(params![conversation_id.as_str()], |row| {
                let kind: String = row.get(1)?;
                Ok(IntegrityRow {
                    event: EventRecord {
                        conversation_id: conversation_id.clone(),
                        seq: EventSeq(row.get(0)?),
                        kind: EventKind::parse(&kind),
                        payload: row.get(2)?,
                        committed_at: row.get(3)?,
                        actor: row.get(4)?,
                    },
                    tag: row.get(5)?,
                    key_id: row.get(6)?,
                    integrity_version: row.get(7)?,
                    prev_tag: row.get(8)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    fn load_integrity_head(
        conn: &rusqlite::Connection,
        conversation_id: &ConversationId,
    ) -> Result<Option<IntegrityHead>, DbError> {
        let head = conn
            .query_row(
                "SELECT chain_start_seq, head_seq, head_tag, genesis_key_id, key_id, checkpoint_tag \
                 FROM state_event_integrity_heads WHERE conversation_id = ?1",
                params![conversation_id.as_str()],
                |row| {
                    Ok(IntegrityHead {
                        chain_start_seq: row.get(0)?,
                        head_seq: row.get(1)?,
                        head_tag: row.get(2)?,
                        genesis_key_id: row.get(3)?,
                        key_id: row.get(4)?,
                        checkpoint_tag: row.get(5)?,
                    })
                },
            )
            .optional()?;
        Ok(head)
    }

    fn verify_integrity(
        &self,
        conversation_id: &ConversationId,
        rows: &[IntegrityRow],
        head: Option<&IntegrityHead>,
    ) -> Result<(), DbError> {
        let Some(ring) = self.key_ring.as_ref() else {
            return Ok(());
        };

        for row in rows.iter().filter(|row| row.integrity_version == 1) {
            self.verify(&row.event, row.tag.clone(), row.key_id)?;
        }

        let v2_rows = rows
            .iter()
            .filter(|row| row.integrity_version == 2)
            .collect::<Vec<_>>();
        let Some(head) = head else {
            if v2_rows.is_empty() {
                return Ok(());
            }
            return Err(DbError::TamperDetected(format!(
                "conversation {} has v2 events but no integrity checkpoint",
                conversation_id.as_str()
            )));
        };

        if head.chain_start_seq < 1 {
            return Err(DbError::TamperDetected(format!(
                "conversation {} has invalid chain start {}",
                conversation_id.as_str(),
                head.chain_start_seq
            )));
        }
        let genesis_key = ring.key_for(head.genesis_key_id).ok_or_else(|| {
            DbError::TamperDetected(format!(
                "conversation {} genesis uses unknown key_id {}",
                conversation_id.as_str(),
                head.genesis_key_id
            ))
        })?;
        let genesis = crate::event_hmac::sign_event(
            genesis_key,
            &Self::legacy_genesis_canonical(conversation_id, head.chain_start_seq, rows),
        );

        let mut expected_prev = genesis;
        let mut expected_seq = head.chain_start_seq;
        for row in v2_rows {
            if row.event.seq.0 != expected_seq {
                return Err(DbError::TamperDetected(format!(
                    "conversation {} v2 sequence expected {expected_seq}, found {}",
                    conversation_id.as_str(),
                    row.event.seq.0
                )));
            }
            let stored_prev = row.prev_tag.as_deref().ok_or_else(|| {
                DbError::TamperDetected(format!(
                    "event {}:{} is v2 but has no prev_tag",
                    conversation_id.as_str(),
                    row.event.seq.0
                ))
            })?;
            let stored_prev = Self::fixed_tag(stored_prev, "event prev_tag")?;
            if stored_prev != expected_prev {
                return Err(DbError::TamperDetected(format!(
                    "event {}:{} breaks the v2 predecessor chain",
                    conversation_id.as_str(),
                    row.event.seq.0
                )));
            }
            let tag = row.tag.as_deref().ok_or_else(|| {
                DbError::TamperDetected(format!(
                    "event {}:{} is v2 but has no tag",
                    conversation_id.as_str(),
                    row.event.seq.0
                ))
            })?;
            let tag = Self::fixed_tag(tag, "event tag")?;
            let key = ring.key_for(row.key_id).ok_or_else(|| {
                DbError::TamperDetected(format!(
                    "event {}:{} signed with unknown key_id {}",
                    conversation_id.as_str(),
                    row.event.seq.0,
                    row.key_id
                ))
            })?;
            let canonical = crate::event_hmac::canonical_chain_event(
                &Self::v1_canonical(&row.event),
                row.key_id,
                &stored_prev,
            );
            if !crate::event_hmac::verify_event(key, &canonical, &tag) {
                return Err(DbError::TamperDetected(format!(
                    "event {}:{} failed v2 HMAC verification",
                    conversation_id.as_str(),
                    row.event.seq.0
                )));
            }
            expected_prev = tag;
            expected_seq += 1;
        }

        let expected_head_seq = expected_seq - 1;
        let stored_head = Self::fixed_tag(&head.head_tag, "integrity head")?;
        if head.head_seq != expected_head_seq || stored_head != expected_prev {
            return Err(DbError::TamperDetected(format!(
                "conversation {} terminal checkpoint does not match event tail",
                conversation_id.as_str()
            )));
        }
        let checkpoint_key = ring.key_for(head.key_id).ok_or_else(|| {
            DbError::TamperDetected(format!(
                "conversation {} checkpoint uses unknown key_id {}",
                conversation_id.as_str(),
                head.key_id
            ))
        })?;
        let checkpoint = Self::fixed_tag(&head.checkpoint_tag, "checkpoint tag")?;
        let canonical = crate::event_hmac::canonical_checkpoint(
            conversation_id.as_str(),
            head.chain_start_seq,
            head.head_seq,
            &stored_head,
            head.genesis_key_id,
            head.key_id,
        );
        if !crate::event_hmac::verify_event(checkpoint_key, &canonical, &checkpoint) {
            return Err(DbError::TamperDetected(format!(
                "conversation {} checkpoint signature is invalid",
                conversation_id.as_str()
            )));
        }
        Ok(())
    }

    fn initial_integrity_head(
        &self,
        conversation_id: &ConversationId,
        rows: &[IntegrityRow],
    ) -> Result<IntegrityHead, DbError> {
        let ring = self
            .key_ring
            .as_ref()
            .ok_or_else(|| DbError::Config("v2 integrity checkpoints require a KeyRing".into()))?;
        let key = ring
            .current_key()
            .ok_or_else(|| DbError::Config("KeyRing has no current key".into()))?;
        if rows.iter().any(|row| row.integrity_version != 1) {
            return Err(DbError::TamperDetected(format!(
                "conversation {} has v2 rows without a checkpoint",
                conversation_id.as_str()
            )));
        }
        let legacy_max = rows.last().map(|row| row.event.seq.0).unwrap_or(0);
        let chain_start_seq = legacy_max + 1;
        let head_tag = crate::event_hmac::sign_event(
            key,
            &Self::legacy_genesis_canonical(conversation_id, chain_start_seq, rows),
        );
        let checkpoint_tag = crate::event_hmac::sign_event(
            key,
            &crate::event_hmac::canonical_checkpoint(
                conversation_id.as_str(),
                chain_start_seq,
                legacy_max,
                &head_tag,
                ring.current_id,
                ring.current_id,
            ),
        );
        Ok(IntegrityHead {
            chain_start_seq,
            head_seq: legacy_max,
            head_tag: head_tag.to_vec(),
            genesis_key_id: ring.current_id,
            key_id: ring.current_id,
            checkpoint_tag: checkpoint_tag.to_vec(),
        })
    }

    fn write_integrity_head(
        conn: &rusqlite::Connection,
        conversation_id: &ConversationId,
        head: &IntegrityHead,
    ) -> Result<(), DbError> {
        conn.execute(
            "INSERT INTO state_event_integrity_heads \
             (conversation_id, integrity_version, chain_start_seq, head_seq, head_tag, \
              genesis_key_id, key_id, checkpoint_tag, updated_at) \
             VALUES (?1, 2, ?2, ?3, ?4, ?5, ?6, ?7, ?8) \
             ON CONFLICT(conversation_id) DO UPDATE SET \
               chain_start_seq = excluded.chain_start_seq, \
               head_seq = excluded.head_seq, head_tag = excluded.head_tag, \
               genesis_key_id = excluded.genesis_key_id, key_id = excluded.key_id, \
               checkpoint_tag = excluded.checkpoint_tag, \
               updated_at = excluded.updated_at",
            params![
                conversation_id.as_str(),
                head.chain_start_seq,
                head.head_seq,
                head.head_tag,
                head.genesis_key_id,
                head.key_id,
                head.checkpoint_tag,
                chrono::Utc::now().timestamp(),
            ],
        )?;
        Ok(())
    }

    fn advance_integrity_head(
        &self,
        conversation_id: &ConversationId,
        chain_start_seq: i64,
        genesis_key_id: i64,
        head_seq: i64,
        head_tag: [u8; 32],
    ) -> Result<IntegrityHead, DbError> {
        let ring = self
            .key_ring
            .as_ref()
            .ok_or_else(|| DbError::Config("v2 integrity checkpoints require a KeyRing".into()))?;
        let key = ring
            .current_key()
            .ok_or_else(|| DbError::Config("KeyRing has no current key".into()))?;
        let checkpoint_tag = crate::event_hmac::sign_event(
            key,
            &crate::event_hmac::canonical_checkpoint(
                conversation_id.as_str(),
                chain_start_seq,
                head_seq,
                &head_tag,
                genesis_key_id,
                ring.current_id,
            ),
        );
        Ok(IntegrityHead {
            chain_start_seq,
            head_seq,
            head_tag: head_tag.to_vec(),
            genesis_key_id,
            key_id: ring.current_id,
            checkpoint_tag: checkpoint_tag.to_vec(),
        })
    }

    /// Establish a signed v2 genesis/checkpoint over the current legacy
    /// prefix without rewriting any existing event tag.
    pub fn establish_v2_checkpoint(
        &self,
        conversation_id: &ConversationId,
    ) -> Result<bool, DbError> {
        self.db.transaction(|tx| {
            let rows = Self::load_integrity_rows(tx, conversation_id)?;
            if let Some(head) = Self::load_integrity_head(tx, conversation_id)? {
                self.verify_integrity(conversation_id, &rows, Some(&head))?;
                return Ok(false);
            }
            let head = self.initial_integrity_head(conversation_id, &rows)?;
            Self::write_integrity_head(tx, conversation_id, &head)?;
            Ok(true)
        })
    }

    /// Establish v2 genesis checkpoints for every conversation that has event
    /// rows. Existing v1 tags are only committed into the genesis anchor; they
    /// are never re-signed or reinterpreted.
    pub fn backfill_v2_checkpoints(&self) -> Result<usize, DbError> {
        let conversation_ids = self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT DISTINCT conversation_id FROM state_events ORDER BY conversation_id",
            )?;
            let ids = stmt
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(ids)
        })?;
        let mut established = 0;
        for id in conversation_ids {
            if self.establish_v2_checkpoint(&ConversationId::from(id))? {
                established += 1;
            }
        }
        Ok(established)
    }

    /// Verify a loaded event row. No-op (returns Ok) when no ring is
    /// attached or when the stored tag is NULL (legacy rows).
    fn verify(&self, ev: &EventRecord, tag: Option<Vec<u8>>, key_id: i64) -> Result<(), DbError> {
        let Some(ring) = self.key_ring.as_ref() else {
            return Ok(());
        };
        let Some(tag_bytes) = tag else {
            return Ok(());
        };
        let Some(key) = ring.key_for(key_id) else {
            return Err(DbError::TamperDetected(format!(
                "event {}:{} signed with key_id {key_id} that isn't in the ring",
                ev.conversation_id.as_str(),
                ev.seq.0,
            )));
        };
        if tag_bytes.len() != 32 {
            return Err(DbError::TamperDetected(format!(
                "event {}:{} has malformed tag (len {})",
                ev.conversation_id.as_str(),
                ev.seq.0,
                tag_bytes.len()
            )));
        }
        let mut fixed = [0u8; 32];
        fixed.copy_from_slice(&tag_bytes);
        let canon = crate::event_hmac::canonical_bytes(
            ev.conversation_id.as_str(),
            ev.seq.0,
            ev.kind.as_str(),
            ev.committed_at,
            ev.actor.as_deref(),
            &ev.payload,
        );
        if !crate::event_hmac::verify_event(key, &canon, &fixed) {
            return Err(DbError::TamperDetected(format!(
                "event {}:{} failed HMAC verification",
                ev.conversation_id.as_str(),
                ev.seq.0
            )));
        }
        Ok(())
    }

    /// Walk every `state_events` row and sign any whose `tag` is NULL
    /// under the ring's current key. Phase-7 Hardening item: lets us
    /// flip `state_events.tag` to NOT NULL once every row carries a
    /// signature.
    ///
    /// Idempotent: re-running after the first pass returns
    /// `signed = 0`. Requires a key ring to be attached — without
    /// one, returns `Err(Config)` since blindly leaving `tag = NULL`
    /// across a back-fill would defeat the purpose.
    pub fn backfill_null_tags(&self) -> Result<BackfillReport, DbError> {
        let ring = self.key_ring.as_ref().ok_or_else(|| {
            DbError::Config(
                "backfill_null_tags requires a KeyRing — attach via with_key_ring or with_hmac_key"
                    .into(),
            )
        })?;
        let key = ring.current_key().ok_or_else(|| {
            DbError::Config("KeyRing has no key registered for current_id".into())
        })?;
        let key_id = ring.current_id;

        let checkpoint_count: i64 = self.db.with_conn(|conn| {
            Ok(conn.query_row(
                "SELECT COUNT(*) FROM state_event_integrity_heads",
                [],
                |row| row.get(0),
            )?)
        })?;
        if checkpoint_count > 0 {
            return Err(DbError::Invariant(
                "cannot rewrite legacy tags after v2 checkpoints exist".into(),
            ));
        }

        // Pull the rows that still need signatures. Linear scan is
        // fine — back-fill runs at most once per fleet, before the
        // operator flips the column constraint.
        let null_rows: Vec<NullTagRow> = self.db.with_conn(|c| {
            let mut stmt = c.prepare(
                "SELECT conversation_id, seq, kind, payload, committed_at, actor \
                     FROM state_events \
                     WHERE tag IS NULL",
            )?;
            let rows = stmt
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, Vec<u8>>(3)?,
                        r.get::<_, i64>(4)?,
                        r.get::<_, Option<String>>(5)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })?;

        let mut signed = 0usize;
        for (conv_id, seq, kind_str, payload, committed_at, actor) in null_rows {
            let canon = crate::event_hmac::canonical_bytes(
                &conv_id,
                seq,
                &kind_str,
                committed_at,
                actor.as_deref(),
                &payload,
            );
            let tag = crate::event_hmac::sign_event(key, &canon).to_vec();
            self.db.with_conn(|c| {
                c.execute(
                    "UPDATE state_events SET tag = ?1, key_id = ?2 \
                     WHERE conversation_id = ?3 AND seq = ?4",
                    params![tag, key_id, conv_id, seq],
                )?;
                Ok(())
            })?;
            signed += 1;
        }

        // Quick stats for the report.
        let null_remaining: i64 = self.db.with_conn(|c| {
            let n: i64 = c
                .query_row(
                    "SELECT COUNT(*) FROM state_events WHERE tag IS NULL",
                    [],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            Ok(n)
        })?;
        let signed_total: i64 = self.db.with_conn(|c| {
            let n: i64 = c
                .query_row(
                    "SELECT COUNT(*) FROM state_events WHERE tag IS NOT NULL",
                    [],
                    |r| r.get(0),
                )
                .unwrap_or(0);
            Ok(n)
        })?;
        Ok(BackfillReport {
            signed,
            skipped: (signed_total as usize).saturating_sub(signed),
            null_remaining: null_remaining as usize,
        })
    }

    /// Re-sign every event in the log under the current ring key,
    /// **overwriting** whatever tag/key_id was there before. Unlike
    /// [`backfill_null_tags`], this clobbers existing tags too — it's
    /// the recovery hatch for operators who lost the original HMAC key
    /// (e.g. OS keyring lost the entry between boots) and need their
    /// existing event log to be readable under a new key.
    ///
    /// **This destroys tamper-evidence for the rows it touches.** Any
    /// row that was previously signed under a different key now looks
    /// validly signed under the current one. Operators should only run
    /// this after confirming via offline channels that the log
    /// content is what they expect (or, more commonly, after losing a
    /// dev-environment key and accepting that history is degraded).
    pub fn resign_all_with_current_key(&self) -> Result<BackfillReport, DbError> {
        let ring = self.key_ring.as_ref().ok_or_else(|| {
            DbError::Config("resign_all_with_current_key requires a KeyRing".into())
        })?;
        let key = ring.current_key().ok_or_else(|| {
            DbError::Config("KeyRing has no key registered for current_id".into())
        })?;
        let key_id = ring.current_id;

        let checkpoint_count: i64 = self.db.with_conn(|conn| {
            Ok(conn.query_row(
                "SELECT COUNT(*) FROM state_event_integrity_heads",
                [],
                |row| row.get(0),
            )?)
        })?;
        if checkpoint_count > 0 {
            return Err(DbError::Invariant(
                "destructive re-signing is disabled after v2 checkpoints exist; retain old keys"
                    .into(),
            ));
        }

        let rows: Vec<NullTagRow> = self.db.with_conn(|c| {
            let mut stmt = c.prepare(
                "SELECT conversation_id, seq, kind, payload, committed_at, actor \
                 FROM state_events",
            )?;
            let rows = stmt
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, Vec<u8>>(3)?,
                        r.get::<_, i64>(4)?,
                        r.get::<_, Option<String>>(5)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })?;

        let total = rows.len();
        for (conv_id, seq, kind_str, payload, committed_at, actor) in rows {
            let canon = crate::event_hmac::canonical_bytes(
                &conv_id,
                seq,
                &kind_str,
                committed_at,
                actor.as_deref(),
                &payload,
            );
            let tag = crate::event_hmac::sign_event(key, &canon).to_vec();
            self.db.with_conn(|c| {
                c.execute(
                    "UPDATE state_events SET tag = ?1, key_id = ?2 \
                     WHERE conversation_id = ?3 AND seq = ?4",
                    params![tag, key_id, conv_id, seq],
                )?;
                Ok(())
            })?;
        }

        Ok(BackfillReport {
            signed: total,
            skipped: 0,
            null_remaining: 0,
        })
    }

    /// Append one event. Enforces `(conversation_id, seq)` uniqueness via
    /// the primary key — returns an error if the caller passed a stale seq.
    pub fn append(&self, ev: &EventRecord) -> Result<(), DbError> {
        if self.key_ring.is_none() {
            return self.db.with_conn(|conn| {
                conn.execute(
                    "INSERT INTO state_events \
                     (conversation_id, seq, kind, payload, committed_at, actor, tag, key_id) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, 0)",
                    params![
                        ev.conversation_id.as_str(),
                        ev.seq.0,
                        ev.kind.as_str(),
                        ev.payload,
                        ev.committed_at,
                        ev.actor,
                    ],
                )?;
                Ok(())
            });
        }

        self.db.transaction(|tx| {
            let rows = Self::load_integrity_rows(tx, &ev.conversation_id)?;
            let mut head = match Self::load_integrity_head(tx, &ev.conversation_id)? {
                Some(head) => {
                    self.verify_integrity(&ev.conversation_id, &rows, Some(&head))?;
                    head
                }
                None => self.initial_integrity_head(&ev.conversation_id, &rows)?,
            };
            if ev.seq.0 != head.head_seq + 1 {
                return Err(DbError::Invariant(format!(
                    "v2 append for {} expected seq {}, got {}",
                    ev.conversation_id.as_str(),
                    head.head_seq + 1,
                    ev.seq.0
                )));
            }
            let ring = self.key_ring.as_ref().expect("checked above");
            let key = ring.current_key().ok_or_else(|| {
                DbError::Config("KeyRing has no key registered for current_id".into())
            })?;
            let prev_tag = Self::fixed_tag(&head.head_tag, "integrity head")?;
            let canonical = crate::event_hmac::canonical_chain_event(
                &Self::v1_canonical(ev),
                ring.current_id,
                &prev_tag,
            );
            let tag = crate::event_hmac::sign_event(key, &canonical);
            tx.execute(
                "INSERT INTO state_events \
                 (conversation_id, seq, kind, payload, committed_at, actor, tag, key_id, \
                  integrity_version, prev_tag) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 2, ?9)",
                params![
                    ev.conversation_id.as_str(),
                    ev.seq.0,
                    ev.kind.as_str(),
                    ev.payload,
                    ev.committed_at,
                    ev.actor,
                    tag.to_vec(),
                    ring.current_id,
                    prev_tag.to_vec(),
                ],
            )?;
            head = self.advance_integrity_head(
                &ev.conversation_id,
                head.chain_start_seq,
                head.genesis_key_id,
                ev.seq.0,
                tag,
            )?;
            Self::write_integrity_head(tx, &ev.conversation_id, &head)?;
            Ok(())
        })
    }

    /// Read events for a conversation strictly greater than `after_seq`,
    /// in ascending order. Verifies every row's HMAC tag when a key is
    /// attached — returning `DbError::TamperDetected` on first mismatch.
    pub fn replay_since(
        &self,
        conversation_id: &ConversationId,
        after_seq: EventSeq,
    ) -> Result<Vec<EventRecord>, DbError> {
        let (rows, head) = self.db.with_conn(|conn| {
            Ok((
                Self::load_integrity_rows(conn, conversation_id)?,
                Self::load_integrity_head(conn, conversation_id)?,
            ))
        })?;
        self.verify_integrity(conversation_id, &rows, head.as_ref())?;
        Ok(rows
            .into_iter()
            .filter(|row| row.event.seq.0 > after_seq.0)
            .map(|row| row.event)
            .collect())
    }

    /// Current last_seq for a conversation (0 if none yet).
    pub fn last_seq(&self, conversation_id: &ConversationId) -> Result<EventSeq, DbError> {
        self.db.with_conn(|c| {
            let got: Option<i64> = c
                .query_row(
                    "SELECT MAX(seq) FROM state_events WHERE conversation_id = ?1",
                    params![conversation_id.as_str()],
                    |r| r.get(0),
                )
                .ok()
                .flatten();
            Ok(EventSeq(got.unwrap_or(0)))
        })
    }

    /// Commit a turn atomically: all events either land together or none do.
    ///
    /// Enforces §2.2 axiom #3: every `tool_use` must pair with a
    /// `tool_result`. If the caller passes a `tool_use` whose ordinal is
    /// not matched by a `tool_result` in the same batch, we synthesize a
    /// cancellation result with `reason`.
    ///
    /// The returned vec contains the events actually written, including any
    /// synthesized cancellations, in order.
    pub fn commit_turn(
        &self,
        conversation_id: &ConversationId,
        base_seq: EventSeq,
        events: Vec<PendingEvent>,
    ) -> Result<Vec<EventRecord>, DbError> {
        let mut materialized = enforce_tool_pairing(conversation_id, base_seq, events)?;

        // Acquire the connection lock + open the write transaction
        // BEFORE re-validating the base_seq against actual state.
        // Two concurrent appenders to the same conversation used to
        // race here: caller A reads last_seq=N and computes seqs
        // N+1..N+k; caller B does the same; both then INSERT and B
        // hits "UNIQUE constraint failed: state_events.(conversation_id,
        // seq)". Re-checking inside the transaction collapses the
        // window — the connection mutex serializes commits, so the
        // second commit picks up the first's writes here, shifts its
        // seqs, re-signs, and lands cleanly.
        self.db.transaction(|tx| {
            let existing_rows = Self::load_integrity_rows(tx, conversation_id)?;
            let actual_max = existing_rows.last().map(|row| row.event.seq.0).unwrap_or(0);
            let delta = actual_max - base_seq.0;
            if delta > 0 {
                for ev in materialized.iter_mut() {
                    ev.seq = EventSeq(ev.seq.0 + delta);
                }
            }

            if self.key_ring.is_none() {
                for ev in materialized.iter() {
                    tx.execute(
                        "INSERT INTO state_events \
                         (conversation_id, seq, kind, payload, committed_at, actor, tag, key_id) \
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, 0)",
                        params![
                            ev.conversation_id.as_str(),
                            ev.seq.0,
                            ev.kind.as_str(),
                            ev.payload,
                            ev.committed_at,
                            ev.actor,
                        ],
                    )?;
                }
                return Ok(());
            }

            let mut head = match Self::load_integrity_head(tx, conversation_id)? {
                Some(head) => {
                    self.verify_integrity(conversation_id, &existing_rows, Some(&head))?;
                    head
                }
                None => self.initial_integrity_head(conversation_id, &existing_rows)?,
            };
            let ring = self.key_ring.as_ref().expect("checked above");
            let key = ring.current_key().ok_or_else(|| {
                DbError::Config("KeyRing has no key registered for current_id".into())
            })?;
            for ev in materialized.iter() {
                if ev.seq.0 != head.head_seq + 1 {
                    return Err(DbError::Invariant(format!(
                        "v2 commit for {} expected seq {}, got {}",
                        conversation_id.as_str(),
                        head.head_seq + 1,
                        ev.seq.0
                    )));
                }
                let prev_tag = Self::fixed_tag(&head.head_tag, "integrity head")?;
                let canonical = crate::event_hmac::canonical_chain_event(
                    &Self::v1_canonical(ev),
                    ring.current_id,
                    &prev_tag,
                );
                let tag = crate::event_hmac::sign_event(key, &canonical);
                tx.execute(
                    "INSERT INTO state_events \
                     (conversation_id, seq, kind, payload, committed_at, actor, tag, key_id, \
                      integrity_version, prev_tag) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 2, ?9)",
                    params![
                        ev.conversation_id.as_str(),
                        ev.seq.0,
                        ev.kind.as_str(),
                        ev.payload,
                        ev.committed_at,
                        ev.actor,
                        tag.to_vec(),
                        ring.current_id,
                        prev_tag.to_vec(),
                    ],
                )?;
                head = self.advance_integrity_head(
                    conversation_id,
                    head.chain_start_seq,
                    head.genesis_key_id,
                    ev.seq.0,
                    tag,
                )?;
            }
            Self::write_integrity_head(tx, conversation_id, &head)?;
            Ok(())
        })?;

        Ok(materialized)
    }
}

/// A materialized snapshot of a conversation's events up to a given seq.
///
/// The design philosophy for Phase 1: a snapshot is a pre-serialized blob
/// of the events leading up to `up_to_seq`. Replay = load the snapshot +
/// read events with `seq > up_to_seq`. This saves the `SELECT` cost on hot
/// conversations; later phases can add actual summarization/compaction of
/// older turns.
///
/// The snapshot is self-contained — no joins, no external references —
/// so we can restore a conversation entirely from `snapshot_blob` +
/// events-since-snapshot_seq.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub conversation_id: ConversationId,
    pub up_to_seq: EventSeq,
    pub events: Vec<EventRecord>,
    pub built_at: i64,
}

impl Snapshot {
    /// Encode to MessagePack for the `state_conversations.snapshot_blob` column.
    pub fn encode(&self) -> Result<Vec<u8>, DbError> {
        rmps::to_vec_named(self).map_err(|e| DbError::Serde(format!("encoding snapshot: {e}")))
    }

    /// Decode from MessagePack.
    pub fn decode(bytes: &[u8]) -> Result<Self, DbError> {
        rmps::from_slice(bytes).map_err(|e| DbError::Serde(format!("decoding snapshot: {e}")))
    }
}

/// How often (in events) to materialize a snapshot. Tunable.
pub const SNAPSHOT_INTERVAL: i64 = 50;

impl<'db> EventLog<'db> {
    /// Build a snapshot containing all events for this conversation with
    /// `seq <= up_to_seq`. Encoded blob is ready to write into
    /// `state_conversations.snapshot_blob`.
    pub fn build_snapshot(
        &self,
        conversation_id: &ConversationId,
        up_to_seq: EventSeq,
    ) -> Result<Snapshot, DbError> {
        let events = self
            .replay_since(conversation_id, EventSeq(0))?
            .into_iter()
            .filter(|event| event.seq.0 <= up_to_seq.0)
            .collect();

        Ok(Snapshot {
            conversation_id: conversation_id.clone(),
            up_to_seq,
            events,
            built_at: chrono::Utc::now().timestamp(),
        })
    }

    /// Decide whether a new snapshot should be materialized. Cheap check —
    /// intended to be called right after each `commit_turn`.
    pub fn should_snapshot(
        &self,
        last_snapshot_seq: Option<EventSeq>,
        current_last_seq: EventSeq,
    ) -> bool {
        let baseline = last_snapshot_seq.map(|s| s.0).unwrap_or(0);
        (current_last_seq.0 - baseline) >= SNAPSHOT_INTERVAL
    }

    /// Hydrate a conversation's full event stream from snapshot (if any)
    /// plus events written after the snapshot. Returned vec is in strict
    /// `seq` order.
    ///
    /// If the snapshot decode fails for any reason (corrupt blob, stale
    /// schema), we fall through to replaying the entire log — correctness
    /// over convenience.
    pub fn hydrate(
        &self,
        conversation_id: &ConversationId,
        snapshot_blob: Option<&[u8]>,
        snapshot_seq: Option<EventSeq>,
    ) -> Result<Vec<EventRecord>, DbError> {
        if self.key_ring.is_some() {
            return self.replay_since(conversation_id, EventSeq(0));
        }
        let (mut events, after) = match (snapshot_blob, snapshot_seq) {
            (Some(blob), Some(seq)) => match Snapshot::decode(blob) {
                Ok(snap) if snap.conversation_id == *conversation_id && snap.up_to_seq == seq => {
                    (snap.events, seq)
                }
                _ => {
                    // Corrupt or mismatched snapshot — replay from scratch.
                    (Vec::new(), EventSeq(0))
                }
            },
            _ => (Vec::new(), EventSeq(0)),
        };

        let tail = self.replay_since(conversation_id, after)?;
        events.extend(tail);
        Ok(events)
    }
}

/// An event the caller wants to append. Seq is assigned by `commit_turn`.
#[derive(Debug, Clone)]
pub struct PendingEvent {
    pub kind: EventKind,
    pub payload: Vec<u8>,
    pub actor: Option<String>,
}

impl PendingEvent {
    pub fn encode<P: Serialize>(
        kind: EventKind,
        payload: &P,
        actor: Option<String>,
    ) -> Result<Self, DbError> {
        let payload = rmps::to_vec_named(payload)
            .map_err(|e| DbError::Serde(format!("encoding event payload: {e}")))?;
        Ok(Self {
            kind,
            payload,
            actor,
        })
    }
}

/// Walk the proposed event list; for every `tool_use(ordinal = N)` without
/// a later `tool_result(ordinal = N)`, insert a synthesized cancellation
/// result right after the `tool_use`.
fn enforce_tool_pairing(
    conversation_id: &ConversationId,
    base_seq: EventSeq,
    events: Vec<PendingEvent>,
) -> Result<Vec<EventRecord>, DbError> {
    // Index tool_result ordinals so we can detect missing matches.
    let mut results_by_ordinal: std::collections::HashSet<u32> = Default::default();
    for ev in &events {
        if ev.kind == EventKind::ToolResult {
            let decoded: ToolResultPayload = rmps::from_slice(&ev.payload)
                .map_err(|e| DbError::Serde(format!("decoding tool_result: {e}")))?;
            results_by_ordinal.insert(decoded.ordinal);
        }
    }

    let mut out: Vec<EventRecord> = Vec::with_capacity(events.len() + 2);
    let mut next_seq = base_seq.next();
    let now = chrono::Utc::now().timestamp();

    for ev in events {
        if ev.kind == EventKind::ToolUse {
            let parsed: ToolUsePayload = rmps::from_slice(&ev.payload)
                .map_err(|e| DbError::Serde(format!("decoding tool_use: {e}")))?;

            // Write the tool_use.
            out.push(EventRecord {
                conversation_id: conversation_id.clone(),
                seq: next_seq,
                kind: EventKind::ToolUse,
                payload: ev.payload.clone(),
                committed_at: now,
                actor: ev.actor.clone(),
            });
            next_seq = next_seq.next();

            // If there's no matching tool_result in the batch, synthesize one.
            if !results_by_ordinal.contains(&parsed.ordinal) {
                let synthetic = ToolResultPayload {
                    ordinal: parsed.ordinal,
                    outcome: Err(format!(
                        "cancelled: no tool_result in same commit for ordinal {}",
                        parsed.ordinal
                    )),
                };
                let payload = rmps::to_vec_named(&synthetic)
                    .map_err(|e| DbError::Serde(format!("encoding synthetic tool_result: {e}")))?;
                out.push(EventRecord {
                    conversation_id: conversation_id.clone(),
                    seq: next_seq,
                    kind: EventKind::ToolResult,
                    payload,
                    committed_at: now,
                    actor: Some("system".to_owned()),
                });
                next_seq = next_seq.next();
            }
        } else {
            out.push(EventRecord {
                conversation_id: conversation_id.clone(),
                seq: next_seq,
                kind: ev.kind,
                payload: ev.payload,
                committed_at: now,
                actor: ev.actor,
            });
            next_seq = next_seq.next();
        }
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Database, DbConfig};
    use crate::migrations::MigrationRunner;
    use serde_json::json;

    fn fresh_db() -> Database {
        let db = Database::open(&DbConfig::in_memory_unencrypted()).unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        db
    }

    #[test]
    fn append_and_replay_roundtrip() {
        let db = fresh_db();
        let log = EventLog::new(&db);
        let cid = ConversationId::from("conv-rt");

        #[derive(Serialize, Deserialize, PartialEq, Debug)]
        struct P {
            text: String,
        }

        let ev = EventRecord::new(
            cid.clone(),
            EventSeq::FIRST,
            EventKind::UserMsg,
            &P { text: "hi".into() },
            Some("controller".into()),
        )
        .unwrap();
        log.append(&ev).unwrap();

        let got = log.replay_since(&cid, EventSeq(0)).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].kind, EventKind::UserMsg);
        let decoded: P = got[0].decode_payload().unwrap();
        assert_eq!(decoded.text, "hi");
    }

    #[test]
    fn last_seq_starts_at_zero() {
        let db = fresh_db();
        let log = EventLog::new(&db);
        let cid = ConversationId::new();
        assert_eq!(log.last_seq(&cid).unwrap(), EventSeq(0));
    }

    #[test]
    fn commit_turn_synthesizes_missing_tool_result() {
        let db = fresh_db();
        let log = EventLog::new(&db);
        let cid = ConversationId::from("conv-pair");

        let turn_events = vec![
            PendingEvent::encode(
                EventKind::ModelTurn,
                &json!({"text": "let me check"}),
                Some("agent".into()),
            )
            .unwrap(),
            PendingEvent::encode(
                EventKind::ToolUse,
                &ToolUsePayload {
                    ordinal: 0,
                    tool_name: "list_events".into(),
                    args_json: json!({}),
                },
                Some("agent".into()),
            )
            .unwrap(),
            // DELIBERATELY no ToolResult — commit_turn must synthesize one.
        ];

        let written = log.commit_turn(&cid, EventSeq(0), turn_events).unwrap();
        assert_eq!(
            written.len(),
            3,
            "expected ModelTurn + ToolUse + synthetic ToolResult"
        );
        assert_eq!(written[0].kind, EventKind::ModelTurn);
        assert_eq!(written[1].kind, EventKind::ToolUse);
        assert_eq!(written[2].kind, EventKind::ToolResult);

        let synthetic: ToolResultPayload = written[2].decode_payload().unwrap();
        assert!(synthetic.outcome.is_err());
        assert_eq!(synthetic.ordinal, 0);
    }

    #[test]
    fn commit_turn_leaves_matched_pairs_untouched() {
        let db = fresh_db();
        let log = EventLog::new(&db);
        let cid = ConversationId::from("conv-match");

        let turn_events = vec![
            PendingEvent::encode(
                EventKind::ToolUse,
                &ToolUsePayload {
                    ordinal: 7,
                    tool_name: "ping".into(),
                    args_json: json!({}),
                },
                Some("agent".into()),
            )
            .unwrap(),
            PendingEvent::encode(
                EventKind::ToolResult,
                &ToolResultPayload {
                    ordinal: 7,
                    outcome: Ok(json!({"pong": true})),
                },
                Some("system".into()),
            )
            .unwrap(),
        ];

        let written = log.commit_turn(&cid, EventSeq(0), turn_events).unwrap();
        assert_eq!(written.len(), 2);

        let res: ToolResultPayload = written[1].decode_payload().unwrap();
        assert!(res.outcome.is_ok());
    }

    #[test]
    fn snapshot_encode_decode_roundtrip() {
        let db = fresh_db();
        let log = EventLog::new(&db);
        let cid = ConversationId::from("conv-snap");

        for i in 1..=10 {
            let ev = EventRecord::new(
                cid.clone(),
                EventSeq(i),
                EventKind::UserMsg,
                &serde_json::json!({"i": i}),
                None,
            )
            .unwrap();
            log.append(&ev).unwrap();
        }

        let snap = log.build_snapshot(&cid, EventSeq(7)).unwrap();
        assert_eq!(snap.events.len(), 7);
        assert_eq!(snap.up_to_seq, EventSeq(7));

        let blob = snap.encode().unwrap();
        let decoded = Snapshot::decode(&blob).unwrap();
        assert_eq!(decoded.events.len(), 7);
        assert_eq!(decoded.conversation_id, cid);
    }

    #[test]
    fn hydrate_combines_snapshot_with_tail() {
        let db = fresh_db();
        let log = EventLog::new(&db);
        let cid = ConversationId::from("conv-hyd");

        for i in 1..=10 {
            let ev = EventRecord::new(
                cid.clone(),
                EventSeq(i),
                EventKind::UserMsg,
                &serde_json::json!({"i": i}),
                None,
            )
            .unwrap();
            log.append(&ev).unwrap();
        }

        let snap = log.build_snapshot(&cid, EventSeq(6)).unwrap();
        let blob = snap.encode().unwrap();

        let hydrated = log.hydrate(&cid, Some(&blob), Some(EventSeq(6))).unwrap();
        assert_eq!(hydrated.len(), 10);
        assert_eq!(hydrated[0].seq, EventSeq(1));
        assert_eq!(hydrated[9].seq, EventSeq(10));
        // Verify ordering after the snapshot boundary.
        assert_eq!(hydrated[6].seq, EventSeq(7));
    }

    #[test]
    fn hydrate_falls_through_on_corrupt_snapshot() {
        let db = fresh_db();
        let log = EventLog::new(&db);
        let cid = ConversationId::from("conv-corrupt");
        let ev = EventRecord::new(
            cid.clone(),
            EventSeq::FIRST,
            EventKind::UserMsg,
            &serde_json::json!({}),
            None,
        )
        .unwrap();
        log.append(&ev).unwrap();

        let garbage = vec![0xffu8; 64];
        let hydrated = log
            .hydrate(&cid, Some(&garbage), Some(EventSeq(99)))
            .unwrap();
        // Should fall through and replay from scratch.
        assert_eq!(hydrated.len(), 1);
    }

    #[test]
    fn should_snapshot_respects_interval() {
        let db = fresh_db();
        let log = EventLog::new(&db);
        assert!(!log.should_snapshot(None, EventSeq(10)));
        assert!(log.should_snapshot(None, EventSeq(SNAPSHOT_INTERVAL)));
        assert!(log.should_snapshot(Some(EventSeq(100)), EventSeq(150)));
        assert!(!log.should_snapshot(Some(EventSeq(100)), EventSeq(149)));
    }

    #[test]
    fn event_kind_parse_unknown_maps_to_other() {
        // Forward-compat contract: unknown kinds MUST map to `Other` so
        // replay can continue.
        assert_eq!(
            EventKind::parse("this_kind_does_not_exist"),
            EventKind::Other
        );
        assert_eq!(EventKind::parse(""), EventKind::Other);
    }

    #[test]
    fn event_kind_display_matches_as_str() {
        assert_eq!(format!("{}", EventKind::UserMsg), "user_msg");
        assert_eq!(format!("{}", EventKind::ToolResult), "tool_result");
    }

    /// `commit_turn` shifts the batch's seqs onto the actual tail of
    /// the conversation if the caller's `base_seq` was stale. The
    /// pre-2026-05 implementation FAILED on `UNIQUE constraint`
    /// instead of shifting; that was the bug that took down Signal
    /// inbound when two frames raced into the same conversation.
    /// The new contract: a stale base_seq is silently corrected
    /// inside the transaction so the batch lands at MAX(seq) + 1.
    #[test]
    fn commit_turn_shifts_seqs_when_base_is_stale() {
        let db = fresh_db();
        let log = EventLog::new(&db);
        let cid = ConversationId::from("conv-shift");

        // Pre-write an event at seq=2 simulating a concurrent
        // committer that landed first.
        let preexisting = EventRecord::new(
            cid.clone(),
            EventSeq(2),
            EventKind::UserMsg,
            &serde_json::json!({"note": "preexisting"}),
            None,
        )
        .unwrap();
        log.append(&preexisting).unwrap();

        let pending = vec![
            PendingEvent::encode(
                EventKind::ModelTurn,
                &serde_json::json!({"text": "p1"}),
                Some("agent".into()),
            )
            .unwrap(),
            PendingEvent::encode(
                EventKind::ModelTurn,
                &serde_json::json!({"text": "p2"}),
                Some("agent".into()),
            )
            .unwrap(),
            PendingEvent::encode(
                EventKind::ModelTurn,
                &serde_json::json!({"text": "p3"}),
                Some("agent".into()),
            )
            .unwrap(),
        ];

        // Caller passed base_seq=0 (stale — they didn't see the
        // pre-existing seq=2 row). commit_turn must shift the batch
        // to start at seq=3, not fail.
        let written = log.commit_turn(&cid, EventSeq(0), pending).unwrap();
        assert_eq!(written.len(), 3);
        assert_eq!(written[0].seq, EventSeq(3));
        assert_eq!(written[1].seq, EventSeq(4));
        assert_eq!(written[2].seq, EventSeq(5));

        let events = log.replay_since(&cid, EventSeq(0)).unwrap();
        assert_eq!(events.len(), 4);
        let seqs: Vec<i64> = events.iter().map(|e| e.seq.0).collect();
        assert_eq!(seqs, vec![2, 3, 4, 5]);
    }

    /// When an HMAC key is attached, the `tag` column is populated on
    /// append and successfully verifies on replay.
    #[test]
    fn hmac_signed_append_roundtrips() {
        let db = fresh_db();
        let key = b"test-hmac-key-32-bytes-long!!!!!".to_vec();
        let log = EventLog::new(&db).with_hmac_key(key);
        let cid = ConversationId::from("conv-hmac");

        let ev = EventRecord::new(
            cid.clone(),
            EventSeq::FIRST,
            EventKind::UserMsg,
            &serde_json::json!({"text": "hi"}),
            Some("controller".into()),
        )
        .unwrap();
        log.append(&ev).unwrap();

        let got = log.replay_since(&cid, EventSeq(0)).unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].kind, EventKind::UserMsg);
    }

    /// Tampering with a committed row's payload must cause replay to
    /// fail with `TamperDetected`. This is the load-bearing invariant
    /// from §7.8.
    #[test]
    fn hmac_detects_post_insert_payload_tamper() {
        let db = fresh_db();
        let key = b"test-hmac-key-32-bytes-long!!!!!".to_vec();
        let log = EventLog::new(&db).with_hmac_key(key.clone());
        let cid = ConversationId::from("conv-tamper");

        let ev = EventRecord::new(
            cid.clone(),
            EventSeq::FIRST,
            EventKind::UserMsg,
            &serde_json::json!({"text": "original"}),
            None,
        )
        .unwrap();
        log.append(&ev).unwrap();

        // Simulate a post-insert tamper — direct SQL UPDATE.
        db.with_conn(|c| {
            c.execute(
                "UPDATE state_events SET payload = ?1 WHERE conversation_id = ?2 AND seq = ?3",
                params![b"evil".to_vec(), cid.as_str(), 1i64],
            )?;
            Ok(())
        })
        .unwrap();

        let err = log.replay_since(&cid, EventSeq(0)).unwrap_err();
        match err {
            DbError::TamperDetected(msg) => assert!(msg.contains("HMAC verification")),
            other => panic!("expected TamperDetected, got {other:?}"),
        }
    }

    /// Tampering with a row's actor (changing who committed an event)
    /// also trips the detector — the actor is part of the canonical
    /// encoding.
    #[test]
    fn hmac_detects_actor_tamper() {
        let db = fresh_db();
        let key = b"k".to_vec();
        let log = EventLog::new(&db).with_hmac_key(key);
        let cid = ConversationId::from("conv-actor-tamper");
        let ev = EventRecord::new(
            cid.clone(),
            EventSeq::FIRST,
            EventKind::Approval,
            &serde_json::json!({"verb": "approve"}),
            Some("controller".into()),
        )
        .unwrap();
        log.append(&ev).unwrap();

        db.with_conn(|c| {
            c.execute(
                "UPDATE state_events SET actor = 'attacker' WHERE conversation_id = ?1",
                params![cid.as_str()],
            )?;
            Ok(())
        })
        .unwrap();

        assert!(matches!(
            log.replay_since(&cid, EventSeq(0)).unwrap_err(),
            DbError::TamperDetected(_)
        ));
    }

    /// Rows with NULL tag (legacy rows written before migration 0002,
    /// or by a keyless log) verify without error even when a key is
    /// later attached. The background back-fill (Phase 2) flips this.
    #[test]
    fn null_tag_rows_are_accepted_for_backward_compat() {
        let db = fresh_db();
        let cid = ConversationId::from("conv-legacy");

        // Write via a keyless log — rows land with tag = NULL.
        let keyless = EventLog::new(&db);
        let ev = EventRecord::new(
            cid.clone(),
            EventSeq::FIRST,
            EventKind::UserMsg,
            &serde_json::json!({}),
            None,
        )
        .unwrap();
        keyless.append(&ev).unwrap();

        // Now read with a keyed log — must NOT reject the NULL-tagged row.
        let keyed = EventLog::new(&db).with_hmac_key(b"k".to_vec());
        let got = keyed.replay_since(&cid, EventSeq(0)).unwrap();
        assert_eq!(got.len(), 1);
    }

    /// Adversarial: a row signed under key-A must not verify under
    /// key-B. Key rotation is Phase 2, but the correctness property is
    /// testable now.
    #[test]
    fn hmac_rejects_row_signed_under_different_key() {
        let db = fresh_db();
        let cid = ConversationId::from("conv-keymismatch");
        EventLog::new(&db)
            .with_hmac_key(b"key-a".to_vec())
            .append(
                &EventRecord::new(
                    cid.clone(),
                    EventSeq::FIRST,
                    EventKind::UserMsg,
                    &serde_json::json!({}),
                    None,
                )
                .unwrap(),
            )
            .unwrap();

        let keyed_b = EventLog::new(&db).with_hmac_key(b"key-b".to_vec());
        assert!(matches!(
            keyed_b.replay_since(&cid, EventSeq(0)).unwrap_err(),
            DbError::TamperDetected(_)
        ));
    }

    /// commit_turn writes all rows with tags populated; a mid-batch
    /// tamper on any row trips replay.
    #[test]
    fn commit_turn_signs_every_row_in_batch() {
        let db = fresh_db();
        let log = EventLog::new(&db).with_hmac_key(b"k".to_vec());
        let cid = ConversationId::from("conv-batch-sign");

        let turn = vec![
            PendingEvent::encode(
                EventKind::ModelTurn,
                &serde_json::json!({"text": "one"}),
                Some("agent".into()),
            )
            .unwrap(),
            PendingEvent::encode(
                EventKind::ModelTurn,
                &serde_json::json!({"text": "two"}),
                Some("agent".into()),
            )
            .unwrap(),
        ];
        log.commit_turn(&cid, EventSeq(0), turn).unwrap();

        // Tamper the SECOND row only. Replay still has to catch it.
        db.with_conn(|c| {
            c.execute(
                "UPDATE state_events SET payload = ?1 WHERE conversation_id = ?2 AND seq = 2",
                params![b"no".to_vec(), cid.as_str()],
            )?;
            Ok(())
        })
        .unwrap();
        assert!(matches!(
            log.replay_since(&cid, EventSeq(0)).unwrap_err(),
            DbError::TamperDetected(_)
        ));
    }

    // ---- Phase 7: KeyRing rotation -----------------------------------

    /// After rotating the ring's current key, NEW events sign under
    /// the new key while OLD events keep verifying under the old one.
    #[test]
    fn key_ring_rotation_keeps_old_events_verifiable() {
        let db = fresh_db();
        let cid = ConversationId::from("conv-rotate");

        // 1. Sign event A under key id=1.
        let mut ring = KeyRing::single(1, b"key-one-32-bytes-long!!!!!!!!!!!".to_vec());
        let log = EventLog::new(&db).with_key_ring(ring.clone());
        let ev_a = EventRecord::new(
            cid.clone(),
            EventSeq::FIRST,
            EventKind::UserMsg,
            &serde_json::json!({"msg": "a"}),
            None,
        )
        .unwrap();
        log.append(&ev_a).unwrap();

        // 2. Rotate to key id=2.
        let prev = ring.rotate(2, b"key-two-32-bytes-long!!!!!!!!!!!".to_vec());
        assert_eq!(prev, 1);
        assert_eq!(ring.current_id(), 2);
        assert_eq!(ring.registered_ids(), vec![1, 2]);

        // 3. Sign event B under the new ring's current key id=2.
        let log = EventLog::new(&db).with_key_ring(ring.clone());
        let ev_b = EventRecord::new(
            cid.clone(),
            EventSeq(2),
            EventKind::UserMsg,
            &serde_json::json!({"msg": "b"}),
            None,
        )
        .unwrap();
        log.append(&ev_b).unwrap();

        // 4. Replay verifies BOTH events.
        let got = log.replay_since(&cid, EventSeq(0)).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].seq, EventSeq::FIRST);
        assert_eq!(got[1].seq, EventSeq(2));
    }

    #[test]
    fn key_rotation_after_legacy_checkpoint_keeps_genesis_verifiable() {
        let db = fresh_db();
        let cid = ConversationId::from("conv-rotate-after-checkpoint");
        EventLog::new(&db)
            .append(
                &EventRecord::new(
                    cid.clone(),
                    EventSeq(1),
                    EventKind::UserMsg,
                    &serde_json::json!({"legacy": true}),
                    None,
                )
                .unwrap(),
            )
            .unwrap();

        let mut ring = KeyRing::single(1, b"key-one".to_vec());
        EventLog::new(&db)
            .with_key_ring(ring.clone())
            .establish_v2_checkpoint(&cid)
            .unwrap();
        ring.rotate(2, b"key-two".to_vec());
        let log = EventLog::new(&db).with_key_ring(ring);
        log.append(
            &EventRecord::new(
                cid.clone(),
                EventSeq(2),
                EventKind::ModelTurn,
                &serde_json::json!({"v2": true}),
                None,
            )
            .unwrap(),
        )
        .unwrap();

        assert_eq!(log.replay_since(&cid, EventSeq(0)).unwrap().len(), 2);
    }

    /// Adversarial: a row whose stored key_id isn't registered in the
    /// ring fails verification with TamperDetected — operators
    /// cannot "lose" a key and silently accept rows that would have
    /// required it.
    #[test]
    fn replay_with_unknown_key_id_is_tamper_detected() {
        let db = fresh_db();
        let cid = ConversationId::from("conv-missing-key");

        let mut ring = KeyRing::single(7, b"key-seven-32-bytes-long!!!!!!!!".to_vec());
        EventLog::new(&db)
            .with_key_ring(ring.clone())
            .append(
                &EventRecord::new(
                    cid.clone(),
                    EventSeq::FIRST,
                    EventKind::UserMsg,
                    &serde_json::json!({}),
                    None,
                )
                .unwrap(),
            )
            .unwrap();

        // Operator restarts with a ring that doesn't contain key id 7.
        ring = KeyRing::single(99, b"different-32-bytes-long!!!!!!!!!".to_vec());
        let log = EventLog::new(&db).with_key_ring(ring);
        let err = log.replay_since(&cid, EventSeq(0)).unwrap_err();
        assert!(matches!(err, DbError::TamperDetected(msg) if msg.contains("unknown key_id")),);
    }

    /// set_current() refuses an id that hasn't been registered first —
    /// avoids a footgun where rotate() is forgotten and current_id
    /// silently points at nothing.
    #[test]
    fn key_ring_set_current_rejects_unregistered_id() {
        let mut ring = KeyRing::single(1, vec![0u8; 32]);
        assert!(ring.set_current(2).is_err());
        ring.add(2, vec![1u8; 32]);
        assert!(ring.set_current(2).is_ok());
        assert_eq!(ring.current_id(), 2);
    }

    /// rotate() returns the previous current_id so the operator can
    /// persist the bookkeeping.
    #[test]
    fn key_ring_rotate_returns_previous_current_id() {
        let mut ring = KeyRing::single(5, vec![0u8; 32]);
        let prev = ring.rotate(8, vec![1u8; 32]);
        assert_eq!(prev, 5);
        assert_eq!(ring.current_id(), 8);
    }

    /// `with_hmac_key` is back-compat: it builds a single-key ring at
    /// id=0 so existing call sites keep working.
    #[test]
    fn with_hmac_key_back_compat_uses_id_zero() {
        let db = fresh_db();
        let cid = ConversationId::from("conv-bc");
        let log = EventLog::new(&db).with_hmac_key(b"k".to_vec());
        log.append(
            &EventRecord::new(
                cid.clone(),
                EventSeq::FIRST,
                EventKind::UserMsg,
                &serde_json::json!({}),
                None,
            )
            .unwrap(),
        )
        .unwrap();
        // The persisted key_id is 0.
        let stored: i64 = db
            .with_conn(|c| {
                let id: i64 = c
                    .query_row(
                        "SELECT key_id FROM state_events WHERE conversation_id = ?1",
                        params![cid.as_str()],
                        |r| r.get(0),
                    )
                    .unwrap();
                Ok(id)
            })
            .unwrap();
        assert_eq!(stored, 0);
    }

    // ---- Phase 7: NULL-tag back-fill verifier ----------------------

    #[test]
    fn backfill_signs_null_tag_rows_under_current_key() {
        let db = fresh_db();
        let cid = ConversationId::from("conv-bf");
        // Step 1: keyless append → row lands with tag = NULL.
        let keyless = EventLog::new(&db);
        keyless
            .append(
                &EventRecord::new(
                    cid.clone(),
                    EventSeq::FIRST,
                    EventKind::UserMsg,
                    &serde_json::json!({"x": 1}),
                    None,
                )
                .unwrap(),
            )
            .unwrap();
        keyless
            .append(
                &EventRecord::new(
                    cid.clone(),
                    EventSeq(2),
                    EventKind::UserMsg,
                    &serde_json::json!({"x": 2}),
                    None,
                )
                .unwrap(),
            )
            .unwrap();

        // Step 2: attach a key ring + run back-fill.
        let ring = KeyRing::single(7, b"backfill-key-32-bytes-long!!!!!!".to_vec());
        let log = EventLog::new(&db).with_key_ring(ring);
        let report = log.backfill_null_tags().unwrap();
        assert_eq!(report.signed, 2);
        assert_eq!(report.null_remaining, 0);

        // Step 3: replay verifies the now-signed rows.
        let got = log.replay_since(&cid, EventSeq(0)).unwrap();
        assert_eq!(got.len(), 2);

        // Step 4: idempotent — second run signs nothing.
        let again = log.backfill_null_tags().unwrap();
        assert_eq!(again.signed, 0);
        assert_eq!(again.null_remaining, 0);
    }

    #[test]
    fn backfill_writes_current_key_id() {
        let db = fresh_db();
        let cid = ConversationId::from("conv-bf-id");
        EventLog::new(&db)
            .append(
                &EventRecord::new(
                    cid.clone(),
                    EventSeq::FIRST,
                    EventKind::UserMsg,
                    &serde_json::json!({}),
                    None,
                )
                .unwrap(),
            )
            .unwrap();
        let ring = KeyRing::single(42, b"some-key-32-bytes-long!!!!!!!!!!".to_vec());
        EventLog::new(&db)
            .with_key_ring(ring)
            .backfill_null_tags()
            .unwrap();
        let stored: i64 = db
            .with_conn(|c| {
                let id: i64 = c
                    .query_row(
                        "SELECT key_id FROM state_events WHERE conversation_id = ?1",
                        params![cid.as_str()],
                        |r| r.get(0),
                    )
                    .unwrap();
                Ok(id)
            })
            .unwrap();
        assert_eq!(stored, 42);
    }

    /// Without a ring attached, back-fill refuses to run rather than
    /// leaving rows unsigned. Adversarial guard so a misconfigured
    /// operator can't accidentally produce a no-op.
    #[test]
    fn backfill_without_keyring_is_rejected() {
        let db = fresh_db();
        let log = EventLog::new(&db);
        let err = log.backfill_null_tags().unwrap_err();
        assert!(matches!(err, DbError::Config(msg) if msg.contains("KeyRing")));
    }

    /// Already-signed rows are left alone — `signed` only counts
    /// rows that were NULL at the start of the pass.
    #[test]
    fn backfill_skips_rows_that_already_have_tag() {
        let db = fresh_db();
        let cid = ConversationId::from("conv-bf-skip");
        let ring = KeyRing::single(1, b"k1-32-bytes-long!!!!!!!!!!!!!!!!".to_vec());
        // Backfill one legacy row first so it is signed without creating a v2
        // checkpoint, then append another unsigned legacy row.
        EventLog::new(&db)
            .append(
                &EventRecord::new(
                    cid.clone(),
                    EventSeq::FIRST,
                    EventKind::UserMsg,
                    &serde_json::json!({}),
                    None,
                )
                .unwrap(),
            )
            .unwrap();
        EventLog::new(&db)
            .with_key_ring(ring.clone())
            .backfill_null_tags()
            .unwrap();
        EventLog::new(&db)
            .append(
                &EventRecord::new(
                    cid.clone(),
                    EventSeq(2),
                    EventKind::UserMsg,
                    &serde_json::json!({}),
                    None,
                )
                .unwrap(),
            )
            .unwrap();
        let report = EventLog::new(&db)
            .with_key_ring(ring)
            .backfill_null_tags()
            .unwrap();
        assert_eq!(report.signed, 1);
        assert_eq!(report.skipped, 1);
        assert_eq!(report.null_remaining, 0);
    }

    fn append_three_v2(db: &Database, cid: &ConversationId, key: &[u8]) {
        let log = EventLog::new(db).with_hmac_key(key.to_vec());
        for seq in 1..=3 {
            log.append(
                &EventRecord::new(
                    cid.clone(),
                    EventSeq(seq),
                    EventKind::UserMsg,
                    &serde_json::json!({"seq": seq}),
                    None,
                )
                .unwrap(),
            )
            .unwrap();
        }
    }

    #[test]
    fn v2_detects_middle_deletion() {
        let db = fresh_db();
        let cid = ConversationId::from("conv-delete");
        append_three_v2(&db, &cid, b"chain-key");
        db.with_conn(|conn| {
            conn.execute(
                "DELETE FROM state_events WHERE conversation_id = ?1 AND seq = 2",
                params![cid.as_str()],
            )?;
            Ok(())
        })
        .unwrap();
        assert!(matches!(
            EventLog::new(&db)
                .with_hmac_key(b"chain-key".to_vec())
                .replay_since(&cid, EventSeq(0)),
            Err(DbError::TamperDetected(_))
        ));
    }

    #[test]
    fn v2_detects_tail_truncation_against_checkpoint() {
        let db = fresh_db();
        let cid = ConversationId::from("conv-truncate");
        append_three_v2(&db, &cid, b"chain-key");
        db.with_conn(|conn| {
            conn.execute(
                "DELETE FROM state_events WHERE conversation_id = ?1 AND seq = 3",
                params![cid.as_str()],
            )?;
            Ok(())
        })
        .unwrap();
        let error = EventLog::new(&db)
            .with_hmac_key(b"chain-key".to_vec())
            .replay_since(&cid, EventSeq(0))
            .unwrap_err();
        assert!(
            matches!(error, DbError::TamperDetected(message) if message.contains("terminal checkpoint"))
        );
    }

    #[test]
    fn v2_detects_forged_insertion_without_checkpoint_update() {
        let db = fresh_db();
        let cid = ConversationId::from("conv-insert");
        append_three_v2(&db, &cid, b"chain-key");
        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO state_events \
                 (conversation_id, seq, kind, payload, committed_at, actor, tag, key_id, \
                  integrity_version, prev_tag) \
                 SELECT conversation_id, 4, kind, payload, committed_at, actor, tag, key_id, \
                        integrity_version, prev_tag \
                 FROM state_events WHERE conversation_id = ?1 AND seq = 3",
                params![cid.as_str()],
            )?;
            Ok(())
        })
        .unwrap();
        assert!(matches!(
            EventLog::new(&db)
                .with_hmac_key(b"chain-key".to_vec())
                .replay_since(&cid, EventSeq(0)),
            Err(DbError::TamperDetected(_))
        ));
    }

    #[test]
    fn v2_detects_reordering() {
        let db = fresh_db();
        let cid = ConversationId::from("conv-reorder");
        append_three_v2(&db, &cid, b"chain-key");
        db.with_conn(|conn| {
            conn.execute_batch(&format!(
                "UPDATE state_events SET seq = 99 WHERE conversation_id = '{}' AND seq = 1; \
                 UPDATE state_events SET seq = 1 WHERE conversation_id = '{}' AND seq = 2; \
                 UPDATE state_events SET seq = 2 WHERE conversation_id = '{}' AND seq = 99;",
                cid.as_str(),
                cid.as_str(),
                cid.as_str()
            ))?;
            Ok(())
        })
        .unwrap();
        assert!(matches!(
            EventLog::new(&db)
                .with_hmac_key(b"chain-key".to_vec())
                .replay_since(&cid, EventSeq(0)),
            Err(DbError::TamperDetected(_))
        ));
    }

    #[test]
    fn v2_genesis_checkpoints_legacy_rows_without_resigning() {
        let db = fresh_db();
        let cid = ConversationId::from("conv-genesis");
        for seq in 1..=2 {
            EventLog::new(&db)
                .append(
                    &EventRecord::new(
                        cid.clone(),
                        EventSeq(seq),
                        EventKind::UserMsg,
                        &serde_json::json!({"legacy": seq}),
                        None,
                    )
                    .unwrap(),
                )
                .unwrap();
        }
        let log = EventLog::new(&db).with_hmac_key(b"chain-key".to_vec());
        assert!(log.establish_v2_checkpoint(&cid).unwrap());
        assert!(!log.establish_v2_checkpoint(&cid).unwrap());
        log.append(
            &EventRecord::new(
                cid.clone(),
                EventSeq(3),
                EventKind::ModelTurn,
                &serde_json::json!({"v2": true}),
                None,
            )
            .unwrap(),
        )
        .unwrap();

        let versions: Vec<(i64, Option<Vec<u8>>)> = db
            .with_conn(|conn| {
                let mut stmt = conn.prepare(
                    "SELECT integrity_version, tag FROM state_events \
                     WHERE conversation_id = ?1 ORDER BY seq",
                )?;
                Ok(stmt
                    .query_map(params![cid.as_str()], |row| Ok((row.get(0)?, row.get(1)?)))?
                    .collect::<Result<Vec<_>, _>>()?)
            })
            .unwrap();
        assert_eq!(versions[0], (1, None));
        assert_eq!(versions[1], (1, None));
        assert_eq!(versions[2].0, 2);
        assert!(versions[2].1.is_some());

        db.with_conn(|conn| {
            conn.execute(
                "UPDATE state_events SET payload = X'00' \
                 WHERE conversation_id = ?1 AND seq = 1",
                params![cid.as_str()],
            )?;
            Ok(())
        })
        .unwrap();
        assert!(matches!(
            log.replay_since(&cid, EventSeq(0)),
            Err(DbError::TamperDetected(_))
        ));
    }

    #[test]
    fn v2_tamper_is_isolated_to_its_conversation() {
        let db = fresh_db();
        let first = ConversationId::from("conv-isolated-a");
        let second = ConversationId::from("conv-isolated-b");
        append_three_v2(&db, &first, b"chain-key");
        append_three_v2(&db, &second, b"chain-key");
        db.with_conn(|conn| {
            conn.execute(
                "DELETE FROM state_events WHERE conversation_id = ?1 AND seq = 3",
                params![first.as_str()],
            )?;
            Ok(())
        })
        .unwrap();
        let log = EventLog::new(&db).with_hmac_key(b"chain-key".to_vec());
        assert!(matches!(
            log.replay_since(&first, EventSeq(0)),
            Err(DbError::TamperDetected(_))
        ));
        assert_eq!(log.replay_since(&second, EventSeq(0)).unwrap().len(), 3);
    }

    #[test]
    fn v2_commit_and_checkpoint_update_roll_back_together() {
        let db = fresh_db();
        let cid = ConversationId::from("conv-atomic");
        let log = EventLog::new(&db).with_hmac_key(b"chain-key".to_vec());
        log.append(
            &EventRecord::new(
                cid.clone(),
                EventSeq(1),
                EventKind::UserMsg,
                &serde_json::json!({"first": true}),
                None,
            )
            .unwrap(),
        )
        .unwrap();
        db.with_conn(|conn| {
            conn.execute_batch(
                "CREATE TRIGGER fail_integrity_head_update \
                 BEFORE UPDATE ON state_event_integrity_heads \
                 BEGIN SELECT RAISE(ABORT, 'simulated checkpoint write failure'); END;",
            )?;
            Ok(())
        })
        .unwrap();
        let result = log.commit_turn(
            &cid,
            EventSeq(1),
            vec![
                PendingEvent::encode(
                    EventKind::ModelTurn,
                    &serde_json::json!({"second": true}),
                    None,
                )
                .unwrap(),
            ],
        );
        assert!(result.is_err());
        assert_eq!(log.replay_since(&cid, EventSeq(0)).unwrap().len(), 1);
    }

    #[test]
    fn v2_backfill_establishes_each_legacy_conversation_once() {
        let db = fresh_db();
        for id in ["conv-backfill-a", "conv-backfill-b"] {
            EventLog::new(&db)
                .append(
                    &EventRecord::new(
                        ConversationId::from(id),
                        EventSeq(1),
                        EventKind::UserMsg,
                        &serde_json::json!({"legacy": true}),
                        None,
                    )
                    .unwrap(),
                )
                .unwrap();
        }
        let log = EventLog::new(&db).with_hmac_key(b"chain-key".to_vec());
        assert_eq!(log.backfill_v2_checkpoints().unwrap(), 2);
        assert_eq!(log.backfill_v2_checkpoints().unwrap(), 0);
        assert_eq!(
            log.replay_since(&ConversationId::from("conv-backfill-a"), EventSeq(0))
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn v2_checkpoint_disables_destructive_resigning() {
        let db = fresh_db();
        let cid = ConversationId::from("conv-no-resign");
        let log = EventLog::new(&db).with_hmac_key(b"chain-key".to_vec());
        log.append(
            &EventRecord::new(
                cid,
                EventSeq(1),
                EventKind::UserMsg,
                &serde_json::json!({}),
                None,
            )
            .unwrap(),
        )
        .unwrap();
        assert!(matches!(
            log.resign_all_with_current_key(),
            Err(DbError::Invariant(message)) if message.contains("disabled")
        ));
        assert!(matches!(
            log.backfill_null_tags(),
            Err(DbError::Invariant(message)) if message.contains("v2 checkpoints")
        ));
    }

    // ---- end Phase 7 -----------------------------------------------

    #[test]
    fn append_refuses_duplicate_seq() {
        let db = fresh_db();
        let log = EventLog::new(&db);
        let cid = ConversationId::from("dup");
        let ev1 = EventRecord::new(
            cid.clone(),
            EventSeq::FIRST,
            EventKind::UserMsg,
            &serde_json::json!({"x": 1}),
            None,
        )
        .unwrap();
        let ev2 = EventRecord::new(
            cid.clone(),
            EventSeq::FIRST,
            EventKind::UserMsg,
            &serde_json::json!({"x": 2}),
            None,
        )
        .unwrap();
        log.append(&ev1).unwrap();
        let err = log.append(&ev2).unwrap_err();
        assert!(
            matches!(err, DbError::Sqlite(_)),
            "duplicate seq should fail with sqlite uniqueness error, got {:?}",
            err
        );
    }
}
