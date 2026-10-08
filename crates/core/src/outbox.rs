//! Outbox + inbox data-model helpers (§2.4).
//!
//! Actual relay behavior — backoff schedule, dispatch to transport plugins,
//! dead-letter handling — lives in the sibling `execlaw-outbox` crate. This
//! module only owns the DB shape and basic CRUD.

use crate::db::{Database, DbError};
use crate::ids::{ConversationId, EventSeq, IdempotencyKey};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutboxStatus {
    Pending,
    InFlight,
    Delivered,
    Failed,
    DeadLetter,
    Unknown,
}

impl OutboxStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            OutboxStatus::Pending => "pending",
            OutboxStatus::InFlight => "in_flight",
            OutboxStatus::Delivered => "delivered",
            OutboxStatus::Failed => "failed",
            OutboxStatus::DeadLetter => "dead_letter",
            OutboxStatus::Unknown => "unknown",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(Self::Pending),
            "in_flight" => Some(Self::InFlight),
            "delivered" => Some(Self::Delivered),
            "failed" => Some(Self::Failed),
            "dead_letter" => Some(Self::DeadLetter),
            "unknown" => Some(Self::Unknown),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutboxRow {
    pub id: Option<i64>, // set after INSERT
    pub idempotency_key: IdempotencyKey,
    pub conversation_id: ConversationId,
    pub effect_kind: String, // e.g. "transport.send", "schedule.wakeup"
    pub payload: Vec<u8>,    // MessagePack
    pub status: OutboxStatus,
    pub attempts: i64,
    pub next_attempt_at: Option<i64>,
    pub last_error: Option<String>,
    pub enqueued_seq: EventSeq,
}

/// Persisted transition in an outbox effect's delivery history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutboxDeliveryEvent {
    pub id: i64,
    pub outbox_id: i64,
    pub transition: String,
    pub occurred_at: i64,
    pub attempt: i64,
    pub detail: Option<String>,
    pub external_receipt: Option<String>,
    pub actor: Option<String>,
}

/// Delivery transition tied to the conversation event that enqueued it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationDeliveryEvent {
    pub event_seq: i64,
    pub transition: String,
    pub occurred_at: i64,
    pub attempt: i64,
    pub external_receipt: Option<String>,
    pub actor: Option<String>,
}

/// Metadata needed for a Controller to review a sink outcome without reading effect payloads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnknownOutboxEffect {
    pub id: i64,
    pub conversation_id: ConversationId,
    pub effect_kind: String,
    pub attempts: i64,
    pub enqueued_seq: EventSeq,
}

/// Payload-free summary of an exhausted outbox effect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeadLetterEffect {
    pub id: i64,
    pub conversation_id: ConversationId,
    pub effect_kind: String,
    pub idempotency_key: String,
    pub attempts: i64,
    pub enqueued_seq: EventSeq,
    pub last_error: Option<String>,
}

pub struct OutboxStore<'db> {
    db: &'db Database,
}

impl<'db> OutboxStore<'db> {
    pub fn new(db: &'db Database) -> Self {
        Self { db }
    }

    /// Insert a new outbox row, returning the assigned rowid.
    pub fn enqueue(&self, row: &OutboxRow) -> Result<i64, DbError> {
        self.db.transaction(|tx| {
            tx.execute(
                "INSERT INTO state_outbox \
                 (idempotency_key, conversation_id, effect_kind, payload, status, \
                  attempts, next_attempt_at, last_error, enqueued_seq) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    row.idempotency_key.as_str(), row.conversation_id.as_str(), row.effect_kind,
                    row.payload, row.status.as_str(), row.attempts, row.next_attempt_at,
                    row.last_error, row.enqueued_seq.0,
                ],
            )?;
            let id = tx.last_insert_rowid();
            tx.execute(
                "INSERT INTO state_outbox_delivery_events(outbox_id, transition, occurred_at, attempt) VALUES (?1, 'enqueued', ?2, ?3)",
                params![id, chrono::Utc::now().timestamp(), row.attempts],
            )?;
            Ok(id)
        })
    }

    /// Insert an effect under a framework-minted key, returning the original
    /// row when an identical enqueue is replayed after a lost acknowledgment.
    /// Reusing a key for a different conversation, payload, or effect is an
    /// invariant violation and never replaces the original effect.
    pub fn enqueue_idempotent(&self, row: &OutboxRow) -> Result<(i64, bool), DbError> {
        self.enqueue_idempotent_guarded(row, None)
    }

    /// Queue an automatic agent effect only while its ownership generation is current.
    pub fn enqueue_idempotent_for_agent(
        &self,
        row: &OutboxRow,
        scope_key: &str,
        agent_id: &str,
        generation: u64,
    ) -> Result<(i64, bool), DbError> {
        self.enqueue_idempotent_guarded(row, Some((scope_key, agent_id, generation)))
    }

    fn enqueue_idempotent_guarded(
        &self,
        row: &OutboxRow,
        owner: Option<(&str, &str, u64)>,
    ) -> Result<(i64, bool), DbError> {
        self.db.transaction(|tx| {
            if let Some((scope_key, agent_id, generation)) = owner {
                let current: Option<(String,Option<String>,u64)> = tx.query_row(
                    "SELECT owner_kind,agent_id,generation FROM state_agent_ownership WHERE scope_key=?1", [scope_key],
                    |record| Ok((record.get(0)?,record.get(1)?,record.get(2)?)),
                ).optional()?;
                if !matches!(current, Some((ref kind,Some(ref id),current_generation)) if kind == "agent" && id == agent_id && current_generation == generation) {
                    return Err(DbError::Invariant("automatic agent lost transport ownership".into()));
                }
            }
            let existing: Option<(i64, String, String, Vec<u8>, String)> = tx
                .query_row(
                    "SELECT id,conversation_id,effect_kind,payload,status \
                     FROM state_outbox WHERE idempotency_key=?1",
                    [row.idempotency_key.as_str()],
                    |record| {
                        Ok((
                            record.get(0)?,
                            record.get(1)?,
                            record.get(2)?,
                            record.get(3)?,
                            record.get(4)?,
                        ))
                    },
                )
                .optional()?;
            if let Some((id, conversation_id, effect_kind, payload, status)) = existing {
                if conversation_id == row.conversation_id.as_str()
                    && effect_kind == row.effect_kind
                    && payload == row.payload
                {
                    if matches!(status.as_str(), "unknown" | "dead_letter" | "failed") {
                        return Err(DbError::Invariant(format!(
                            "outbox key '{}' requires explicit outcome resolution before replay",
                            row.idempotency_key
                        )));
                    }
                    return Ok((id, false));
                }
                return Err(DbError::Invariant(format!(
                    "outbox idempotency key '{}' was reused for a different effect",
                    row.idempotency_key
                )));
            }
            tx.execute(
                "INSERT INTO state_outbox \
                 (idempotency_key, conversation_id, effect_kind, payload, status, \
                  attempts, next_attempt_at, last_error, enqueued_seq, ownership_scope_key, ownership_generation) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    row.idempotency_key.as_str(),
                    row.conversation_id.as_str(),
                    row.effect_kind,
                    row.payload,
                    row.status.as_str(),
                    row.attempts,
                    row.next_attempt_at,
                    row.last_error,
                    row.enqueued_seq.0,
                    owner.map(|(scope, _, _)| scope),
                    owner.map(|(_, _, generation)| generation),
                ],
            )?;
            let id = tx.last_insert_rowid();
            tx.execute(
                "INSERT INTO state_outbox_delivery_events(outbox_id,transition,occurred_at,attempt) \
                 VALUES (?1,'enqueued',?2,?3)",
                params![id, chrono::Utc::now().timestamp(), row.attempts],
            )?;
            Ok((id, true))
        })
    }

    pub fn mark_status(
        &self,
        id: i64,
        status: OutboxStatus,
        last_error: Option<&str>,
        next_attempt_at: Option<i64>,
    ) -> Result<(), DbError> {
        self.db.with_conn(|c| {
            c.execute(
                "UPDATE state_outbox SET status = ?1, last_error = ?2, \
                 next_attempt_at = ?3, attempts = attempts + 1 WHERE id = ?4",
                params![status.as_str(), last_error, next_attempt_at, id],
            )?;
            Ok(())
        })
    }

    /// Try to record a delivery in the inbox. Returns `true` if this was the
    /// first time we saw this idempotency key (caller should proceed with
    /// side effect), `false` if it was already recorded (caller should skip).
    pub fn inbox_record_if_new(&self, key: &IdempotencyKey) -> Result<bool, DbError> {
        self.db.with_conn(|c| {
            let changed = c.execute(
                "INSERT OR IGNORE INTO state_inbox(idempotency_key, received_at) VALUES (?1, ?2)",
                params![key.as_str(), chrono::Utc::now().timestamp()],
            )?;
            Ok(changed > 0)
        })
    }

    /// Fetch up to `limit` outbox rows that are ready to deliver — `pending`
    /// status and either no `next_attempt_at` or `next_attempt_at <= now`.
    /// Ordered by id (FIFO).
    pub fn ready_pending(&self, now_ts: i64, limit: i64) -> Result<Vec<OutboxRow>, DbError> {
        self.db.with_conn(|c| {
            let mut stmt = c.prepare_cached(
                "SELECT id, idempotency_key, conversation_id, effect_kind, payload, status, \
                        attempts, next_attempt_at, last_error, enqueued_seq \
                 FROM state_outbox \
                 WHERE (status = 'pending' AND (next_attempt_at IS NULL OR next_attempt_at <= ?1)) \
                    OR (status = 'in_flight' AND lease_expires_at IS NOT NULL AND lease_expires_at <= ?1) \
                 ORDER BY id ASC \
                 LIMIT ?2",
            )?;
            let rows = stmt
                .query_map(params![now_ts, limit], row_to_outbox)?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
    }

    /// Atomically mark a row `in_flight` if and only if it is currently
    /// `pending`. Returns true if the caller now owns dispatch.
    ///
    /// This is the leasing primitive that prevents two drain-loop
    /// iterations from dispatching the same row.
    pub fn claim(&self, id: i64) -> Result<bool, DbError> {
        self.claim_with_lease(id, "legacy-relay", chrono::Utc::now().timestamp(), 120)
    }

    /// Atomically claim a pending row or reclaim an abandoned expired lease.
    pub fn claim_with_lease(
        &self,
        id: i64,
        owner: &str,
        now_ts: i64,
        lease_secs: i64,
    ) -> Result<bool, DbError> {
        self.db.transaction(|tx| {
            let previous_status: Option<String> = tx
                .query_row("SELECT status FROM state_outbox WHERE id=?1", [id], |row| row.get(0))
                .optional()?;
            let n = tx.execute(
                "UPDATE state_outbox SET status = 'in_flight', lease_owner = ?1, lease_expires_at = ?2 \
                 WHERE id = ?3 AND (status = 'pending' OR (status = 'in_flight' AND lease_expires_at <= ?4))",
                params![owner, now_ts.saturating_add(lease_secs.max(1)), id, now_ts],
            )?;
            if n == 1 {
                let attempt: i64 = tx.query_row("SELECT attempts + 1 FROM state_outbox WHERE id = ?1", params![id], |r| r.get(0))?;
                let transition = if previous_status.as_deref() == Some("in_flight") {
                    "lease_reclaimed"
                } else {
                    "send_requested"
                };
                tx.execute(
                    "INSERT INTO state_outbox_delivery_events(outbox_id, transition, occurred_at, attempt) VALUES (?1, ?2, ?3, ?4)",
                    params![id, transition, now_ts, attempt],
                )?;
            }
            Ok(n == 1)
        })
    }

    /// Mark a claimed row as successfully delivered.
    pub fn mark_delivered(&self, id: i64) -> Result<(), DbError> {
        self.mark_delivered_with_receipt(id, None)
    }

    /// Record delivery and an optional opaque transport receipt.
    pub fn mark_delivered_with_receipt(
        &self,
        id: i64,
        receipt: Option<&str>,
    ) -> Result<(), DbError> {
        self.db.transaction(|tx| {
            tx.execute(
                "UPDATE state_outbox SET status = 'delivered', last_error = NULL, lease_owner = NULL, lease_expires_at = NULL WHERE id = ?1",
                params![id],
            )?;
            let attempt: i64 = tx.query_row("SELECT attempts + 1 FROM state_outbox WHERE id = ?1", params![id], |r| r.get(0))?;
            tx.execute(
                "INSERT INTO state_outbox_delivery_events(outbox_id, transition, occurred_at, attempt, external_receipt) VALUES (?1, 'delivered', ?2, ?3, ?4)",
                params![id, chrono::Utc::now().timestamp(), attempt, receipt],
            )?;
            Ok(())
        })
    }

    /// Stop automatic retries after a sink may have accepted a non-idempotent effect.
    pub fn mark_unknown(&self, id: i64, reason: &str) -> Result<(), DbError> {
        let detail = bounded_reason(reason);
        self.db.transaction(|tx| {
            let attempt: i64 = tx.query_row(
                "SELECT attempts + 1 FROM state_outbox WHERE id = ?1 AND status = 'in_flight'",
                params![id],
                |row| row.get(0),
            )?;
            tx.execute(
                "UPDATE state_outbox SET status = 'unknown', last_error = ?1, attempts = ?2, \
                 lease_owner = NULL, lease_expires_at = NULL WHERE id = ?3 AND status = 'in_flight'",
                params![detail, attempt, id],
            )?;
            tx.execute(
                "INSERT INTO state_outbox_delivery_events(outbox_id, transition, occurred_at, attempt, detail) \
                 VALUES (?1, 'outcome_unknown', ?2, ?3, ?4)",
                params![id, chrono::Utc::now().timestamp(), attempt, detail],
            )?;
            Ok(())
        })
    }

    /// Authorize another attempt after an operator reviewed an unknown outcome.
    pub fn authorize_unknown_retry(
        &self,
        id: i64,
        actor: &str,
        reason: &str,
    ) -> Result<(), DbError> {
        let actor = validate_resolution_actor(actor)?;
        let detail = bounded_reason(reason);
        if detail.is_empty() {
            return Err(DbError::Invariant(
                "unknown outcome retry requires an operator reason".into(),
            ));
        }
        self.db.transaction(|tx| {
            let attempt: i64 = tx.query_row(
                "SELECT attempts FROM state_outbox WHERE id = ?1 AND status = 'unknown'",
                params![id],
                |row| row.get(0),
            )?;
            tx.execute(
                "UPDATE state_outbox SET status = 'pending', last_error = ?1, next_attempt_at = NULL \
                 WHERE id = ?2 AND status = 'unknown'",
                params![detail, id],
            )?;
            tx.execute(
                "INSERT INTO state_outbox_delivery_events(outbox_id, transition, occurred_at, attempt, detail, actor) \
                 VALUES (?1, 'retry_authorized', ?2, ?3, ?4, ?5)",
                params![id, chrono::Utc::now().timestamp(), attempt, detail, actor],
            )?;
            Ok(())
        })
    }

    /// Confirm an unknown effect was accepted, preserving the operator's receipt evidence.
    pub fn confirm_unknown_delivered(
        &self,
        id: i64,
        actor: &str,
        receipt: &str,
    ) -> Result<(), DbError> {
        let actor = validate_resolution_actor(actor)?;
        let receipt = bounded_reason(receipt);
        if receipt.is_empty() {
            return Err(DbError::Invariant(
                "unknown outcome confirmation requires a receipt reference".into(),
            ));
        }
        self.db.transaction(|tx| {
            let attempt: i64 = tx.query_row(
                "SELECT attempts FROM state_outbox WHERE id = ?1 AND status = 'unknown'",
                params![id],
                |row| row.get(0),
            )?;
            tx.execute(
                "UPDATE state_outbox SET status = 'delivered', last_error = NULL WHERE id = ?1 AND status = 'unknown'",
                params![id],
            )?;
            tx.execute(
                "INSERT INTO state_outbox_delivery_events(outbox_id, transition, occurred_at, attempt, detail, external_receipt, actor) \
                 VALUES (?1, 'operator_confirmed_delivered', ?2, ?3, ?4, ?5, ?6)",
                params![id, chrono::Utc::now().timestamp(), attempt, receipt, receipt, actor],
            )?;
            Ok(())
        })
    }

    /// Record a failed attempt. Bumps `attempts`; if under the retry
    /// budget, sets status back to `pending` with `next_attempt_at` for
    /// the backoff schedule. If over budget, status → `dead_letter`.
    pub fn record_failure(
        &self,
        id: i64,
        error: &str,
        retry_budget_max: u32,
        backoff_secs: i64,
    ) -> Result<bool, DbError> {
        // Returns true if retrying, false if moved to dead_letter.
        self.db.transaction(|tx| {
            let attempts: i64 = tx.query_row(
                "SELECT attempts FROM state_outbox WHERE id = ?1",
                params![id],
                |r| r.get(0),
            )?;
            let new_attempts = attempts + 1;
            let retrying = (new_attempts as u32) < retry_budget_max;
            if !retrying {
                tx.execute(
                    "UPDATE state_outbox SET status = 'dead_letter', last_error = ?1, \
                         attempts = ?2, lease_owner = NULL, lease_expires_at = NULL WHERE id = ?3",
                    params![error, new_attempts, id],
                )?;
            } else {
                let next_attempt_at = chrono::Utc::now().timestamp() + backoff_secs;
                tx.execute(
                    "UPDATE state_outbox SET status = 'pending', last_error = ?1, \
                         attempts = ?2, next_attempt_at = ?3, lease_owner = NULL, lease_expires_at = NULL WHERE id = ?4",
                    params![error, new_attempts, next_attempt_at, id],
                )?;
            }
            tx.execute(
                "INSERT INTO state_outbox_delivery_events(outbox_id, transition, occurred_at, attempt, detail) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![id, if retrying { "retry_scheduled" } else { "dead_letter" }, chrono::Utc::now().timestamp(), new_attempts, error],
            )?;
            Ok(retrying)
        })
    }

    /// Read the delivery history in append order.
    pub fn delivery_timeline(&self, id: i64) -> Result<Vec<OutboxDeliveryEvent>, DbError> {
        self.db.with_conn(|c| {
            let mut stmt = c.prepare_cached(
                "SELECT id, outbox_id, transition, occurred_at, attempt, detail, external_receipt, actor \
                 FROM state_outbox_delivery_events WHERE outbox_id = ?1 ORDER BY id ASC",
            )?;
            let events = stmt
                .query_map(params![id], |row| {
                    Ok(OutboxDeliveryEvent {
                        id: row.get(0)?,
                        outbox_id: row.get(1)?,
                        transition: row.get(2)?,
                        occurred_at: row.get(3)?,
                        attempt: row.get(4)?,
                        detail: row.get(5)?,
                        external_receipt: row.get(6)?,
                        actor: row.get(7)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(events)
        })
    }

    /// Read transport effects associated with a conversation's event sequence.
    pub fn conversation_delivery_timeline(
        &self,
        conversation_id: &ConversationId,
    ) -> Result<Vec<ConversationDeliveryEvent>, DbError> {
        self.db.with_conn(|connection| {
            let mut statement = connection.prepare_cached(
                "SELECT o.enqueued_seq, e.transition, e.occurred_at, e.attempt, e.external_receipt, e.actor \
                 FROM state_outbox o JOIN state_outbox_delivery_events e ON e.outbox_id = o.id \
                 WHERE o.conversation_id = ?1 AND o.effect_kind LIKE 'transport.%' \
                 ORDER BY o.enqueued_seq, e.id",
            )?;
            let events = statement
                .query_map(params![conversation_id.as_str()], |row| {
                    Ok(ConversationDeliveryEvent {
                        event_seq: row.get(0)?,
                        transition: row.get(1)?,
                        occurred_at: row.get(2)?,
                        attempt: row.get(3)?,
                        external_receipt: row.get(4)?,
                        actor: row.get(5)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(events)
        })
    }

    /// Read a bounded delivery timeline correlated to one durable run's input
    /// event and ending before the next run in the same conversation.
    pub fn run_delivery_timeline(
        &self,
        run_id: &str,
        limit: usize,
    ) -> Result<Vec<ConversationDeliveryEvent>, DbError> {
        self.db.with_conn(|connection| {
            let mut statement = connection.prepare_cached(
                "SELECT o.enqueued_seq, e.transition, e.occurred_at, e.attempt, e.external_receipt, e.actor \
                 FROM state_runs r \
                 JOIN state_outbox o ON o.conversation_id=r.conversation_id \
                   AND o.enqueued_seq>=r.input_event_seq \
                   AND o.enqueued_seq<COALESCE((SELECT MIN(next.input_event_seq) FROM state_runs next \
                     WHERE next.conversation_id=r.conversation_id AND next.input_event_seq>r.input_event_seq), 9223372036854775807) \
                 JOIN state_outbox_delivery_events e ON e.outbox_id=o.id \
                 WHERE r.run_id=?1 AND o.effect_kind LIKE 'transport.%' \
                 ORDER BY o.enqueued_seq,e.id LIMIT ?2",
            )?;
            statement.query_map(params![run_id, limit.clamp(1, 500)], |row| {
                Ok(ConversationDeliveryEvent {
                    event_seq: row.get(0)?,
                    transition: row.get(1)?,
                    occurred_at: row.get(2)?,
                    attempt: row.get(3)?,
                    external_receipt: row.get(4)?,
                    actor: row.get(5)?,
                })
            })?.collect::<Result<Vec<_>, _>>().map_err(DbError::from)
        })
    }

    /// Count rows currently in dead_letter. Useful for alerting.
    pub fn dead_letter_count(&self) -> Result<i64, DbError> {
        self.db.with_conn(|c| {
            let n: i64 = c.query_row(
                "SELECT COUNT(*) FROM state_outbox WHERE status = 'dead_letter'",
                [],
                |r| r.get(0),
            )?;
            Ok(n)
        })
    }

    /// List exhausted effects without returning the effect payload.
    pub fn dead_letters(&self, limit: usize) -> Result<Vec<DeadLetterEffect>, DbError> {
        self.db.with_conn(|connection| {
            let mut statement = connection.prepare_cached(
                "SELECT id,conversation_id,effect_kind,idempotency_key,attempts,enqueued_seq,last_error \
                 FROM state_outbox WHERE status='dead_letter' ORDER BY id LIMIT ?1",
            )?;
            statement
                .query_map([limit.clamp(1, 200)], |row| {
                    Ok(DeadLetterEffect {
                        id: row.get(0)?,
                        conversation_id: ConversationId::from(row.get::<_, String>(1)?),
                        effect_kind: row.get(2)?,
                        idempotency_key: row.get(3)?,
                        attempts: row.get(4)?,
                        enqueued_seq: EventSeq(row.get(5)?),
                        last_error: row.get(6)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()
                .map_err(DbError::from)
        })
    }

    /// Requeue a dead-lettered effect with its existing idempotency identity.
    /// Unknown outcomes are deliberately excluded; they require receipt
    /// reconciliation through `authorize_unknown_retry` or confirmation.
    pub fn redrive_dead_letter(&self, id: i64, actor: &str, reason: &str) -> Result<(), DbError> {
        let (actor, reason) = crate::job_redrive::validate_redrive_request(actor, reason)?;
        self.db.transaction(|tx| {
            let (key, attempts): (String, i64) = tx.query_row(
                "SELECT idempotency_key,attempts FROM state_outbox \
                 WHERE id=?1 AND status='dead_letter'",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            if key.trim().is_empty() {
                return Err(DbError::Invariant(
                    "dead-lettered effect has no stable idempotency identity".into(),
                ));
            }
            let now = chrono::Utc::now().timestamp();
            let changed = tx.execute(
                "UPDATE state_outbox SET status='pending',next_attempt_at=NULL,lease_owner=NULL, \
                    lease_expires_at=NULL WHERE id=?1 AND status='dead_letter'",
                [id],
            )?;
            if changed != 1 {
                return Err(DbError::Invariant("dead-letter status changed during redrive".into()));
            }
            tx.execute(
                "INSERT INTO state_outbox_delivery_events(outbox_id,transition,occurred_at,attempt,detail,actor) \
                 VALUES (?1,'operator_redrive',?2,?3,?4,?5)",
                params![id, now, attempts, reason, actor],
            )?;
            crate::job_redrive::append_redrive_event(
                tx,
                crate::job_redrive::RedriveKind::Outbox,
                &id.to_string(),
                &key,
                &actor,
                &reason,
                attempts,
                now,
            )?;
            Ok(())
        })
    }

    /// List bounded metadata for effects that need operator reconciliation.
    pub fn unknown_effects(&self, limit: usize) -> Result<Vec<UnknownOutboxEffect>, DbError> {
        self.db.with_conn(|connection| {
            let mut statement = connection.prepare_cached(
                "SELECT id, conversation_id, effect_kind, attempts, enqueued_seq \
                 FROM state_outbox WHERE status = 'unknown' ORDER BY id LIMIT ?1",
            )?;
            statement
                .query_map([limit.clamp(1, 200)], |row| {
                    Ok(UnknownOutboxEffect {
                        id: row.get(0)?,
                        conversation_id: ConversationId::from(row.get::<_, String>(1)?),
                        effect_kind: row.get(2)?,
                        attempts: row.get(3)?,
                        enqueued_seq: EventSeq(row.get(4)?),
                    })
                })?
                .collect::<Result<Vec<_>, _>>()
                .map_err(DbError::from)
        })
    }

    /// Fetch one unknown effect by ID without exposing its payload or error detail.
    pub fn unknown_effect(&self, id: i64) -> Result<Option<UnknownOutboxEffect>, DbError> {
        self.db.with_conn(|connection| {
            connection
                .query_row(
                    "SELECT id, conversation_id, effect_kind, attempts, enqueued_seq \
                     FROM state_outbox WHERE id = ?1 AND status = 'unknown'",
                    [id],
                    |row| {
                        Ok(UnknownOutboxEffect {
                            id: row.get(0)?,
                            conversation_id: ConversationId::from(row.get::<_, String>(1)?),
                            effect_kind: row.get(2)?,
                            attempts: row.get(3)?,
                            enqueued_seq: EventSeq(row.get(4)?),
                        })
                    },
                )
                .optional()
                .map_err(DbError::from)
        })
    }
}

fn row_to_outbox(row: &rusqlite::Row<'_>) -> rusqlite::Result<OutboxRow> {
    let id: i64 = row.get(0)?;
    let idempotency_key: String = row.get(1)?;
    let conversation_id: String = row.get(2)?;
    let effect_kind: String = row.get(3)?;
    let payload: Vec<u8> = row.get(4)?;
    let status: String = row.get(5)?;
    let attempts: i64 = row.get(6)?;
    let next_attempt_at: Option<i64> = row.get(7)?;
    let last_error: Option<String> = row.get(8)?;
    let enqueued_seq: i64 = row.get(9)?;
    Ok(OutboxRow {
        id: Some(id),
        idempotency_key: IdempotencyKey::from_string(idempotency_key),
        conversation_id: ConversationId::from(conversation_id),
        effect_kind,
        payload,
        status: OutboxStatus::parse(&status).unwrap_or(OutboxStatus::Pending),
        attempts,
        next_attempt_at,
        last_error,
        enqueued_seq: EventSeq(enqueued_seq),
    })
}

fn bounded_reason(reason: &str) -> String {
    reason
        .chars()
        .take(1024)
        .collect::<String>()
        .trim()
        .to_owned()
}

fn validate_resolution_actor(actor: &str) -> Result<String, DbError> {
    let actor = actor.trim();
    if actor.is_empty() || actor.len() > 128 {
        return Err(DbError::Invariant(
            "unknown outcome resolution needs a bounded actor id".into(),
        ));
    }
    Ok(actor.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Database, DbConfig};
    use crate::migrations::MigrationRunner;

    fn fresh_db() -> Database {
        let db = Database::open(&DbConfig::in_memory_unencrypted()).unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        db
    }

    #[test]
    fn enqueue_and_dedup_idempotency_key() {
        let db = fresh_db();
        let store = OutboxStore::new(&db);
        let cid = ConversationId::from("c1");
        let key = IdempotencyKey::mint(&cid, crate::ids::TurnSeq(1), 0);
        let row = OutboxRow {
            id: None,
            idempotency_key: key.clone(),
            conversation_id: cid.clone(),
            effect_kind: "transport.send".into(),
            payload: b"payload".to_vec(),
            status: OutboxStatus::Pending,
            attempts: 0,
            next_attempt_at: None,
            last_error: None,
            enqueued_seq: EventSeq(1),
        };
        let id = store.enqueue(&row).unwrap();
        assert!(id > 0);

        // Inserting the same idempotency_key must fail (UNIQUE constraint).
        let dup = store.enqueue(&row);
        assert!(dup.is_err(), "duplicate idempotency key must be rejected");
    }

    #[test]
    fn idempotent_enqueue_replays_identical_rows_and_refuses_unknown_retry() {
        let db = fresh_db();
        let store = OutboxStore::new(&db);
        let cid = ConversationId::from("queued-transport");
        let row = OutboxRow {
            id: None,
            idempotency_key: IdempotencyKey::mint(&cid, crate::ids::TurnSeq(7), 2),
            conversation_id: cid,
            effect_kind: "transport.send".into(),
            payload: b"same-effect".to_vec(),
            status: OutboxStatus::Pending,
            attempts: 0,
            next_attempt_at: Some(i64::MAX),
            last_error: None,
            enqueued_seq: EventSeq(11),
        };
        let (id, created) = store.enqueue_idempotent(&row).unwrap();
        assert!(created);
        assert_eq!(store.enqueue_idempotent(&row).unwrap(), (id, false));

        let mut conflicting = row.clone();
        conflicting.payload = b"different-effect".to_vec();
        assert!(store.enqueue_idempotent(&conflicting).is_err());

        store.claim(id).unwrap();
        store
            .mark_unknown(id, "peer closed after request body")
            .unwrap();
        assert!(store.enqueue_idempotent(&row).is_err());
    }

    #[test]
    fn automatic_agent_enqueue_is_fenced_by_controller_takeover() {
        let db = fresh_db();
        let ownership = crate::agent_ownership::AgentOwnershipStore::new(&db);
        let cid = ConversationId::from("owner-fenced-chat");
        let current = ownership
            .assign_agent(cid.as_str(), "whatsapp", "group@g.us", "camper", 10)
            .unwrap();
        let row = OutboxRow {
            id: None,
            idempotency_key: IdempotencyKey::mint(&cid, crate::ids::TurnSeq(1), 0),
            conversation_id: cid.clone(),
            effect_kind: "transport.send".into(),
            payload: b"draft".to_vec(),
            status: OutboxStatus::Pending,
            attempts: 0,
            next_attempt_at: None,
            last_error: None,
            enqueued_seq: EventSeq(1),
        };
        let key = crate::agent_ownership::scope_key(cid.as_str(), "whatsapp", "group@g.us");
        let (outbox_id, inserted) = OutboxStore::new(&db)
            .enqueue_idempotent_for_agent(&row, &key, "camper", current.generation)
            .unwrap();
        assert!(inserted);
        ownership
            .takeover(cid.as_str(), "whatsapp", "group@g.us", 11)
            .unwrap();
        assert_eq!(outbox_status(&db, outbox_id), "failed");
        let transitions = OutboxStore::new(&db).delivery_timeline(outbox_id).unwrap();
        assert_eq!(
            transitions.last().unwrap().transition,
            "cancelled_by_takeover"
        );
        assert!(
            OutboxStore::new(&db)
                .enqueue_idempotent_for_agent(&row, &key, "camper", current.generation)
                .is_err()
        );
    }

    #[test]
    fn takeover_marks_in_flight_automatic_send_unknown_for_reconciliation() {
        let db = fresh_db();
        let ownership = crate::agent_ownership::AgentOwnershipStore::new(&db);
        let cid = ConversationId::from("owner-race-chat");
        let owner = ownership
            .assign_agent(cid.as_str(), "signal", "peer", "watcher", 1)
            .unwrap();
        let row = OutboxRow {
            id: None,
            idempotency_key: IdempotencyKey::mint(&cid, crate::ids::TurnSeq(2), 0),
            conversation_id: cid.clone(),
            effect_kind: "transport.send".into(),
            payload: b"reply".to_vec(),
            status: OutboxStatus::Pending,
            attempts: 0,
            next_attempt_at: None,
            last_error: None,
            enqueued_seq: EventSeq(2),
        };
        let scope = crate::agent_ownership::scope_key(cid.as_str(), "signal", "peer");
        let (id, _) = OutboxStore::new(&db)
            .enqueue_idempotent_for_agent(&row, &scope, "watcher", owner.generation)
            .unwrap();
        assert!(
            OutboxStore::new(&db)
                .claim_with_lease(id, "relay", 5, 60)
                .unwrap()
        );
        ownership
            .takeover(cid.as_str(), "signal", "peer", 6)
            .unwrap();
        assert_eq!(outbox_status(&db, id), "unknown");
        assert!(
            OutboxStore::new(&db)
                .ready_pending(100, 10)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            OutboxStore::new(&db)
                .delivery_timeline(id)
                .unwrap()
                .last()
                .unwrap()
                .transition,
            "takeover_outcome_unknown"
        );
    }

    #[test]
    fn takeover_and_automatic_enqueue_race_never_leaves_a_pending_agent_send() {
        use std::sync::{Arc, Barrier};
        let db = fresh_db();
        let cid = ConversationId::from("owner-concurrent-chat");
        let current = crate::agent_ownership::AgentOwnershipStore::new(&db)
            .assign_agent(cid.as_str(), "whatsapp", "peer", "watcher", 1)
            .unwrap();
        let scope = crate::agent_ownership::scope_key(cid.as_str(), "whatsapp", "peer");
        let row = OutboxRow {
            id: None,
            idempotency_key: IdempotencyKey::mint(&cid, crate::ids::TurnSeq(3), 0),
            conversation_id: cid.clone(),
            effect_kind: "transport.send".into(),
            payload: b"reply".to_vec(),
            status: OutboxStatus::Pending,
            attempts: 0,
            next_attempt_at: None,
            last_error: None,
            enqueued_seq: EventSeq(3),
        };
        let barrier = Arc::new(Barrier::new(2));
        let enqueue_db = db.clone();
        let enqueue_barrier = barrier.clone();
        let enqueue_scope = scope.clone();
        let enqueue_row = row.clone();
        let generation = current.generation;
        let enqueue = std::thread::spawn(move || {
            enqueue_barrier.wait();
            OutboxStore::new(&enqueue_db).enqueue_idempotent_for_agent(
                &enqueue_row,
                &enqueue_scope,
                "watcher",
                generation,
            )
        });
        let takeover_db = db.clone();
        let takeover_barrier = barrier.clone();
        let takeover_cid = cid.clone();
        let takeover = std::thread::spawn(move || {
            takeover_barrier.wait();
            crate::agent_ownership::AgentOwnershipStore::new(&takeover_db).takeover(
                takeover_cid.as_str(),
                "whatsapp",
                "peer",
                2,
            )
        });
        let enqueue_result = enqueue.join().unwrap();
        takeover.join().unwrap().unwrap();
        assert!(
            OutboxStore::new(&db)
                .ready_pending(100, 10)
                .unwrap()
                .is_empty()
        );
        if let Ok((id, true)) = enqueue_result {
            assert_eq!(outbox_status(&db, id), "failed");
        }
    }

    #[test]
    fn unknown_outcome_stops_retries_until_operator_resolution() {
        let db = fresh_db();
        let store = OutboxStore::new(&db);
        let cid = ConversationId::from("unknown-delivery");
        let id = store
            .enqueue(&OutboxRow {
                id: None,
                idempotency_key: IdempotencyKey::mint(&cid, crate::ids::TurnSeq(1), 1),
                conversation_id: cid,
                effect_kind: "transport.send".into(),
                payload: vec![1],
                status: OutboxStatus::Pending,
                attempts: 0,
                next_attempt_at: None,
                last_error: None,
                enqueued_seq: EventSeq(1),
            })
            .unwrap();
        assert!(store.claim_with_lease(id, "relay-test", 100, 30).unwrap());
        store
            .mark_unknown(id, "peer closed after request body")
            .unwrap();
        assert!(store.ready_pending(1_000, 10).unwrap().is_empty());
        assert_eq!(outbox_status(&db, id), "unknown");
        assert!(
            store
                .authorize_unknown_retry(id, "operator-1", " ")
                .is_err()
        );

        store
            .authorize_unknown_retry(id, "operator-1", "operator checked the transport receipt")
            .unwrap();
        assert_eq!(outbox_status(&db, id), "pending");
        assert_eq!(
            store
                .delivery_timeline(id)
                .unwrap()
                .last()
                .unwrap()
                .transition,
            "retry_authorized"
        );

        assert!(store.claim_with_lease(id, "relay-test", 200, 30).unwrap());
        store.mark_unknown(id, "second response was lost").unwrap();
        store
            .confirm_unknown_delivered(id, "operator-1", "receipt:operator-confirmed-1")
            .unwrap();
        assert_eq!(outbox_status(&db, id), "delivered");
        let timeline = store.delivery_timeline(id).unwrap();
        assert_eq!(
            timeline.last().unwrap().transition,
            "operator_confirmed_delivered"
        );
        assert_eq!(
            timeline.last().unwrap().external_receipt.as_deref(),
            Some("receipt:operator-confirmed-1")
        );
        assert_eq!(
            timeline.last().unwrap().actor.as_deref(),
            Some("operator-1")
        );
    }

    fn outbox_status(db: &Database, id: i64) -> String {
        db.with_conn(|connection| {
            Ok(connection.query_row(
                "SELECT status FROM state_outbox WHERE id = ?1",
                [id],
                |row| row.get(0),
            )?)
        })
        .unwrap()
    }

    #[test]
    fn conversation_projection_joins_only_transport_effect_transitions() {
        let db = fresh_db();
        let store = OutboxStore::new(&db);
        let cid = ConversationId::from("timeline-conversation");
        let mut transport = mk_row(&cid, 10);
        transport.effect_kind = "transport.send".into();
        transport.enqueued_seq = EventSeq(7);
        let transport_id = store.enqueue(&transport).unwrap();
        let mut other = mk_row(&cid, 11);
        other.effect_kind = "schedule.wakeup".into();
        other.enqueued_seq = EventSeq(8);
        store.enqueue(&other).unwrap();

        assert!(
            store
                .claim_with_lease(transport_id, "relay", 100, 30)
                .unwrap()
        );
        store
            .mark_delivered_with_receipt(transport_id, Some("remote-ack"))
            .unwrap();
        let timeline = store.conversation_delivery_timeline(&cid).unwrap();
        assert_eq!(
            timeline
                .iter()
                .map(|event| (event.event_seq, event.transition.as_str()))
                .collect::<Vec<_>>(),
            vec![(7, "enqueued"), (7, "send_requested"), (7, "delivered")]
        );
        assert_eq!(
            timeline.last().unwrap().external_receipt.as_deref(),
            Some("remote-ack")
        );
    }

    #[test]
    fn inbox_dedup_only_records_once() {
        let db = fresh_db();
        let store = OutboxStore::new(&db);
        let key = IdempotencyKey::from_string("k1");
        assert!(store.inbox_record_if_new(&key).unwrap());
        assert!(!store.inbox_record_if_new(&key).unwrap());
    }

    fn mk_row(cid: &ConversationId, ord: u32) -> OutboxRow {
        OutboxRow {
            id: None,
            idempotency_key: IdempotencyKey::mint(cid, crate::ids::TurnSeq(1), ord),
            conversation_id: cid.clone(),
            effect_kind: "test.effect".into(),
            payload: b"p".to_vec(),
            status: OutboxStatus::Pending,
            attempts: 0,
            next_attempt_at: None,
            last_error: None,
            enqueued_seq: EventSeq(1),
        }
    }

    /// `claim` is the leasing primitive — two concurrent drain loops
    /// must not both win a claim on the same row.
    #[test]
    fn claim_is_mutually_exclusive() {
        let db = fresh_db();
        let store = OutboxStore::new(&db);
        let id = store
            .enqueue(&mk_row(&ConversationId::from("c"), 0))
            .unwrap();
        assert!(store.claim(id).unwrap(), "first claim should succeed");
        assert!(!store.claim(id).unwrap(), "second claim must fail");
    }

    #[test]
    fn expired_outbox_lease_is_reclaimed_and_timeline_survives_store_reopen() {
        let db = fresh_db();
        let store = OutboxStore::new(&db);
        let id = store
            .enqueue(&mk_row(&ConversationId::from("c"), 0))
            .unwrap();
        assert!(store.claim_with_lease(id, "runner-a", 100, 5).unwrap());
        assert!(!store.claim_with_lease(id, "runner-b", 104, 5).unwrap());
        assert!(store.claim_with_lease(id, "runner-b", 105, 5).unwrap());
        store.record_failure(id, "transient", 4, 1).unwrap();
        let timeline = store.delivery_timeline(id).unwrap();
        assert_eq!(
            timeline
                .iter()
                .map(|event| event.transition.as_str())
                .collect::<Vec<_>>(),
            [
                "enqueued",
                "send_requested",
                "lease_reclaimed",
                "retry_scheduled"
            ]
        );
        assert_eq!(timeline[2].attempt, 1);
    }

    #[test]
    fn dead_letter_is_an_auditable_final_transition() {
        let db = fresh_db();
        let store = OutboxStore::new(&db);
        let id = store
            .enqueue(&mk_row(&ConversationId::from("c"), 0))
            .unwrap();
        assert!(!store.record_failure(id, "permanent", 1, 0).unwrap());
        let timeline = store.delivery_timeline(id).unwrap();
        assert_eq!(timeline.last().unwrap().transition, "dead_letter");
    }

    #[test]
    fn operator_redrive_preserves_effect_identity_and_records_actor_and_reason() {
        let db = fresh_db();
        let store = OutboxStore::new(&db);
        let id = store
            .enqueue(&mk_row(&ConversationId::from("redrive"), 3))
            .unwrap();
        assert!(!store.record_failure(id, "remote rejected", 1, 0).unwrap());
        let before = store.dead_letters(10).unwrap().pop().unwrap();
        store
            .redrive_dead_letter(
                id,
                "controller-1",
                "operator confirmed the recipient is ready",
            )
            .unwrap();
        assert!(store.dead_letters(10).unwrap().is_empty());
        let ready = store
            .ready_pending(chrono::Utc::now().timestamp(), 10)
            .unwrap();
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].idempotency_key.0, before.idempotency_key);
        let timeline = store.delivery_timeline(id).unwrap();
        let redrive = timeline.last().unwrap();
        assert_eq!(redrive.transition, "operator_redrive");
        assert_eq!(redrive.actor.as_deref(), Some("controller-1"));
        assert_eq!(
            redrive.detail.as_deref(),
            Some("operator confirmed the recipient is ready")
        );
        assert!(
            store
                .redrive_dead_letter(id, "controller-1", "duplicate attempt")
                .is_err()
        );
        let audit_rows: i64 = db
            .with_conn(|conn| Ok(conn.query_row(
                "SELECT COUNT(*) FROM state_job_redrive_events WHERE job_kind='outbox' AND job_id=?1",
                [id.to_string()],
                |row| row.get(0),
            )?))
            .unwrap();
        assert_eq!(audit_rows, 1);
    }

    #[test]
    fn process_kill_after_outbox_claim_is_recovered_after_database_reopen() {
        use std::process::{Command, Stdio};
        use std::time::{Duration, Instant};

        let temp = tempfile::tempdir().unwrap();
        let database_path = temp.path().join("outbox-crash.db");
        let ready_path = temp.path().join("claimed");
        let db = Database::open(&DbConfig {
            path: database_path.clone(),
            key: None,
        })
        .unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        let id = OutboxStore::new(&db)
            .enqueue(&mk_row(&ConversationId::from("crash-test"), 0))
            .unwrap();
        drop(db);

        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "outbox::tests::outbox_crash_child_holds_claim_until_killed",
                "--nocapture",
            ])
            .env("EXECLAW_OUTBOX_CRASH_DB", &database_path)
            .env("EXECLAW_OUTBOX_CRASH_READY", &ready_path)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !ready_path.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            ready_path.exists(),
            "child process did not acquire its lease"
        );
        child.kill().unwrap();
        let _ = child.wait();

        std::thread::sleep(Duration::from_millis(1_100));
        let reopened = Database::open(&DbConfig {
            path: database_path,
            key: None,
        })
        .unwrap();
        let recovered = OutboxStore::new(&reopened)
            .claim_with_lease(id, "recovery-process", chrono::Utc::now().timestamp(), 30)
            .unwrap();
        assert!(recovered);
        let events = OutboxStore::new(&reopened).delivery_timeline(id).unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| event.transition == "enqueued")
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| event.transition == "send_requested")
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| event.transition == "lease_reclaimed")
                .count(),
            1
        );
    }

    #[test]
    fn outbox_crash_child_holds_claim_until_killed() {
        let (Some(database_path), Some(ready_path)) = (
            std::env::var_os("EXECLAW_OUTBOX_CRASH_DB"),
            std::env::var_os("EXECLAW_OUTBOX_CRASH_READY"),
        ) else {
            return;
        };
        let db = Database::open(&DbConfig {
            path: database_path.into(),
            key: None,
        })
        .unwrap();
        let store = OutboxStore::new(&db);
        let id = store.ready_pending(i64::MAX, 1).unwrap()[0].id.unwrap();
        assert!(
            store
                .claim_with_lease(id, "killed-process", chrono::Utc::now().timestamp(), 1)
                .unwrap()
        );
        std::fs::write(ready_path, b"claimed").unwrap();
        std::thread::park();
    }

    /// `record_failure` bumps attempts and sets next_attempt_at while
    /// under budget, then transitions to dead_letter when over.
    #[test]
    fn record_failure_retries_under_budget_then_dead_letters() {
        let db = fresh_db();
        let store = OutboxStore::new(&db);
        let id = store
            .enqueue(&mk_row(&ConversationId::from("c"), 0))
            .unwrap();

        // Under budget: status returns to pending, row has next_attempt_at set.
        let retrying = store.record_failure(id, "boom", 3, 60).unwrap();
        assert!(retrying);
        let (status, attempts, next): (String, i64, Option<i64>) = db
            .with_conn(|c| {
                let v = c
                    .query_row(
                        "SELECT status, attempts, next_attempt_at FROM state_outbox WHERE id = ?1",
                        params![id],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                    )
                    .unwrap();
                Ok(v)
            })
            .unwrap();
        assert_eq!(status, "pending");
        assert_eq!(attempts, 1);
        assert!(next.is_some() && next.unwrap() > 0);

        // Two more failures push attempts past budget → dead_letter.
        let _ = store.record_failure(id, "boom", 3, 60).unwrap();
        let retrying3 = store.record_failure(id, "boom", 3, 60).unwrap();
        assert!(!retrying3, "third failure past budget must dead-letter");
        assert_eq!(store.dead_letter_count().unwrap(), 1);
    }

    /// `ready_pending` must skip rows whose `next_attempt_at` is in
    /// the future — otherwise backoff would have no effect.
    #[test]
    fn ready_pending_skips_rows_with_future_next_attempt() {
        let db = fresh_db();
        let store = OutboxStore::new(&db);
        let cid = ConversationId::from("c");

        // Row 1: due now.
        let _ = store
            .enqueue(&OutboxRow {
                next_attempt_at: Some(100),
                ..mk_row(&cid, 0)
            })
            .unwrap();
        // Row 2: due far in the future.
        let _ = store
            .enqueue(&OutboxRow {
                next_attempt_at: Some(10_000_000),
                ..mk_row(&cid, 1)
            })
            .unwrap();
        // Row 3: no schedule — always ready.
        let _ = store.enqueue(&mk_row(&cid, 2)).unwrap();

        let ready = store.ready_pending(500, 100).unwrap();
        assert_eq!(ready.len(), 2, "only rows 1 and 3 are due at ts=500");
        for r in &ready {
            assert!(r.next_attempt_at.is_none() || r.next_attempt_at.unwrap() <= 500);
        }
    }

    /// `mark_delivered` clears `last_error` and sets status=delivered.
    #[test]
    fn mark_delivered_clears_error() {
        let db = fresh_db();
        let store = OutboxStore::new(&db);
        let id = store
            .enqueue(&mk_row(&ConversationId::from("c"), 0))
            .unwrap();
        // First record a failure so last_error is non-null.
        let _ = store.record_failure(id, "transient", 5, 1).unwrap();
        // Claim is required in production but the SQL works without; drive
        // via mark_delivered directly.
        store.mark_delivered(id).unwrap();

        let (status, err): (String, Option<String>) = db
            .with_conn(|c| {
                let v = c
                    .query_row(
                        "SELECT status, last_error FROM state_outbox WHERE id = ?1",
                        params![id],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .unwrap();
                Ok(v)
            })
            .unwrap();
        assert_eq!(status, "delivered");
        assert!(err.is_none(), "mark_delivered must clear last_error");
    }

    /// OutboxStatus parse covers every variant — forward-compat guard.
    #[test]
    fn outbox_status_parse_roundtrips_all_variants() {
        for v in [
            OutboxStatus::Pending,
            OutboxStatus::InFlight,
            OutboxStatus::Delivered,
            OutboxStatus::Failed,
            OutboxStatus::DeadLetter,
            OutboxStatus::Unknown,
        ] {
            assert_eq!(OutboxStatus::parse(v.as_str()), Some(v));
        }
        assert_eq!(OutboxStatus::parse("bogus"), None);
    }
}
