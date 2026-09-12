//! Durable retry, trace, and circuit-breaker state for tool dispatch.

use crate::db::{Database, DbError};
use crate::tool::{ToolFailure, ToolFailureKind};
use rusqlite::{OptionalExtension, params};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolInvocationDefinition {
    pub run_id: String,
    pub step_id: String,
    pub tool_name: String,
    pub integration_id: String,
    pub call_fingerprint: String,
    pub input_schema_hash: Option<String>,
    pub result_schema_hash: Option<String>,
    pub retry_budget_total: u32,
    pub now_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolInvocationRecord {
    pub run_id: String,
    pub step_id: String,
    pub tool_name: String,
    pub integration_id: String,
    pub call_fingerprint: String,
    pub repeated_call_count: u32,
    pub input_schema_hash: Option<String>,
    pub result_schema_hash: Option<String>,
    pub retry_budget_total: u32,
    pub attempts_used: u32,
    pub next_retry_at_ms: Option<i64>,
    pub backoff_ms: Option<u64>,
    pub status: String,
    pub failure: Option<ToolFailure>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CircuitPermit {
    Closed,
    HalfOpen,
    Open { retry_after_ms: u64 },
    HalfOpenBusy,
}

pub struct ToolExecutionStore<'db> {
    db: &'db Database,
}

impl<'db> ToolExecutionStore<'db> {
    pub fn new(db: &'db Database) -> Self {
        Self { db }
    }

    pub fn ensure_run_retry_budget(
        &self,
        run_id: &str,
        total_budget: u32,
        now_ms: i64,
    ) -> Result<(), DbError> {
        self.db.with_conn(|conn| {
            conn.execute(
                "INSERT OR IGNORE INTO state_tool_retry_budgets(\
                    run_id, total_budget, consumed_retries, updated_at_ms\
                 ) VALUES (?1, ?2, 0, ?3)",
                params![run_id, total_budget, now_ms],
            )?;
            let persisted: i64 = conn.query_row(
                "SELECT total_budget FROM state_tool_retry_budgets WHERE run_id = ?1",
                [run_id],
                |row| row.get(0),
            )?;
            if persisted != i64::from(total_budget) {
                return Err(DbError::Invariant(format!(
                    "run '{run_id}' retry budget changed from {persisted} to {total_budget}"
                )));
            }
            Ok(())
        })
    }

    /// Atomically reserve one retry from the run-wide budget.
    pub fn consume_run_retry(&self, run_id: &str, now_ms: i64) -> Result<bool, DbError> {
        self.db.with_conn(|conn| {
            let changed = conn.execute(
                "UPDATE state_tool_retry_budgets \
                 SET consumed_retries = consumed_retries + 1, updated_at_ms = ?2 \
                 WHERE run_id = ?1 AND consumed_retries < total_budget",
                params![run_id, now_ms],
            )?;
            Ok(changed == 1)
        })
    }

    pub fn define_invocation(
        &self,
        definition: &ToolInvocationDefinition,
    ) -> Result<ToolInvocationRecord, DbError> {
        self.db.transaction(|tx| {
            let existing = tx
                .query_row(
                    "SELECT run_id, step_id, tool_name, integration_id, call_fingerprint, \
                            repeated_call_count, input_schema_hash, result_schema_hash, \
                            retry_budget_total, attempts_used, next_retry_at_ms, backoff_ms, \
                            status, failure_kind, failure_code, failure_message, \
                            failure_retryable, failure_retry_after_ms, failure_guidance \
                     FROM state_tool_invocations WHERE run_id = ?1 AND step_id = ?2",
                    params![definition.run_id, definition.step_id],
                    row_to_invocation,
                )
                .optional()?;
            if let Some(existing) = existing {
                if existing.tool_name != definition.tool_name
                    || existing.integration_id != definition.integration_id
                    || existing.call_fingerprint != definition.call_fingerprint
                    || existing.input_schema_hash != definition.input_schema_hash
                    || existing.result_schema_hash != definition.result_schema_hash
                    || existing.retry_budget_total != definition.retry_budget_total
                {
                    return Err(DbError::Invariant(format!(
                        "tool invocation '{}:{}' was redefined with a different contract",
                        definition.run_id, definition.step_id
                    )));
                }
                return Ok(existing);
            }

            let repeated_call_count: i64 = tx.query_row(
                "SELECT COUNT(*) + 1 FROM state_tool_invocations \
                 WHERE run_id = ?1 AND call_fingerprint = ?2",
                params![definition.run_id, definition.call_fingerprint],
                |row| row.get(0),
            )?;
            tx.execute(
                "INSERT INTO state_tool_invocations (\
                    run_id, step_id, tool_name, integration_id, call_fingerprint, \
                    repeated_call_count, input_schema_hash, result_schema_hash, \
                    retry_budget_total, created_at_ms, updated_at_ms\
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10)",
                params![
                    definition.run_id,
                    definition.step_id,
                    definition.tool_name,
                    definition.integration_id,
                    definition.call_fingerprint,
                    repeated_call_count,
                    definition.input_schema_hash,
                    definition.result_schema_hash,
                    definition.retry_budget_total,
                    definition.now_ms,
                ],
            )?;
            tx.query_row(
                "SELECT run_id, step_id, tool_name, integration_id, call_fingerprint, \
                        repeated_call_count, input_schema_hash, result_schema_hash, \
                        retry_budget_total, attempts_used, next_retry_at_ms, backoff_ms, \
                        status, failure_kind, failure_code, failure_message, \
                        failure_retryable, failure_retry_after_ms, failure_guidance \
                 FROM state_tool_invocations WHERE run_id = ?1 AND step_id = ?2",
                params![definition.run_id, definition.step_id],
                row_to_invocation,
            )
            .map_err(DbError::from)
        })
    }

    pub fn get_invocation(
        &self,
        run_id: &str,
        step_id: &str,
    ) -> Result<Option<ToolInvocationRecord>, DbError> {
        self.db.with_conn(|conn| {
            conn.query_row(
                "SELECT run_id, step_id, tool_name, integration_id, call_fingerprint, \
                        repeated_call_count, input_schema_hash, result_schema_hash, \
                        retry_budget_total, attempts_used, next_retry_at_ms, backoff_ms, \
                        status, failure_kind, failure_code, failure_message, \
                        failure_retryable, failure_retry_after_ms, failure_guidance \
                 FROM state_tool_invocations WHERE run_id = ?1 AND step_id = ?2",
                params![run_id, step_id],
                row_to_invocation,
            )
            .optional()
            .map_err(DbError::from)
        })
    }

    pub fn begin_attempt(
        &self,
        run_id: &str,
        step_id: &str,
        now_ms: i64,
    ) -> Result<ToolInvocationRecord, DbError> {
        self.db.with_conn(|conn| {
            let changed = conn.execute(
                "UPDATE state_tool_invocations \
                 SET attempts_used = attempts_used + 1, status = 'running', \
                     next_retry_at_ms = NULL, backoff_ms = NULL, updated_at_ms = ?3 \
                 WHERE run_id = ?1 AND step_id = ?2 \
                   AND attempts_used < retry_budget_total \
                   AND (next_retry_at_ms IS NULL OR next_retry_at_ms <= ?3)",
                params![run_id, step_id, now_ms],
            )?;
            if changed != 1 {
                return Err(DbError::Invariant(format!(
                    "tool invocation '{run_id}:{step_id}' is not eligible for an attempt"
                )));
            }
            conn.query_row(
                "SELECT run_id, step_id, tool_name, integration_id, call_fingerprint, \
                        repeated_call_count, input_schema_hash, result_schema_hash, \
                        retry_budget_total, attempts_used, next_retry_at_ms, backoff_ms, \
                        status, failure_kind, failure_code, failure_message, \
                        failure_retryable, failure_retry_after_ms, failure_guidance \
                 FROM state_tool_invocations WHERE run_id = ?1 AND step_id = ?2",
                params![run_id, step_id],
                row_to_invocation,
            )
            .map_err(DbError::from)
        })
    }

    pub fn schedule_retry(
        &self,
        run_id: &str,
        step_id: &str,
        failure: &ToolFailure,
        next_retry_at_ms: i64,
        backoff_ms: u64,
        now_ms: i64,
    ) -> Result<(), DbError> {
        let failure = failure.clone().normalized();
        if !failure.retryable {
            return Err(DbError::Invariant(
                "terminal tool failure cannot be scheduled for retry".into(),
            ));
        }
        self.write_failure(
            run_id,
            step_id,
            "pending",
            &failure,
            Some(next_retry_at_ms),
            Some(backoff_ms),
            now_ms,
        )
    }

    pub fn complete_success(
        &self,
        run_id: &str,
        step_id: &str,
        now_ms: i64,
    ) -> Result<(), DbError> {
        self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE state_tool_invocations SET status = 'succeeded', \
                    next_retry_at_ms = NULL, backoff_ms = NULL, failure_kind = NULL, \
                    failure_code = NULL, failure_message = NULL, failure_retryable = NULL, \
                    failure_retry_after_ms = NULL, failure_guidance = NULL, updated_at_ms = ?3 \
                 WHERE run_id = ?1 AND step_id = ?2",
                params![run_id, step_id, now_ms],
            )?;
            Ok(())
        })
    }

    pub fn complete_failure(
        &self,
        run_id: &str,
        step_id: &str,
        failure: &ToolFailure,
        now_ms: i64,
    ) -> Result<(), DbError> {
        let failure = failure.clone().normalized();
        let status = if matches!(
            failure.kind,
            ToolFailureKind::PolicyDenied | ToolFailureKind::ApprovalDenied
        ) {
            "denied"
        } else {
            "failed"
        };
        self.write_failure(run_id, step_id, status, &failure, None, None, now_ms)
    }

    fn write_failure(
        &self,
        run_id: &str,
        step_id: &str,
        status: &str,
        failure: &ToolFailure,
        next_retry_at_ms: Option<i64>,
        backoff_ms: Option<u64>,
        now_ms: i64,
    ) -> Result<(), DbError> {
        self.db.with_conn(|conn| {
            let changed = conn.execute(
                "UPDATE state_tool_invocations SET status = ?3, next_retry_at_ms = ?4, \
                    backoff_ms = ?5, failure_kind = ?6, failure_code = ?7, \
                    failure_message = ?8, failure_retryable = ?9, \
                    failure_retry_after_ms = ?10, failure_guidance = ?11, updated_at_ms = ?12 \
                 WHERE run_id = ?1 AND step_id = ?2",
                params![
                    run_id,
                    step_id,
                    status,
                    next_retry_at_ms,
                    backoff_ms.and_then(|value| i64::try_from(value).ok()),
                    failure.kind.as_str(),
                    failure.code,
                    failure.message,
                    failure.retryable,
                    failure
                        .retry_after_ms
                        .and_then(|value| i64::try_from(value).ok()),
                    failure.guidance,
                    now_ms,
                ],
            )?;
            if changed != 1 {
                return Err(DbError::Invariant(format!(
                    "tool invocation '{run_id}:{step_id}' was not found"
                )));
            }
            Ok(())
        })
    }

    pub fn acquire_circuit(
        &self,
        integration_id: &str,
        run_id: &str,
        step_id: &str,
        now_ms: i64,
    ) -> Result<CircuitPermit, DbError> {
        self.db.transaction(|tx| {
            let row: Option<(String, Option<i64>, Option<String>, Option<String>)> = tx
                .query_row(
                    "SELECT state, open_until_ms, probe_run_id, probe_step_id \
                     FROM state_tool_circuits WHERE integration_id = ?1",
                    [integration_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .optional()?;
            match row {
                None => Ok(CircuitPermit::Closed),
                Some((state, _, _, _)) if state == "closed" => Ok(CircuitPermit::Closed),
                Some((state, open_until, _, _)) if state == "open" => {
                    let open_until = open_until.ok_or_else(|| {
                        DbError::Invariant(format!(
                            "open circuit '{integration_id}' has no deadline"
                        ))
                    })?;
                    if open_until > now_ms {
                        return Ok(CircuitPermit::Open {
                            retry_after_ms: u64::try_from(open_until - now_ms).unwrap_or(u64::MAX),
                        });
                    }
                    tx.execute(
                        "UPDATE state_tool_circuits SET state = 'half_open', open_until_ms = NULL, \
                            probe_run_id = ?2, probe_step_id = ?3, updated_at_ms = ?4 \
                         WHERE integration_id = ?1",
                        params![integration_id, run_id, step_id, now_ms],
                    )?;
                    Ok(CircuitPermit::HalfOpen)
                }
                Some((state, _, probe_run, probe_step)) if state == "half_open" => {
                    if probe_run.as_deref() == Some(run_id)
                        && probe_step.as_deref() == Some(step_id)
                    {
                        Ok(CircuitPermit::HalfOpen)
                    } else {
                        Ok(CircuitPermit::HalfOpenBusy)
                    }
                }
                Some((state, _, _, _)) => Err(DbError::Invariant(format!(
                    "unknown circuit state '{state}'"
                ))),
            }
        })
    }

    pub fn record_circuit_success(&self, integration_id: &str, now_ms: i64) -> Result<(), DbError> {
        self.db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO state_tool_circuits(\
                    integration_id, state, consecutive_failures, updated_at_ms\
                 ) VALUES (?1, 'closed', 0, ?2) \
                 ON CONFLICT(integration_id) DO UPDATE SET state = 'closed', \
                    consecutive_failures = 0, open_until_ms = NULL, probe_run_id = NULL, \
                    probe_step_id = NULL, updated_at_ms = excluded.updated_at_ms",
                params![integration_id, now_ms],
            )?;
            Ok(())
        })
    }

    pub fn record_circuit_failure(
        &self,
        integration_id: &str,
        threshold: u32,
        cooldown_ms: u64,
        now_ms: i64,
    ) -> Result<(), DbError> {
        let cooldown_ms = i64::try_from(cooldown_ms).unwrap_or(i64::MAX);
        self.db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO state_tool_circuits(\
                    integration_id, state, consecutive_failures, open_until_ms, updated_at_ms\
                 ) VALUES (?1, CASE WHEN ?2 <= 1 THEN 'open' ELSE 'closed' END, 1, \
                    CASE WHEN ?2 <= 1 THEN ?3 + ?4 ELSE NULL END, ?3) \
                 ON CONFLICT(integration_id) DO UPDATE SET \
                    consecutive_failures = state_tool_circuits.consecutive_failures + 1, \
                    state = CASE WHEN state_tool_circuits.state = 'half_open' \
                                      OR state_tool_circuits.consecutive_failures + 1 >= ?2 \
                                 THEN 'open' ELSE 'closed' END, \
                    open_until_ms = CASE WHEN state_tool_circuits.state = 'half_open' \
                                             OR state_tool_circuits.consecutive_failures + 1 >= ?2 \
                                        THEN ?3 + ?4 ELSE NULL END, \
                    probe_run_id = NULL, probe_step_id = NULL, updated_at_ms = ?3",
                params![integration_id, threshold, now_ms, cooldown_ms],
            )?;
            Ok(())
        })
    }
}

fn row_to_invocation(row: &rusqlite::Row<'_>) -> rusqlite::Result<ToolInvocationRecord> {
    let failure_kind: Option<String> = row.get(13)?;
    let failure = failure_kind
        .map(|kind| {
            let kind = ToolFailureKind::parse(&kind).ok_or_else(|| {
                rusqlite::Error::FromSqlConversionFailure(
                    13,
                    rusqlite::types::Type::Text,
                    format!("unknown tool failure kind '{kind}'").into(),
                )
            })?;
            Ok::<ToolFailure, rusqlite::Error>(ToolFailure {
                kind,
                code: row.get(14)?,
                message: row.get(15)?,
                retryable: row.get(16)?,
                retry_after_ms: row
                    .get::<_, Option<i64>>(17)?
                    .and_then(|value| u64::try_from(value).ok()),
                attempt: row.get::<_, i64>(9)?.try_into().unwrap_or(u32::MAX),
                guidance: row.get(18)?,
            })
        })
        .transpose()?;
    Ok(ToolInvocationRecord {
        run_id: row.get(0)?,
        step_id: row.get(1)?,
        tool_name: row.get(2)?,
        integration_id: row.get(3)?,
        call_fingerprint: row.get(4)?,
        repeated_call_count: row.get::<_, i64>(5)?.try_into().unwrap_or(u32::MAX),
        input_schema_hash: row.get(6)?,
        result_schema_hash: row.get(7)?,
        retry_budget_total: row.get::<_, i64>(8)?.try_into().unwrap_or(u32::MAX),
        attempts_used: row.get::<_, i64>(9)?.try_into().unwrap_or(u32::MAX),
        next_retry_at_ms: row.get(10)?,
        backoff_ms: row
            .get::<_, Option<i64>>(11)?
            .and_then(|value| u64::try_from(value).ok()),
        status: row.get(12)?,
        failure,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::{
        ConversationKind, ConversationRow, ConversationStore, Modality, Phase,
    };
    use crate::db::DbConfig;
    use crate::events::{EventKind, EventLog, EventRecord};
    use crate::ids::{ConversationId, EventSeq};
    use crate::migrations::MigrationRunner;
    use crate::runs::{NewRun, NewRunStep, RunStepKind, RunStore};

    fn file_db(path: &std::path::Path) -> Database {
        let db = Database::open(&DbConfig {
            path: path.into(),
            key: None,
        })
        .unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        db
    }

    fn seed_step(db: &Database, run_id: &str, step_id: &str, ordinal: i64) {
        let conversation_id = ConversationId::from("tool-contract");
        if ConversationStore::new(db)
            .get(&conversation_id)
            .unwrap()
            .is_none()
        {
            ConversationStore::new(db)
                .upsert(&ConversationRow {
                    conversation_id: conversation_id.clone(),
                    kind: ConversationKind::ControllerDM,
                    last_seq: EventSeq(0),
                    phase: Phase::Idle,
                    controller_id: None,
                    trust_class: "Controller".into(),
                    snapshot_blob: None,
                    snapshot_seq: None,
                    lease_owner: None,
                    lease_expires: None,
                    modality: Modality::Text,
                    display_name: None,
                    display_name_source: "auto".into(),
                    is_pinned: false,
                    is_ephemeral: false,
                    ephemeral_expires_at: None,
                    last_activity_at: 0,
                    context_window_policy: None,
                })
                .unwrap();
            EventLog::new(db)
                .append(
                    &EventRecord::new(
                        conversation_id.clone(),
                        EventSeq(1),
                        EventKind::UserMsg,
                        &serde_json::json!({"text":"test"}),
                        Some("operator".into()),
                    )
                    .unwrap(),
                )
                .unwrap();
        }
        let runs = RunStore::new(db);
        runs.create_run_with_id(
            run_id,
            &NewRun {
                conversation_id,
                parent_run_id: None,
                input_event_seq: EventSeq(1),
                started_at: 1,
                deadline_at: None,
            },
        )
        .unwrap();
        runs.add_step(
            run_id,
            &NewRunStep {
                step_id: step_id.into(),
                ordinal,
                kind: RunStepKind::ToolDispatch,
                input_hash: format!("input-{ordinal}"),
                approval_id: None,
                outbox_idempotency_key: None,
            },
        )
        .unwrap();
    }

    fn definition(run_id: &str, step_id: &str) -> ToolInvocationDefinition {
        ToolInvocationDefinition {
            run_id: run_id.into(),
            step_id: step_id.into(),
            tool_name: "calendar.lookup".into(),
            integration_id: "calendar".into(),
            call_fingerprint: "a".repeat(64),
            input_schema_hash: Some("b".repeat(64)),
            result_schema_hash: Some("c".repeat(64)),
            retry_budget_total: 3,
            now_ms: 1_000,
        }
    }

    #[test]
    fn retry_schedule_and_schema_trace_survive_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tool.db");
        {
            let db = file_db(&path);
            seed_step(&db, "run", "tool:0", 0);
            let store = ToolExecutionStore::new(&db);
            store.ensure_run_retry_budget("run", 5, 1_000).unwrap();
            assert!(store.consume_run_retry("run", 1_001).unwrap());
            let trace = store
                .define_invocation(&definition("run", "tool:0"))
                .unwrap();
            assert_eq!(trace.input_schema_hash, Some("b".repeat(64)));
            store.begin_attempt("run", "tool:0", 1_000).unwrap();
            let failure = ToolFailure::new(ToolFailureKind::Transient, "busy", "try later");
            store
                .schedule_retry("run", "tool:0", &failure, 1_250, 250, 1_001)
                .unwrap();
        }
        let db = file_db(&path);
        let trace = ToolExecutionStore::new(&db)
            .get_invocation("run", "tool:0")
            .unwrap()
            .unwrap();
        assert_eq!(trace.attempts_used, 1);
        assert_eq!(trace.retry_budget_total, 3);
        assert_eq!(trace.next_retry_at_ms, Some(1_250));
        assert_eq!(trace.backoff_ms, Some(250));
        assert_eq!(trace.result_schema_hash, Some("c".repeat(64)));
        assert!(
            ToolExecutionStore::new(&db)
                .consume_run_retry("run", 1_300)
                .unwrap()
        );
    }

    #[test]
    fn policy_denial_cannot_be_scheduled_for_retry() {
        let db = file_db(&tempfile::tempdir().unwrap().path().join("tool.db"));
        seed_step(&db, "run", "tool:0", 0);
        let store = ToolExecutionStore::new(&db);
        store
            .define_invocation(&definition("run", "tool:0"))
            .unwrap();
        store.begin_attempt("run", "tool:0", 1_000).unwrap();
        let mut failure = ToolFailure::new(ToolFailureKind::PolicyDenied, "denied", "no");
        failure.retryable = true;
        assert!(
            store
                .schedule_retry("run", "tool:0", &failure, 1_100, 100, 1_001)
                .is_err()
        );
        store
            .complete_failure("run", "tool:0", &failure, 1_001)
            .unwrap();
        let trace = store.get_invocation("run", "tool:0").unwrap().unwrap();
        assert_eq!(trace.status, "denied");
        assert!(!trace.failure.unwrap().retryable);
    }

    #[test]
    fn open_circuit_reopens_through_one_half_open_probe_after_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tool.db");
        {
            let db = file_db(&path);
            let store = ToolExecutionStore::new(&db);
            store
                .record_circuit_failure("calendar", 1, 500, 1_000)
                .unwrap();
            assert_eq!(
                store
                    .acquire_circuit("calendar", "run-a", "step-a", 1_100)
                    .unwrap(),
                CircuitPermit::Open {
                    retry_after_ms: 400
                }
            );
        }
        let db = file_db(&path);
        let store = ToolExecutionStore::new(&db);
        assert_eq!(
            store
                .acquire_circuit("calendar", "run-a", "step-a", 1_500)
                .unwrap(),
            CircuitPermit::HalfOpen
        );
        assert_eq!(
            store
                .acquire_circuit("calendar", "run-b", "step-b", 1_500)
                .unwrap(),
            CircuitPermit::HalfOpenBusy
        );
        store
            .record_circuit_failure("calendar", 3, 500, 1_501)
            .unwrap();
        assert_eq!(
            store
                .acquire_circuit("calendar", "run-c", "step-c", 1_600)
                .unwrap(),
            CircuitPermit::Open {
                retry_after_ms: 401
            }
        );
        assert_eq!(
            store
                .acquire_circuit("calendar", "run-c", "step-c", 2_001)
                .unwrap(),
            CircuitPermit::HalfOpen
        );
        store.record_circuit_success("calendar", 2_002).unwrap();
        assert_eq!(
            store
                .acquire_circuit("calendar", "run-d", "step-d", 2_003)
                .unwrap(),
            CircuitPermit::Closed
        );
    }
}
