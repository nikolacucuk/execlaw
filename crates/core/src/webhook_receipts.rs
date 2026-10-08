//! Durable webhook delivery deduplication after successful host authentication.

use crate::db::{Database, DbError};
use rusqlite::params;

pub struct WebhookReceiptStore<'db> {
    db: &'db Database,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebhookClaim {
    Claimed,
    AlreadyProcessed,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DbConfig, MigrationRunner};

    #[test]
    fn accepted_delivery_is_deduplicated_and_stale_claim_is_recoverable() {
        let db = Database::open(&DbConfig::in_memory_unencrypted()).unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        let store = WebhookReceiptStore::new(&db);
        assert_eq!(
            store.claim("p", "POST /event", "e1", 10).unwrap(),
            WebhookClaim::Claimed
        );
        assert_eq!(
            store.claim("p", "POST /event", "e1", 11).unwrap(),
            WebhookClaim::AlreadyProcessed
        );
        store
            .complete("p", "POST /event", "e1", "accepted")
            .unwrap();
        assert_eq!(
            store.claim("p", "POST /event", "e1", 1000).unwrap(),
            WebhookClaim::AlreadyProcessed
        );
        assert_eq!(store.outcome_count("accepted").unwrap(), 1);
        assert_eq!(
            store.claim("p", "POST /event", "e2", 10).unwrap(),
            WebhookClaim::Claimed
        );
        assert_eq!(
            store.claim("p", "POST /event", "e2", 311).unwrap(),
            WebhookClaim::Claimed
        );
        store
            .complete("p", "POST /event", "e2", "rejected")
            .unwrap();
        assert_eq!(store.outcome_count("rejected").unwrap(), 1);
    }
}

impl<'db> WebhookReceiptStore<'db> {
    pub fn new(db: &'db Database) -> Self {
        Self { db }
    }

    /// Claim a provider delivery ID once. Only stale in-progress claims can
    /// be recovered after a process crash; accepted deliveries remain deduped.
    pub fn claim(
        &self,
        plugin_id: &str,
        route_key: &str,
        event_id: &str,
        now: i64,
    ) -> Result<WebhookClaim, DbError> {
        if plugin_id.trim().is_empty() || route_key.trim().is_empty() || event_id.trim().is_empty()
        {
            return Err(DbError::Invariant(
                "webhook receipt identity is required".into(),
            ));
        }
        self.db.transaction(|transaction| {
            let reclaimed = transaction.execute(
                "UPDATE state_webhook_receipts SET received_at=?1,outcome='processing',lease_expires_at=?2
                 WHERE plugin_id=?3 AND route_key=?4 AND event_id=?5 AND outcome='processing'
                   AND lease_expires_at<=?1",
                params![now, now.saturating_add(300), plugin_id, route_key, event_id],
            )?;
            if reclaimed == 1 { return Ok(WebhookClaim::Claimed); }
            let inserted = transaction.execute(
                "INSERT OR IGNORE INTO state_webhook_receipts(plugin_id,route_key,event_id,received_at,outcome,lease_expires_at)
                 VALUES (?1,?2,?3,?4,'processing',?5)",
                params![plugin_id, route_key, event_id, now, now.saturating_add(300)],
            )? == 1;
            Ok(if inserted { WebhookClaim::Claimed } else { WebhookClaim::AlreadyProcessed })
        })
    }

    /// Mark a successfully dispatched delivery as durable and replay-proof.
    pub fn complete(
        &self,
        plugin_id: &str,
        route_key: &str,
        event_id: &str,
        outcome: &str,
    ) -> Result<(), DbError> {
        if !matches!(outcome, "accepted" | "rejected") {
            return Err(DbError::Invariant(
                "webhook outcome must be accepted or rejected".into(),
            ));
        }
        self.db.with_conn(|connection| {
            connection.execute(
                "UPDATE state_webhook_receipts SET outcome=?1,lease_expires_at=NULL
                 WHERE plugin_id=?2 AND route_key=?3 AND event_id=?4 AND outcome='processing'",
                params![outcome, plugin_id, route_key, event_id],
            )?;
            Ok(())
        })
    }

    /// Make a failed handler retryable. A crash is recovered after its lease.
    pub fn release(&self, plugin_id: &str, route_key: &str, event_id: &str) -> Result<(), DbError> {
        self.db.with_conn(|connection| {
            connection.execute(
                "DELETE FROM state_webhook_receipts WHERE plugin_id=?1 AND route_key=?2 AND event_id=?3 AND outcome='processing'",
                params![plugin_id, route_key, event_id],
            )?;
            Ok(())
        })
    }

    /// Count only sanitized outcome labels, never delivery IDs or body data.
    pub fn outcome_count(&self, outcome: &str) -> Result<u64, DbError> {
        self.db.with_conn(|connection| {
            Ok(connection.query_row(
                "SELECT COUNT(*) FROM state_webhook_receipts WHERE outcome=?1",
                [outcome],
                |row| row.get(0),
            )?)
        })
    }
}
