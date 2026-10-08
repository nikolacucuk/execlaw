//! Deterministic archive index for messages received through transports.
//!
//! This is intentionally separate from inference, skills, and semantic
//! memory. Archive writes preserve the original message and never call an
//! LLM or rewrite its contents.

use crate::db::{Database, DbError};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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
    pub topic_keywords: &'a str,
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
    pub is_deleted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveSearchHit {
    pub archive_message_id: String,
    pub archive_id: String,
    pub body: String,
    pub sender_name: Option<String>,
    pub topic_keywords: String,
}

/// A provider-independent message mutation from a transport adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveOperationKind {
    Create,
    Edit,
    Delete,
    ReactionAdd,
    ReactionRemove,
}

impl ArchiveOperationKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Edit => "edit",
            Self::Delete => "delete",
            Self::ReactionAdd => "reaction_add",
            Self::ReactionRemove => "reaction_remove",
        }
    }
}

/// Append-only source operation. `source_version` is the transport's
/// monotonic revision/sequence for the target message, not receive time.
#[derive(Debug, Clone)]
pub struct ArchiveOperation<'a> {
    pub event_id: &'a str,
    pub archive_id: &'a str,
    pub archive_message_id: &'a str,
    pub source_message_id: &'a str,
    pub kind: ArchiveOperationKind,
    pub source_version: i64,
    pub occurred_at: i64,
    pub body: Option<&'a str>,
    pub sender_id: Option<&'a str>,
    pub sender_name: Option<&'a str>,
    pub reply_to_message_id: Option<&'a str>,
    pub reaction: Option<&'a str>,
    pub actor_id: Option<&'a str>,
    pub recorded_at: i64,
}

/// Whether a transport operation was recorded and whether it changed the
/// current display projection. Audit history is retained in both cases.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArchiveOperationResult {
    pub recorded: bool,
    pub current_state_changed: bool,
}

/// Current normalized reaction attached to an archived message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveReaction {
    pub source_message_id: String,
    pub actor_id: String,
    pub reaction: String,
}

/// Extract a small deterministic topic vocabulary for FTS lookup.
/// The archive must remain useful when no inference backend is available.
pub fn extract_topic_keywords(body: &str, limit: usize) -> String {
    let stopwords = [
        "about", "after", "are", "been", "could", "from", "have", "into", "just", "that", "the",
        "their", "there", "this", "what", "when", "where", "with", "would", "your", "you", "and",
        "for", "not", "was", "were", "will",
    ];
    let mut counts = HashMap::<String, usize>::new();
    for raw in body.split(|c: char| !c.is_alphanumeric()) {
        let word = raw.trim().to_lowercase();
        if word.len() < 3
            || stopwords.contains(&word.as_str())
            || word.chars().all(|c| c.is_numeric())
        {
            continue;
        }
        *counts.entry(word).or_default() += 1;
    }
    let mut words = counts.into_iter().collect::<Vec<_>>();
    words.sort_by(|(left_word, left_count), (right_word, right_count)| {
        right_count
            .cmp(left_count)
            .then_with(|| left_word.cmp(right_word))
    });
    words
        .into_iter()
        .take(limit)
        .map(|(word, _)| word)
        .collect::<Vec<_>>()
        .join(" ")
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
                    direction, sender_id, sender_name, body, topic_keywords, occurred_at,
                          source_message_id, created_at, delivery_status, reply_to_message_id
                      ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
                params![
                    message.archive_message_id,
                    message.archive_id,
                    message.source_event_seq,
                    message.source_event_kind,
                    message.direction,
                    message.sender_id,
                    message.sender_name,
                    message.body,
                    message.topic_keywords,
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

    /// Record a normalized edit/delete/reaction/create and update the current
    /// display projection only when the transport revision is newer. A
    /// delete arriving before its create leaves a tombstone that an older
    /// create cannot resurrect.
    pub fn apply_operation(
        &self,
        operation: &ArchiveOperation<'_>,
    ) -> Result<ArchiveOperationResult, DbError> {
        if operation.event_id.trim().is_empty()
            || operation.source_message_id.trim().is_empty()
            || operation.archive_id.trim().is_empty()
        {
            return Err(DbError::Invariant(
                "archive operation identity fields are required".to_owned(),
            ));
        }
        if matches!(
            operation.kind,
            ArchiveOperationKind::Create | ArchiveOperationKind::Edit
        ) && operation.body.is_none()
        {
            return Err(DbError::Invariant(
                "create and edit operations require a body".to_owned(),
            ));
        }
        if matches!(
            operation.kind,
            ArchiveOperationKind::ReactionAdd | ArchiveOperationKind::ReactionRemove
        ) && operation.reaction.is_none()
        {
            return Err(DbError::Invariant(
                "reaction operations require a reaction value".to_owned(),
            ));
        }
        if matches!(
            operation.kind,
            ArchiveOperationKind::ReactionAdd | ArchiveOperationKind::ReactionRemove
        ) && operation.actor_id.is_none()
        {
            return Err(DbError::Invariant(
                "reaction operations require an actor identity".to_owned(),
            ));
        }
        self.db.transaction(|tx| {
            let recorded = tx.execute(
                "INSERT OR IGNORE INTO message_archive_revisions(
                    event_id,archive_id,source_message_id,operation,source_version,
                    occurred_at,body,reply_to_message_id,reaction,actor_id,recorded_at
                 ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
                params![
                    operation.event_id,
                    operation.archive_id,
                    operation.source_message_id,
                    operation.kind.as_str(),
                    operation.source_version,
                    operation.occurred_at,
                    operation.body,
                    operation.reply_to_message_id,
                    operation.reaction,
                    operation.actor_id,
                    operation.recorded_at,
                ],
            )? == 1;
            if !recorded {
                return Ok(ArchiveOperationResult { recorded: false, current_state_changed: false });
            }
            if matches!(operation.kind, ArchiveOperationKind::ReactionAdd | ArchiveOperationKind::ReactionRemove) {
                tx.execute(
                    "INSERT INTO message_archive_reactions(archive_id,source_message_id,actor_id,reaction,source_version,source_event_id,active,updated_at)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8)
                     ON CONFLICT(archive_id,source_message_id,actor_id,reaction) DO UPDATE SET
                        source_version=excluded.source_version,source_event_id=excluded.source_event_id,
                        active=excluded.active,updated_at=excluded.updated_at
                     WHERE excluded.source_version > message_archive_reactions.source_version
                        OR (excluded.source_version = message_archive_reactions.source_version
                            AND excluded.source_event_id > message_archive_reactions.source_event_id)",
                    params![operation.archive_id,operation.source_message_id,operation.actor_id,operation.reaction,operation.source_version,
                        operation.event_id,operation.kind == ArchiveOperationKind::ReactionAdd,operation.recorded_at],
                )?;
                return Ok(ArchiveOperationResult { recorded: true, current_state_changed: false });
            }

            let current_revision: Option<(i64, String)> = tx.query_row(
                "SELECT source_version,source_revision_event_id FROM message_archive_messages
                 WHERE archive_id=?1 AND source_message_id=?2",
                params![operation.archive_id, operation.source_message_id],
                |row| Ok((row.get(0)?,row.get(1)?)),
            ).optional()?;
            let newer_revision = current_revision.as_ref().is_none_or(|(version,event_id)| {
                operation.source_version > *version
                    || (operation.source_version == *version && operation.event_id > event_id.as_str())
            });
            if !newer_revision {
                return Ok(ArchiveOperationResult { recorded: true, current_state_changed: false });
            }
            let deleted = operation.kind == ArchiveOperationKind::Delete;
            let body = if deleted { "" } else { operation.body.unwrap_or("") };
            let keywords = extract_topic_keywords(body, 5);
            let direction = "inbound";
            let source_kind = operation.kind.as_str();
            tx.execute(
                "INSERT INTO message_archive_messages(
                    archive_message_id,archive_id,source_event_kind,direction,sender_id,sender_name,
                    body,topic_keywords,occurred_at,source_message_id,created_at,delivery_status,
                    reply_to_message_id,deleted_at,source_version,source_revision_event_id
                 ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,'delivered',?12,?13,?14,?15)
                 ON CONFLICT(archive_id,source_message_id) DO UPDATE SET
                    source_event_kind=excluded.source_event_kind,
                    sender_id=COALESCE(excluded.sender_id,message_archive_messages.sender_id),
                    sender_name=COALESCE(excluded.sender_name,message_archive_messages.sender_name),
                    body=excluded.body, topic_keywords=excluded.topic_keywords,
                    occurred_at=excluded.occurred_at,
                    reply_to_message_id=COALESCE(excluded.reply_to_message_id,message_archive_messages.reply_to_message_id),
                    deleted_at=excluded.deleted_at, source_version=excluded.source_version,
                    source_revision_event_id=excluded.source_revision_event_id
                 WHERE excluded.source_version > message_archive_messages.source_version
                    OR (excluded.source_version = message_archive_messages.source_version
                        AND excluded.source_revision_event_id > message_archive_messages.source_revision_event_id)",
                params![
                    operation.archive_message_id,
                    operation.archive_id,
                    source_kind,
                    direction,
                    operation.sender_id,
                    operation.sender_name,
                    body,
                    keywords,
                    operation.occurred_at,
                    operation.source_message_id,
                    operation.recorded_at,
                    operation.reply_to_message_id,
                    if deleted { Some(operation.recorded_at) } else { None::<i64> },
                    operation.source_version,
                    operation.event_id,
                ],
            )?;
            Ok(ArchiveOperationResult { recorded: true, current_state_changed: true })
        })
    }

    /// Return active reactions only. Reactions remain informational and this
    /// API provides no approval or policy interpretation.
    pub fn active_reactions(
        &self,
        archive_id: &str,
        source_message_id: &str,
    ) -> Result<Vec<ArchiveReaction>, DbError> {
        self.db.with_conn(|connection| {
            let mut statement = connection.prepare(
                "SELECT source_message_id,actor_id,reaction FROM message_archive_reactions
                 WHERE archive_id=?1 AND source_message_id=?2 AND active=1 ORDER BY actor_id,reaction",
            )?;
            let rows = statement.query_map(params![archive_id, source_message_id], |row| {
                Ok(ArchiveReaction {
                    source_message_id: row.get(0)?,
                    actor_id: row.get(1)?,
                    reaction: row.get(2)?,
                })
            })?;
            rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
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
                "SELECT archive_message_id, sender_id, sender_name, CASE WHEN deleted_at IS NULL THEN body ELSE '' END, occurred_at,
                        direction, delivery_status, reply_to_message_id, deleted_at IS NOT NULL
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
                        is_deleted: row.get(8)?,
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

    pub fn get_conversation_by_remote_id(
        &self,
        channel: &str,
        remote_id: &str,
    ) -> Result<Option<ArchiveConversation>, DbError> {
        self.db.with_conn(|c| {
            Ok(c.query_row(
                "SELECT archive_id, channel, remote_id, conversation_kind, display_name,
                        conversation_id, first_seen_at, last_seen_at
                 FROM message_archive_conversations WHERE channel = ?1 AND remote_id = ?2",
                params![channel, remote_id],
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
                "SELECT message_archive_search.archive_message_id, message_archive_search.archive_id,
                        message_archive_search.body, message_archive_search.sender_name, message_archive_search.topic_keywords
                 FROM message_archive_search
                 JOIN message_archive_messages m ON m.archive_message_id=message_archive_search.archive_message_id
                 WHERE message_archive_search MATCH ?1 AND m.deleted_at IS NULL
                 ORDER BY rank LIMIT ?2",
            )?;
            Ok(stmt
                .query_map(params![query, limit.min(100) as i64], |row| {
                    Ok(ArchiveSearchHit {
                        archive_message_id: row.get(0)?,
                        archive_id: row.get(1)?,
                        body: row.get(2)?,
                        sender_name: row.get(3)?,
                        topic_keywords: row.get(4)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?)
        })
    }

    pub fn related_recent_messages(
        &self,
        conversation_id: &str,
        query_terms: &[String],
        limit: u32,
    ) -> Result<Vec<StoredArchiveMessage>, DbError> {
        self.related_recent_messages_scoped(conversation_id, None, None, query_terms, limit)
    }

    /// Search archived messages for one transport recipient inside a shared conversation.
    pub fn related_recent_messages_for_transport(
        &self,
        conversation_id: &str,
        channel: &str,
        remote_id: &str,
        query_terms: &[String],
        limit: u32,
    ) -> Result<Vec<StoredArchiveMessage>, DbError> {
        self.related_recent_messages_scoped(
            conversation_id,
            Some(channel),
            Some(remote_id),
            query_terms,
            limit,
        )
    }

    /// Read one archived message only when it belongs to the exact transport recipient.
    pub fn message_for_transport(
        &self,
        conversation_id: &str,
        channel: &str,
        remote_id: &str,
        message_id: &str,
    ) -> Result<Option<StoredArchiveMessage>, DbError> {
        self.db.with_conn(|connection| Ok(connection.query_row(
            "SELECT m.archive_message_id,m.sender_id,m.sender_name,m.body,m.occurred_at,m.direction,m.delivery_status,m.reply_to_message_id,m.deleted_at IS NOT NULL FROM message_archive_messages m JOIN message_archive_conversations c ON c.archive_id=m.archive_id WHERE c.conversation_id=?1 AND c.channel=?2 AND c.remote_id=?3 AND m.archive_message_id=?4 AND m.deleted_at IS NULL",
            params![conversation_id,channel,remote_id,message_id],
            |row| Ok(StoredArchiveMessage { archive_message_id: row.get(0)?, sender_id: row.get(1)?, sender_name: row.get(2)?, body: row.get(3)?, occurred_at: row.get(4)?, direction: row.get(5)?, delivery_status: row.get(6)?, reply_to_message_id: row.get(7)?, is_deleted: row.get(8)? }),
        ).optional()?))
    }

    fn related_recent_messages_scoped(
        &self,
        conversation_id: &str,
        channel: Option<&str>,
        remote_id: Option<&str>,
        query_terms: &[String],
        limit: u32,
    ) -> Result<Vec<StoredArchiveMessage>, DbError> {
        if query_terms.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let match_query = query_terms
            .iter()
            .map(|term| format!("topic_keywords:{term} OR body:{term}"))
            .collect::<Vec<_>>()
            .join(" OR ");
        self.db.with_conn(|c| {
            let mut stmt = c.prepare(
                "SELECT m.archive_message_id, m.sender_id, m.sender_name, m.body,
                        m.occurred_at, m.direction, m.delivery_status, m.reply_to_message_id
                 FROM message_archive_messages m
                 JOIN message_archive_conversations c ON c.archive_id = m.archive_id
                 JOIN message_archive_search s ON s.archive_message_id = m.archive_message_id
                 WHERE c.conversation_id = ?1
                   AND m.deleted_at IS NULL
                   AND (?4 IS NULL OR (c.channel = ?4 AND c.remote_id = ?5))
                   AND message_archive_search MATCH ?2
                 ORDER BY m.occurred_at DESC, m.created_at DESC
                 LIMIT ?3",
            )?;
            Ok(stmt
                .query_map(
                    params![
                        conversation_id,
                        match_query,
                        limit.min(200) as i64,
                        channel,
                        remote_id
                    ],
                    |row| {
                        Ok(StoredArchiveMessage {
                            archive_message_id: row.get(0)?,
                            sender_id: row.get(1)?,
                            sender_name: row.get(2)?,
                            body: row.get(3)?,
                            occurred_at: row.get(4)?,
                            direction: row.get(5)?,
                            delivery_status: row.get(6)?,
                            reply_to_message_id: row.get(7)?,
                            is_deleted: false,
                        })
                    },
                )?
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
            topic_keywords: "hello",
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
        assert_eq!(
            store
                .get_conversation_by_remote_id("whatsapp", "group-1")
                .unwrap()
                .unwrap()
                .display_name
                .as_deref(),
            Some("Family")
        );
    }

    #[test]
    fn shared_conversation_history_is_scoped_to_whatsapp_group() {
        let db = Database::open(&DbConfig::in_memory_unencrypted()).unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        let store = MessageArchiveStore::new(&db);
        for (archive_id, remote_id, message_id, body) in [
            ("group-a", "a@g.us", "msg-a", "camper dates for A"),
            ("group-b", "b@g.us", "msg-b", "camper price for B"),
        ] {
            store
                .upsert_conversation(
                    archive_id,
                    "whatsapp",
                    remote_id,
                    "group",
                    None,
                    Some("shared"),
                    10,
                )
                .unwrap();
            store
                .append_message(&ArchiveMessage {
                    archive_message_id: message_id,
                    archive_id,
                    source_event_seq: None,
                    source_event_kind: "transport_inbound",
                    direction: "inbound",
                    sender_id: None,
                    sender_name: None,
                    body,
                    topic_keywords: "camper",
                    occurred_at: 10,
                    source_message_id: None,
                    created_at: 10,
                    delivery_status: "delivered",
                    reply_to_message_id: None,
                })
                .unwrap();
        }
        let hits = store
            .related_recent_messages_for_transport(
                "shared",
                "whatsapp",
                "a@g.us",
                &["camper".into()],
                10,
            )
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].body, "camper dates for A");
    }

    #[test]
    fn delete_before_create_and_out_of_order_edits_converge_without_reviving_text() {
        let db = Database::open(&DbConfig::in_memory_unencrypted()).unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        let store = MessageArchiveStore::new(&db);
        store
            .upsert_conversation("a", "signal", "peer", "direct", None, Some("c"), 1)
            .unwrap();
        let apply = |event_id: &str, kind, version, body: Option<&str>| {
            store
                .apply_operation(&ArchiveOperation {
                    event_id,
                    archive_id: "a",
                    archive_message_id: "m1",
                    source_message_id: "remote-1",
                    kind,
                    source_version: version,
                    occurred_at: version,
                    body,
                    sender_id: Some("peer"),
                    sender_name: None,
                    reply_to_message_id: None,
                    reaction: None,
                    actor_id: Some("peer"),
                    recorded_at: version,
                })
                .unwrap()
        };
        apply("deleted-first", ArchiveOperationKind::Delete, 3, None);
        apply(
            "created-late",
            ArchiveOperationKind::Create,
            1,
            Some("revoked secret"),
        );
        apply(
            "edited-new",
            ArchiveOperationKind::Edit,
            4,
            Some("current text"),
        );
        apply(
            "edited-old",
            ArchiveOperationKind::Edit,
            2,
            Some("stale text"),
        );
        assert!(
            !apply(
                "edited-old",
                ArchiveOperationKind::Edit,
                2,
                Some("stale text")
            )
            .recorded
        );

        let current = store
            .message_for_transport("c", "signal", "peer", "m1")
            .unwrap()
            .unwrap();
        assert_eq!(current.body, "current text");
        let search = store.search("revoked", 10).unwrap();
        assert!(
            search.is_empty(),
            "revoked text must not remain current search evidence"
        );
        apply("deleted-current", ArchiveOperationKind::Delete, 5, None);
        assert!(
            store
                .message_for_transport("c", "signal", "peer", "m1")
                .unwrap()
                .is_none()
        );
        assert!(store.search("current", 10).unwrap().is_empty());
        apply("z-edit", ArchiveOperationKind::Edit, 6, Some("tie winner"));
        apply("a-edit", ArchiveOperationKind::Edit, 6, Some("tie loser"));
        assert_eq!(
            store
                .message_for_transport("c", "signal", "peer", "m1")
                .unwrap()
                .unwrap()
                .body,
            "tie winner",
            "same-version conflicts converge by stable event ID"
        );
        let audit: i64 = db.with_conn(|connection| Ok(connection.query_row(
            "SELECT COUNT(*) FROM message_archive_revisions WHERE source_message_id='remote-1'", [], |row| row.get(0)
        )?)).unwrap();
        assert_eq!(
            audit, 7,
            "duplicate delivery is ignored but every unique operation stays auditable"
        );
    }

    #[test]
    fn reactions_are_audited_without_becoming_message_approval_or_edit() {
        let db = Database::open(&DbConfig::in_memory_unencrypted()).unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        let store = MessageArchiveStore::new(&db);
        store
            .upsert_conversation("a", "signal", "peer", "direct", None, Some("c"), 1)
            .unwrap();
        store
            .apply_operation(&ArchiveOperation {
                event_id: "create",
                archive_id: "a",
                archive_message_id: "m1",
                source_message_id: "remote-1",
                kind: ArchiveOperationKind::Create,
                source_version: 1,
                occurred_at: 1,
                body: Some("hello"),
                sender_id: Some("peer"),
                sender_name: None,
                reply_to_message_id: Some("parent"),
                reaction: None,
                actor_id: Some("peer"),
                recorded_at: 1,
            })
            .unwrap();
        let result = store
            .apply_operation(&ArchiveOperation {
                event_id: "reaction",
                archive_id: "a",
                archive_message_id: "m1",
                source_message_id: "remote-1",
                kind: ArchiveOperationKind::ReactionAdd,
                source_version: 2,
                occurred_at: 2,
                body: None,
                sender_id: None,
                sender_name: None,
                reply_to_message_id: None,
                reaction: Some("approve"),
                actor_id: Some("peer"),
                recorded_at: 2,
            })
            .unwrap();
        assert!(!result.current_state_changed);
        assert_eq!(store.active_reactions("a", "remote-1").unwrap().len(), 1);
        store
            .apply_operation(&ArchiveOperation {
                event_id: "reaction-removed",
                archive_id: "a",
                archive_message_id: "m1",
                source_message_id: "remote-1",
                kind: ArchiveOperationKind::ReactionRemove,
                source_version: 3,
                occurred_at: 3,
                body: None,
                sender_id: None,
                sender_name: None,
                reply_to_message_id: None,
                reaction: Some("approve"),
                actor_id: Some("peer"),
                recorded_at: 3,
            })
            .unwrap();
        assert!(store.active_reactions("a", "remote-1").unwrap().is_empty());
        let current = store
            .message_for_transport("c", "signal", "peer", "m1")
            .unwrap()
            .unwrap();
        assert_eq!(current.body, "hello");
        assert_eq!(current.reply_to_message_id.as_deref(), Some("parent"));
    }
}
