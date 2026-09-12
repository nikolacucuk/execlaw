//! Durable Graphiti ingestion and reconciliation jobs.

use crate::db::{Database, DbError};
use crate::ids::{ConversationId, EventSeq};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphitiJobKind {
    Ingest,
    Reconcile,
}

impl GraphitiJobKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Ingest => "ingest",
            Self::Reconcile => "reconcile",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "ingest" => Some(Self::Ingest),
            "reconcile" => Some(Self::Reconcile),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct NewGraphitiJob {
    pub kind: GraphitiJobKind,
    pub conversation_id: ConversationId,
    pub trust_class: String,
    pub source_event_seq: EventSeq,
    pub evidence_id: String,
    pub payload: serde_json::Value,
    pub max_attempts: i64,
    pub now: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GraphitiJob {
    pub job_id: String,
    pub kind: GraphitiJobKind,
    pub conversation_id: ConversationId,
    pub trust_class: String,
    pub source_event_seq: EventSeq,
    pub evidence_id: String,
    pub payload: serde_json::Value,
    pub attempt: i64,
    pub max_attempts: i64,
}

#[derive(Debug, Error)]
pub enum GraphitiStoreError {
    #[error(transparent)]
    Db(#[from] DbError),
    #[error("invalid Graphiti job: {0}")]
    Invalid(String),
    #[error("corrupt Graphiti job: {0}")]
    Corrupt(String),
}

pub struct GraphitiJobStore<'db> {
    db: &'db Database,
}

impl<'db> GraphitiJobStore<'db> {
    pub fn new(db: &'db Database) -> Self {
        Self { db }
    }

    pub fn enqueue(&self, job: &NewGraphitiJob) -> Result<String, GraphitiStoreError> {
        if job.evidence_id.trim().is_empty() {
            return Err(GraphitiStoreError::Invalid(
                "evidence_id is required".into(),
            ));
        }
        if job.max_attempts <= 0 {
            return Err(GraphitiStoreError::Invalid(
                "max_attempts must be positive".into(),
            ));
        }
        let job_id = Uuid::new_v4().to_string();
        let payload_json = serde_json::to_string(&job.payload)
            .map_err(|error| GraphitiStoreError::Invalid(error.to_string()))?;
        self.db
            .with_conn(|conn| {
                conn.execute(
                    "INSERT OR IGNORE INTO state_graphiti_jobs \
                 (job_id, kind, conversation_id, trust_class, source_event_seq, evidence_id, \
                  payload_json, max_attempts, next_attempt_at, created_at, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9, ?9)",
                    params![
                        job_id,
                        job.kind.as_str(),
                        job.conversation_id.as_str(),
                        job.trust_class,
                        job.source_event_seq.0,
                        job.evidence_id,
                        payload_json,
                        job.max_attempts,
                        job.now,
                    ],
                )?;
                conn.query_row(
                    "SELECT job_id FROM state_graphiti_jobs WHERE kind = ?1 AND evidence_id = ?2",
                    params![job.kind.as_str(), job.evidence_id],
                    |row| row.get(0),
                )
                .map_err(DbError::from)
            })
            .map_err(GraphitiStoreError::from)
    }

    pub fn claim_next(
        &self,
        lease_owner: &str,
        now: i64,
        lease_expires_at: i64,
    ) -> Result<Option<GraphitiJob>, GraphitiStoreError> {
        if lease_owner.trim().is_empty() || lease_expires_at <= now {
            return Err(GraphitiStoreError::Invalid(
                "a valid lease owner and future expiry are required".into(),
            ));
        }
        self.db
            .transaction(|tx| {
                let candidate: Option<String> = tx
                    .query_row(
                        "SELECT job_id FROM state_graphiti_jobs \
                     WHERE attempt < max_attempts AND (\
                         (status = 'pending' AND next_attempt_at <= ?1) OR \
                         (status = 'running' AND lease_expires_at <= ?1)\
                     ) ORDER BY created_at, job_id LIMIT 1",
                        [now],
                        |row| row.get(0),
                    )
                    .optional()?;
                let Some(job_id) = candidate else {
                    return Ok(None);
                };
                let changed = tx.execute(
                    "UPDATE state_graphiti_jobs SET status = 'running', attempt = attempt + 1, \
                 lease_owner = ?2, lease_expires_at = ?3, updated_at = ?1 \
                 WHERE job_id = ?4 AND attempt < max_attempts AND (\
                     (status = 'pending' AND next_attempt_at <= ?1) OR \
                     (status = 'running' AND lease_expires_at <= ?1)\
                 )",
                    params![now, lease_owner, lease_expires_at, job_id],
                )?;
                if changed == 0 {
                    return Ok(None);
                }
                Ok(tx
                    .query_row(
                        "SELECT job_id, kind, conversation_id, trust_class, source_event_seq, \
                        evidence_id, payload_json, attempt, max_attempts \
                 FROM state_graphiti_jobs WHERE job_id = ?1",
                        [job_id],
                        row_to_job,
                    )
                    .optional()?)
            })
            .map_err(GraphitiStoreError::from)?
            .map(parse_job)
            .transpose()
    }

    pub fn complete(
        &self,
        job_id: &str,
        lease_owner: &str,
        now: i64,
    ) -> Result<bool, GraphitiStoreError> {
        self.finish_update(
            "UPDATE state_graphiti_jobs SET status = 'completed', lease_owner = NULL, \
             lease_expires_at = NULL, completed_at = ?3, updated_at = ?3, last_error = ?4 \
             WHERE job_id = ?1 AND status = 'running' AND lease_owner = ?2",
            job_id,
            lease_owner,
            now,
            None,
        )
    }

    pub fn retry(
        &self,
        job_id: &str,
        lease_owner: &str,
        error: &str,
        next_attempt_at: i64,
    ) -> Result<bool, GraphitiStoreError> {
        self.finish_update(
            "UPDATE state_graphiti_jobs SET \
             status = CASE WHEN attempt >= max_attempts THEN 'failed' ELSE 'pending' END, \
             lease_owner = NULL, lease_expires_at = NULL, next_attempt_at = ?3, \
             updated_at = ?3, last_error = ?4 \
             WHERE job_id = ?1 AND status = 'running' AND lease_owner = ?2",
            job_id,
            lease_owner,
            next_attempt_at,
            Some(error),
        )
    }

    fn finish_update(
        &self,
        sql: &str,
        job_id: &str,
        lease_owner: &str,
        at: i64,
        error: Option<&str>,
    ) -> Result<bool, GraphitiStoreError> {
        self.db
            .with_conn(|conn| Ok(conn.execute(sql, params![job_id, lease_owner, at, error])? == 1))
            .map_err(GraphitiStoreError::from)
    }
}

type RawJob = (
    String,
    String,
    String,
    String,
    i64,
    String,
    String,
    i64,
    i64,
);

fn row_to_job(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawJob> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
    ))
}

fn parse_job(raw: RawJob) -> Result<GraphitiJob, GraphitiStoreError> {
    Ok(GraphitiJob {
        job_id: raw.0,
        kind: GraphitiJobKind::parse(&raw.1)
            .ok_or_else(|| GraphitiStoreError::Corrupt(format!("unknown kind {}", raw.1)))?,
        conversation_id: ConversationId::from(raw.2),
        trust_class: raw.3,
        source_event_seq: EventSeq(raw.4),
        evidence_id: raw.5,
        payload: serde_json::from_str(&raw.6)
            .map_err(|error| GraphitiStoreError::Corrupt(error.to_string()))?,
        attempt: raw.7,
        max_attempts: raw.8,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::DbConfig;
    use crate::events::{EventKind, EventLog, EventRecord};
    use crate::migrations::MigrationRunner;

    fn fixture() -> (Database, ConversationId, EventSeq) {
        let db = Database::open(&DbConfig::in_memory_unencrypted()).unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        let conversation_id = ConversationId::new();
        let source_event_seq = EventSeq(1);
        let event = EventRecord::new(
            conversation_id.clone(),
            source_event_seq,
            EventKind::UserMsg,
            &serde_json::json!({"text": "remember this"}),
            Some("controller".into()),
        )
        .unwrap();
        EventLog::new(&db).append(&event).unwrap();
        (db, conversation_id, source_event_seq)
    }

    #[test]
    fn enqueue_is_durable_and_idempotent_by_evidence() {
        let (db, conversation_id, source_event_seq) = fixture();
        let store = GraphitiJobStore::new(&db);
        let new_job = NewGraphitiJob {
            kind: GraphitiJobKind::Ingest,
            conversation_id,
            trust_class: "Controller".into(),
            source_event_seq,
            evidence_id: "evidence-1".into(),
            payload: serde_json::json!({"body": "fact"}),
            max_attempts: 3,
            now: 20,
        };
        let first = store.enqueue(&new_job).unwrap();
        assert_eq!(store.enqueue(&new_job).unwrap(), first);
        let claimed = store.claim_next("worker-a", 20, 30).unwrap().unwrap();
        assert_eq!(claimed.job_id, first);
        assert_eq!(claimed.source_event_seq, source_event_seq);
        assert_eq!(claimed.evidence_id, "evidence-1");
        assert_eq!(claimed.attempt, 1);
    }

    #[test]
    fn expired_lease_is_reclaimed_after_restart() {
        let (db, conversation_id, source_event_seq) = fixture();
        let store = GraphitiJobStore::new(&db);
        store
            .enqueue(&NewGraphitiJob {
                kind: GraphitiJobKind::Reconcile,
                conversation_id,
                trust_class: "KnownTrusted".into(),
                source_event_seq,
                evidence_id: "reconcile-1".into(),
                payload: serde_json::json!({}),
                max_attempts: 3,
                now: 20,
            })
            .unwrap();
        assert!(store.claim_next("old-worker", 20, 25).unwrap().is_some());
        assert!(store.claim_next("new-worker", 24, 30).unwrap().is_none());
        let reclaimed = store.claim_next("new-worker", 25, 35).unwrap().unwrap();
        assert_eq!(reclaimed.attempt, 2);
        assert!(store.complete(&reclaimed.job_id, "old-worker", 26).unwrap() == false);
        assert!(store.complete(&reclaimed.job_id, "new-worker", 26).unwrap());
    }

    #[test]
    fn retry_requeues_until_attempt_budget_is_exhausted() {
        let (db, conversation_id, source_event_seq) = fixture();
        let store = GraphitiJobStore::new(&db);
        store
            .enqueue(&NewGraphitiJob {
                kind: GraphitiJobKind::Ingest,
                conversation_id,
                trust_class: "Controller".into(),
                source_event_seq,
                evidence_id: "evidence-retry".into(),
                payload: serde_json::json!({}),
                max_attempts: 2,
                now: 20,
            })
            .unwrap();
        let first = store.claim_next("worker", 20, 25).unwrap().unwrap();
        assert!(
            store
                .retry(&first.job_id, "worker", "temporary", 30)
                .unwrap()
        );
        assert!(store.claim_next("worker", 29, 35).unwrap().is_none());
        let second = store.claim_next("worker", 30, 35).unwrap().unwrap();
        assert_eq!(second.attempt, 2);
        assert!(
            store
                .retry(&second.job_id, "worker", "terminal", 40)
                .unwrap()
        );
        assert!(store.claim_next("worker", 40, 45).unwrap().is_none());
    }
}
