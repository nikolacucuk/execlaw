//! Durable ownership for a transport recipient inside a conversation.

use crate::db::{Database, DbError};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Current owner of one conversation and transport-recipient scope.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentOwnership {
    pub conversation_id: String,
    pub owner_kind: String,
    pub agent_id: Option<String>,
    pub previous_agent_id: Option<String>,
    pub generation: u64,
    pub updated_at: i64,
}

/// SQLite-backed ownership and Controller takeover state.
pub struct AgentOwnershipStore<'db> {
    db: &'db Database,
}

impl<'db> AgentOwnershipStore<'db> {
    pub fn new(db: &'db Database) -> Self {
        Self { db }
    }

    /// Read the owner of one channel recipient in a shared chat.
    pub fn get(
        &self,
        conversation_id: &str,
        channel: &str,
        recipient: &str,
    ) -> Result<Option<AgentOwnership>, DbError> {
        let key = scope_key(conversation_id, channel, recipient);
        self.db.with_conn(|conn| Ok(conn.query_row("SELECT conversation_id,owner_kind,agent_id,previous_agent_id,generation,updated_at FROM state_agent_ownership WHERE scope_key=?1", [key], |row| Ok(AgentOwnership {
            conversation_id: row.get(0)?, owner_kind: row.get(1)?, agent_id: row.get(2)?, previous_agent_id: row.get(3)?, generation: row.get(4)?, updated_at: row.get(5)?,
        })).optional()?))
    }

    /// Select the first matching lead specialist until the Controller intervenes.
    pub fn assign_agent(
        &self,
        conversation_id: &str,
        channel: &str,
        recipient: &str,
        agent_id: &str,
        now: i64,
    ) -> Result<AgentOwnership, DbError> {
        let key = scope_key(conversation_id, channel, recipient);
        self.db.with_conn(|conn| {
            conn.execute("INSERT INTO state_agent_ownership(scope_key,conversation_id,owner_kind,agent_id,generation,updated_at) VALUES (?1,?2,'agent',?3,1,?4) ON CONFLICT(scope_key) DO NOTHING", params![key,conversation_id,agent_id,now])?;
            Ok(())
        })?;
        self.get(conversation_id, channel, recipient)?
            .ok_or_else(|| DbError::Invariant("owner disappeared".into()))
    }

    /// Transfer control to the Controller, fencing earlier automatic sends.
    pub fn takeover(
        &self,
        conversation_id: &str,
        channel: &str,
        recipient: &str,
        now: i64,
    ) -> Result<AgentOwnership, DbError> {
        let key = scope_key(conversation_id, channel, recipient);
        self.db.transaction(|tx| {
            let next_generation: i64 = tx.query_row(
                "SELECT COALESCE(MAX(generation),0)+1 FROM state_agent_ownership WHERE scope_key=?1", [&key], |row| row.get(0)
            )?;
            tx.execute("INSERT INTO state_agent_ownership(scope_key,conversation_id,owner_kind,agent_id,previous_agent_id,generation,updated_at) VALUES (?1,?2,'controller',NULL,NULL,?3,?4) ON CONFLICT(scope_key) DO UPDATE SET owner_kind='controller',previous_agent_id=COALESCE(state_agent_ownership.agent_id,state_agent_ownership.previous_agent_id),agent_id=NULL,generation=excluded.generation,updated_at=excluded.updated_at", params![key,conversation_id,next_generation,now])?;
            tx.execute("INSERT INTO state_agent_handoff_events(scope_key,generation,event_kind,event_id,conversation_seq,payload_json,occurred_at) VALUES (?1,?2,'takeover',?3,NULL,'{}',?4)", params![key,next_generation,format!("takeover:{next_generation}"),now])?;
            let mut statement = tx.prepare("SELECT id,status,attempts FROM state_outbox WHERE ownership_scope_key=?1 AND ownership_generation<?2 AND status IN ('pending','in_flight')")?;
            let rows = statement.query_map(params![key,next_generation], |row| Ok((row.get::<_,i64>(0)?, row.get::<_,String>(1)?, row.get::<_,i64>(2)?)))?.collect::<Result<Vec<_>,_>>()?;
            drop(statement);
            for (outbox_id, status, attempts) in rows {
                let (next_status, transition) = if status == "pending" { ("failed", "cancelled_by_takeover") } else { ("unknown", "takeover_outcome_unknown") };
                tx.execute("UPDATE state_outbox SET status=?1,last_error='operator takeover requires delivery reconciliation',lease_owner=NULL,lease_expires_at=NULL WHERE id=?2", params![next_status,outbox_id])?;
                tx.execute("INSERT INTO state_outbox_delivery_events(outbox_id,transition,occurred_at,attempt,detail) VALUES (?1,?2,?3,?4,'ownership_generation_changed')", params![outbox_id,transition,now,attempts])?;
            }
            Ok(())
        })?;
        self.get(conversation_id, channel, recipient)?
            .ok_or_else(|| DbError::Invariant("takeover disappeared".into()))
    }

    /// Hand back ownership only when the Controller's observed generation matches.
    pub fn handback(
        &self,
        conversation_id: &str,
        channel: &str,
        recipient: &str,
        generation: u64,
    ) -> Result<(), DbError> {
        self.handback_with_context(conversation_id, channel, recipient, generation)
            .map(|_| ())
    }

    /// Hand back control and return the previous specialist plus durable
    /// operator-owned events it must read before continuing.
    pub fn handback_with_context(
        &self,
        conversation_id: &str,
        channel: &str,
        recipient: &str,
        generation: u64,
    ) -> Result<(Option<String>, Vec<serde_json::Value>), DbError> {
        let key = scope_key(conversation_id, channel, recipient);
        self.db.transaction(|tx| {
            let previous_agent: Option<String> = tx.query_row(
                "SELECT previous_agent_id FROM state_agent_ownership WHERE scope_key=?1 AND owner_kind='controller' AND generation=?2",
                params![key,generation], |row| row.get(0),
            ).optional()?.flatten();
            let mut statement = tx.prepare("SELECT payload_json FROM state_agent_handoff_events WHERE scope_key=?1 AND generation=?2 AND event_kind='operator_action' ORDER BY conversation_seq,event_id")?;
            let actions = statement.query_map(params![key,generation], |row| {
                let payload: String = row.get(0)?;
                Ok(serde_json::from_str(&payload).unwrap_or(serde_json::Value::Null))
            })?.collect::<Result<Vec<_>, _>>()?;
            drop(statement);
            let changed = if let Some(agent_id) = previous_agent.as_deref() {
                tx.execute("UPDATE state_agent_ownership SET owner_kind='agent',agent_id=?1,previous_agent_id=NULL,generation=generation+1,updated_at=unixepoch() WHERE scope_key=?2 AND owner_kind='controller' AND generation=?3", params![agent_id,key,generation])?
            } else {
                tx.execute("DELETE FROM state_agent_ownership WHERE scope_key=?1 AND owner_kind='controller' AND generation=?2", params![key,generation])?
            };
            if changed != 1 { return Err(DbError::Invariant("ownership changed before handback".into())); }
            tx.execute("INSERT INTO state_agent_handoff_events(scope_key,generation,event_kind,event_id,conversation_seq,payload_json,occurred_at) VALUES (?1,?2,'handback',?3,NULL,?4,unixepoch())", params![key,generation,format!("handback:{generation}"),serde_json::to_string(&actions).map_err(|error| DbError::Invariant(error.to_string()))?])?;
            Ok((previous_agent, actions))
        })
    }

    /// Record an operator-era event without exposing it to autonomous work
    /// until hand-back. Repeated source IDs are ignored.
    pub fn record_operator_action(
        &self,
        conversation_id: &str,
        channel: &str,
        recipient: &str,
        event_id: &str,
        conversation_seq: i64,
        payload: &serde_json::Value,
        now: i64,
    ) -> Result<bool, DbError> {
        let key = scope_key(conversation_id, channel, recipient);
        let encoded = serde_json::to_string(payload)
            .map_err(|error| DbError::Invariant(error.to_string()))?;
        self.db.with_conn(|connection| Ok(connection.execute(
            "INSERT OR IGNORE INTO state_agent_handoff_events(scope_key,generation,event_kind,event_id,conversation_seq,payload_json,occurred_at)
             SELECT scope_key,generation,'operator_action',?1,?2,?3,?4 FROM state_agent_ownership
             WHERE scope_key=?5 AND owner_kind='controller'",
            params![event_id, conversation_seq, encoded, now, key],
        )? == 1))
    }
}

/// Stable SQLite key for one conversation and transport recipient.
pub fn scope_key(conversation_id: &str, channel: &str, recipient: &str) -> String {
    let digest =
        Sha256::digest(format!("{conversation_id}\u{1f}{channel}\u{1f}{recipient}").as_bytes());
    hex::encode(digest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DbConfig, MigrationRunner};

    #[test]
    fn takeover_is_scoped_to_recipient_and_fences_stale_handback() {
        let db = Database::open(&DbConfig::in_memory_unencrypted()).unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        let store = AgentOwnershipStore::new(&db);
        store
            .assign_agent("shared", "whatsapp", "group-a", "camper", 10)
            .unwrap();
        let first = store.takeover("shared", "whatsapp", "group-a", 11).unwrap();
        assert_eq!(first.owner_kind, "controller");
        assert!(
            store
                .get("shared", "whatsapp", "group-b")
                .unwrap()
                .is_none()
        );
        let second = store.takeover("shared", "whatsapp", "group-a", 12).unwrap();
        assert!(
            store
                .handback("shared", "whatsapp", "group-a", first.generation)
                .is_err()
        );
        store
            .handback("shared", "whatsapp", "group-a", second.generation)
            .unwrap();
        let restored = store.get("shared", "whatsapp", "group-a").unwrap().unwrap();
        assert_eq!(restored.owner_kind, "agent");
        assert_eq!(restored.agent_id.as_deref(), Some("camper"));
    }

    #[test]
    fn handback_returns_durable_intervening_actions_and_restores_agent() {
        let db = Database::open(&DbConfig::in_memory_unencrypted()).unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        let store = AgentOwnershipStore::new(&db);
        store
            .assign_agent("c", "signal", "peer", "watcher", 1)
            .unwrap();
        let owner = store.takeover("c", "signal", "peer", 2).unwrap();
        assert!(
            store
                .record_operator_action(
                    "c",
                    "signal",
                    "peer",
                    "event-1",
                    3,
                    &serde_json::json!({"text":"Already sent"}),
                    3
                )
                .unwrap()
        );
        assert!(
            !store
                .record_operator_action(
                    "c",
                    "signal",
                    "peer",
                    "event-1",
                    3,
                    &serde_json::json!({"text":"duplicate"}),
                    4
                )
                .unwrap()
        );
        let (agent, actions) = store
            .handback_with_context("c", "signal", "peer", owner.generation)
            .unwrap();
        assert_eq!(agent.as_deref(), Some("watcher"));
        assert_eq!(actions, vec![serde_json::json!({"text":"Already sent"})]);
        assert!(
            store
                .handback_with_context("c", "signal", "peer", owner.generation)
                .is_err()
        );
    }
}
