//! Evidence-backed, append-only memory assertions and durable extraction jobs.

use crate::db::{Database, DbError};
use crate::ids::{ConversationId, EventSeq};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryKind {
    Profile,
    Semantic,
    Episodic,
    Procedural,
    Summary,
    Decision,
}

impl MemoryKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Profile => "profile",
            Self::Semantic => "semantic",
            Self::Episodic => "episodic",
            Self::Procedural => "procedural",
            Self::Summary => "summary",
            Self::Decision => "decision",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "profile" => Some(Self::Profile),
            "semantic" => Some(Self::Semantic),
            "episodic" => Some(Self::Episodic),
            "procedural" => Some(Self::Procedural),
            "summary" => Some(Self::Summary),
            "decision" => Some(Self::Decision),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssertionStatus {
    Proposed,
    Approved,
    Rejected,
    Retracted,
}

impl AssertionStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Proposed => "proposed",
            Self::Approved => "approved",
            Self::Rejected => "rejected",
            Self::Retracted => "retracted",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "proposed" => Some(Self::Proposed),
            "approved" => Some(Self::Approved),
            "rejected" => Some(Self::Rejected),
            "retracted" => Some(Self::Retracted),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct NewMemoryAssertion {
    pub assertion_id: String,
    pub scope: String,
    pub trust_class: String,
    pub kind: MemoryKind,
    pub subject: String,
    pub predicate: String,
    pub object: serde_json::Value,
    pub confidence: f64,
    pub status: AssertionStatus,
    pub observed_from: i64,
    pub observed_to: Option<i64>,
    pub valid_from: i64,
    pub valid_to: Option<i64>,
    pub supersedes_id: Option<String>,
    pub extraction_run_id: String,
    pub created_event_seq: EventSeq,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MemoryAssertion {
    pub assertion_id: String,
    pub scope: String,
    pub trust_class: String,
    pub kind: MemoryKind,
    pub subject: String,
    pub predicate: String,
    pub object: serde_json::Value,
    pub confidence: f64,
    pub status: AssertionStatus,
    pub observed_from: i64,
    pub observed_to: Option<i64>,
    pub valid_from: i64,
    pub valid_to: Option<i64>,
    pub supersedes_id: Option<String>,
    pub extraction_run_id: String,
    pub created_event_seq: EventSeq,
    pub created_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    DirectQuote,
    ToolResult,
    OperatorCorrection,
    Derived,
}

impl EvidenceKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::DirectQuote => "direct_quote",
            Self::ToolResult => "tool_result",
            Self::OperatorCorrection => "operator_correction",
            Self::Derived => "derived",
        }
    }
}

#[derive(Debug, Clone)]
pub struct NewMemoryEvidence {
    pub evidence_id: String,
    pub assertion_id: String,
    pub conversation_id: ConversationId,
    pub event_seq: EventSeq,
    pub payload_path: String,
    pub quote_hash: String,
    pub evidence_kind: EvidenceKind,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateEvidence {
    pub event_seq: i64,
    pub payload_path: String,
    pub quote: String,
    pub quote_hash: String,
    pub evidence_kind: EvidenceKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryCandidate {
    pub kind: MemoryKind,
    pub subject: String,
    pub predicate: String,
    pub object: serde_json::Value,
    pub confidence: f64,
    pub supersedes_id: Option<String>,
    pub evidence: Vec<CandidateEvidence>,
}

#[derive(Debug, Clone, Copy)]
pub struct MemoryExtractionPolicy {
    pub automatic_approval: bool,
    pub minimum_confidence: f64,
}

impl Default for MemoryExtractionPolicy {
    fn default() -> Self {
        Self {
            automatic_approval: false,
            minimum_confidence: 1.0,
        }
    }
}

#[derive(Debug, Error)]
pub enum MemoryAssertionError {
    #[error(transparent)]
    Db(#[from] DbError),
    #[error("invalid memory assertion: {0}")]
    Invalid(String),
    #[error("corrupt memory assertion: {0}")]
    Corrupt(String),
}

pub struct MemoryAssertionStore<'db> {
    db: &'db Database,
}

impl<'db> MemoryAssertionStore<'db> {
    pub fn new(db: &'db Database) -> Self {
        Self { db }
    }

    pub fn append(&self, assertion: &NewMemoryAssertion) -> Result<(), MemoryAssertionError> {
        if assertion.assertion_id.trim().is_empty()
            || assertion.scope.trim().is_empty()
            || assertion.trust_class.trim().is_empty()
            || assertion.subject.trim().is_empty()
            || assertion.predicate.trim().is_empty()
            || assertion.extraction_run_id.trim().is_empty()
            || !(0.0..=1.0).contains(&assertion.confidence)
        {
            return Err(MemoryAssertionError::Invalid(
                "required fields and confidence must be valid".into(),
            ));
        }
        let object_json = serde_json::to_string(&assertion.object)
            .map_err(|error| MemoryAssertionError::Invalid(error.to_string()))?;
        self.db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO memory_assertions(assertion_id, scope, trust_class, kind, subject, \
                 predicate, object_json, confidence, status, observed_from, observed_to, \
                 valid_from, valid_to, supersedes_id, extraction_run_id, created_event_seq, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
                params![
                    assertion.assertion_id,
                    assertion.scope,
                    assertion.trust_class,
                    assertion.kind.as_str(),
                    assertion.subject,
                    assertion.predicate,
                    object_json,
                    assertion.confidence,
                    assertion.status.as_str(),
                    assertion.observed_from,
                    assertion.observed_to,
                    assertion.valid_from,
                    assertion.valid_to,
                    assertion.supersedes_id,
                    assertion.extraction_run_id,
                    assertion.created_event_seq.0,
                    assertion.created_at,
                ],
            )?;
            Ok(())
        })?;
        Ok(())
    }

    pub fn add_evidence(&self, evidence: &NewMemoryEvidence) -> Result<(), MemoryAssertionError> {
        if evidence.payload_path.trim().is_empty()
            || evidence.quote_hash.len() != 64
            || !evidence
                .quote_hash
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(MemoryAssertionError::Invalid(
                "evidence requires a payload path and SHA-256 quote hash".into(),
            ));
        }
        self.db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO memory_evidence(evidence_id, assertion_id, conversation_id, event_seq, \
                 payload_path, quote_hash, evidence_kind, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    evidence.evidence_id,
                    evidence.assertion_id,
                    evidence.conversation_id.as_str(),
                    evidence.event_seq.0,
                    evidence.payload_path,
                    evidence.quote_hash.to_ascii_lowercase(),
                    evidence.evidence_kind.as_str(),
                    evidence.created_at,
                ],
            )?;
            Ok(())
        })?;
        Ok(())
    }

    /// Validate model-produced evidence against the committed job range and
    /// persist the assertion, evidence, and optional projection atomically.
    /// Scope and trust are supplied only by the host.
    pub fn persist_candidate(
        &self,
        job: &MemoryJob,
        host_scope: &str,
        host_trust_class: &str,
        candidate: &MemoryCandidate,
        policy: MemoryExtractionPolicy,
        now: i64,
    ) -> Result<String, MemoryAssertionError> {
        if job.kind != MemoryJobKind::MemoryExtract
            || host_scope.trim().is_empty()
            || host_trust_class.trim().is_empty()
            || candidate.subject.trim().is_empty()
            || candidate.predicate.trim().is_empty()
            || !(0.0..=1.0).contains(&candidate.confidence)
            || candidate.evidence.is_empty()
        {
            return Err(MemoryAssertionError::Invalid(
                "invalid extraction candidate".into(),
            ));
        }

        let object_json = serde_json::to_string(&candidate.object)
            .map_err(|error| MemoryAssertionError::Invalid(error.to_string()))?;
        let candidate_json = serde_json::to_vec(candidate)
            .map_err(|error| MemoryAssertionError::Invalid(error.to_string()))?;
        let assertion_id = hex::encode(Sha256::digest(
            [job.job_id.as_bytes(), candidate_json.as_slice()].concat(),
        ));
        let status = if policy.automatic_approval
            && candidate.kind != MemoryKind::Procedural
            && candidate.confidence >= policy.minimum_confidence
            && candidate
                .evidence
                .iter()
                .all(|item| item.evidence_kind != EvidenceKind::Derived)
        {
            AssertionStatus::Approved
        } else {
            AssertionStatus::Proposed
        };
        let projection_key = format!("{}:{}", candidate.subject, candidate.predicate);

        self.db.transaction(|tx| {
            if let Some(supersedes_id) = candidate.supersedes_id.as_deref() {
                let matches_host: bool = tx
                    .query_row(
                        "SELECT EXISTS(SELECT 1 FROM memory_assertions WHERE assertion_id = ?1 \
                         AND scope = ?2 AND trust_class = ?3)",
                        params![supersedes_id, host_scope, host_trust_class],
                        |row| row.get(0),
                    )?;
                if !matches_host {
                    return Err(DbError::Serde(
                        "superseded assertion is outside the host-derived scope or trust class"
                            .into(),
                    ));
                }
            }

            for item in &candidate.evidence {
                if item.event_seq < job.event_start_seq.0
                    || item.event_seq > job.event_end_seq.0
                    || item.quote_hash.len() != 64
                    || !item.quote_hash.bytes().all(|byte| byte.is_ascii_hexdigit())
                {
                    return Err(DbError::Serde(
                        "candidate evidence is outside the committed job range or malformed".into(),
                    ));
                }
                let payload: Vec<u8> = tx.query_row(
                    "SELECT payload FROM state_events WHERE conversation_id = ?1 AND seq = ?2",
                    params![job.conversation_id.as_str(), item.event_seq],
                    |row| row.get(0),
                )?;
                let payload: serde_json::Value = rmp_serde::from_slice(&payload)
                    .map_err(|error| DbError::Serde(format!("decode evidence payload: {error}")))?;
                let value = value_at_path(&payload, &item.payload_path).ok_or_else(|| {
                    DbError::Serde(format!("evidence path not found: {}", item.payload_path))
                })?;
                let source_quote = match value {
                    serde_json::Value::String(text) => text.clone(),
                    other => serde_json::to_string(other)
                        .map_err(|error| DbError::Serde(error.to_string()))?,
                };
                let actual_hash = hex::encode(Sha256::digest(source_quote.as_bytes()));
                if source_quote != item.quote || actual_hash != item.quote_hash.to_ascii_lowercase()
                {
                    return Err(DbError::Serde(
                        "evidence quote or SHA-256 hash does not match the committed event".into(),
                    ));
                }
            }

            tx.execute(
                "INSERT OR IGNORE INTO memory_assertions(assertion_id, scope, trust_class, kind, subject, \
                 predicate, object_json, confidence, status, observed_from, observed_to, \
                 valid_from, valid_to, supersedes_id, extraction_run_id, created_event_seq, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, NULL, ?13, ?14, ?15, ?16)",
                params![assertion_id, host_scope, host_trust_class, candidate.kind.as_str(),
                    candidate.subject, candidate.predicate, object_json, candidate.confidence,
                    status.as_str(), job.event_start_seq.0, job.event_end_seq.0, now,
                    candidate.supersedes_id, job.run_id, job.event_end_seq.0, now],
            )?;
            for (index, item) in candidate.evidence.iter().enumerate() {
                let evidence_id = hex::encode(Sha256::digest(format!(
                    "{}:{index}:{}:{}:{}",
                    assertion_id, item.event_seq, item.payload_path, item.quote_hash
                )));
                tx.execute(
                    "INSERT OR IGNORE INTO memory_evidence(evidence_id, assertion_id, conversation_id, event_seq, \
                     payload_path, quote_hash, evidence_kind, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    params![evidence_id, assertion_id, job.conversation_id.as_str(),
                        item.event_seq, item.payload_path, item.quote_hash.to_ascii_lowercase(),
                        item.evidence_kind.as_str(), now],
                )?;
            }
            if status == AssertionStatus::Approved {
                tx.execute(
                    "INSERT INTO memory_current_projection(scope, trust_class, key, assertion_id, projected_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5) ON CONFLICT(scope, trust_class, key) DO UPDATE SET \
                     assertion_id = excluded.assertion_id, projected_at = excluded.projected_at",
                    params![host_scope, host_trust_class, projection_key, assertion_id, now],
                )?;
            }
            Ok(())
        })?;
        Ok(assertion_id)
    }

    pub fn project_approved(
        &self,
        assertion_id: &str,
        key: &str,
        projected_at: i64,
    ) -> Result<(), MemoryAssertionError> {
        self.db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO memory_current_projection(scope, trust_class, key, assertion_id, projected_at) \
                 SELECT scope, trust_class, ?2, assertion_id, ?3 FROM memory_assertions \
                 WHERE assertion_id = ?1 \
                 ON CONFLICT(scope, trust_class, key) DO UPDATE SET \
                    assertion_id = excluded.assertion_id, projected_at = excluded.projected_at",
                params![assertion_id, key, projected_at],
            )?;
            Ok(())
        })?;
        Ok(())
    }

    /// Trust is constrained in the inner materialized CTE before confidence ordering.
    pub fn current_ranked(
        &self,
        scope: &str,
        trust_classes: &[&str],
        as_of: i64,
        limit: u32,
    ) -> Result<Vec<MemoryAssertion>, MemoryAssertionError> {
        if trust_classes.is_empty() {
            return Ok(Vec::new());
        }
        self.db.with_conn(|conn| {
            let placeholders = (0..trust_classes.len())
                .map(|index| format!("?{}", index + 3))
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                "WITH trusted AS MATERIALIZED (\
                    SELECT a.* FROM memory_assertions a \
                    WHERE a.scope = ?1 AND a.valid_from <= ?2 \
                      AND (a.valid_to IS NULL OR a.valid_to > ?2) \
                      AND a.status = 'approved' AND a.trust_class IN ({placeholders}) \
                      AND EXISTS (SELECT 1 FROM memory_evidence e WHERE e.assertion_id = a.assertion_id)\
                 ) SELECT t.assertion_id, t.scope, t.trust_class, t.kind, t.subject, t.predicate, \
                          t.object_json, t.confidence, t.status, t.observed_from, t.observed_to, \
                          t.valid_from, t.valid_to, t.supersedes_id, t.extraction_run_id, \
                          t.created_event_seq, t.created_at \
                   FROM trusted t \
                   WHERE NOT EXISTS (SELECT 1 FROM trusted newer WHERE newer.supersedes_id = t.assertion_id) \
                   ORDER BY t.confidence DESC, t.created_at DESC LIMIT ?{}",
                trust_classes.len() + 3
            );
            let mut values: Vec<Box<dyn rusqlite::ToSql>> =
                vec![Box::new(scope.to_owned()), Box::new(as_of)];
            for trust_class in trust_classes {
                values.push(Box::new((*trust_class).to_owned()));
            }
            values.push(Box::new(limit as i64));
            let mut stmt = conn.prepare(&sql)?;
            let raw = stmt
                .query_map(rusqlite::params_from_iter(values.iter()), row_to_assertion)?
                .collect::<Result<Vec<_>, _>>()?;
            raw.into_iter()
                .map(parse_assertion)
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| DbError::Serde(error.to_string()))
        })
        .map_err(MemoryAssertionError::from)
    }

    pub fn history(
        &self,
        scope: &str,
        trust_class: &str,
        subject: &str,
        predicate: &str,
    ) -> Result<Vec<MemoryAssertion>, MemoryAssertionError> {
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT assertion_id, scope, trust_class, kind, subject, predicate, object_json, \
                 confidence, status, observed_from, observed_to, valid_from, valid_to, supersedes_id, \
                 extraction_run_id, created_event_seq, created_at FROM memory_assertions \
                 WHERE scope = ?1 AND trust_class = ?2 AND subject = ?3 AND predicate = ?4 \
                 ORDER BY valid_from, created_at",
            )?;
            let raw = stmt
                .query_map(params![scope, trust_class, subject, predicate], row_to_assertion)?
                .collect::<Result<Vec<_>, _>>()?;
            raw.into_iter()
                .map(parse_assertion)
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| DbError::Serde(error.to_string()))
        })
        .map_err(MemoryAssertionError::from)
    }
}

fn value_at_path<'a>(value: &'a serde_json::Value, path: &str) -> Option<&'a serde_json::Value> {
    if path == "$" {
        return Some(value);
    }
    if let Some(pointer) = path.strip_prefix('/') {
        return value.pointer(&format!("/{pointer}"));
    }
    path.strip_prefix("$.")?
        .split('.')
        .try_fold(value, |current, segment| current.get(segment))
}

type RawAssertion = (
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    f64,
    String,
    i64,
    Option<i64>,
    i64,
    Option<i64>,
    Option<String>,
    String,
    i64,
    i64,
);

fn row_to_assertion(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawAssertion> {
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
        row.get(9)?,
        row.get(10)?,
        row.get(11)?,
        row.get(12)?,
        row.get(13)?,
        row.get(14)?,
        row.get(15)?,
        row.get(16)?,
    ))
}

fn parse_assertion(raw: RawAssertion) -> Result<MemoryAssertion, MemoryAssertionError> {
    Ok(MemoryAssertion {
        assertion_id: raw.0,
        scope: raw.1,
        trust_class: raw.2,
        kind: MemoryKind::parse(&raw.3).ok_or_else(|| MemoryAssertionError::Corrupt(raw.3))?,
        subject: raw.4,
        predicate: raw.5,
        object: serde_json::from_str(&raw.6)
            .map_err(|error| MemoryAssertionError::Corrupt(error.to_string()))?,
        confidence: raw.7,
        status: AssertionStatus::parse(&raw.8)
            .ok_or_else(|| MemoryAssertionError::Corrupt(raw.8))?,
        observed_from: raw.9,
        observed_to: raw.10,
        valid_from: raw.11,
        valid_to: raw.12,
        supersedes_id: raw.13,
        extraction_run_id: raw.14,
        created_event_seq: EventSeq(raw.15),
        created_at: raw.16,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryJobKind {
    MemoryExtract,
    SkillCapture,
}

impl MemoryJobKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::MemoryExtract => "memory_extract",
            Self::SkillCapture => "skill_capture",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "memory_extract" => Some(Self::MemoryExtract),
            "skill_capture" => Some(Self::SkillCapture),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct NewMemoryJob {
    pub kind: MemoryJobKind,
    pub conversation_id: ConversationId,
    pub event_start_seq: EventSeq,
    pub event_end_seq: EventSeq,
    pub run_id: String,
    pub policy_hash: String,
    pub model_hash: String,
    pub max_attempts: i64,
    pub now: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryJob {
    pub job_id: String,
    pub kind: MemoryJobKind,
    pub conversation_id: ConversationId,
    pub event_start_seq: EventSeq,
    pub event_end_seq: EventSeq,
    pub run_id: String,
    pub policy_hash: String,
    pub model_hash: String,
    pub authority_scope: Option<String>,
    pub authority_trust_class: Option<String>,
    pub attempt: i64,
    pub max_attempts: i64,
}

pub struct MemoryJobStore<'db> {
    db: &'db Database,
}

impl<'db> MemoryJobStore<'db> {
    pub fn new(db: &'db Database) -> Self {
        Self { db }
    }

    pub fn enqueue(&self, job: &NewMemoryJob) -> Result<String, MemoryAssertionError> {
        if job.event_start_seq.0 <= 0
            || job.event_end_seq.0 < job.event_start_seq.0
            || job.policy_hash.trim().is_empty()
            || job.model_hash.trim().is_empty()
            || job.max_attempts <= 0
        {
            return Err(MemoryAssertionError::Invalid(
                "invalid durable memory job".into(),
            ));
        }
        let job_id = Uuid::new_v4().to_string();
        self.db.with_conn(|conn| {
            conn.execute(
                "INSERT OR IGNORE INTO memory_jobs(job_id, kind, conversation_id, event_start_seq, \
                 event_end_seq, run_id, policy_hash, model_hash, max_attempts, next_attempt_at, created_at, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10, ?10)",
                params![job_id, job.kind.as_str(), job.conversation_id.as_str(), job.event_start_seq.0,
                    job.event_end_seq.0, job.run_id, job.policy_hash, job.model_hash, job.max_attempts, job.now],
            )?;
            conn.query_row(
                "SELECT job_id FROM memory_jobs WHERE kind = ?1 AND conversation_id = ?2 \
                 AND event_start_seq = ?3 AND event_end_seq = ?4 AND policy_hash = ?5 AND model_hash = ?6",
                params![job.kind.as_str(), job.conversation_id.as_str(), job.event_start_seq.0,
                    job.event_end_seq.0, job.policy_hash, job.model_hash],
                |row| row.get(0),
            ).map_err(DbError::from)
        }).map_err(MemoryAssertionError::from)
    }

    pub fn enqueue_extraction(
        &self,
        job: &NewMemoryJob,
        authority_scope: &str,
        authority_trust_class: &str,
    ) -> Result<String, MemoryAssertionError> {
        if job.kind != MemoryJobKind::MemoryExtract
            || job.event_start_seq.0 <= 0
            || job.event_end_seq.0 < job.event_start_seq.0
            || job.policy_hash.trim().is_empty()
            || job.model_hash.trim().is_empty()
            || authority_scope.trim().is_empty()
            || !matches!(
                authority_trust_class,
                "Controller"
                    | "Delegated"
                    | "KnownTrusted"
                    | "KnownLimited"
                    | "UnknownPending"
                    | "Blocked"
            )
        {
            return Err(MemoryAssertionError::Invalid(
                "invalid authorized memory extraction job".into(),
            ));
        }
        let job_id = Uuid::new_v4().to_string();
        self.db.with_conn(|conn| {
            conn.execute(
                "INSERT OR IGNORE INTO memory_jobs(job_id, kind, conversation_id, event_start_seq, \
                 event_end_seq, run_id, policy_hash, model_hash, max_attempts, next_attempt_at, \
                 created_at, updated_at, authority_scope, authority_trust_class) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10, ?10, ?11, ?12)",
                params![job_id, job.kind.as_str(), job.conversation_id.as_str(),
                    job.event_start_seq.0, job.event_end_seq.0, job.run_id, job.policy_hash,
                    job.model_hash, job.max_attempts, job.now, authority_scope,
                    authority_trust_class],
            )?;
            conn.query_row(
                "SELECT job_id FROM memory_jobs WHERE kind = ?1 AND conversation_id = ?2 \
                 AND event_start_seq = ?3 AND event_end_seq = ?4 AND policy_hash = ?5 AND model_hash = ?6",
                params![job.kind.as_str(), job.conversation_id.as_str(), job.event_start_seq.0,
                    job.event_end_seq.0, job.policy_hash, job.model_hash],
                |row| row.get(0),
            ).map_err(DbError::from)
        }).map_err(MemoryAssertionError::from)
    }

    pub fn claim_next(
        &self,
        kind: MemoryJobKind,
        owner: &str,
        now: i64,
        lease_expires_at: i64,
    ) -> Result<Option<MemoryJob>, MemoryAssertionError> {
        if owner.trim().is_empty() || lease_expires_at <= now {
            return Err(MemoryAssertionError::Invalid("invalid job lease".into()));
        }
        self.db.transaction(|tx| {
            let id: Option<String> = tx.query_row(
                "SELECT job_id FROM memory_jobs WHERE kind = ?1 AND attempt < max_attempts AND \
                 ((status = 'pending' AND next_attempt_at <= ?2) OR \
                  (status = 'running' AND lease_expires_at <= ?2)) \
                 ORDER BY created_at, job_id LIMIT 1",
                params![kind.as_str(), now], |row| row.get(0)).optional()?;
            let Some(id) = id else { return Ok(None); };
            let changed = tx.execute(
                "UPDATE memory_jobs SET status = 'running', attempt = attempt + 1, lease_owner = ?2, \
                 lease_expires_at = ?3, updated_at = ?1 WHERE job_id = ?4 AND attempt < max_attempts AND \
                 ((status = 'pending' AND next_attempt_at <= ?1) OR \
                  (status = 'running' AND lease_expires_at <= ?1))",
                params![now, owner, lease_expires_at, id])?;
            if changed == 0 { return Ok(None); }
            tx.query_row(
                "SELECT job_id, kind, conversation_id, event_start_seq, event_end_seq, run_id, \
                 policy_hash, model_hash, attempt, max_attempts, authority_scope, authority_trust_class \
                 FROM memory_jobs WHERE job_id = ?1",
                [id], row_to_job).optional().map_err(DbError::from)
        }).map_err(MemoryAssertionError::from)?.map(parse_job).transpose()
    }

    pub fn complete(
        &self,
        job_id: &str,
        owner: &str,
        now: i64,
    ) -> Result<bool, MemoryAssertionError> {
        self.db.with_conn(|conn| Ok(conn.execute(
            "UPDATE memory_jobs SET status = 'completed', lease_owner = NULL, lease_expires_at = NULL, \
             completed_at = ?3, updated_at = ?3, last_error = NULL \
             WHERE job_id = ?1 AND status = 'running' AND lease_owner = ?2",
            params![job_id, owner, now])? == 1)).map_err(MemoryAssertionError::from)
    }

    pub fn retry(
        &self,
        job_id: &str,
        owner: &str,
        error: &str,
        next_attempt_at: i64,
    ) -> Result<bool, MemoryAssertionError> {
        self.db.with_conn(|conn| Ok(conn.execute(
            "UPDATE memory_jobs SET status = CASE WHEN attempt >= max_attempts THEN 'failed' ELSE 'pending' END, \
             lease_owner = NULL, lease_expires_at = NULL, next_attempt_at = ?3, updated_at = ?3, last_error = ?4 \
             WHERE job_id = ?1 AND status = 'running' AND lease_owner = ?2",
            params![job_id, owner, next_attempt_at, error])? == 1)).map_err(MemoryAssertionError::from)
    }
}

type RawJob = (
    String,
    String,
    String,
    i64,
    i64,
    String,
    String,
    String,
    i64,
    i64,
    Option<String>,
    Option<String>,
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
        row.get(9)?,
        row.get(10)?,
        row.get(11)?,
    ))
}
fn parse_job(raw: RawJob) -> Result<MemoryJob, MemoryAssertionError> {
    Ok(MemoryJob {
        job_id: raw.0,
        kind: MemoryJobKind::parse(&raw.1).ok_or_else(|| MemoryAssertionError::Corrupt(raw.1))?,
        conversation_id: ConversationId::from(raw.2),
        event_start_seq: EventSeq(raw.3),
        event_end_seq: EventSeq(raw.4),
        run_id: raw.5,
        policy_hash: raw.6,
        model_hash: raw.7,
        attempt: raw.8,
        max_attempts: raw.9,
        authority_scope: raw.10,
        authority_trust_class: raw.11,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::DbConfig;
    use crate::events::{EventKind, EventLog, EventRecord};
    use crate::memory::MemoryStore;
    use crate::migrations::MigrationRunner;

    fn fresh() -> Database {
        let db = Database::open(&DbConfig::in_memory_unencrypted()).unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        db
    }

    fn event(db: &Database, conversation_id: &ConversationId, seq: i64) {
        EventLog::new(db)
            .append(
                &EventRecord::new(
                    conversation_id.clone(),
                    EventSeq(seq),
                    EventKind::UserMsg,
                    &serde_json::json!({"text": format!("evidence {seq}")}),
                    Some("controller".into()),
                )
                .unwrap(),
            )
            .unwrap();
    }

    fn assertion(
        id: &str,
        trust: &str,
        value: &str,
        supersedes: Option<&str>,
        at: i64,
    ) -> NewMemoryAssertion {
        NewMemoryAssertion {
            assertion_id: id.into(),
            scope: "principal:p1".into(),
            trust_class: trust.into(),
            kind: MemoryKind::Profile,
            subject: "p1".into(),
            predicate: "favorite_color".into(),
            object: serde_json::json!(value),
            confidence: 0.9,
            status: AssertionStatus::Approved,
            observed_from: at,
            observed_to: None,
            valid_from: at,
            valid_to: None,
            supersedes_id: supersedes.map(str::to_owned),
            extraction_run_id: format!("run-{id}"),
            created_event_seq: EventSeq(1),
            created_at: at,
        }
    }

    fn evidence(
        id: &str,
        assertion_id: &str,
        conversation_id: &ConversationId,
    ) -> NewMemoryEvidence {
        NewMemoryEvidence {
            evidence_id: id.into(),
            assertion_id: assertion_id.into(),
            conversation_id: conversation_id.clone(),
            event_seq: EventSeq(1),
            payload_path: "$.text".into(),
            quote_hash: "a".repeat(64),
            evidence_kind: EvidenceKind::DirectQuote,
            created_at: 10,
        }
    }

    fn extraction_job(conversation_id: &ConversationId) -> MemoryJob {
        MemoryJob {
            job_id: "job-1".into(),
            kind: MemoryJobKind::MemoryExtract,
            conversation_id: conversation_id.clone(),
            event_start_seq: EventSeq(1),
            event_end_seq: EventSeq(2),
            run_id: "extract-1".into(),
            policy_hash: "policy".into(),
            model_hash: "model".into(),
            authority_scope: Some("principal:p1".into()),
            authority_trust_class: Some("Controller".into()),
            attempt: 1,
            max_attempts: 5,
        }
    }

    fn candidate(quote: &str) -> MemoryCandidate {
        MemoryCandidate {
            kind: MemoryKind::Profile,
            subject: "p1".into(),
            predicate: "favorite_color".into(),
            object: serde_json::json!("blue"),
            confidence: 1.0,
            supersedes_id: None,
            evidence: vec![CandidateEvidence {
                event_seq: 1,
                payload_path: "$.text".into(),
                quote: quote.into(),
                quote_hash: hex::encode(Sha256::digest(quote.as_bytes())),
                evidence_kind: EvidenceKind::DirectQuote,
            }],
        }
    }

    #[test]
    fn extraction_rejects_invalid_evidence_without_partial_insert() {
        let db = fresh();
        let conversation_id = ConversationId::from("extract");
        event(&db, &conversation_id, 1);
        event(&db, &conversation_id, 2);
        let result = MemoryAssertionStore::new(&db).persist_candidate(
            &extraction_job(&conversation_id),
            "principal:p1",
            "Controller",
            &candidate("fabricated quote"),
            MemoryExtractionPolicy::default(),
            20,
        );
        assert!(result.is_err());
        let count: i64 = db
            .with_conn(|conn| {
                conn.query_row("SELECT COUNT(*) FROM memory_assertions", [], |row| {
                    row.get(0)
                })
                .map_err(DbError::from)
            })
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn extraction_rejects_cross_trust_supersession() {
        let db = fresh();
        let conversation_id = ConversationId::from("extract");
        event(&db, &conversation_id, 1);
        event(&db, &conversation_id, 2);
        MemoryAssertionStore::new(&db)
            .append(&assertion("controller-only", "Controller", "red", None, 10))
            .unwrap();
        let mut replacement = candidate("evidence 1");
        replacement.supersedes_id = Some("controller-only".into());
        let result = MemoryAssertionStore::new(&db).persist_candidate(
            &extraction_job(&conversation_id),
            "principal:p1",
            "KnownTrusted",
            &replacement,
            MemoryExtractionPolicy::default(),
            20,
        );
        assert!(result.is_err());
    }

    #[test]
    fn conservative_approval_projects_superseding_non_procedural_candidate() {
        let db = fresh();
        let conversation_id = ConversationId::from("extract");
        event(&db, &conversation_id, 1);
        event(&db, &conversation_id, 2);
        let store = MemoryAssertionStore::new(&db);
        let first = store
            .persist_candidate(
                &extraction_job(&conversation_id),
                "principal:p1",
                "KnownTrusted",
                &candidate("evidence 1"),
                MemoryExtractionPolicy {
                    automatic_approval: true,
                    minimum_confidence: 1.0,
                },
                10,
            )
            .unwrap();
        let mut replacement = candidate("evidence 2");
        replacement.evidence[0].event_seq = 2;
        replacement.evidence[0].quote_hash =
            hex::encode(Sha256::digest(replacement.evidence[0].quote.as_bytes()));
        replacement.supersedes_id = Some(first.clone());
        let second = store
            .persist_candidate(
                &extraction_job(&conversation_id),
                "principal:p1",
                "KnownTrusted",
                &replacement,
                MemoryExtractionPolicy {
                    automatic_approval: true,
                    minimum_confidence: 1.0,
                },
                20,
            )
            .unwrap();
        let current = store
            .current_ranked("principal:p1", &["KnownTrusted"], 21, 10)
            .unwrap();
        assert_eq!(current.len(), 1);
        assert_eq!(current[0].assertion_id, second);
        assert_eq!(current[0].supersedes_id.as_deref(), Some(first.as_str()));
    }

    #[test]
    fn projection_requires_evidence_and_existing_read_returns_approved_projection() {
        let db = fresh();
        let conversation_id = ConversationId::from("c1");
        event(&db, &conversation_id, 1);
        let store = MemoryAssertionStore::new(&db);
        store
            .append(&assertion("a1", "Controller", "blue", None, 10))
            .unwrap();
        assert!(store.project_approved("a1", "color", 11).is_err());
        store
            .add_evidence(&evidence("e1", "a1", &conversation_id))
            .unwrap();
        store.project_approved("a1", "color", 12).unwrap();

        let injected = MemoryStore::new(&db)
            .get("principal:p1", "Controller", "color")
            .unwrap()
            .unwrap();
        assert_eq!(injected.value_blob, b"blue");
    }

    #[test]
    fn correction_supersedes_current_but_history_retains_both_validities() {
        let db = fresh();
        let conversation_id = ConversationId::from("c1");
        event(&db, &conversation_id, 1);
        let store = MemoryAssertionStore::new(&db);
        let mut old = assertion("old", "KnownTrusted", "blue", None, 10);
        old.valid_to = Some(20);
        store.append(&old).unwrap();
        store
            .add_evidence(&evidence("e-old", "old", &conversation_id))
            .unwrap();
        store
            .append(&assertion("new", "KnownTrusted", "green", Some("old"), 20))
            .unwrap();
        store
            .add_evidence(&evidence("e-new", "new", &conversation_id))
            .unwrap();

        let at_15 = store
            .current_ranked("principal:p1", &["KnownTrusted"], 15, 10)
            .unwrap();
        assert_eq!(
            at_15
                .iter()
                .map(|row| row.assertion_id.as_str())
                .collect::<Vec<_>>(),
            vec!["old"]
        );
        let at_25 = store
            .current_ranked("principal:p1", &["KnownTrusted"], 25, 10)
            .unwrap();
        assert_eq!(
            at_25
                .iter()
                .map(|row| row.assertion_id.as_str())
                .collect::<Vec<_>>(),
            vec!["new"]
        );
        let history = store
            .history("principal:p1", "KnownTrusted", "p1", "favorite_color")
            .unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].valid_to, Some(20));
        assert_eq!(history[1].supersedes_id.as_deref(), Some("old"));
    }

    #[test]
    fn trust_filter_precedes_ranking_and_blocks_cross_trust_leakage() {
        let db = fresh();
        let conversation_id = ConversationId::from("c1");
        event(&db, &conversation_id, 1);
        let store = MemoryAssertionStore::new(&db);
        let mut secret = assertion("secret", "Controller", "secret", None, 20);
        secret.confidence = 1.0;
        let mut visible = assertion("visible", "KnownTrusted", "visible", None, 10);
        visible.confidence = 0.1;
        store.append(&secret).unwrap();
        store.append(&visible).unwrap();
        store
            .add_evidence(&evidence("e-secret", "secret", &conversation_id))
            .unwrap();
        store
            .add_evidence(&evidence("e-visible", "visible", &conversation_id))
            .unwrap();

        let rows = store
            .current_ranked("principal:p1", &["KnownTrusted"], 30, 1)
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].assertion_id, "visible");
        assert!(rows.iter().all(|row| row.trust_class != "Controller"));
    }

    #[test]
    fn jobs_deduplicate_claim_reclaim_retry_and_survive_reopen() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("memory-jobs.db");
        let config = DbConfig { path, key: None };
        let conversation_id = ConversationId::from("c1");
        let job = {
            let db = Database::open(&config).unwrap();
            MigrationRunner::new(&db).apply_all().unwrap();
            event(&db, &conversation_id, 1);
            event(&db, &conversation_id, 2);
            let store = MemoryJobStore::new(&db);
            let new_job = NewMemoryJob {
                kind: MemoryJobKind::SkillCapture,
                conversation_id: conversation_id.clone(),
                event_start_seq: EventSeq(1),
                event_end_seq: EventSeq(2),
                run_id: "turn-2".into(),
                policy_hash: "policy-v1".into(),
                model_hash: "small-model-v1".into(),
                max_attempts: 2,
                now: 10,
            };
            let first = store.enqueue(&new_job).unwrap();
            assert_eq!(store.enqueue(&new_job).unwrap(), first);
            let claimed = store
                .claim_next(MemoryJobKind::SkillCapture, "old", 10, 20)
                .unwrap()
                .unwrap();
            assert_eq!(claimed.attempt, 1);
            first
        };

        let db = Database::open(&config).unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        let store = MemoryJobStore::new(&db);
        assert!(
            store
                .claim_next(MemoryJobKind::SkillCapture, "new", 19, 30)
                .unwrap()
                .is_none()
        );
        let reclaimed = store
            .claim_next(MemoryJobKind::SkillCapture, "new", 20, 30)
            .unwrap()
            .unwrap();
        assert_eq!(reclaimed.job_id, job);
        assert_eq!(reclaimed.attempt, 2);
        assert!(!store.complete(&job, "old", 21).unwrap());
        assert!(store.retry(&job, "new", "terminal", 22).unwrap());
        assert!(
            store
                .claim_next(MemoryJobKind::SkillCapture, "third", 22, 30)
                .unwrap()
                .is_none()
        );
    }
}
