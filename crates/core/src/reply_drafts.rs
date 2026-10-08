//! Durable, revisioned Controller review for transport agent drafts.

use crate::db::{Database, DbError};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

/// One proposed reply, bound to an originating event and transport audience.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReplyDraft {
    pub id: String,
    pub agent_id: String,
    pub conversation_id: String,
    pub channel: String,
    pub recipient: String,
    pub source_event_id: Option<String>,
    pub source_event_seq: Option<i64>,
    pub model_seq: Option<i64>,
    pub revision: u32,
    pub audience: serde_json::Value,
    pub inbound_text: String,
    pub draft_text: String,
    pub status: String,
    pub stale_at: Option<i64>,
    pub reviewed_at: Option<i64>,
    pub reviewer_id: Option<String>,
    pub approved_text: Option<String>,
    pub created_at: i64,
}

/// Immutable identity and content for a new review proposal.
pub struct NewReplyDraft<'a> {
    pub id: &'a str,
    pub agent_id: &'a str,
    pub run_id: &'a str,
    pub conversation_id: &'a str,
    pub channel: &'a str,
    pub recipient: &'a str,
    pub source_event_id: Option<&'a str>,
    pub source_event_seq: Option<i64>,
    pub audience: &'a serde_json::Value,
    pub inbound_text: &'a str,
    pub draft_text: &'a str,
    pub now: i64,
}

/// SQLite-backed review lifecycle; transport effects remain owned by the outbox.
pub struct ReplyDraftStore<'db> {
    db: &'db Database,
}

impl<'db> ReplyDraftStore<'db> {
    pub fn new(db: &'db Database) -> Self {
        Self { db }
    }

    /// Persist a proposal once for the same mailbox identity.
    pub fn create(&self, input: &NewReplyDraft<'_>) -> Result<ReplyDraft, DbError> {
        if input.id.is_empty() || input.draft_text.trim().is_empty() || input.recipient.is_empty() {
            return Err(DbError::Invariant(
                "draft ID, text, and recipient are required".into(),
            ));
        }
        let audience = serde_json::to_string(input.audience)
            .map_err(|error| DbError::Invariant(error.to_string()))?;
        self.db.with_conn(|conn| {
            conn.execute("INSERT INTO state_reply_drafts(id,agent_id,conversation_id,channel,recipient,inbound_text,draft_text,status,created_at,run_id,source_event_id,source_event_seq,audience_json,source_last_seq) VALUES (?1,?2,?3,?4,?5,?6,?7,'pending',?8,?9,?10,?11,?12,?11) ON CONFLICT(id) DO NOTHING",
                params![input.id,input.agent_id,input.conversation_id,input.channel,input.recipient,input.inbound_text,input.draft_text,input.now,input.run_id,input.source_event_id,input.source_event_seq,audience])?;
            Ok(())
        })?;
        let draft = self
            .get(input.id)?
            .ok_or_else(|| DbError::Invariant("draft disappeared after insert".into()))?;
        if draft.agent_id != input.agent_id
            || draft.conversation_id != input.conversation_id
            || draft.channel != input.channel
            || draft.recipient != input.recipient
            || draft.source_event_id.as_deref() != input.source_event_id
            || draft.draft_text != input.draft_text
        {
            return Err(DbError::Invariant("draft identity collision".into()));
        }
        Ok(draft)
    }

    /// Bind the visible event-log message to the proposal once.
    pub fn attach_model_seq(&self, id: &str, model_seq: i64) -> Result<(), DbError> {
        self.db.with_conn(|conn| {
            let changed = conn.execute("UPDATE state_reply_drafts SET model_seq=?2 WHERE id=?1 AND (model_seq IS NULL OR model_seq=?2)", params![id,model_seq])?;
            if changed != 1 { return Err(DbError::Invariant("draft model event changed".into())); }
            Ok(())
        })
    }

    /// Read a proposal by its stable mailbox-derived ID.
    pub fn get(&self, id: &str) -> Result<Option<ReplyDraft>, DbError> {
        self.db.with_conn(|conn| Ok(conn.query_row(
            "SELECT id,agent_id,conversation_id,channel,recipient,source_event_id,source_event_seq,model_seq,revision,audience_json,inbound_text,draft_text,status,stale_at,reviewed_at,reviewer_id,approved_text,created_at FROM state_reply_drafts WHERE id=?1",
            [id], map_draft).optional()?))
    }

    /// Read the draft corresponding to a Chat model event.
    pub fn for_model_seq(
        &self,
        conversation_id: &str,
        model_seq: i64,
    ) -> Result<Option<ReplyDraft>, DbError> {
        self.db.with_conn(|conn| Ok(conn.query_row(
            "SELECT id,agent_id,conversation_id,channel,recipient,source_event_id,source_event_seq,model_seq,revision,audience_json,inbound_text,draft_text,status,stale_at,reviewed_at,reviewer_id,approved_text,created_at FROM state_reply_drafts WHERE conversation_id=?1 AND model_seq=?2",
            params![conversation_id,model_seq], map_draft).optional()?))
    }

    /// Load a conversation's review proposals for chat projection.
    pub fn for_conversation(&self, conversation_id: &str) -> Result<Vec<ReplyDraft>, DbError> {
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare("SELECT id,agent_id,conversation_id,channel,recipient,source_event_id,source_event_seq,model_seq,revision,audience_json,inbound_text,draft_text,status,stale_at,reviewed_at,reviewer_id,approved_text,created_at FROM state_reply_drafts WHERE conversation_id=?1 AND model_seq IS NOT NULL")?;
            Ok(stmt.query_map([conversation_id], map_draft)?.collect::<Result<Vec<_>,_>>()?)
        })
    }

    /// List pending and stale proposals for Controller review.
    pub fn inbox(&self, limit: u32) -> Result<Vec<ReplyDraft>, DbError> {
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare("SELECT id,agent_id,conversation_id,channel,recipient,source_event_id,source_event_seq,model_seq,revision,audience_json,inbound_text,draft_text,status,stale_at,reviewed_at,reviewer_id,approved_text,created_at FROM state_reply_drafts WHERE status='pending' ORDER BY created_at DESC LIMIT ?1")?;
            Ok(stmt.query_map([limit.min(200)], map_draft)?.collect::<Result<Vec<_>,_>>()?)
        })
    }

    /// Mark older proposals for the same recipient stale after a new inbound.
    pub fn stale_after_inbound(
        &self,
        conversation_id: &str,
        channel: &str,
        recipient: &str,
        source_seq: i64,
        now: i64,
    ) -> Result<usize, DbError> {
        self.db.with_conn(|conn| Ok(conn.execute(
            "UPDATE state_reply_drafts SET stale_at=?5 WHERE conversation_id=?1 AND channel=?2 AND recipient=?3 AND status='pending' AND stale_at IS NULL AND source_event_seq IS NOT NULL AND source_event_seq<?4",
            params![conversation_id,channel,recipient,source_seq,now])?))
    }

    /// Edit a pending draft; a stale draft remains stale until regenerated.
    pub fn edit(
        &self,
        id: &str,
        expected_revision: u32,
        text: &str,
    ) -> Result<ReplyDraft, DbError> {
        if text.trim().is_empty() {
            return Err(DbError::Invariant("draft text is empty".into()));
        }
        self.db.with_conn(|conn| {
            let changed = conn.execute("UPDATE state_reply_drafts SET draft_text=?3,revision=revision+1 WHERE id=?1 AND revision=?2 AND status='pending'", params![id,expected_revision,text.trim()])?;
            if changed != 1 { return Err(DbError::Invariant("draft revision or status changed".into())); }
            Ok(())
        })?;
        self.get(id)?
            .ok_or_else(|| DbError::Invariant("draft disappeared after edit".into()))
    }

    /// Claim one exact, fresh revision for Controller-approved sending.
    pub fn approve(
        &self,
        id: &str,
        expected_revision: u32,
        reviewer_id: &str,
        now: i64,
    ) -> Result<ReplyDraft, DbError> {
        self.db.with_conn(|conn| {
            let changed = conn.execute("UPDATE state_reply_drafts SET status='approved',reviewed_at=?3,reviewer_id=?4,approved_text=draft_text WHERE id=?1 AND revision=?2 AND status='pending' AND stale_at IS NULL AND model_seq IS NOT NULL",
                params![id,expected_revision,now,reviewer_id])?;
            if changed != 1 { return Err(DbError::Invariant("draft is stale, changed, or already reviewed".into())); }
            Ok(())
        })?;
        self.get(id)?
            .ok_or_else(|| DbError::Invariant("draft disappeared after approval".into()))
    }

    /// Reject one exact pending revision without creating an effect.
    pub fn reject(
        &self,
        id: &str,
        expected_revision: u32,
        reviewer_id: &str,
        now: i64,
    ) -> Result<(), DbError> {
        self.db.with_conn(|conn| {
            let changed = conn.execute("UPDATE state_reply_drafts SET status='rejected',reviewed_at=?3,reviewer_id=?4 WHERE id=?1 AND revision=?2 AND status='pending'", params![id,expected_revision,now,reviewer_id])?;
            if changed != 1 { return Err(DbError::Invariant("draft revision or status changed".into())); }
            Ok(())
        })
    }

    /// Record a queued delivery or terminal failure after outbox admission.
    pub fn mark_delivery(&self, id: &str, status: &str, now: i64) -> Result<(), DbError> {
        if !matches!(status, "sent" | "failed") {
            return Err(DbError::Invariant("invalid draft delivery status".into()));
        }
        self.db.with_conn(|conn| {
            conn.execute("UPDATE state_reply_drafts SET status=?2,sent_at=CASE WHEN ?2='sent' THEN ?3 ELSE sent_at END WHERE id=?1 AND status='approved'", params![id,status,now])?;
            Ok(())
        })
    }

    /// Verify that the draft still targets its captured transport and observed audience.
    /// Group membership comes from opaque principal IDs, never display names.
    pub fn validate_audience(&self, id: &str) -> Result<(), DbError> {
        let draft = self
            .get(id)?
            .ok_or_else(|| DbError::Invariant("reply draft not found".into()))?;
        let audience = &draft.audience;
        if audience.get("channel").and_then(serde_json::Value::as_str)
            != Some(draft.channel.as_str())
            || audience
                .get("recipient")
                .and_then(serde_json::Value::as_str)
                != Some(draft.recipient.as_str())
        {
            return Err(DbError::Invariant(
                "reply draft transport destination changed".into(),
            ));
        }
        let native_group = audience.get("group_id").and_then(serde_json::Value::as_str);
        if native_group.is_none() {
            let bound: bool = self.db.with_conn(|connection| {
                Ok(connection.query_row(
                    "SELECT EXISTS(SELECT 1 FROM state_transport_bindings binding \
                     JOIN state_conversations conversation \
                       ON conversation.conversation_id = ?1 \
                     WHERE binding.channel = ?2 AND binding.foreign_id = ?3 \
                       AND binding.principal_group_id = conversation.principal_group_id \
                       AND binding.is_group = 0)",
                    params![draft.conversation_id, draft.channel, draft.recipient],
                    |row| row.get(0),
                )?)
            })?;
            return if bound {
                Ok(())
            } else {
                Err(DbError::Invariant(
                    "reply draft recipient identity is no longer bound to this conversation".into(),
                ))
            };
        }
        let current = crate::principal_groups::PrincipalGroupStore::new(self.db)
            .audience_state(&draft.conversation_id)?;
        let Some((group_id, current_native_group, epoch, member_hash, _members)) = current else {
            return Err(DbError::Invariant(
                "reply draft group audience is unavailable".into(),
            ));
        };
        if current_native_group.as_deref() != native_group
            || audience
                .get("principal_group_id")
                .and_then(serde_json::Value::as_str)
                != Some(group_id.as_str())
            || audience
                .get("membership_epoch")
                .and_then(serde_json::Value::as_i64)
                != Some(epoch)
            || audience
                .get("members_sha256")
                .and_then(serde_json::Value::as_str)
                != Some(member_hash.as_str())
        {
            return Err(DbError::Invariant(
                "reply draft audience changed since it was proposed".into(),
            ));
        }
        Ok(())
    }
}

fn map_draft(row: &rusqlite::Row<'_>) -> rusqlite::Result<ReplyDraft> {
    let audience: String = row.get(9)?;
    Ok(ReplyDraft {
        id: row.get(0)?,
        agent_id: row.get(1)?,
        conversation_id: row.get(2)?,
        channel: row.get(3)?,
        recipient: row.get(4)?,
        source_event_id: row.get(5)?,
        source_event_seq: row.get(6)?,
        model_seq: row.get(7)?,
        revision: row.get(8)?,
        audience: serde_json::from_str(&audience).unwrap_or(serde_json::Value::Null),
        inbound_text: row.get(10)?,
        draft_text: row.get(11)?,
        status: row.get(12)?,
        stale_at: row.get(13)?,
        reviewed_at: row.get(14)?,
        reviewer_id: row.get(15)?,
        approved_text: row.get(16)?,
        created_at: row.get(17)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DbConfig, MigrationRunner};

    #[test]
    fn stale_and_revisioned_draft_cannot_be_approved_twice() {
        let db = Database::open(&DbConfig::in_memory_unencrypted()).unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        let store = ReplyDraftStore::new(&db);
        let input = NewReplyDraft {
            id: "draft-1",
            agent_id: "agent-1",
            run_id: "run-1",
            conversation_id: "chat-1",
            channel: "whatsapp",
            recipient: "group@g.us",
            source_event_id: Some("msg-1"),
            source_event_seq: Some(7),
            audience: &serde_json::json!({"group_id":"group@g.us"}),
            inbound_text: "Camper?",
            draft_text: "Please send dates.",
            now: 10,
        };
        store.create(&input).unwrap();
        store.attach_model_seq("draft-1", 8).unwrap();
        assert_eq!(
            store.edit("draft-1", 1, "Which dates?").unwrap().revision,
            2
        );
        assert!(store.approve("draft-1", 1, "controller", 11).is_err());
        store
            .stale_after_inbound("chat-1", "whatsapp", "group@g.us", 9, 12)
            .unwrap();
        assert!(store.approve("draft-1", 2, "controller", 13).is_err());
        let input2 = NewReplyDraft {
            id: "draft-2",
            source_event_id: Some("msg-2"),
            source_event_seq: Some(9),
            ..input
        };
        store.create(&input2).unwrap();
        store.attach_model_seq("draft-2", 10).unwrap();
        assert_eq!(
            store
                .approve("draft-2", 1, "controller", 14)
                .unwrap()
                .status,
            "approved"
        );
        assert!(store.approve("draft-2", 1, "controller", 14).is_err());
    }

    #[test]
    fn observed_group_membership_change_invalidates_reviewed_draft_audience() {
        use crate::{
            ids::PrincipalId,
            principal_groups::{GroupKey, PrincipalGroupStore},
            transport_bindings::TransportBindingStore,
        };

        let db = Database::open(&DbConfig::in_memory_unencrypted()).unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        db.with_conn(|connection| {
            connection.execute(
                "INSERT INTO state_conversations(conversation_id,kind,phase,trust_class,modality) \
                 VALUES ('group-chat','GroupWithControllerPresent','idle','KnownTrusted','Text')",
                [],
            )?;
            Ok(())
        })
        .unwrap();
        let groups = PrincipalGroupStore::new(&db);
        let group = groups
            .resolve(
                &GroupKey {
                    channel: "whatsapp",
                    native_group_id: Some("native-group-1"),
                    principals: &[],
                    includes_controller: true,
                },
                100,
            )
            .unwrap();
        groups
            .bind_conversation("group-chat", &group.group_id)
            .unwrap();
        TransportBindingStore::new(&db)
            .insert_binding("whatsapp", "native-group-1", &group.group_id, true, 100)
            .unwrap();
        let (group_id, _, epoch, members_sha256, members) =
            groups.audience_state("group-chat").unwrap().unwrap();
        let audience = serde_json::json!({
            "channel":"whatsapp", "recipient":"native-group-1",
            "group_id":"native-group-1", "principal_group_id":group_id,
            "membership_epoch":epoch, "members_sha256":members_sha256,
            "members":members
        });
        let store = ReplyDraftStore::new(&db);
        store
            .create(&NewReplyDraft {
                id: "audience-draft",
                agent_id: "agent",
                run_id: "run",
                conversation_id: "group-chat",
                channel: "whatsapp",
                recipient: "native-group-1",
                source_event_id: Some("source-1"),
                source_event_seq: Some(1),
                audience: &audience,
                inbound_text: "Question",
                draft_text: "Private answer",
                now: 100,
            })
            .unwrap();
        assert!(store.validate_audience("audience-draft").is_ok());
        groups
            .add_member(
                &group.group_id,
                &PrincipalId::from("lower-trust-member"),
                101,
            )
            .unwrap();
        assert!(store.validate_audience("audience-draft").is_err());
    }
}
