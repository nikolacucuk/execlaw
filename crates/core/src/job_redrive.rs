use crate::db::DbError;
use rusqlite::{Transaction, params};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedriveKind {
    Outbox,
    MemoryExtraction,
    Automation,
}

impl RedriveKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Outbox => "outbox",
            Self::MemoryExtraction => "memory_extraction",
            Self::Automation => "automation",
        }
    }
}

pub fn validate_redrive_request(actor: &str, reason: &str) -> Result<(String, String), DbError> {
    let actor = actor.trim();
    let reason = reason.trim();
    if actor.is_empty()
        || actor.len() > 128
        || actor.chars().any(char::is_control)
        || reason.is_empty()
        || reason.len() > 512
        || reason.chars().any(char::is_control)
    {
        return Err(DbError::Invariant(
            "redrive requires a bounded operator identity and reason".into(),
        ));
    }
    Ok((actor.to_owned(), reason.to_owned()))
}

pub fn append_redrive_event(
    tx: &Transaction<'_>,
    kind: RedriveKind,
    job_id: &str,
    effect_identity: &str,
    actor: &str,
    reason: &str,
    prior_attempt: i64,
    occurred_at: i64,
) -> Result<(), DbError> {
    tx.execute(
        "INSERT INTO state_job_redrive_events \
         (job_kind,job_id,effect_identity,actor,reason,prior_attempt,occurred_at) \
         VALUES (?1,?2,?3,?4,?5,?6,?7)",
        params![
            kind.as_str(),
            job_id,
            effect_identity,
            actor,
            reason,
            prior_attempt,
            occurred_at
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Database, DbConfig, MigrationRunner};

    #[test]
    fn redrive_request_requires_bounded_actor_and_reason() {
        assert!(validate_redrive_request("controller", "checked receipt").is_ok());
        assert!(validate_redrive_request(" ", "checked receipt").is_err());
        assert!(validate_redrive_request("controller", "\n").is_err());
        assert!(validate_redrive_request("controller", &"x".repeat(513)).is_err());
    }

    #[test]
    fn redrive_audit_is_append_only() {
        let db = Database::open(&DbConfig::in_memory_unencrypted()).unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        db.transaction(|tx| {
            append_redrive_event(
                tx,
                RedriveKind::Outbox,
                "41",
                "conversation:1:2",
                "controller",
                "checked receipt",
                3,
                100,
            )
        })
        .unwrap();
        assert!(
            db.with_conn(|conn| {
                conn.execute("UPDATE state_job_redrive_events SET reason='changed'", [])?;
                Ok(())
            })
            .is_err()
        );
        assert!(
            db.with_conn(|conn| {
                conn.execute("DELETE FROM state_job_redrive_events", [])?;
                Ok(())
            })
            .is_err()
        );
    }
}
