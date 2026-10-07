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
        params![kind.as_str(), job_id, effect_identity, actor, reason, prior_attempt, occurred_at],
    )?;
    Ok(())
}
