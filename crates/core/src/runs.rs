//! Durable run and step execution state.
//!
//! Conversation history remains canonical in `state_events`. These tables
//! only checkpoint execution boundaries so a worker can resume without
//! repeating an effect or losing an approval wait.

use crate::db::{Database, DbError};
use crate::ids::{ConversationId, EventSeq};
use crate::outbox::OutboxRow;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

/// Lifecycle state of a durable run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunStatus {
    Pending,
    Running,
    Waiting,
    Completed,
    Failed,
    Cancelled,
}

impl RunStatus {
    /// Return the stable SQLite representation.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Waiting => "waiting",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "pending" => Some(Self::Pending),
            "running" => Some(Self::Running),
            "waiting" => Some(Self::Waiting),
            "completed" => Some(Self::Completed),
            "failed" => Some(Self::Failed),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }
}

/// Kind of checkpointed execution step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunStepKind {
    ModelRequest,
    DeterministicCompute,
    ToolDispatch,
    ApprovalWait,
    OutboxEnqueue,
    ChildRunSpawn,
    ChildRunJoin,
    ArtifactPublish,
}

impl RunStepKind {
    /// Return the stable SQLite representation.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ModelRequest => "model_request",
            Self::DeterministicCompute => "deterministic_compute",
            Self::ToolDispatch => "tool_dispatch",
            Self::ApprovalWait => "approval_wait",
            Self::OutboxEnqueue => "outbox_enqueue",
            Self::ChildRunSpawn => "child_run_spawn",
            Self::ChildRunJoin => "child_run_join",
            Self::ArtifactPublish => "artifact_publish",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "model_request" => Some(Self::ModelRequest),
            "deterministic_compute" => Some(Self::DeterministicCompute),
            "tool_dispatch" => Some(Self::ToolDispatch),
            "approval_wait" => Some(Self::ApprovalWait),
            "outbox_enqueue" => Some(Self::OutboxEnqueue),
            "child_run_spawn" => Some(Self::ChildRunSpawn),
            "child_run_join" => Some(Self::ChildRunJoin),
            "artifact_publish" => Some(Self::ArtifactPublish),
            _ => None,
        }
    }
}

/// Lifecycle state of one durable step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunStepStatus {
    Pending,
    Running,
    Waiting,
    Completed,
    Failed,
}

impl RunStepStatus {
    /// Return the stable SQLite representation.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Waiting => "waiting",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "pending" => Some(Self::Pending),
            "running" => Some(Self::Running),
            "waiting" => Some(Self::Waiting),
            "completed" => Some(Self::Completed),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }
}

/// Values required to create a durable run.
#[derive(Debug, Clone)]
pub struct NewRun {
    pub conversation_id: ConversationId,
    pub parent_run_id: Option<String>,
    pub input_event_seq: EventSeq,
    pub started_at: i64,
    pub deadline_at: Option<i64>,
}

/// Persisted durable run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunRecord {
    pub run_id: String,
    pub conversation_id: ConversationId,
    pub parent_run_id: Option<String>,
    pub status: RunStatus,
    pub cursor: i64,
    pub input_event_seq: EventSeq,
    pub started_at: i64,
    pub updated_at: i64,
    pub deadline_at: Option<i64>,
}

/// Values required to add a stable, retry-safe step definition.
#[derive(Debug, Clone)]
pub struct NewRunStep {
    pub step_id: String,
    pub ordinal: i64,
    pub kind: RunStepKind,
    pub input_hash: String,
    pub approval_id: Option<String>,
    pub outbox_idempotency_key: Option<String>,
}

/// Persisted durable step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunStepRecord {
    pub run_id: String,
    pub step_id: String,
    pub ordinal: i64,
    pub kind: RunStepKind,
    pub status: RunStepStatus,
    pub attempt: i64,
    pub input_hash: String,
    pub output_ref: Option<String>,
    pub approval_id: Option<String>,
    pub outbox_idempotency_key: Option<String>,
    pub lease_owner: Option<String>,
    pub lease_expires_at: Option<i64>,
    pub started_at: Option<i64>,
    pub completed_at: Option<i64>,
}

/// Recovery decision for the run's current cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NextSafeAction {
    Claim(RunStepRecord),
    ReclaimExpired(RunStepRecord),
    WaitForLease(RunStepRecord),
    WaitForApproval(RunStepRecord),
    Wait(RunStepRecord),
    AdvanceCursor(RunStepRecord),
    CompleteRun { cursor: i64 },
    RunCompleted,
    RunFailed,
    RunCancelled,
}

/// Typed failures from durable run persistence.
#[derive(Debug, Error)]
pub enum RunStoreError {
    #[error(transparent)]
    Db(#[from] DbError),
    #[error("{entity} '{id}' was not found")]
    NotFound { entity: &'static str, id: String },
    #[error("cannot {operation} {entity} '{id}' while it is {status}")]
    InvalidTransition {
        entity: &'static str,
        id: String,
        status: String,
        operation: &'static str,
    },
    #[error("durable run conflict: {0}")]
    Conflict(String),
    #[error("lease expiry {lease_expires_at} must be later than claim time {now}")]
    InvalidLease { now: i64, lease_expires_at: i64 },
    #[error("corrupt durable run state: {0}")]
    Corrupt(String),
}

/// SQLite-backed durable run and step store.
pub struct RunStore<'db> {
    db: &'db Database,
}

impl<'db> RunStore<'db> {
    /// Bind a store to an initialized core database.
    pub fn new(db: &'db Database) -> Self {
        Self { db }
    }

    /// Create a pending run and return its generated identifier.
    pub fn create_run(&self, new_run: &NewRun) -> Result<String, RunStoreError> {
        let run_id = Uuid::new_v4().to_string();
        self.create_run_with_id(&run_id, new_run)?;
        Ok(run_id)
    }

    /// Create a pending run with a caller-derived stable identifier.
    ///
    /// Repeating the same identifier and immutable definition returns the
    /// existing run. A conflicting definition is rejected.
    pub fn create_run_with_id(
        &self,
        run_id: &str,
        new_run: &NewRun,
    ) -> Result<RunRecord, RunStoreError> {
        self.db.transaction(|tx| {
            if let Some(parent_run_id) = &new_run.parent_run_id {
                let parent_conversation: Option<String> = tx
                    .query_row(
                        "SELECT conversation_id FROM state_runs WHERE run_id = ?1",
                        [parent_run_id],
                        |row| row.get(0),
                    )
                    .optional()?;
                match parent_conversation {
                    None => {
                        return Err(DbError::Invariant(format!(
                            "parent run '{parent_run_id}' was not found"
                        )));
                    }
                    Some(conversation_id)
                        if conversation_id != new_run.conversation_id.as_str() =>
                    {
                        return Err(DbError::Invariant(
                            "parent and child runs must share a conversation".into(),
                        ));
                    }
                    Some(_) => {}
                }
            }
            tx.execute(
                "INSERT OR IGNORE INTO state_runs \
                 (run_id, conversation_id, parent_run_id, status, cursor, input_event_seq, \
                  started_at, updated_at, deadline_at) \
                 VALUES (?1, ?2, ?3, 'pending', 0, ?4, ?5, ?5, ?6)",
                params![
                    run_id,
                    new_run.conversation_id.as_str(),
                    new_run.parent_run_id,
                    new_run.input_event_seq.0,
                    new_run.started_at,
                    new_run.deadline_at,
                ],
            )?;
            Ok(())
        })?;
        let existing = self.required_run(run_id)?;
        if existing.conversation_id != new_run.conversation_id
            || existing.parent_run_id != new_run.parent_run_id
            || existing.input_event_seq != new_run.input_event_seq
            || existing.deadline_at != new_run.deadline_at
        {
            return Err(RunStoreError::Conflict(format!(
                "run '{run_id}' was retried with a different definition"
            )));
        }
        Ok(existing)
    }

    /// Load a run by identifier.
    pub fn get_run(&self, run_id: &str) -> Result<Option<RunRecord>, RunStoreError> {
        self.db
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT run_id, conversation_id, parent_run_id, status, cursor, \
                            input_event_seq, started_at, updated_at, deadline_at \
                     FROM state_runs WHERE run_id = ?1",
                    [run_id],
                    row_to_run,
                )
                .optional()
                .map_err(DbError::from)
            })
            .map_err(RunStoreError::from)
    }

    /// Find the stable run associated with one triggering event.
    pub fn find_run_for_input(
        &self,
        conversation_id: &ConversationId,
        input_event_seq: EventSeq,
    ) -> Result<Option<RunRecord>, RunStoreError> {
        self.db
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT run_id, conversation_id, parent_run_id, status, cursor, \
                            input_event_seq, started_at, updated_at, deadline_at \
                     FROM state_runs WHERE conversation_id = ?1 AND input_event_seq = ?2 \
                     ORDER BY started_at, run_id LIMIT 1",
                    params![conversation_id.as_str(), input_event_seq.0],
                    row_to_run,
                )
                .optional()
                .map_err(DbError::from)
            })
            .map_err(RunStoreError::from)
    }

    /// List non-terminal runs in deterministic recovery order.
    pub fn list_recoverable(&self) -> Result<Vec<RunRecord>, RunStoreError> {
        self.db
            .with_conn(|conn| {
                let mut stmt = conn.prepare_cached(
                    "SELECT run_id, conversation_id, parent_run_id, status, cursor, \
                            input_event_seq, started_at, updated_at, deadline_at \
                     FROM state_runs \
                     WHERE status IN ('pending', 'running', 'waiting') \
                     ORDER BY started_at, run_id",
                )?;
                stmt.query_map([], row_to_run)?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(DbError::from)
            })
            .map_err(RunStoreError::from)
    }

    /// List direct child runs in deterministic creation order.
    pub fn list_children(&self, parent_run_id: &str) -> Result<Vec<RunRecord>, RunStoreError> {
        self.db
            .with_conn(|conn| {
                let mut stmt = conn.prepare_cached(
                    "SELECT run_id, conversation_id, parent_run_id, status, cursor, \
                            input_event_seq, started_at, updated_at, deadline_at \
                     FROM state_runs WHERE parent_run_id = ?1 ORDER BY started_at, run_id",
                )?;
                stmt.query_map([parent_run_id], row_to_run)?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(DbError::from)
            })
            .map_err(RunStoreError::from)
    }

    /// Add a step idempotently.
    ///
    /// Repeating the same `(run_id, step_id)` and immutable definition returns
    /// the existing row. Reusing an ordinal, approval id, or outbox key for a
    /// different definition is rejected.
    pub fn add_step(
        &self,
        run_id: &str,
        new_step: &NewRunStep,
    ) -> Result<RunStepRecord, RunStoreError> {
        let result = self.db.transaction(|tx| {
            let run_status: Option<String> = tx
                .query_row(
                    "SELECT status FROM state_runs WHERE run_id = ?1",
                    [run_id],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(run_status) = run_status else {
                return Err(DbError::Invariant(format!("run '{run_id}' was not found")));
            };
            if matches!(run_status.as_str(), "completed" | "failed" | "cancelled") {
                return Err(DbError::Invariant(format!(
                    "cannot add step to run '{run_id}' while it is {run_status}"
                )));
            }

            tx.execute(
                "INSERT OR IGNORE INTO state_run_steps \
                 (run_id, step_id, ordinal, kind, status, attempt, input_hash, approval_id, \
                  outbox_idempotency_key) \
                 VALUES (?1, ?2, ?3, ?4, 'pending', 0, ?5, ?6, ?7)",
                params![
                    run_id,
                    new_step.step_id,
                    new_step.ordinal,
                    new_step.kind.as_str(),
                    new_step.input_hash,
                    new_step.approval_id,
                    new_step.outbox_idempotency_key,
                ],
            )?;

            Ok(tx
                .query_row(
                    "SELECT run_id, step_id, ordinal, kind, status, attempt, input_hash, \
                        output_ref, approval_id, outbox_idempotency_key, lease_owner, \
                        lease_expires_at, started_at, completed_at \
                 FROM state_run_steps WHERE run_id = ?1 AND step_id = ?2",
                    params![run_id, new_step.step_id],
                    row_to_step,
                )
                .optional()?)
        })?;

        let Some(existing) = result else {
            return Err(RunStoreError::Conflict(format!(
                "step '{}' collides with an existing ordinal, approval id, or outbox key",
                new_step.step_id
            )));
        };
        if existing.ordinal != new_step.ordinal
            || existing.kind != new_step.kind
            || existing.input_hash != new_step.input_hash
            || existing.approval_id != new_step.approval_id
            || existing.outbox_idempotency_key != new_step.outbox_idempotency_key
        {
            return Err(RunStoreError::Conflict(format!(
                "step '{}' was retried with a different definition",
                new_step.step_id
            )));
        }
        Ok(existing)
    }

    /// Load a step by its run-scoped identifier.
    pub fn get_step(
        &self,
        run_id: &str,
        step_id: &str,
    ) -> Result<Option<RunStepRecord>, RunStoreError> {
        self.db
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT run_id, step_id, ordinal, kind, status, attempt, input_hash, \
                            output_ref, approval_id, outbox_idempotency_key, lease_owner, \
                            lease_expires_at, started_at, completed_at \
                     FROM state_run_steps WHERE run_id = ?1 AND step_id = ?2",
                    params![run_id, step_id],
                    row_to_step,
                )
                .optional()
                .map_err(DbError::from)
            })
            .map_err(RunStoreError::from)
    }

    /// Claim a pending step or reclaim a running step whose lease expired.
    pub fn claim_step(
        &self,
        run_id: &str,
        step_id: &str,
        lease_owner: &str,
        now: i64,
        lease_expires_at: i64,
    ) -> Result<RunStepRecord, RunStoreError> {
        if lease_expires_at <= now {
            return Err(RunStoreError::InvalidLease {
                now,
                lease_expires_at,
            });
        }
        let changed = self.db.transaction(|tx| {
            let changed = tx.execute(
                "UPDATE state_run_steps \
                 SET status = 'running', attempt = attempt + 1, lease_owner = ?3, \
                     lease_expires_at = ?4, started_at = COALESCE(started_at, ?5) \
                 WHERE run_id = ?1 AND step_id = ?2 \
                   AND (status = 'pending' OR (status = 'running' AND lease_expires_at <= ?5)) \
                                     AND ordinal = (SELECT cursor FROM state_runs \
                                                                    WHERE run_id = ?1 \
                                                                        AND status IN ('pending', 'running', 'waiting'))",
                params![run_id, step_id, lease_owner, lease_expires_at, now],
            )?;
            if changed == 1 {
                tx.execute(
                    "UPDATE state_runs SET status = 'running', updated_at = ?2 WHERE run_id = ?1",
                    params![run_id, now],
                )?;
            }
            Ok(changed == 1)
        })?;
        if !changed {
            return Err(self.transition_error(run_id, step_id, "claim")?);
        }
        self.required_step(run_id, step_id)
    }

    /// Mark a leased step complete without advancing the run cursor.
    pub fn complete_step(
        &self,
        run_id: &str,
        step_id: &str,
        lease_owner: &str,
        output_ref: Option<&str>,
        completed_at: i64,
    ) -> Result<RunStepRecord, RunStoreError> {
        self.finish_step(
            run_id,
            step_id,
            lease_owner,
            RunStepStatus::Completed,
            output_ref,
            completed_at,
        )
    }

    /// Durably enqueue an effect and complete its leased step atomically.
    ///
    /// Retrying an already-committed call with the same outbox row is safe and
    /// returns the completed step. The step definition must reserve the same
    /// framework-minted idempotency key as `outbox_row`.
    pub fn enqueue_outbox_and_complete_step(
        &self,
        run_id: &str,
        step_id: &str,
        lease_owner: &str,
        outbox_row: &OutboxRow,
        output_ref: Option<&str>,
        completed_at: i64,
    ) -> Result<RunStepRecord, RunStoreError> {
        let step = self.required_step(run_id, step_id)?;
        if !matches!(
            step.kind,
            RunStepKind::OutboxEnqueue | RunStepKind::ToolDispatch
        ) {
            return Err(RunStoreError::Conflict(format!(
                "step '{step_id}' is not an effect-enqueue step"
            )));
        }
        if step.outbox_idempotency_key.as_deref() != Some(outbox_row.idempotency_key.as_str()) {
            return Err(RunStoreError::Conflict(format!(
                "step '{step_id}' does not reserve outbox key '{}'",
                outbox_row.idempotency_key
            )));
        }
        let run = self.required_run(run_id)?;
        if run.conversation_id != outbox_row.conversation_id {
            return Err(RunStoreError::Conflict(
                "run and outbox row must share a conversation".into(),
            ));
        }

        self.db.transaction(|tx| {
            tx.execute(
                "INSERT OR IGNORE INTO state_outbox \
                 (idempotency_key, conversation_id, effect_kind, payload, status, attempts, \
                  next_attempt_at, last_error, enqueued_seq) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    outbox_row.idempotency_key.as_str(),
                    outbox_row.conversation_id.as_str(),
                    outbox_row.effect_kind,
                    outbox_row.payload,
                    outbox_row.status.as_str(),
                    outbox_row.attempts,
                    outbox_row.next_attempt_at,
                    outbox_row.last_error,
                    outbox_row.enqueued_seq.0,
                ],
            )?;
            let persisted: (String, String, Vec<u8>, i64) = tx.query_row(
                "SELECT conversation_id, effect_kind, payload, enqueued_seq \
                 FROM state_outbox WHERE idempotency_key = ?1",
                [outbox_row.idempotency_key.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )?;
            if persisted.0 != outbox_row.conversation_id.as_str()
                || persisted.1 != outbox_row.effect_kind
                || persisted.2 != outbox_row.payload
                || persisted.3 != outbox_row.enqueued_seq.0
            {
                return Err(DbError::Invariant(format!(
                    "outbox key '{}' was already used for a different effect",
                    outbox_row.idempotency_key
                )));
            }

            let changed = tx.execute(
                "UPDATE state_run_steps \
                 SET status = 'completed', output_ref = ?4, lease_owner = NULL, \
                     lease_expires_at = NULL, completed_at = ?5 \
                 WHERE run_id = ?1 AND step_id = ?2 \
                   AND status = 'running' AND lease_owner = ?3",
                params![run_id, step_id, lease_owner, output_ref, completed_at],
            )?;
            if changed == 1 {
                tx.execute(
                    "UPDATE state_runs SET status = 'running', updated_at = ?2 WHERE run_id = ?1",
                    params![run_id, completed_at],
                )?;
            } else {
                let status: Option<String> = tx
                    .query_row(
                        "SELECT status FROM state_run_steps WHERE run_id = ?1 AND step_id = ?2",
                        params![run_id, step_id],
                        |row| row.get(0),
                    )
                    .optional()?;
                if status.as_deref() != Some("completed") {
                    return Err(DbError::Invariant(format!(
                        "step '{run_id}/{step_id}' cannot complete from {}",
                        status.as_deref().unwrap_or("missing")
                    )));
                }
            }
            Ok(())
        })?;
        self.required_step(run_id, step_id)
    }

    /// Mark a leased step failed and fail its run in the same transaction.
    pub fn fail_step(
        &self,
        run_id: &str,
        step_id: &str,
        lease_owner: &str,
        output_ref: Option<&str>,
        completed_at: i64,
    ) -> Result<RunStepRecord, RunStoreError> {
        self.finish_step(
            run_id,
            step_id,
            lease_owner,
            RunStepStatus::Failed,
            output_ref,
            completed_at,
        )
    }

    /// Persist a leased step as waiting and release its worker lease.
    pub fn wait_step(
        &self,
        run_id: &str,
        step_id: &str,
        lease_owner: &str,
        approval_id: Option<&str>,
        updated_at: i64,
    ) -> Result<RunStepRecord, RunStoreError> {
        let step = self.required_step(run_id, step_id)?;
        if step.kind == RunStepKind::ApprovalWait
            && approval_id.or(step.approval_id.as_deref()).is_none()
        {
            return Err(RunStoreError::Conflict(
                "approval_wait steps require an approval_id".into(),
            ));
        }
        let changed = self.db.transaction(|tx| {
            let changed = tx.execute(
                "UPDATE state_run_steps \
                 SET status = 'waiting', approval_id = COALESCE(?4, approval_id), \
                     lease_owner = NULL, lease_expires_at = NULL \
                 WHERE run_id = ?1 AND step_id = ?2 \
                   AND status = 'running' AND lease_owner = ?3",
                params![run_id, step_id, lease_owner, approval_id],
            )?;
            if changed == 1 {
                tx.execute(
                    "UPDATE state_runs SET status = 'waiting', updated_at = ?2 WHERE run_id = ?1",
                    params![run_id, updated_at],
                )?;
            }
            Ok(changed == 1)
        })?;
        if !changed {
            return Err(self.transition_error(run_id, step_id, "wait")?);
        }
        self.required_step(run_id, step_id)
    }

    /// Return a waiting step to pending after its durable wait is satisfied.
    pub fn resume_waiting_step(
        &self,
        run_id: &str,
        step_id: &str,
        approval_id: Option<&str>,
        updated_at: i64,
    ) -> Result<RunStepRecord, RunStoreError> {
        let changed = self.db.transaction(|tx| {
            let changed = tx.execute(
                "UPDATE state_run_steps SET status = 'pending' \
                 WHERE run_id = ?1 AND step_id = ?2 AND status = 'waiting' \
                   AND (approval_id IS NULL OR approval_id = ?3)",
                params![run_id, step_id, approval_id],
            )?;
            if changed == 1 {
                tx.execute(
                    "UPDATE state_runs SET status = 'pending', updated_at = ?2 WHERE run_id = ?1",
                    params![run_id, updated_at],
                )?;
            }
            Ok(changed == 1)
        })?;
        if !changed {
            return Err(self.transition_error(run_id, step_id, "resume")?);
        }
        self.required_step(run_id, step_id)
    }

    /// Advance from a completed cursor step and update run state atomically.
    pub fn advance_cursor(
        &self,
        run_id: &str,
        expected_cursor: i64,
        updated_at: i64,
    ) -> Result<RunRecord, RunStoreError> {
        let advanced = self.db.transaction(|tx| {
            let current_status: Option<String> = tx
                .query_row(
                    "SELECT status FROM state_run_steps WHERE run_id = ?1 AND ordinal = ?2",
                    params![run_id, expected_cursor],
                    |row| row.get(0),
                )
                .optional()?;
            if current_status.as_deref() != Some("completed") {
                return Ok(false);
            }
            let changed = tx.execute(
                "UPDATE state_runs \
                 SET cursor = cursor + 1, updated_at = ?3, status = 'pending' \
                 WHERE run_id = ?1 AND cursor = ?2 \
                   AND status IN ('pending', 'running', 'waiting')",
                params![run_id, expected_cursor, updated_at],
            )?;
            Ok(changed == 1)
        })?;
        if !advanced {
            let run = self.required_run(run_id)?;
            return Err(RunStoreError::InvalidTransition {
                entity: "run",
                id: run_id.to_owned(),
                status: run.status.as_str().to_owned(),
                operation: "advance cursor",
            });
        }
        self.required_run(run_id)
    }

    /// Mark a run complete only when its current cursor has no defined step.
    pub fn complete_run(
        &self,
        run_id: &str,
        expected_cursor: i64,
        completed_at: i64,
    ) -> Result<RunRecord, RunStoreError> {
        let changed = self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE state_runs SET status = 'completed', updated_at = ?3 \
                 WHERE run_id = ?1 AND cursor = ?2 \
                   AND status IN ('pending', 'running', 'waiting') \
                   AND NOT EXISTS (SELECT 1 FROM state_run_steps \
                                   WHERE run_id = ?1 AND ordinal = ?2)",
                params![run_id, expected_cursor, completed_at],
            )
            .map_err(DbError::from)
        })?;
        if changed != 1 {
            let run = self.required_run(run_id)?;
            return Err(RunStoreError::InvalidTransition {
                entity: "run",
                id: run_id.to_owned(),
                status: run.status.as_str().to_owned(),
                operation: "complete",
            });
        }
        self.required_run(run_id)
    }

    /// Explain the only recovery-safe action at the run's current cursor.
    pub fn next_safe_action(
        &self,
        run_id: &str,
        now: i64,
    ) -> Result<NextSafeAction, RunStoreError> {
        let run = self.required_run(run_id)?;
        match run.status {
            RunStatus::Completed => return Ok(NextSafeAction::RunCompleted),
            RunStatus::Failed => return Ok(NextSafeAction::RunFailed),
            RunStatus::Cancelled => return Ok(NextSafeAction::RunCancelled),
            RunStatus::Pending | RunStatus::Running | RunStatus::Waiting => {}
        }
        let step = self.db.with_conn(|conn| {
            conn.query_row(
                "SELECT run_id, step_id, ordinal, kind, status, attempt, input_hash, \
                        output_ref, approval_id, outbox_idempotency_key, lease_owner, \
                        lease_expires_at, started_at, completed_at \
                 FROM state_run_steps WHERE run_id = ?1 AND ordinal = ?2",
                params![run_id, run.cursor],
                row_to_step,
            )
            .optional()
            .map_err(DbError::from)
        })?;
        let Some(step) = step else {
            return Ok(NextSafeAction::CompleteRun { cursor: run.cursor });
        };
        Ok(match step.status {
            RunStepStatus::Pending => NextSafeAction::Claim(step),
            RunStepStatus::Running if step.lease_expires_at.is_some_and(|expiry| expiry <= now) => {
                NextSafeAction::ReclaimExpired(step)
            }
            RunStepStatus::Running => NextSafeAction::WaitForLease(step),
            RunStepStatus::Waiting if step.approval_id.is_some() => {
                NextSafeAction::WaitForApproval(step)
            }
            RunStepStatus::Waiting => NextSafeAction::Wait(step),
            RunStepStatus::Completed => NextSafeAction::AdvanceCursor(step),
            RunStepStatus::Failed => NextSafeAction::RunFailed,
        })
    }

    fn finish_step(
        &self,
        run_id: &str,
        step_id: &str,
        lease_owner: &str,
        status: RunStepStatus,
        output_ref: Option<&str>,
        completed_at: i64,
    ) -> Result<RunStepRecord, RunStoreError> {
        let changed = self.db.transaction(|tx| {
            let changed = tx.execute(
                "UPDATE state_run_steps \
                 SET status = ?4, output_ref = ?5, lease_owner = NULL, lease_expires_at = NULL, \
                     completed_at = ?6 \
                 WHERE run_id = ?1 AND step_id = ?2 \
                   AND status = 'running' AND lease_owner = ?3",
                params![
                    run_id,
                    step_id,
                    lease_owner,
                    status.as_str(),
                    output_ref,
                    completed_at,
                ],
            )?;
            if changed == 1 {
                let run_status = if status == RunStepStatus::Failed {
                    "failed"
                } else {
                    "running"
                };
                tx.execute(
                    "UPDATE state_runs SET status = ?2, updated_at = ?3 WHERE run_id = ?1",
                    params![run_id, run_status, completed_at],
                )?;
            }
            Ok(changed == 1)
        })?;
        if !changed {
            return Err(self.transition_error(run_id, step_id, status.as_str())?);
        }
        self.required_step(run_id, step_id)
    }

    fn required_run(&self, run_id: &str) -> Result<RunRecord, RunStoreError> {
        self.get_run(run_id)?
            .ok_or_else(|| RunStoreError::NotFound {
                entity: "run",
                id: run_id.to_owned(),
            })
    }

    fn required_step(&self, run_id: &str, step_id: &str) -> Result<RunStepRecord, RunStoreError> {
        self.get_step(run_id, step_id)?
            .ok_or_else(|| RunStoreError::NotFound {
                entity: "step",
                id: format!("{run_id}/{step_id}"),
            })
    }

    fn transition_error(
        &self,
        run_id: &str,
        step_id: &str,
        operation: &'static str,
    ) -> Result<RunStoreError, RunStoreError> {
        let step = self.required_step(run_id, step_id)?;
        Ok(RunStoreError::InvalidTransition {
            entity: "step",
            id: format!("{run_id}/{step_id}"),
            status: step.status.as_str().to_owned(),
            operation,
        })
    }
}

fn row_to_run(row: &rusqlite::Row<'_>) -> rusqlite::Result<RunRecord> {
    let status: String = row.get(3)?;
    Ok(RunRecord {
        run_id: row.get(0)?,
        conversation_id: ConversationId::from_string(row.get::<_, String>(1)?),
        parent_run_id: row.get(2)?,
        status: RunStatus::parse(&status).ok_or_else(|| invalid_text(3, "run status", &status))?,
        cursor: row.get(4)?,
        input_event_seq: EventSeq(row.get(5)?),
        started_at: row.get(6)?,
        updated_at: row.get(7)?,
        deadline_at: row.get(8)?,
    })
}

fn row_to_step(row: &rusqlite::Row<'_>) -> rusqlite::Result<RunStepRecord> {
    let kind: String = row.get(3)?;
    let status: String = row.get(4)?;
    Ok(RunStepRecord {
        run_id: row.get(0)?,
        step_id: row.get(1)?,
        ordinal: row.get(2)?,
        kind: RunStepKind::parse(&kind).ok_or_else(|| invalid_text(3, "step kind", &kind))?,
        status: RunStepStatus::parse(&status)
            .ok_or_else(|| invalid_text(4, "step status", &status))?,
        attempt: row.get(5)?,
        input_hash: row.get(6)?,
        output_ref: row.get(7)?,
        approval_id: row.get(8)?,
        outbox_idempotency_key: row.get(9)?,
        lease_owner: row.get(10)?,
        lease_expires_at: row.get(11)?,
        started_at: row.get(12)?,
        completed_at: row.get(13)?,
    })
}

fn invalid_text(index: usize, field: &str, value: &str) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        index,
        rusqlite::types::Type::Text,
        format!("unknown {field}: {value}").into(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::DbConfig;
    use crate::ids::IdempotencyKey;
    use crate::migrations::MigrationRunner;
    use crate::outbox::OutboxStatus;

    fn fresh_db() -> Database {
        let db = Database::open(&DbConfig::in_memory_unencrypted()).unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        seed_conversation(&db);
        db
    }

    fn seed_conversation(db: &Database) {
        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO state_conversations \
                 (conversation_id, kind, phase, trust_class, modality) \
                 VALUES ('conversation-1', 'ControllerDM', 'idle', 'Controller', 'Text')",
                [],
            )?;
            conn.execute(
                "INSERT INTO state_events \
                 (conversation_id, seq, kind, payload, committed_at, actor) \
                 VALUES ('conversation-1', 1, 'user_msg', X'00', 10, 'operator')",
                [],
            )?;
            Ok(())
        })
        .unwrap();
    }

    fn create_run(store: &RunStore<'_>, parent_run_id: Option<String>) -> String {
        store
            .create_run(&NewRun {
                conversation_id: ConversationId::from("conversation-1"),
                parent_run_id,
                input_event_seq: EventSeq(1),
                started_at: 10,
                deadline_at: None,
            })
            .unwrap()
    }

    fn step(step_id: &str, ordinal: i64, kind: RunStepKind) -> NewRunStep {
        NewRunStep {
            step_id: step_id.into(),
            ordinal,
            kind,
            input_hash: format!("hash-{step_id}"),
            approval_id: None,
            outbox_idempotency_key: None,
        }
    }

    #[test]
    fn lease_excludes_other_workers_and_expiry_allows_reclaim() {
        let db = fresh_db();
        let store = RunStore::new(&db);
        let run_id = create_run(&store, None);
        store
            .add_step(&run_id, &step("model", 0, RunStepKind::ModelRequest))
            .unwrap();

        let first = store
            .claim_step(&run_id, "model", "worker-a", 100, 200)
            .unwrap();
        assert_eq!(first.attempt, 1);
        assert!(matches!(
            store.claim_step(&run_id, "model", "worker-b", 150, 250),
            Err(RunStoreError::InvalidTransition { .. })
        ));

        let reclaimed = store
            .claim_step(&run_id, "model", "worker-b", 200, 300)
            .unwrap();
        assert_eq!(reclaimed.attempt, 2);
        assert_eq!(reclaimed.lease_owner.as_deref(), Some("worker-b"));
        assert!(matches!(
            store.complete_step(&run_id, "model", "worker-a", None, 210),
            Err(RunStoreError::InvalidTransition { .. })
        ));
    }

    #[test]
    fn approval_wait_is_durable_and_resumable() {
        let db = fresh_db();
        let store = RunStore::new(&db);
        let run_id = create_run(&store, None);
        store
            .add_step(&run_id, &step("approval", 0, RunStepKind::ApprovalWait))
            .unwrap();
        store
            .claim_step(&run_id, "approval", "worker-a", 100, 200)
            .unwrap();
        store
            .wait_step(&run_id, "approval", "worker-a", Some("approval-1"), 110)
            .unwrap();

        let action = store.next_safe_action(&run_id, i64::MAX).unwrap();
        assert!(matches!(action, NextSafeAction::WaitForApproval(_)));
        let waiting = store.get_step(&run_id, "approval").unwrap().unwrap();
        assert_eq!(waiting.approval_id.as_deref(), Some("approval-1"));
        assert_eq!(waiting.lease_owner, None);

        let resumed = store
            .resume_waiting_step(&run_id, "approval", Some("approval-1"), 120)
            .unwrap();
        assert_eq!(resumed.status, RunStepStatus::Pending);
    }

    #[test]
    fn outbox_step_definition_and_key_are_idempotent() {
        let db = fresh_db();
        let store = RunStore::new(&db);
        let run_id = create_run(&store, None);
        let mut enqueue = step("enqueue", 0, RunStepKind::OutboxEnqueue);
        enqueue.outbox_idempotency_key = Some("effect-key-1".into());

        let first = store.add_step(&run_id, &enqueue).unwrap();
        let repeated = store.add_step(&run_id, &enqueue).unwrap();
        assert_eq!(first, repeated);

        store
            .claim_step(&run_id, "enqueue", "worker-a", 100, 200)
            .unwrap();
        let outbox_row = OutboxRow {
            id: None,
            idempotency_key: IdempotencyKey::from_string("effect-key-1"),
            conversation_id: ConversationId::from("conversation-1"),
            effect_kind: "transport.send".into(),
            payload: b"payload".to_vec(),
            status: OutboxStatus::Pending,
            attempts: 0,
            next_attempt_at: None,
            last_error: None,
            enqueued_seq: EventSeq(1),
        };
        store
            .enqueue_outbox_and_complete_step(
                &run_id,
                "enqueue",
                "worker-a",
                &outbox_row,
                Some("outbox:effect-key-1"),
                120,
            )
            .unwrap();
        store
            .enqueue_outbox_and_complete_step(
                &run_id,
                "enqueue",
                "worker-a",
                &outbox_row,
                Some("outbox:effect-key-1"),
                120,
            )
            .unwrap();
        let outbox_count: i64 = db
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM state_outbox WHERE idempotency_key = 'effect-key-1'",
                    [],
                    |row| row.get(0),
                )
                .map_err(DbError::from)
            })
            .unwrap();
        assert_eq!(outbox_count, 1);

        let mut duplicate_key = step("enqueue-again", 1, RunStepKind::OutboxEnqueue);
        duplicate_key.outbox_idempotency_key = Some("effect-key-1".into());
        assert!(matches!(
            store.add_step(&run_id, &duplicate_key),
            Err(RunStoreError::Conflict(_))
        ));
    }

    #[test]
    fn parent_child_relation_round_trips() {
        let db = fresh_db();
        let store = RunStore::new(&db);
        let parent_id = create_run(&store, None);
        let child_id = create_run(&store, Some(parent_id.clone()));
        let child = store.get_run(&child_id).unwrap().unwrap();
        assert_eq!(child.parent_run_id.as_deref(), Some(parent_id.as_str()));
    }

    #[test]
    fn invalid_transitions_do_not_mutate_step() {
        let db = fresh_db();
        let store = RunStore::new(&db);
        let run_id = create_run(&store, None);
        store
            .add_step(
                &run_id,
                &step("compute", 0, RunStepKind::DeterministicCompute),
            )
            .unwrap();
        store
            .add_step(&run_id, &step("future", 1, RunStepKind::ModelRequest))
            .unwrap();

        assert!(matches!(
            store.complete_step(&run_id, "compute", "worker-a", None, 100),
            Err(RunStoreError::InvalidTransition { .. })
        ));
        assert!(matches!(
            store.claim_step(&run_id, "future", "worker-a", 100, 200),
            Err(RunStoreError::InvalidTransition { .. })
        ));
        let untouched = store.get_step(&run_id, "compute").unwrap().unwrap();
        assert_eq!(untouched.status, RunStepStatus::Pending);
        assert_eq!(untouched.attempt, 0);
    }

    #[test]
    fn completion_checkpoint_explains_and_advances_cursor() {
        let db = fresh_db();
        let store = RunStore::new(&db);
        let run_id = create_run(&store, None);
        store
            .add_step(
                &run_id,
                &step("compute", 0, RunStepKind::DeterministicCompute),
            )
            .unwrap();
        store
            .claim_step(&run_id, "compute", "worker-a", 100, 200)
            .unwrap();
        store
            .complete_step(&run_id, "compute", "worker-a", Some("blob:1"), 120)
            .unwrap();

        assert!(matches!(
            store.next_safe_action(&run_id, 120).unwrap(),
            NextSafeAction::AdvanceCursor(_)
        ));
        let run = store.advance_cursor(&run_id, 0, 121).unwrap();
        assert_eq!(run.cursor, 1);
        assert_eq!(run.status, RunStatus::Pending);
        assert!(matches!(
            store.next_safe_action(&run_id, 121).unwrap(),
            NextSafeAction::CompleteRun { cursor: 1 }
        ));
        let run = store.complete_run(&run_id, 1, 122).unwrap();
        assert_eq!(run.status, RunStatus::Completed);
    }

    #[test]
    fn file_backed_reopen_recovers_expired_lease() {
        let dir = tempfile::tempdir().unwrap();
        let config = DbConfig {
            path: dir.path().join("runs.db"),
            key: None,
        };
        let db = Database::open(&config).unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        seed_conversation(&db);
        let run_id = {
            let store = RunStore::new(&db);
            let run_id = create_run(&store, None);
            store
                .add_step(&run_id, &step("tool", 0, RunStepKind::ToolDispatch))
                .unwrap();
            store
                .claim_step(&run_id, "tool", "worker-a", 100, 150)
                .unwrap();
            run_id
        };
        drop(db);

        let reopened = Database::open(&config).unwrap();
        MigrationRunner::new(&reopened).apply_all().unwrap();
        let store = RunStore::new(&reopened);
        assert!(matches!(
            store.next_safe_action(&run_id, 151).unwrap(),
            NextSafeAction::ReclaimExpired(_)
        ));
        let step = store
            .claim_step(&run_id, "tool", "worker-b", 151, 250)
            .unwrap();
        assert_eq!(step.attempt, 2);
        assert_eq!(step.lease_owner.as_deref(), Some("worker-b"));
    }
}
