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
        self.db.with_conn(|conn| Ok(conn.query_row("SELECT conversation_id,owner_kind,agent_id,generation,updated_at FROM state_agent_ownership WHERE scope_key=?1", [key], |row| Ok(AgentOwnership {
            conversation_id: row.get(0)?, owner_kind: row.get(1)?, agent_id: row.get(2)?, generation: row.get(3)?, updated_at: row.get(4)?,
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
        self.db.with_conn(|conn| {
            conn.execute("INSERT INTO state_agent_ownership(scope_key,conversation_id,owner_kind,agent_id,generation,updated_at) VALUES (?1,?2,'controller',NULL,1,?3) ON CONFLICT(scope_key) DO UPDATE SET owner_kind='controller',agent_id=NULL,generation=generation+1,updated_at=excluded.updated_at", params![key,conversation_id,now])?;
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
        let key = scope_key(conversation_id, channel, recipient);
        self.db.with_conn(|conn| {
            let changed = conn.execute("DELETE FROM state_agent_ownership WHERE scope_key=?1 AND owner_kind='controller' AND generation=?2", params![key,generation])?;
            if changed != 1 { return Err(DbError::Invariant("ownership changed before handback".into())); }
            Ok(())
        })
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
        assert!(
            store
                .get("shared", "whatsapp", "group-a")
                .unwrap()
                .is_none()
        );
    }
}
