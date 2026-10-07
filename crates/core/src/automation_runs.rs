//! Automation run persistence (M2 of Automations).
//!
//! Owns `state_automation_runs`. One row per (automation, triggering
//! event) pair. The `step_traces` JSON is the audit log of what
//! happened during the run — per-node `(input, output, ms, error?)`
//! tuples, written before each edge advance so a crash mid-run leaves
//! a partial trail.
//!
//! Run status lifecycle:
//!
//! ```text
//!   pending  -> running -> success
//!                       \-> failed
//!                       \-> skipped   (filter dropped the run)
//! ```
//!
//! The runtime in `execlaw-server` advances the status; this module
//! is opinion-free durability.

use crate::db::{Database, DbError};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub enum AutomationRunStatus {
    Pending,
    Running,
    Success,
    Failed,
    Skipped,
}

impl AutomationRunStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Success => "success",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(Self::Pending),
            "running" => Some(Self::Running),
            "success" => Some(Self::Success),
            "failed" => Some(Self::Failed),
            "skipped" => Some(Self::Skipped),
            _ => None,
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Success | Self::Failed | Self::Skipped)
    }
}

/// One entry in `step_traces`. Written before the runtime advances
/// past the node — node-boundary checkpointing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, ToSchema)]
pub struct StepTrace {
    pub node_id: String,
    /// JSON snapshot of the inputs the node saw — used for replay /
    /// debugging. Authors of large-payload nodes should be aware
    /// this gets persisted.
    #[schema(value_type = Object)]
    pub input: serde_json::Value,
    /// JSON output of the node. For Terminal / Filter (drop) nodes
    /// this is `null`.
    #[schema(value_type = Object)]
    pub output: serde_json::Value,
    /// Wall-clock duration of the node in milliseconds.
    pub ms: u64,
    /// `Some(_)` when the node failed. Run status flips to `failed`.
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, ToSchema)]
pub struct AutomationRunRow {
    pub id: String,
    pub automation_id: String,
    pub event_id: String,
    pub status: AutomationRunStatus,
    pub step_traces: Vec<StepTrace>,
    pub started_at: i64,
    pub finished_at: Option<i64>,
}

#[derive(Debug, Error)]
pub enum AutomationRunError {
    #[error(transparent)]
    Db(#[from] DbError),
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("traces encode: {0}")]
    Encode(#[from] serde_json::Error),
}

pub struct AutomationRunStore<'a> {
    db: &'a Database,
}

impl<'a> AutomationRunStore<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    /// Mint a fresh run row in `pending` state. Returns the new id.
    pub fn insert_pending(
        &self,
        automation_id: &str,
        event_id: &str,
        started_at: i64,
    ) -> Result<String, AutomationRunError> {
        self.insert_pending_with_definition(automation_id, event_id, "{}", started_at)
    }

    /// Reuse one durable run per automation/event and preserve the exact graph
    /// definition needed to resume it after process restart.
    pub fn insert_pending_with_definition(
        &self,
        automation_id: &str,
        event_id: &str,
        definition_json: &str,
        started_at: i64,
    ) -> Result<String, AutomationRunError> {
        let id = Uuid::new_v4().to_string();
        let empty_traces = "[]";
        Ok(self.db.transaction(|tx| {
            tx.execute(
                "INSERT OR IGNORE INTO state_automation_run_claims(automation_id,event_id,run_id) \
                 VALUES (?1,?2,?3)",
                params![automation_id, event_id, &id],
            )?;
            let run_id: String = tx.query_row(
                "SELECT run_id FROM state_automation_run_claims WHERE automation_id = ?1 AND event_id = ?2",
                params![automation_id, event_id],
                |row| row.get(0),
            )?;
            if run_id == id {
                tx.execute(
                    "INSERT INTO state_automation_runs \
                     (id, automation_id, event_id, status, step_traces, started_at, definition_json) \
                     VALUES (?1, ?2, ?3, 'pending', ?4, ?5, ?6)",
                    params![&id, automation_id, event_id, empty_traces, started_at, definition_json],
                )?;
            }
            Ok(run_id)
        })?)
    }

    /// Return the graph snapshot recorded with the durable run.
    pub fn definition_snapshot(&self, run_id: &str) -> Result<Option<String>, AutomationRunError> {
        Ok(self.db.with_conn(|connection| {
            connection
                .query_row(
                    "SELECT definition_json FROM state_automation_runs WHERE id = ?1",
                    [run_id],
                    |row| row.get(0),
                )
                .map_err(Into::into)
        })?)
    }

    /// Append a step trace + flip the run to `running` if it was
    /// `pending`. Idempotent on traces — callers are responsible
    /// for calling once per node-boundary advance.
    pub fn append_trace(&self, run_id: &str, trace: &StepTrace) -> Result<(), AutomationRunError> {
        let trace_json = serde_json::to_string(trace)?;
        self.db.with_conn(|c| {
            // SQLite json_insert appends to an array via $[#]; the `#`
            // sigil is "last+1" — appends to the end. We use the
            // json() wrapper to ensure the input is treated as JSON,
            // not a quoted string.
            c.execute(
                "UPDATE state_automation_runs \
                 SET step_traces = json_insert(step_traces, '$[#]', json(?2)), \
                     status = CASE WHEN status = 'pending' THEN 'running' ELSE status END \
                 WHERE id = ?1",
                params![run_id, trace_json],
            )?;
            Ok(())
        })?;
        Ok(())
    }

    /// Flip to a terminal status + stamp `finished_at`. Idempotent.
    pub fn finish(
        &self,
        run_id: &str,
        status: AutomationRunStatus,
        finished_at: i64,
    ) -> Result<(), AutomationRunError> {
        debug_assert!(status.is_terminal(), "finish requires a terminal status");
        self.db.with_conn(|c| {
            c.execute(
                "UPDATE state_automation_runs \
                 SET status = ?2, finished_at = ?3 \
                 WHERE id = ?1",
                params![run_id, status.as_str(), finished_at],
            )?;
            Ok(())
        })?;
        Ok(())
    }

    pub fn get(&self, id: &str) -> Result<Option<AutomationRunRow>, AutomationRunError> {
        let row = self.db.with_conn(|c| {
            let mut stmt = c.prepare(
                "SELECT id, automation_id, event_id, status, step_traces, started_at, finished_at \
                 FROM state_automation_runs WHERE id = ?1",
            )?;
            let r = stmt
                .query_row([id], |r| {
                    let status_str: String = r.get(3)?;
                    let status = AutomationRunStatus::parse(&status_str).ok_or_else(|| {
                        rusqlite::Error::FromSqlConversionFailure(
                            3,
                            rusqlite::types::Type::Text,
                            format!("unknown run status: {status_str}").into(),
                        )
                    })?;
                    let traces_str: String = r.get(4)?;
                    let step_traces: Vec<StepTrace> =
                        serde_json::from_str(&traces_str).map_err(|e| {
                            rusqlite::Error::FromSqlConversionFailure(
                                4,
                                rusqlite::types::Type::Text,
                                Box::new(e),
                            )
                        })?;
                    Ok(AutomationRunRow {
                        id: r.get(0)?,
                        automation_id: r.get(1)?,
                        event_id: r.get(2)?,
                        status,
                        step_traces,
                        started_at: r.get(5)?,
                        finished_at: r.get(6)?,
                    })
                })
                .ok();
            Ok(r)
        })?;
        Ok(row)
    }

    pub fn list_for_automation(
        &self,
        automation_id: &str,
        limit: i64,
    ) -> Result<Vec<AutomationRunRow>, AutomationRunError> {
        let rows = self.db.with_conn(|c| {
            let mut stmt = c.prepare(
                "SELECT id, automation_id, event_id, status, step_traces, started_at, finished_at \
                 FROM state_automation_runs \
                 WHERE automation_id = ?1 \
                 ORDER BY started_at DESC \
                 LIMIT ?2",
            )?;
            let rows = stmt.query_map(params![automation_id, limit], |r| {
                let status_str: String = r.get(3)?;
                let status = AutomationRunStatus::parse(&status_str).ok_or_else(|| {
                    rusqlite::Error::FromSqlConversionFailure(
                        3,
                        rusqlite::types::Type::Text,
                        format!("unknown run status: {status_str}").into(),
                    )
                })?;
                let traces_str: String = r.get(4)?;
                let step_traces: Vec<StepTrace> =
                    serde_json::from_str(&traces_str).map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            4,
                            rusqlite::types::Type::Text,
                            Box::new(e),
                        )
                    })?;
                Ok(AutomationRunRow {
                    id: r.get(0)?,
                    automation_id: r.get(1)?,
                    event_id: r.get(2)?,
                    status,
                    step_traces,
                    started_at: r.get(5)?,
                    finished_at: r.get(6)?,
                })
            })?;
            let mut out = Vec::new();
            for r in rows {
                out.push(r?);
            }
            Ok(out)
        })?;
        Ok(rows)
    }

    pub fn list_failed(&self, limit: usize) -> Result<Vec<AutomationRunRow>, AutomationRunError> {
        let rows = self.db.with_conn(|connection| {
            let mut statement = connection.prepare_cached(
                "SELECT id,automation_id,event_id,status,step_traces,started_at,finished_at \
                 FROM state_automation_runs WHERE status='failed' \
                 ORDER BY COALESCE(finished_at,started_at),id LIMIT ?1",
            )?;
            let rows = statement.query_map([limit.clamp(1, 200)], |row| {
                let status = AutomationRunStatus::parse(&row.get::<_, String>(3)?)
                    .unwrap_or(AutomationRunStatus::Failed);
                let traces: String = row.get(4)?;
                let step_traces = serde_json::from_str(&traces).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        4,
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                })?;
                Ok(AutomationRunRow {
                    id: row.get(0)?,
                    automation_id: row.get(1)?,
                    event_id: row.get(2)?,
                    status,
                    step_traces,
                    started_at: row.get(5)?,
                    finished_at: row.get(6)?,
                })
            })?;
            rows.collect::<Result<Vec<_>, _>>().map_err(DbError::from)
        })?;
        Ok(rows)
    }

    /// Requeue a failed run in place, preserving its run id, frozen graph,
    /// event identity, and completed-step trace for safe resume.
    pub fn redrive_failed(
        &self,
        run_id: &str,
        actor: &str,
        reason: &str,
        now: i64,
    ) -> Result<AutomationRunRow, AutomationRunError> {
        let (actor, reason) = crate::job_redrive::validate_redrive_request(actor, reason)?;
        self.db.transaction(|tx| {
            let (automation_id, event_id): (String, String) = tx.query_row(
                "SELECT automation_id,event_id FROM state_automation_runs WHERE id=?1 AND status='failed'",
                [run_id],
                |row| Ok((row.get(0)?,row.get(1)?)),
            )?;
            let prior_redrives: i64 = tx.query_row(
                "SELECT COUNT(*) FROM state_job_redrive_events \
                 WHERE job_kind='automation' AND job_id=?1",
                [run_id],
                |row| row.get(0),
            )?;
            let changed = tx.execute(
                "UPDATE state_automation_runs SET status='pending',started_at=?2,finished_at=NULL \
                 WHERE id=?1 AND status='failed'",
                params![run_id, now],
            )?;
            if changed != 1 {
                return Err(DbError::Invariant("failed automation run changed during redrive".into()));
            }
            crate::job_redrive::append_redrive_event(
                tx,
                crate::job_redrive::RedriveKind::Automation,
                run_id,
                &format!("{automation_id}:{event_id}"),
                &actor,
                &reason,
                prior_redrives,
                now,
            )?;
            Ok(())
        })?;
        self.get(run_id)?.ok_or_else(|| {
            AutomationRunError::Db(DbError::Invariant("redriven automation run disappeared".into()))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::DbConfig;
    use crate::migrations::MigrationRunner;

    fn fresh_db() -> Database {
        let db = Database::open(&DbConfig::in_memory_unencrypted()).unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        db
    }

    fn t(node_id: &str, ms: u64) -> StepTrace {
        StepTrace {
            node_id: node_id.into(),
            input: serde_json::json!({"in": node_id}),
            output: serde_json::json!({"out": node_id}),
            ms,
            error: None,
        }
    }

    #[test]
    fn insert_pending_creates_row_with_empty_traces() {
        let db = fresh_db();
        let store = AutomationRunStore::new(&db);
        let id = store.insert_pending("auto-1", "evt-1", 100).unwrap();
        let row = store.get(&id).unwrap().unwrap();
        assert_eq!(row.automation_id, "auto-1");
        assert_eq!(row.event_id, "evt-1");
        assert_eq!(row.status, AutomationRunStatus::Pending);
        assert!(row.step_traces.is_empty());
        assert_eq!(row.started_at, 100);
        assert_eq!(row.finished_at, None);
    }

    #[test]
    fn append_trace_flips_pending_to_running_and_preserves_running() {
        let db = fresh_db();
        let store = AutomationRunStore::new(&db);
        let id = store.insert_pending("auto-1", "evt-1", 100).unwrap();
        store.append_trace(&id, &t("n1", 5)).unwrap();
        let row = store.get(&id).unwrap().unwrap();
        assert_eq!(row.status, AutomationRunStatus::Running);
        assert_eq!(row.step_traces.len(), 1);
        assert_eq!(row.step_traces[0].node_id, "n1");

        store.append_trace(&id, &t("n2", 7)).unwrap();
        let row = store.get(&id).unwrap().unwrap();
        assert_eq!(row.status, AutomationRunStatus::Running);
        assert_eq!(row.step_traces.len(), 2);
        assert_eq!(row.step_traces[1].node_id, "n2");
    }

    #[test]
    fn finish_terminal_sets_status_and_finished_at() {
        let db = fresh_db();
        let store = AutomationRunStore::new(&db);
        let id = store.insert_pending("auto-1", "evt-1", 100).unwrap();
        store.append_trace(&id, &t("n1", 5)).unwrap();
        store
            .finish(&id, AutomationRunStatus::Success, 200)
            .unwrap();
        let row = store.get(&id).unwrap().unwrap();
        assert_eq!(row.status, AutomationRunStatus::Success);
        assert_eq!(row.finished_at, Some(200));
    }

    #[test]
    fn list_for_automation_returns_descending_by_started_at() {
        let db = fresh_db();
        let store = AutomationRunStore::new(&db);
        for (index, ts) in [100, 300, 200].into_iter().enumerate() {
            store
                .insert_pending("auto-1", &format!("evt-{index}"), ts)
                .unwrap();
        }
        let rows = store.list_for_automation("auto-1", 10).unwrap();
        assert_eq!(
            rows.iter().map(|r| r.started_at).collect::<Vec<_>>(),
            vec![300, 200, 100],
        );
    }

    #[test]
    fn insert_pending_reuses_one_run_for_automation_event_pair() {
        let db = fresh_db();
        let store = AutomationRunStore::new(&db);
        let first = store
            .insert_pending_with_definition("auto-1", "evt-1", "{\"version\":1}", 100)
            .unwrap();
        let retry = store
            .insert_pending_with_definition("auto-1", "evt-1", "{\"version\":2}", 200)
            .unwrap();
        assert_eq!(first, retry);
        assert_eq!(store.list_for_automation("auto-1", 10).unwrap().len(), 1);
        assert_eq!(
            store.definition_snapshot(&first).unwrap().as_deref(),
            Some("{\"version\":1}")
        );
    }

    #[test]
    fn step_trace_with_error_round_trips() {
        let db = fresh_db();
        let store = AutomationRunStore::new(&db);
        let id = store.insert_pending("auto-1", "evt-1", 100).unwrap();
        let trace = StepTrace {
            node_id: "n1".into(),
            input: serde_json::json!({}),
            output: serde_json::Value::Null,
            ms: 12,
            error: Some("rhai parse failure at line 1: oops".into()),
        };
        store.append_trace(&id, &trace).unwrap();
        let row = store.get(&id).unwrap().unwrap();
        assert_eq!(
            row.step_traces[0].error.as_deref(),
            Some("rhai parse failure at line 1: oops")
        );
    }

    #[test]
    fn run_status_round_trip() {
        for s in [
            AutomationRunStatus::Pending,
            AutomationRunStatus::Running,
            AutomationRunStatus::Success,
            AutomationRunStatus::Failed,
            AutomationRunStatus::Skipped,
        ] {
            assert_eq!(AutomationRunStatus::parse(s.as_str()), Some(s));
        }
        assert_eq!(AutomationRunStatus::parse("garbage"), None);
    }

    #[test]
    fn failed_automation_redrive_resumes_the_original_run_and_checkpoints() {
        let db = fresh_db();
        let store = AutomationRunStore::new(&db);
        let run_id = store
            .insert_pending_with_definition("auto-1", "evt-1", "{\"version\":1}", 100)
            .unwrap();
        let checkpoint = t("send", 5);
        store.append_trace(&run_id, &checkpoint).unwrap();
        store
            .finish(&run_id, AutomationRunStatus::Failed, 110)
            .unwrap();
        let redriven = store
            .redrive_failed(&run_id, "controller-1", "sink reconciled", 120)
            .unwrap();
        assert_eq!(redriven.id, run_id);
        assert_eq!(redriven.automation_id, "auto-1");
        assert_eq!(redriven.event_id, "evt-1");
        assert_eq!(redriven.status, AutomationRunStatus::Pending);
        assert_eq!(redriven.step_traces, vec![checkpoint]);
        assert_eq!(store.list_failed(10).unwrap().len(), 0);
        assert!(store
            .redrive_failed(&run_id, "controller-1", "duplicate", 121)
            .is_err());
        let audit: (String, String, String) = db
            .with_conn(|conn| Ok(conn.query_row(
                "SELECT actor,reason,effect_identity FROM state_job_redrive_events \
                 WHERE job_kind='automation' AND job_id=?1",
                [&run_id],
                |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)),
            )?))
            .unwrap();
        assert_eq!(audit, ("controller-1".into(), "sink reconciled".into(), "auto-1:evt-1".into()));
    }
}
