//! Deterministic archive index for messages received through transports.
//!
//! This is intentionally separate from inference, skills, and semantic
//! memory. Archive writes preserve the original message and never call an
//! LLM or rewrite its contents.

use crate::db::{Database, DbError};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveConversation {
    pub archive_id: String,
    pub channel: String,
    pub remote_id: String,
    pub conversation_kind: String,
    pub display_name: Option<String>,
    pub conversation_id: Option<String>,
    pub first_seen_at: i64,
    pub last_seen_at: i64,
}

#[derive(Debug, Clone)]
pub struct ArchiveMessage<'a> {
    pub archive_message_id: &'a str,
    pub archive_id: &'a str,
    pub source_event_seq: Option<i64>,
    pub source_event_kind: &'a str,
    pub direction: &'a str,
    pub sender_id: Option<&'a str>,
    pub sender_name: Option<&'a str>,
    pub body: &'a str,
    pub occurred_at: i64,
    pub source_message_id: Option<&'a str>,
    pub created_at: i64,
    pub delivery_status: &'a str,
    pub reply_to_message_id: Option<&'a str>,
}

#[derive(Debug, Clone)]
pub struct StoredArchiveMessage {
    pub archive_message_id: String,
    pub sender_id: Option<String>,
    pub sender_name: Option<String>,
    pub body: String,
    pub occurred_at: i64,
    pub direction: String,
    pub delivery_status: String,
    pub reply_to_message_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveSearchHit {
    pub archive_message_id: String,
    pub archive_id: String,
    pub body: String,
    pub sender_name: Option<String>,
}

pub struct MessageArchiveStore<'db> {
    db: &'db Database,
}

impl<'db> MessageArchiveStore<'db> {
    pub fn new(db: &'db Database) -> Self {
        Self { db }
    }

    pub fn upsert_conversation(
        &self,
        archive_id: &str,
        channel: &str,
        remote_id: &str,
        conversation_kind: &str,
        display_name: Option<&str>,
        conversation_id: Option<&str>,
        now: i64,
    ) -> Result<(), DbError> {
        self.db.with_conn(|c| {
            c.execute(
                "INSERT INTO message_archive_conversations(
                    archive_id, channel, remote_id, conversation_kind,
                    display_name, conversation_id, first_seen_at, last_seen_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)
                 ON CONFLICT(channel, remote_id) DO UPDATE SET
                    display_name = COALESCE(excluded.display_name, message_archive_conversations.display_name),
                    conversation_id = COALESCE(excluded.conversation_id, message_archive_conversations.conversation_id),
                    last_seen_at = excluded.last_seen_at",
                params![
                    archive_id,
                    channel,
                    remote_id,
                    conversation_kind,
                    display_name,
                    conversation_id,
                    now,
                ],
            )?;
            Ok(())
        })
    }

    pub fn append_message(&self, message: &ArchiveMessage<'_>) -> Result<bool, DbError> {
        self.db.with_conn(|c| {
            let inserted = c.execute(
                "INSERT OR IGNORE INTO message_archive_messages(
                    archive_message_id, archive_id, source_event_seq, source_event_kind,
                    direction, sender_id, sender_name, body, occurred_at,
                          source_message_id, created_at, delivery_status, reply_to_message_id
                      ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                params![
                    message.archive_message_id,
                    message.archive_id,
                    message.source_event_seq,
                    message.source_event_kind,
                    message.direction,
                    message.sender_id,
                    message.sender_name,
                    message.body,
                    message.occurred_at,
                    message.source_message_id,
                    message.created_at,
                    message.delivery_status,
                    message.reply_to_message_id,
                ],
            )?;
            Ok(inserted == 1)
        })
    }

    pub fn update_delivery_status(
        &self,
        archive_message_id: &str,
        delivery_status: &str,
    ) -> Result<(), DbError> {
        self.db.with_conn(|c| {
            c.execute(
                "UPDATE message_archive_messages
                 SET delivery_status = ?1
                 WHERE archive_message_id = ?2",
                params![delivery_status, archive_message_id],
            )?;
            Ok(())
        })
    }

    pub fn upsert_participant(
        &self,
        archive_id: &str,
        participant_id: &str,
        display_name: Option<&str>,
        now: i64,
    ) -> Result<(), DbError> {
        self.db.with_conn(|c| {
            c.execute(
                "INSERT INTO message_archive_participants(archive_id, participant_id, display_name, first_seen_at, last_seen_at)
                 VALUES (?1, ?2, ?3, ?4, ?4)
                 ON CONFLICT(archive_id, participant_id) DO UPDATE SET
                    display_name = COALESCE(excluded.display_name, message_archive_participants.display_name),
                    last_seen_at = excluded.last_seen_at",
                params![archive_id, participant_id, display_name, now],
            )?;
            Ok(())
        })
    }

    pub fn list_messages_for_month(
        &self,
        archive_id: &str,
        year: i32,
        month: u32,
    ) -> Result<Vec<StoredArchiveMessage>, DbError> {
        let start = chrono::NaiveDate::from_ymd_opt(year, month, 1)
            .ok_or_else(|| DbError::Invariant("invalid archive month".to_owned()))?
            .and_hms_opt(0, 0, 0)
            .ok_or_else(|| DbError::Invariant("invalid archive month time".to_owned()))?
            .and_utc()
            .timestamp();
        let end = if month == 12 {
            chrono::NaiveDate::from_ymd_opt(year + 1, 1, 1)
        } else {
            chrono::NaiveDate::from_ymd_opt(year, month + 1, 1)
        }
        .ok_or_else(|| DbError::Invariant("invalid archive month".to_owned()))?
        .and_hms_opt(0, 0, 0)
        .ok_or_else(|| DbError::Invariant("invalid archive month time".to_owned()))?
        .and_utc()
        .timestamp();
        self.db.with_conn(|c| {
            let mut stmt = c.prepare(
                "SELECT archive_message_id, sender_id, sender_name, body, occurred_at,
                        direction, delivery_status, reply_to_message_id
                 FROM message_archive_messages
                 WHERE archive_id = ?1 AND occurred_at >= ?2 AND occurred_at < ?3
                 ORDER BY occurred_at, created_at, archive_message_id",
            )?;
            Ok(stmt
                .query_map(params![archive_id, start, end], |row| {
                    Ok(StoredArchiveMessage {
                        archive_message_id: row.get(0)?,
                        sender_id: row.get(1)?,
                        sender_name: row.get(2)?,
                        body: row.get(3)?,
                        occurred_at: row.get(4)?,
                        direction: row.get(5)?,
                        delivery_status: row.get(6)?,
                        reply_to_message_id: row.get(7)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?)
        })
    }

    pub fn get_conversation(
        &self,
        archive_id: &str,
    ) -> Result<Option<ArchiveConversation>, DbError> {
        self.db.with_conn(|c| {
            Ok(c.query_row(
                "SELECT archive_id, channel, remote_id, conversation_kind, display_name,
                        conversation_id, first_seen_at, last_seen_at
                 FROM message_archive_conversations WHERE archive_id = ?1",
                params![archive_id],
                |row| {
                    Ok(ArchiveConversation {
                        archive_id: row.get(0)?,
                        channel: row.get(1)?,
                        remote_id: row.get(2)?,
                        conversation_kind: row.get(3)?,
                        display_name: row.get(4)?,
                        conversation_id: row.get(5)?,
                        first_seen_at: row.get(6)?,
                        last_seen_at: row.get(7)?,
                    })
                },
            )
            .optional()?)
        })
    }

    pub fn search(&self, query: &str, limit: u32) -> Result<Vec<ArchiveSearchHit>, DbError> {
        self.db.with_conn(|c| {
            let mut stmt = c.prepare(
                "SELECT archive_message_id, archive_id, body, sender_name
                 FROM message_archive_search
                 WHERE message_archive_search MATCH ?1
                 ORDER BY rank LIMIT ?2",
            )?;
            Ok(stmt
                .query_map(params![query, limit.min(100) as i64], |row| {
                    Ok(ArchiveSearchHit {
                        archive_message_id: row.get(0)?,
                        archive_id: row.get(1)?,
                        body: row.get(2)?,
                        sender_name: row.get(3)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Database, DbConfig};
    use crate::migrations::MigrationRunner;

    #[test]
    fn archive_message_insert_is_idempotent() {
        let config = DbConfig::in_memory_unencrypted();
        let db = Database::open(&config).unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        let store = MessageArchiveStore::new(&db);
        store
            .upsert_conversation(
                "wa-group-1",
                "whatsapp",
                "group-1",
                "group",
                Some("Family"),
                None,
                10,
            )
            .unwrap();
        let message = ArchiveMessage {
            archive_message_id: "msg-1",
            archive_id: "wa-group-1",
            source_event_seq: Some(1),
            source_event_kind: "user_msg",
            direction: "inbound",
            sender_id: Some("alice"),
            sender_name: Some("Alice"),
            body: "hello",
            occurred_at: 10,
            source_message_id: Some("remote-1"),
            created_at: 10,
            delivery_status: "delivered",
            reply_to_message_id: None,
        };
        assert!(store.append_message(&message).unwrap());
        assert!(!store.append_message(&message).unwrap());
        assert_eq!(
            store
                .get_conversation("wa-group-1")
                .unwrap()
                .unwrap()
                .channel,
            "whatsapp"
        );
    }
}
