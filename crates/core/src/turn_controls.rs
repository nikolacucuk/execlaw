//! Durable intent and acknowledgements for chat steering and stop controls.

use crate::db::{Database, DbError};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnControlKind {
    QueueNextTurn,
    Steer,
    Pause,
    Resume,
    Cancel,
}

impl TurnControlKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::QueueNextTurn => "queue_next_turn",
            Self::Steer => "steer",
            Self::Pause => "pause",
            Self::Resume => "resume",
            Self::Cancel => "cancel",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "queue_next_turn" => Some(Self::QueueNextTurn),
            "steer" => Some(Self::Steer),
            "pause" => Some(Self::Pause),
            "resume" => Some(Self::Resume),
            "cancel" => Some(Self::Cancel),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnControlStatus {
    Accepted,
    Delivered,
    Applied,
    Acknowledged,
    Cancelled,
    Failed,
}

impl TurnControlStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Delivered => "delivered",
            Self::Applied => "applied",
            Self::Acknowledged => "acknowledged",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnControlRecord {
    pub control_id: String,
    pub conversation_id: String,
    pub turn_id: Option<String>,
    pub kind: TurnControlKind,
    pub payload: Value,
    pub status: TurnControlStatus,
    pub acknowledgement: Option<Value>,
    pub created_at: i64,
    pub updated_at: i64,
    pub acknowledged_at: Option<i64>,
}

#[derive(Debug, Error)]
pub enum TurnControlError {
    #[error(transparent)]
    Db(#[from] DbError),
    #[error("turn-control idempotency key was reused with a different intent")]
    IdempotencyConflict,
    #[error("invalid turn-control transition: {0}")]
    Transition(String),
}

/// SQLite store for accepted user intent and its durable application state.
pub struct TurnControlStore<'db> {
    db: &'db Database,
}

impl<'db> TurnControlStore<'db> {
    pub fn new(db: &'db Database) -> Self {
        Self { db }
    }

    /// Persist one bounded control request and its initial accepted transition.
    pub fn enqueue(
        &self,
        conversation_id: &str,
        turn_id: Option<&str>,
        kind: TurnControlKind,
        payload: &Value,
        now: i64,
    ) -> Result<TurnControlRecord, TurnControlError> {
        let payload_json = serde_json::to_string(payload)
            .map_err(|error| TurnControlError::Transition(format!("encode payload: {error}")))?;
        if conversation_id.is_empty()
            || conversation_id.len() > 128
            || payload_json.len() > 64 * 1024
        {
            return Err(TurnControlError::Transition(
                "conversation or payload exceeds its bounds".into(),
            ));
        }
        if turn_id.is_some_and(|value| value.is_empty() || value.len() > 128) {
            return Err(TurnControlError::Transition(
                "turn id is empty or oversized".into(),
            ));
        }
        let control_id = uuid::Uuid::new_v4().to_string();
        self.db.transaction(|tx| {
            tx.execute("INSERT INTO state_turn_controls(control_id,conversation_id,turn_id,kind,payload_json,status,created_at,updated_at) VALUES (?1,?2,?3,?4,?5,'accepted',?6,?6)", params![control_id,conversation_id,turn_id,kind.as_str(),payload_json,now])?;
            tx.execute("INSERT INTO state_turn_control_transitions(control_id,ordinal,status,detail_json,occurred_at) VALUES (?1,0,'accepted',NULL,?2)", params![control_id,now])?;
            Ok(())
        })?;
        self.get(&control_id)?.ok_or_else(|| {
            TurnControlError::Transition("control disappeared after insertion".into())
        })
    }

    /// Enqueue a control once per conversation-scoped client request key.
    /// Matching retries return the saved control without duplicating delivery;
    /// reusing a key for another intent conflicts.
    pub fn enqueue_idempotent(
        &self,
        conversation_id: &str,
        turn_id: Option<&str>,
        kind: TurnControlKind,
        payload: &Value,
        client_request_id: &str,
        now: i64,
    ) -> Result<(TurnControlRecord, bool), TurnControlError> {
        let payload_json = serde_json::to_string(payload)
            .map_err(|error| TurnControlError::Transition(format!("encode payload: {error}")))?;
        if conversation_id.is_empty()
            || conversation_id.len() > 128
            || payload_json.len() > 64 * 1024
            || client_request_id.is_empty()
            || client_request_id.len() > 128
            || !client_request_id
                .bytes()
                .all(|byte| (0x21..=0x7e).contains(&byte))
        {
            return Err(TurnControlError::Transition(
                "conversation, payload, or idempotency key exceeds its bounds".into(),
            ));
        }
        if turn_id.is_some_and(|value| value.is_empty() || value.len() > 128) {
            return Err(TurnControlError::Transition(
                "turn id is empty or oversized".into(),
            ));
        }
        let request_hash = hex::encode(Sha256::digest(format!(
            "{}\0{}",
            kind.as_str(),
            payload_json
        )));
        let proposed_id = uuid::Uuid::new_v4().to_string();
        let inserted = self.db.transaction(|tx| {
            let existing: Option<(String, Option<String>)> = tx
                .query_row(
                    "SELECT control_id, request_body_sha256 FROM state_turn_controls \
                     WHERE conversation_id = ?1 AND client_request_id = ?2",
                    params![conversation_id, client_request_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            if let Some((control_id, stored_hash)) = existing {
                if stored_hash.as_deref() != Some(request_hash.as_str()) {
                    return Err(DbError::Invariant(
                        "turn-control idempotency key was reused with a different intent".into(),
                    ));
                }
                return Ok((control_id, false));
            }
            tx.execute(
                "INSERT INTO state_turn_controls(control_id,conversation_id,turn_id,kind,payload_json,\
                 status,created_at,updated_at,client_request_id,request_body_sha256) \
                 VALUES (?1,?2,?3,?4,?5,'accepted',?6,?6,?7,?8)",
                params![
                    proposed_id,
                    conversation_id,
                    turn_id,
                    kind.as_str(),
                    payload_json,
                    now,
                    client_request_id,
                    request_hash,
                ],
            )?;
            tx.execute(
                "INSERT INTO state_turn_control_transitions(control_id,ordinal,status,detail_json,occurred_at) \
                 VALUES (?1,0,'accepted',NULL,?2)",
                params![proposed_id, now],
            )?;
            Ok((proposed_id, true))
        });
        let (control_id, is_new) = match inserted {
            Ok(result) => result,
            Err(DbError::Invariant(message))
                if message == "turn-control idempotency key was reused with a different intent" =>
            {
                return Err(TurnControlError::IdempotencyConflict);
            }
            Err(error) => return Err(TurnControlError::Db(error)),
        };
        self.get(&control_id)?
            .map(|control| (control, is_new))
            .ok_or_else(|| {
                TurnControlError::Transition("control disappeared after insertion".into())
            })
    }

    /// Load a control request by its opaque identifier.
    pub fn get(&self, control_id: &str) -> Result<Option<TurnControlRecord>, TurnControlError> {
        self.db.with_conn(|connection| {
            connection.query_row("SELECT control_id,conversation_id,turn_id,kind,payload_json,status,acknowledgement_json,created_at,updated_at,acknowledged_at FROM state_turn_controls WHERE control_id=?1", [control_id], map_record).optional().map_err(DbError::from)
        }).map_err(TurnControlError::from)
    }

    /// Return queued messages in order. They remain durable until explicitly
    /// marked applied by the turn that consumes them.
    pub fn queued_messages(
        &self,
        conversation_id: &str,
    ) -> Result<Vec<TurnControlRecord>, TurnControlError> {
        self.db.with_conn(|connection| {
            let mut statement = connection.prepare_cached("SELECT control_id,conversation_id,turn_id,kind,payload_json,status,acknowledgement_json,created_at,updated_at,acknowledged_at FROM state_turn_controls WHERE conversation_id=?1 AND kind='queue_next_turn' AND status IN ('accepted','delivered') ORDER BY created_at,control_id")?;
            statement.query_map([conversation_id], map_record)?.collect::<Result<Vec<_>,_>>().map_err(DbError::from)
        }).map_err(TurnControlError::from)
    }

    /// Advance a control only along the accepted lifecycle. Every transition
    /// is retained so a reconnect can distinguish accepted, applied, and acked.
    pub fn transition(
        &self,
        control_id: &str,
        next: TurnControlStatus,
        acknowledgement: Option<&Value>,
        now: i64,
    ) -> Result<TurnControlRecord, TurnControlError> {
        self.db.transaction(|tx| {
            Self::transition_in_transaction(tx, control_id, next, acknowledgement, now)
        })?;
        self.get(control_id)?.ok_or_else(|| {
            TurnControlError::Transition("control disappeared after transition".into())
        })
    }

    /// Return unapplied runner controls in creation order for restart replay.
    pub fn reconnectable(
        &self,
        conversation_id: &str,
        limit: usize,
    ) -> Result<Vec<TurnControlRecord>, TurnControlError> {
        self.db.with_conn(|connection| {
            let mut statement = connection.prepare_cached("SELECT control_id,conversation_id,turn_id,kind,payload_json,status,acknowledgement_json,created_at,updated_at,acknowledged_at FROM state_turn_controls WHERE conversation_id=?1 AND kind IN ('steer','pause','resume') AND status IN ('accepted','delivered','applied') ORDER BY created_at,control_id LIMIT ?2")?;
            statement.query_map(params![conversation_id,limit.clamp(1,500)], map_record)?.collect::<Result<Vec<_>,_>>().map_err(DbError::from)
        }).map_err(TurnControlError::from)
    }

    /// Persist one lifecycle transition inside a transaction shared with another
    /// durable event, such as applying a queued message to a committed chat event.
    pub fn transition_in_transaction(
        tx: &rusqlite::Transaction<'_>,
        control_id: &str,
        next: TurnControlStatus,
        acknowledgement: Option<&Value>,
        now: i64,
    ) -> Result<(), DbError> {
        let ack_json = acknowledgement
            .map(serde_json::to_string)
            .transpose()
            .map_err(|error| DbError::Serde(format!("encode acknowledgement: {error}")))?;
        if ack_json
            .as_ref()
            .is_some_and(|value| value.len() > 16 * 1024)
        {
            return Err(DbError::Invariant(
                "turn-control acknowledgement exceeds 16 KiB".into(),
            ));
        }
        let current: Option<String> = tx
            .query_row(
                "SELECT status FROM state_turn_controls WHERE control_id=?1",
                [control_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(current) = current else {
            return Err(DbError::Invariant("turn control was not found".into()));
        };
        if current == next.as_str() {
            return Ok(());
        }
        if matches!(current.as_str(), "acknowledged" | "cancelled" | "failed") {
            return Err(DbError::Invariant(format!(
                "terminal control state '{current}' cannot transition"
            )));
        }
        let allowed = match (current.as_str(), next) {
            (
                "accepted",
                TurnControlStatus::Delivered
                | TurnControlStatus::Applied
                | TurnControlStatus::Acknowledged
                | TurnControlStatus::Cancelled
                | TurnControlStatus::Failed,
            ) => true,
            (
                "delivered",
                TurnControlStatus::Applied
                | TurnControlStatus::Acknowledged
                | TurnControlStatus::Cancelled
                | TurnControlStatus::Failed,
            ) => true,
            (
                "applied",
                TurnControlStatus::Acknowledged
                | TurnControlStatus::Cancelled
                | TurnControlStatus::Failed,
            ) => true,
            _ => false,
        };
        if !allowed {
            return Err(DbError::Invariant(format!(
                "invalid control transition {current} -> {}",
                next.as_str()
            )));
        }
        let ordinal: i64 = tx.query_row(
            "SELECT COALESCE(MAX(ordinal),-1)+1 FROM state_turn_control_transitions WHERE control_id=?1",
            [control_id],
            |row| row.get(0),
        )?;
        tx.execute(
            "UPDATE state_turn_controls SET status=?2,acknowledgement_json=COALESCE(?3,acknowledgement_json),updated_at=?4,acknowledged_at=CASE WHEN ?2='acknowledged' THEN ?4 ELSE acknowledged_at END WHERE control_id=?1",
            params![control_id,next.as_str(),ack_json,now],
        )?;
        tx.execute(
            "INSERT INTO state_turn_control_transitions(control_id,ordinal,status,detail_json,occurred_at) VALUES (?1,?2,?3,?4,?5)",
            params![control_id,ordinal,next.as_str(),ack_json,now],
        )?;
        Ok(())
    }

    /// Atomically claim the only dispatch attempt for an accepted control.
    /// Returning false means another request already delivered or terminalized it.
    pub fn claim_delivery(&self, control_id: &str, now: i64) -> Result<bool, TurnControlError> {
        self.db
            .transaction(|tx| {
                let changed = tx.execute(
                    "UPDATE state_turn_controls SET status='delivered', updated_at=?2 \
                     WHERE control_id=?1 AND status='accepted'",
                    params![control_id, now],
                )?;
                if changed == 0 {
                    return Ok(false);
                }
                let ordinal: i64 = tx.query_row(
                    "SELECT COALESCE(MAX(ordinal),-1)+1 FROM state_turn_control_transitions WHERE control_id=?1",
                    [control_id],
                    |row| row.get(0),
                )?;
                tx.execute(
                    "INSERT INTO state_turn_control_transitions(control_id,ordinal,status,detail_json,occurred_at) \
                     VALUES (?1,?2,'delivered','{\"dispatch_claimed\":true}',?3)",
                    params![control_id, ordinal, now],
                )?;
                Ok(true)
            })
            .map_err(TurnControlError::from)
    }

    /// List queue-next-turn controls that still need a committed message event.
    pub fn pending_queue_controls(
        &self,
        limit: usize,
    ) -> Result<Vec<TurnControlRecord>, TurnControlError> {
        self.db
            .with_conn(|connection| {
                let mut statement = connection.prepare_cached(
                    "SELECT control_id,conversation_id,turn_id,kind,payload_json,status,\
                     acknowledgement_json,created_at,updated_at,acknowledged_at \
                     FROM state_turn_controls WHERE kind='queue_next_turn' \
                     AND status IN ('accepted','delivered') ORDER BY created_at,control_id LIMIT ?1",
                )?;
                statement
                    .query_map([limit.clamp(1, 500)], map_record)?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(DbError::from)
            })
            .map_err(TurnControlError::from)
    }

    /// Apply a queued message only once, in the transaction that commits its
    /// corresponding user event.
    pub fn apply_queued_message_in_transaction(
        tx: &rusqlite::Transaction<'_>,
        control_id: &str,
        conversation_id: &str,
        event_seq: i64,
        now: i64,
    ) -> Result<(), DbError> {
        let state: Option<(String, String, String)> = tx
            .query_row(
                "SELECT conversation_id, kind, status FROM state_turn_controls WHERE control_id=?1",
                [control_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let Some((stored_conversation, kind, status)) = state else {
            return Err(DbError::Invariant(
                "queued turn control was not found".into(),
            ));
        };
        if stored_conversation != conversation_id || kind != TurnControlKind::QueueNextTurn.as_str()
        {
            return Err(DbError::Invariant(
                "queued turn control scope mismatch".into(),
            ));
        }
        if !matches!(status.as_str(), "accepted" | "delivered") {
            return Err(DbError::Invariant(format!(
                "queued turn control is already {status}"
            )));
        }
        Self::transition_in_transaction(
            tx,
            control_id,
            TurnControlStatus::Applied,
            Some(&serde_json::json!({"event_seq":event_seq})),
            now,
        )
    }

    /// List a bounded status window for reconnectable chat clients.
    pub fn list(
        &self,
        conversation_id: &str,
        after_created_at: i64,
        limit: usize,
    ) -> Result<Vec<TurnControlRecord>, TurnControlError> {
        self.db.with_conn(|connection| {
            let mut statement = connection.prepare_cached("SELECT control_id,conversation_id,turn_id,kind,payload_json,status,acknowledgement_json,created_at,updated_at,acknowledged_at FROM state_turn_controls WHERE conversation_id=?1 AND created_at>=?2 ORDER BY created_at,control_id LIMIT ?3")?;
            statement.query_map(params![conversation_id,after_created_at,limit.clamp(1,200)], map_record)?.collect::<Result<Vec<_>,_>>().map_err(DbError::from)
        }).map_err(TurnControlError::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{db::DbConfig, migrations::MigrationRunner};

    fn seeded_db(path: &std::path::Path) -> Database {
        let db = Database::open(&DbConfig {
            path: path.to_path_buf(),
            key: None,
        })
        .unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        db.with_conn(|connection| {
            connection.execute(
                "INSERT INTO state_conversations (conversation_id,kind,phase,trust_class,modality) VALUES ('control-test','ControllerDM','idle','Controller','Text')",
                [],
            )?;
            Ok(())
        })
        .unwrap();
        db
    }

    #[test]
    fn request_key_replay_is_durable_and_changed_intent_conflicts() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("turn-controls.db");
        let db = seeded_db(&path);
        let store = TurnControlStore::new(&db);
        let payload = serde_json::json!({"text":"check the latest diff"});
        let (first, created) = store
            .enqueue_idempotent(
                "control-test",
                Some("turn-1"),
                TurnControlKind::Steer,
                &payload,
                "request-1",
                10,
            )
            .unwrap();
        assert!(created);
        let (retry, created) = store
            .enqueue_idempotent(
                "control-test",
                Some("turn-1"),
                TurnControlKind::Steer,
                &payload,
                "request-1",
                11,
            )
            .unwrap();
        assert!(!created);
        assert_eq!(retry.control_id, first.control_id);
        assert!(matches!(
            store.enqueue_idempotent(
                "control-test",
                Some("turn-1"),
                TurnControlKind::Steer,
                &serde_json::json!({"text":"different correction"}),
                "request-1",
                12,
            ),
            Err(TurnControlError::IdempotencyConflict)
        ));

        drop(store);
        drop(db);
        let reopened = Database::open(&DbConfig { path, key: None }).unwrap();
        MigrationRunner::new(&reopened).apply_all().unwrap();
        let store = TurnControlStore::new(&reopened);
        let recovered = store.get(&first.control_id).unwrap().unwrap();
        assert_eq!(recovered.payload, payload);
        assert_eq!(recovered.status, TurnControlStatus::Accepted);
        assert_eq!(store.list("control-test", 0, 20).unwrap().len(), 1);
    }

    #[test]
    fn delivery_claim_is_single_use_and_acknowledgment_is_persisted() {
        let temp = tempfile::tempdir().unwrap();
        let db = seeded_db(&temp.path().join("turn-controls.db"));
        let store = TurnControlStore::new(&db);
        let control = store
            .enqueue(
                "control-test",
                Some("turn-1"),
                TurnControlKind::Cancel,
                &serde_json::json!({}),
                10,
            )
            .unwrap();
        assert!(store.claim_delivery(&control.control_id, 11).unwrap());
        assert!(!store.claim_delivery(&control.control_id, 12).unwrap());
        store
            .transition(&control.control_id, TurnControlStatus::Delivered, None, 13)
            .unwrap();
        let acknowledged = store
            .transition(
                &control.control_id,
                TurnControlStatus::Acknowledged,
                Some(&serde_json::json!({"runner":"cancelled"})),
                14,
            )
            .unwrap();
        assert_eq!(acknowledged.status, TurnControlStatus::Acknowledged);
        assert_eq!(
            acknowledged.acknowledgement,
            Some(serde_json::json!({"runner":"cancelled"}))
        );
        assert_eq!(acknowledged.acknowledged_at, Some(14));
    }
}

fn map_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<TurnControlRecord> {
    let kind = match row.get::<_, String>(3)?.as_str() {
        "queue_next_turn" => TurnControlKind::QueueNextTurn,
        "steer" => TurnControlKind::Steer,
        "pause" => TurnControlKind::Pause,
        "resume" => TurnControlKind::Resume,
        "cancel" => TurnControlKind::Cancel,
        _ => return Err(rusqlite::Error::InvalidQuery),
    };
    let status = match row.get::<_, String>(5)?.as_str() {
        "accepted" => TurnControlStatus::Accepted,
        "delivered" => TurnControlStatus::Delivered,
        "applied" => TurnControlStatus::Applied,
        "acknowledged" => TurnControlStatus::Acknowledged,
        "cancelled" => TurnControlStatus::Cancelled,
        "failed" => TurnControlStatus::Failed,
        _ => return Err(rusqlite::Error::InvalidQuery),
    };
    let payload: String = row.get(4)?;
    let acknowledgement: Option<String> = row.get(6)?;
    Ok(TurnControlRecord {
        control_id: row.get(0)?,
        conversation_id: row.get(1)?,
        turn_id: row.get(2)?,
        kind,
        payload: serde_json::from_str(&payload).map_err(|_| rusqlite::Error::InvalidQuery)?,
        status,
        acknowledgement: acknowledgement
            .map(|value| serde_json::from_str(&value).unwrap_or(Value::Null)),
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
        acknowledged_at: row.get(9)?,
    })
}
