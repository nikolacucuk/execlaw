//! Explicit, Controller-approved links between authenticated transport accounts.

use crate::db::{Database, DbError};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TransportIdentityLink {
    pub link_id: String,
    pub controller_id: String,
    pub left_channel: String,
    pub left_subject: String,
    pub right_channel: String,
    pub right_subject: String,
    pub verification_id: String,
    pub linked_at: i64,
    pub revoked_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContinuityTransfer {
    pub origin_channel: String,
    pub origin_subject: String,
    pub origin_conversation: String,
    pub destination_channel: String,
    pub destination_subject: String,
    pub destination_conversation: String,
    pub selected_message_ids: Vec<String>,
    pub audience_kind: String,
}

pub struct TransportIdentityStore<'db> {
    db: &'db Database,
}

impl<'db> TransportIdentityStore<'db> {
    pub fn new(db: &'db Database) -> Self {
        Self { db }
    }

    /// Link two exact, already-authenticated transport subjects after explicit
    /// Controller approval. Names and message similarity are never consulted.
    pub fn link_verified(
        &self,
        controller_id: &str,
        left_channel: &str,
        left_subject: &str,
        right_channel: &str,
        right_subject: &str,
        verification_id: &str,
        now: i64,
    ) -> Result<TransportIdentityLink, DbError> {
        if [
            controller_id,
            left_channel,
            left_subject,
            right_channel,
            right_subject,
            verification_id,
        ]
        .iter()
        .any(|value| value.trim().is_empty())
            || (left_channel == right_channel && left_subject == right_subject)
        {
            return Err(DbError::Invariant(
                "explicit identities and verification are required".into(),
            ));
        }
        let identities_verified = self.db.with_conn(|connection| {
            let verified: bool = connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM principal_identifiers WHERE transport=?1 AND handle=?2)
                    AND EXISTS(SELECT 1 FROM principal_identifiers WHERE transport=?3 AND handle=?4)",
                params![left_channel, left_subject, right_channel, right_subject],
                |row| row.get(0),
            )?;
            Ok(verified)
        })?;
        if !identities_verified {
            return Err(DbError::Invariant(
                "both transport accounts must be observed and authenticated before linking".into(),
            ));
        }
        let link_id = Uuid::new_v4().to_string();
        self.db.with_conn(|connection| {
            connection.execute(
                "INSERT INTO state_transport_identity_links(link_id,controller_id,left_channel,left_subject,right_channel,right_subject,verification_id,selected_history_json,linked_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,'[]',?8)",
                params![link_id, controller_id, left_channel, left_subject, right_channel, right_subject, verification_id, now],
            )?;
            Ok(())
        })?;
        self.get(&link_id)?
            .ok_or_else(|| DbError::Invariant("identity link disappeared".into()))
    }

    pub fn get(&self, link_id: &str) -> Result<Option<TransportIdentityLink>, DbError> {
        self.db.with_conn(|connection| Ok(connection.query_row(
            "SELECT link_id,controller_id,left_channel,left_subject,right_channel,right_subject,verification_id,linked_at,revoked_at
             FROM state_transport_identity_links WHERE link_id=?1", [link_id], map_link,
        ).optional()?))
    }

    /// Revoke future continuity. Existing event/archive records retain their
    /// original scope and are never copied by this operation.
    pub fn unlink(&self, link_id: &str, controller_id: &str, now: i64) -> Result<bool, DbError> {
        self.db.with_conn(|connection| Ok(connection.execute(
            "UPDATE state_transport_identity_links SET revoked_at=?1 WHERE link_id=?2 AND controller_id=?3 AND revoked_at IS NULL",
            params![now, link_id, controller_id],
        )? == 1))
    }

    /// Validate a selected transfer against one active verified link. Group
    /// destinations are rejected so private history cannot widen its audience.
    pub fn authorize_transfer(
        &self,
        link_id: &str,
        transfer: &ContinuityTransfer,
    ) -> Result<(), DbError> {
        let link = self
            .get(link_id)?
            .ok_or_else(|| DbError::Invariant("identity link not found".into()))?;
        let pair_matches = (transfer.origin_channel == link.left_channel
            && transfer.origin_subject == link.left_subject
            && transfer.destination_channel == link.right_channel
            && transfer.destination_subject == link.right_subject)
            || (transfer.origin_channel == link.right_channel
                && transfer.origin_subject == link.right_subject
                && transfer.destination_channel == link.left_channel
                && transfer.destination_subject == link.left_subject);
        if link.revoked_at.is_some()
            || !pair_matches
            || transfer.audience_kind != "direct"
            || transfer.selected_message_ids.is_empty()
        {
            return Err(DbError::Invariant(
                "continuity transfer is not authorized".into(),
            ));
        }
        Ok(())
    }

    /// Persist an explicitly selected direct-message context transfer.
    pub fn record_transfer(
        &self,
        link_id: &str,
        transfer: &ContinuityTransfer,
        selected_context: &[serde_json::Value],
        now: i64,
    ) -> Result<String, DbError> {
        self.authorize_transfer(link_id, transfer)?;
        let transfer_id = Uuid::new_v4().to_string();
        let selected = serde_json::to_string(&transfer.selected_message_ids).map_err(|error| {
            DbError::Invariant(format!("serialize transfer selection: {error}"))
        })?;
        let context_ids = selected_context
            .iter()
            .filter_map(|entry| entry.get("message_id").and_then(serde_json::Value::as_str))
            .collect::<Vec<_>>();
        if context_ids.len() != selected_context.len()
            || context_ids.len() != transfer.selected_message_ids.len()
            || context_ids
                .iter()
                .copied()
                .ne(transfer.selected_message_ids.iter().map(String::as_str))
        {
            return Err(DbError::Invariant(
                "selected history content must match the explicit message selection".into(),
            ));
        }
        let context = serde_json::to_string(selected_context)
            .map_err(|error| DbError::Invariant(format!("serialize selected history: {error}")))?;
        self.db.with_conn(|connection| {
            connection.execute(
                "INSERT INTO state_transport_continuity_transfers(
                    transfer_id,link_id,origin_channel,origin_subject,origin_conversation,
                    destination_channel,destination_subject,destination_conversation,
                    audience_kind,selected_message_ids_json,selected_context_json,created_at
                 ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
                params![
                    transfer_id,
                    link_id,
                    transfer.origin_channel,
                    transfer.origin_subject,
                    transfer.origin_conversation,
                    transfer.destination_channel,
                    transfer.destination_subject,
                    transfer.destination_conversation,
                    transfer.audience_kind,
                    selected,
                    context,
                    now,
                ],
            )?;
            Ok(())
        })?;
        Ok(transfer_id)
    }
}

fn map_link(row: &rusqlite::Row<'_>) -> rusqlite::Result<TransportIdentityLink> {
    Ok(TransportIdentityLink {
        link_id: row.get(0)?,
        controller_id: row.get(1)?,
        left_channel: row.get(2)?,
        left_subject: row.get(3)?,
        right_channel: row.get(4)?,
        right_subject: row.get(5)?,
        verification_id: row.get(6)?,
        linked_at: row.get(7)?,
        revoked_at: row.get(8)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DbConfig, MigrationRunner};

    #[test]
    fn exact_verified_accounts_can_transfer_selected_history_until_unlinked() {
        let db = Database::open(&DbConfig::in_memory_unencrypted()).unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        let store = TransportIdentityStore::new(&db);
        let principals = crate::principal::PrincipalStore::new(&db);
        for (id, channel, subject) in [
            ("signal-account", "signal", "+15550001"),
            ("whatsapp-account", "whatsapp", "15550001@s.whatsapp.net"),
        ] {
            principals
                .upsert(&crate::principal::Principal {
                    id: crate::ids::PrincipalId::from(id),
                    identifiers: vec![crate::principal::Identifier {
                        transport: channel.into(),
                        handle: subject.into(),
                    }],
                    trust_level: crate::principal::TrustLevel::KnownTrusted {
                        resolvers: Vec::new(),
                        approved_by: crate::ids::PrincipalId::from("controller"),
                        approved_at: 1,
                    },
                    resolved_by: Vec::new(),
                    metadata: serde_json::json!({}),
                    first_seen: 1,
                    last_seen: Some(10),
                    controller_notes: None,
                })
                .unwrap();
        }
        let link = store
            .link_verified(
                "controller",
                "signal",
                "+15550001",
                "whatsapp",
                "15550001@s.whatsapp.net",
                "approval-1",
                10,
            )
            .unwrap();
        let transfer = ContinuityTransfer {
            origin_channel: "signal".into(),
            origin_subject: "+15550001".into(),
            origin_conversation: "dm-a".into(),
            destination_channel: "whatsapp".into(),
            destination_subject: "15550001@s.whatsapp.net".into(),
            destination_conversation: "dm-b".into(),
            selected_message_ids: vec!["msg-7".into()],
            audience_kind: "direct".into(),
        };
        store.authorize_transfer(&link.link_id, &transfer).unwrap();
        store
            .record_transfer(
                &link.link_id,
                &transfer,
                &[serde_json::json!({"message_id":"msg-7","body":"selected context"})],
                10,
            )
            .unwrap();
        let context: String = db.with_conn(|connection| Ok(connection.query_row(
            "SELECT selected_context_json FROM state_transport_continuity_transfers WHERE link_id=?1", [&link.link_id], |row| row.get(0)
        )?)).unwrap();
        assert!(context.contains("selected context"));
        let mut widened = transfer.clone();
        widened.audience_kind = "group".into();
        assert!(store.authorize_transfer(&link.link_id, &widened).is_err());
        assert!(store.unlink(&link.link_id, "controller", 11).unwrap());
        assert!(store.authorize_transfer(&link.link_id, &transfer).is_err());
    }

    #[test]
    fn matching_names_or_text_cannot_create_a_link() {
        let db = Database::open(&DbConfig::in_memory_unencrypted()).unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        let store = TransportIdentityStore::new(&db);
        assert!(
            store
                .link_verified(
                    "controller",
                    "signal",
                    "same display name",
                    "whatsapp",
                    "same display name",
                    "",
                    1
                )
                .is_err()
        );
        assert!(
            store
                .link_verified(
                    "controller",
                    "signal",
                    "same display name",
                    "whatsapp",
                    "same display name",
                    "approval-1",
                    1
                )
                .is_err(),
            "display labels without authenticated principal identifiers must not link"
        );
    }
}
