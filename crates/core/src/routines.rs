//! Routines (cron-shaped agent automations) — §5.6.
//!
//! Operator-authored cron schedules paired with prompts the agent
//! runs on its own. The store is the durability layer; the
//! `RoutineRunner` (in the server crate) ticks every minute, picks
//! the routines whose `next_run_at` has elapsed, and dispatches the
//! prompt as a controller turn.
//!
//! Cron parsing uses the `cron` crate. We restrict to standard
//! 5-field syntax (`minute hour day-of-month month day-of-week`) at
//! the validation layer for predictable operator UX, even though the
//! crate accepts 6- and 7-field forms. The operator's chosen IANA
//! timezone is used for evaluation; `next_run_at` is stored as Unix
//! UTC so the scheduler tick is timezone-agnostic.

use crate::db::{Database, DbError};
use crate::runs::RunCompletionContractDraft;
use chrono::{DateTime, TimeZone, Utc};
use chrono_tz::Tz;
use cron::Schedule;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use thiserror::Error;
use utoipa::ToSchema;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RoutineRunStatus {
    Pending,
    Success,
    Failed,
    Skipped,
}

/// Defines how a scheduler handles occurrences that passed during downtime.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum MissedRunPolicy {
    #[default]
    Skip,
    Coalesce,
    CatchUp,
}

impl MissedRunPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Skip => "skip",
            Self::Coalesce => "coalesce",
            Self::CatchUp => "catch_up",
        }
    }
    fn parse(value: &str) -> Option<Self> {
        match value {
            "skip" => Some(Self::Skip),
            "coalesce" => Some(Self::Coalesce),
            "catch_up" => Some(Self::CatchUp),
            _ => None,
        }
    }
}

/// Defines how a routine handles a new occurrence while a prior run is active.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RoutineOverlapPolicy {
    #[default]
    Forbid,
    Queue,
    Replace,
}

impl RoutineOverlapPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Forbid => "forbid",
            Self::Queue => "queue",
            Self::Replace => "replace",
        }
    }
    fn parse(value: &str) -> Option<Self> {
        match value {
            "forbid" => Some(Self::Forbid),
            "queue" => Some(Self::Queue),
            "replace" => Some(Self::Replace),
            _ => None,
        }
    }
}

impl RoutineRunStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "Pending",
            Self::Success => "Success",
            Self::Failed => "Failed",
            Self::Skipped => "Skipped",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "Pending" => Some(Self::Pending),
            "Success" => Some(Self::Success),
            "Failed" => Some(Self::Failed),
            "Skipped" => Some(Self::Skipped),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RoutineRow {
    pub id: String,
    pub name: String,
    pub schedule_cron: String,
    pub timezone: String,
    pub prompt: String,
    pub target_conversation_id: Option<String>,
    pub enabled: bool,
    pub last_run_at: Option<i64>,
    pub last_run_status: Option<RoutineRunStatus>,
    pub next_run_at: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
    pub completion_contract: Option<RunCompletionContractDraft>,
    pub missed_run_policy: MissedRunPolicy,
    pub missed_run_limit: u32,
    pub overlap_policy: RoutineOverlapPolicy,
}

#[derive(Debug, Clone)]
pub struct RoutineUpsert {
    /// `None` on create (id is minted server-side), `Some` on update.
    pub id: Option<String>,
    pub name: String,
    pub schedule_cron: String,
    pub timezone: String,
    pub prompt: String,
    pub target_conversation_id: Option<String>,
    pub enabled: bool,
    pub completion_contract: Option<RunCompletionContractDraft>,
    pub missed_run_policy: MissedRunPolicy,
    pub missed_run_limit: u32,
    pub overlap_policy: RoutineOverlapPolicy,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RoutineRunRow {
    pub id: String,
    pub routine_id: String,
    pub fired_at: i64,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
    pub status: RoutineRunStatus,
    pub error: Option<String>,
    pub conversation_id: Option<String>,
    pub occurrence_at: Option<i64>,
}

#[derive(Debug, Error)]
pub enum RoutineError {
    #[error(transparent)]
    Db(#[from] DbError),
    #[error("rusqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("invalid routine: {0}")]
    Invalid(String),
    #[error("routine not found: {0}")]
    NotFound(String),
}

/// Parse a cron expression in the standard 5-field form. Reject
/// 6- and 7-field forms even though the underlying crate accepts
/// them, so the operator UX is predictable.
pub fn parse_cron(expr: &str) -> Result<Schedule, RoutineError> {
    let trimmed = expr.trim();
    let field_count = trimmed.split_whitespace().count();
    if field_count != 5 {
        return Err(RoutineError::Invalid(format!(
            "cron expression must have 5 fields (minute hour day-of-month month day-of-week), got {field_count}"
        )));
    }
    // The `cron` crate expects 6- or 7-field syntax (it requires a
    // seconds field). Prepend `0` so 5-field operator input parses
    // as "fire at second 0 of the minute."
    let promoted = format!("0 {trimmed}");
    Schedule::from_str(&promoted)
        .map_err(|e| RoutineError::Invalid(format!("cron parse failed: {e}")))
}

/// Validate an IANA tz string. Returns the parsed `Tz` so callers
/// can plug it straight into the cron evaluator.
pub fn parse_timezone(tz: &str) -> Result<Tz, RoutineError> {
    Tz::from_str(tz).map_err(|e| RoutineError::Invalid(format!("invalid timezone '{tz}': {e}")))
}

/// Compute the next fire time strictly after `after` for the given
/// schedule, evaluated in `tz`. Returns `None` if the schedule has
/// no upcoming occurrence in the next ~10 years (catches typos like
/// `0 0 30 2 *` that would never fire).
pub fn next_fire_after(schedule: &Schedule, tz: Tz, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let after_local = after.with_timezone(&tz);
    schedule
        .after(&after_local)
        .next()
        .map(|t| t.with_timezone(&Utc))
}

/// Compute the next N fire times for the SPA preview pane.
pub fn next_n_fires(
    schedule: &Schedule,
    tz: Tz,
    after: DateTime<Utc>,
    n: usize,
) -> Vec<DateTime<Utc>> {
    let after_local = after.with_timezone(&tz);
    schedule
        .after(&after_local)
        .take(n)
        .map(|t| t.with_timezone(&Utc))
        .collect()
}

/// Deterministic result of reconciling scheduled occurrences after a delayed tick.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DueOccurrencePlan {
    pub execute: Vec<i64>,
    pub skip: Vec<i64>,
}

/// Apply a missed-run policy to sorted, due UTC occurrence timestamps.
/// An occurrence up to 60 seconds late is treated as an ordinary scheduler
/// delay; older occurrences are handled by the configured downtime policy.
pub fn plan_due_occurrences(
    policy: MissedRunPolicy,
    missed_run_limit: u32,
    now: i64,
    occurrences: &[i64],
) -> DueOccurrencePlan {
    let due: Vec<i64> = occurrences
        .iter()
        .copied()
        .filter(|at| *at <= now)
        .collect();
    let (ordinary, missed): (Vec<_>, Vec<_>) = due
        .iter()
        .copied()
        .partition(|at| now.saturating_sub(*at) <= 60);
    let mut execute = Vec::new();
    let mut skip = Vec::new();
    match policy {
        MissedRunPolicy::Skip => skip.extend(missed),
        MissedRunPolicy::Coalesce => {
            if missed.is_empty() {
                execute.extend(ordinary.iter().copied());
            } else if let Some(latest) = due.last() {
                execute.push(*latest);
                skip.extend(due.iter().copied().take_while(|at| at != latest));
            }
        }
        MissedRunPolicy::CatchUp => {
            let limit = missed_run_limit.clamp(1, 100) as usize;
            execute.extend(missed.iter().take(limit).copied());
            skip.extend(missed.into_iter().skip(limit));
        }
    }
    if policy != MissedRunPolicy::Coalesce {
        execute.extend(ordinary);
    }
    execute.sort_unstable();
    skip.sort_unstable();
    DueOccurrencePlan { execute, skip }
}

pub struct RoutineStore<'db> {
    db: &'db Database,
}

impl<'db> RoutineStore<'db> {
    pub fn new(db: &'db Database) -> Self {
        Self { db }
    }

    /// Insert or update a routine. On insert the caller must NOT
    /// pre-populate `id`; the store mints a UUID and writes it back
    /// in the returned row. `next_run_at` is computed from the
    /// schedule + tz; failure to compute it is NOT a hard error
    /// (a routine with no upcoming run is still saveable, just
    /// dormant).
    pub fn upsert(&self, payload: &RoutineUpsert, now: i64) -> Result<RoutineRow, RoutineError> {
        if payload.name.trim().is_empty() {
            return Err(RoutineError::Invalid("routine name is required".into()));
        }
        if let Some(contract) = &payload.completion_contract {
            contract
                .validate()
                .map_err(|error| RoutineError::Invalid(error.to_string()))?;
        }
        let schedule = parse_cron(&payload.schedule_cron)?;
        let tz = parse_timezone(&payload.timezone)?;
        let after_dt = Utc
            .timestamp_opt(now, 0)
            .single()
            .ok_or_else(|| RoutineError::Invalid(format!("invalid `now` timestamp: {now}")))?;
        let next_run_at = next_fire_after(&schedule, tz, after_dt).map(|t| t.timestamp());

        // Reject schedules whose next fire is more than ~1 year out
        // (catches `0 0 1 1 *` typos that would mean "once per year").
        if let Some(nra) = next_run_at {
            if nra - now > 366 * 24 * 60 * 60 {
                return Err(RoutineError::Invalid(format!(
                    "schedule's next fire is over a year out (cron '{}' tz '{}'); reject as likely typo",
                    payload.schedule_cron, payload.timezone
                )));
            }
        }

        let id = payload
            .id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let enabled_int: i64 = if payload.enabled { 1 } else { 0 };

        // Ownership-style copies so the closure captures by value.
        let id_for_query = id.clone();
        let name = payload.name.clone();
        let schedule_cron = payload.schedule_cron.clone();
        let tz_str = payload.timezone.clone();
        let prompt = payload.prompt.clone();
        let target = payload.target_conversation_id.clone();
        let completion_contract_json = payload
            .completion_contract
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|error| RoutineError::Invalid(error.to_string()))?;
        if !(1..=100).contains(&payload.missed_run_limit) {
            return Err(RoutineError::Invalid(
                "missed_run_limit must be between 1 and 100".into(),
            ));
        }
        let missed_run_policy = payload.missed_run_policy.as_str();
        let missed_run_limit = payload.missed_run_limit;
        let overlap_policy = payload.overlap_policy.as_str();

        self.db.with_conn(|c| {
            c.execute(
                "INSERT INTO config_routines \
                   (id, name, schedule_cron, timezone, prompt, \
                    target_conversation_id, enabled, last_run_at, \
                    last_run_status, next_run_at, created_at, updated_at, completion_contract_json, \
                    missed_run_policy, missed_run_limit, overlap_policy) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, NULL, ?8, ?9, ?9, ?10, ?11, ?12, ?13) \
                 ON CONFLICT(id) DO UPDATE SET \
                    name                  = excluded.name, \
                    schedule_cron         = excluded.schedule_cron, \
                    timezone              = excluded.timezone, \
                    prompt                = excluded.prompt, \
                    target_conversation_id= excluded.target_conversation_id, \
                    enabled               = excluded.enabled, \
                    next_run_at           = excluded.next_run_at, \
                    completion_contract_json = excluded.completion_contract_json, \
                    missed_run_policy = excluded.missed_run_policy, \
                    missed_run_limit = excluded.missed_run_limit, \
                    overlap_policy = excluded.overlap_policy, \
                    updated_at            = excluded.updated_at",
                params![
                    id_for_query,
                    name,
                    schedule_cron,
                    tz_str,
                    prompt,
                    target,
                    enabled_int,
                    next_run_at,
                    now,
                    completion_contract_json,
                    missed_run_policy,
                    missed_run_limit,
                    overlap_policy,
                ],
            )?;
            if payload.id.is_some() {
                c.execute(
                    "UPDATE state_routine_runs SET status='Skipped',finished_at=?2, \
                     error='routine definition changed before this queued occurrence started' \
                     WHERE routine_id=?1 AND status='Pending' AND started_at IS NULL",
                    params![id_for_query, now],
                )?;
            }
            Ok(())
        })?;

        self.get(&id)?.ok_or(RoutineError::NotFound(id))
    }

    pub fn get(&self, id: &str) -> Result<Option<RoutineRow>, RoutineError> {
        let id_owned = id.to_owned();
        self.db
            .with_conn(|c| {
                c.query_row(
                        "SELECT id, name, schedule_cron, timezone, prompt, \
                            target_conversation_id, enabled, last_run_at, \
                            last_run_status, next_run_at, created_at, updated_at, completion_contract_json, \
                            missed_run_policy, missed_run_limit, overlap_policy \
                     FROM config_routines WHERE id = ?1",
                        params![id_owned],
                        row_to_routine,
                    )
                    .optional()
                    .map_err(DbError::from)
            })
            .map_err(RoutineError::from)
    }

    /// List every routine, ordered by enabled-first then by next fire
    /// (soonest at the top). Disabled routines sink below.
    pub fn list_all(&self) -> Result<Vec<RoutineRow>, RoutineError> {
        let rows = self.db.with_conn(|c| {
            let mut stmt = c.prepare(
                "SELECT id, name, schedule_cron, timezone, prompt, \
                        target_conversation_id, enabled, last_run_at, \
                        last_run_status, next_run_at, created_at, updated_at, completion_contract_json, \
                        missed_run_policy, missed_run_limit, overlap_policy \
                 FROM config_routines \
                 ORDER BY enabled DESC, \
                          CASE WHEN next_run_at IS NULL THEN 1 ELSE 0 END, \
                          next_run_at ASC, \
                          name ASC",
            )?;
            let rows = stmt
                .query_map([], row_to_routine)?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })?;
        Ok(rows)
    }

    pub fn delete(&self, id: &str) -> Result<bool, RoutineError> {
        let id_owned = id.to_owned();
        let n = self.db.with_conn(|c| {
            let n = c.execute(
                "DELETE FROM config_routines WHERE id = ?1",
                params![id_owned],
            )?;
            Ok(n)
        })?;
        Ok(n > 0)
    }

    /// Routines whose `next_run_at <= now` and `enabled = 1`. The
    /// scheduler tick uses this to find what to fire.
    pub fn list_due(&self, now: i64) -> Result<Vec<RoutineRow>, RoutineError> {
        let rows = self.db.with_conn(|c| {
            let mut stmt = c.prepare(
                "SELECT id, name, schedule_cron, timezone, prompt, \
                        target_conversation_id, enabled, last_run_at, \
                        last_run_status, next_run_at, created_at, updated_at, completion_contract_json, \
                        missed_run_policy, missed_run_limit, overlap_policy \
                 FROM config_routines \
                 WHERE enabled = 1 AND next_run_at IS NOT NULL AND next_run_at <= ?1 \
                 ORDER BY next_run_at ASC",
            )?;
            let rows = stmt
                .query_map(params![now], row_to_routine)?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })?;
        Ok(rows)
    }

    /// Mark a fire as completed and recompute next_run_at. Single
    /// atomic statement so two scheduler ticks racing on the same
    /// routine can't double-advance.
    pub fn record_run(
        &self,
        id: &str,
        status: RoutineRunStatus,
        last_run_at: i64,
        next_run_at: Option<i64>,
    ) -> Result<(), RoutineError> {
        let id_owned = id.to_owned();
        self.db
            .with_conn(|c| {
                c.execute(
                    "UPDATE config_routines \
                 SET last_run_at = ?1, last_run_status = ?2, \
                     next_run_at = ?3, updated_at = ?1 \
                 WHERE id = ?4",
                    params![last_run_at, status.as_str(), next_run_at, id_owned],
                )?;
                Ok(())
            })
            .map_err(RoutineError::from)
    }

    /// Advance the schedule cursor without changing the last completed-run status.
    pub fn advance_schedule(
        &self,
        id: &str,
        now: i64,
        next_run_at: Option<i64>,
    ) -> Result<(), RoutineError> {
        self.db
            .with_conn(|connection| {
                connection.execute(
                    "UPDATE config_routines SET next_run_at=?2,updated_at=?3 WHERE id=?1",
                    params![id, next_run_at, now],
                )?;
                Ok(())
            })
            .map_err(Into::into)
    }

    /// Insert a new run-history row in `Pending` status. Returns the
    /// minted run id.
    pub fn insert_run_pending(
        &self,
        routine_id: &str,
        fired_at: i64,
    ) -> Result<String, RoutineError> {
        self.insert_run_for_occurrence(routine_id, fired_at, fired_at)
    }

    /// Insert one durable run for a scheduled occurrence, returning the existing
    /// run id when another scheduler already created that occurrence.
    pub fn insert_run_for_occurrence(
        &self,
        routine_id: &str,
        occurrence_at: i64,
        fired_at: i64,
    ) -> Result<String, RoutineError> {
        let run_id = uuid::Uuid::new_v4().to_string();
        let routine_id_owned = routine_id.to_owned();
        let run_id_for_query = run_id.clone();
        let saved_id = self.db.with_conn(|c| {
            let inserted = c.execute(
                "INSERT OR IGNORE INTO state_routine_runs \
                   (id, routine_id, fired_at, started_at, finished_at, \
                    status, error, conversation_id, completion_contract_json, occurrence_at) \
                 SELECT ?1, ?2, ?3, NULL, NULL, 'Pending', NULL, NULL, completion_contract_json, ?4 \
                 FROM config_routines WHERE id = ?2",
                params![run_id_for_query, routine_id_owned, fired_at, occurrence_at],
            )?;
            if inserted == 0 {
                let existing: Option<String> = c.query_row(
                    "SELECT id FROM state_routine_runs WHERE routine_id=?1 AND occurrence_at=?2",
                    params![routine_id, occurrence_at],
                    |row| row.get(0),
                ).optional()?;
                if let Some(id) = existing { return Ok(id); }
                return Err(DbError::Invariant(format!(
                    "routine not found: {routine_id}"
                )));
            }
            Ok(run_id_for_query)
        })?;
        Ok(saved_id)
    }

    /// List unfinished routine fires so the scheduler can resume their stable
    /// run identities after a server restart instead of creating another fire.
    pub fn list_pending_run_ids(&self, limit: u32) -> Result<Vec<(String, String)>, RoutineError> {
        self.db.with_conn(|connection| {
            let mut statement = connection.prepare_cached(
                "SELECT id,routine_id FROM state_routine_runs WHERE status='Pending' AND started_at IS NULL \
                 ORDER BY fired_at,id LIMIT ?1",
            )?;
            let rows = statement.query_map([limit.clamp(1, 500)], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?;
            rows.collect::<Result<Vec<_>, _>>().map_err(DbError::from)
        }).map_err(RoutineError::from)
    }

    /// Return the immutable completion requirements captured when a routine fired.
    pub fn run_completion_contract(
        &self,
        run_id: &str,
    ) -> Result<Option<RunCompletionContractDraft>, RoutineError> {
        let encoded: Option<String> = self.db.with_conn(|connection| {
            connection
                .query_row(
                    "SELECT completion_contract_json FROM state_routine_runs WHERE id = ?1",
                    [run_id],
                    |row| row.get(0),
                )
                .optional()
                .map(|stored| stored.flatten())
                .map_err(DbError::from)
        })?;
        encoded
            .map(|json| {
                serde_json::from_str(&json)
                    .map_err(|error| RoutineError::Invalid(error.to_string()))
            })
            .transpose()
    }

    /// Return the occurrence timestamp pinned to a routine run.
    pub fn run_occurrence_at(&self, run_id: &str) -> Result<Option<i64>, RoutineError> {
        self.db
            .with_conn(|connection| {
                connection
                    .query_row(
                        "SELECT occurrence_at FROM state_routine_runs WHERE id=?1",
                        [run_id],
                        |row| row.get(0),
                    )
                    .optional()
                    .map_err(DbError::from)
            })
            .map_err(Into::into)
    }

    /// Whether this routine has a claimed, unfinished execution.
    pub fn has_active_run(&self, routine_id: &str) -> Result<bool, RoutineError> {
        self.db.with_conn(|connection| {
            connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM state_routine_runs WHERE routine_id=?1 AND status='Pending' AND started_at IS NOT NULL)",
                [routine_id],
                |row| row.get(0),
            ).map_err(DbError::from)
        }).map_err(Into::into)
    }

    /// Supersede queued occurrences while preserving work already claimed by a runner.
    pub fn supersede_queued_runs(
        &self,
        routine_id: &str,
        keep_occurrence: i64,
        at: i64,
    ) -> Result<usize, RoutineError> {
        self.db.with_conn(|connection| {
            connection.execute(
                "UPDATE state_routine_runs SET status='Skipped',finished_at=?3,error='replaced by a newer queued occurrence' \
                 WHERE routine_id=?1 AND status='Pending' AND started_at IS NULL AND occurrence_at<>?2",
                params![routine_id, keep_occurrence, at],
            ).map_err(DbError::from)
        }).map_err(Into::into)
    }

    /// Terminally mark a run that policy intentionally skipped.
    pub fn skip_run(&self, run_id: &str, at: i64, reason: &str) -> Result<(), RoutineError> {
        self.db.with_conn(|connection| {
            connection.execute(
                "UPDATE state_routine_runs SET status='Skipped',finished_at=?2,error=?3 WHERE id=?1 AND status='Pending'",
                params![run_id, at, reason],
            )?;
            Ok(())
        }).map_err(Into::into)
    }

    /// Drop in-process claims left by a stopped server. Called once during
    /// scheduler startup before pending run ids are enumerated.
    pub fn reset_pending_run_claims(&self) -> Result<usize, RoutineError> {
        self.db
            .with_conn(|connection| {
                Ok(connection.execute(
                    "UPDATE state_routine_runs SET started_at=NULL \
                     WHERE status='Pending' AND started_at IS NOT NULL",
                    [],
                )?)
            })
            .map_err(Into::into)
    }

    /// Claim an unstarted Pending run so concurrent ticks cannot dispatch it
    /// twice within this server process.
    pub fn claim_pending_run(&self, run_id: &str, started_at: i64) -> Result<bool, RoutineError> {
        self.db
            .with_conn(|connection| {
                Ok(connection.execute(
                    "UPDATE state_routine_runs SET started_at=?1 \
                 WHERE id=?2 AND status='Pending' AND started_at IS NULL \
                   AND NOT EXISTS (SELECT 1 FROM state_routine_runs active \
                     WHERE active.routine_id=state_routine_runs.routine_id \
                       AND active.status='Pending' AND active.started_at IS NOT NULL \
                       AND active.id<>state_routine_runs.id)",
                    params![started_at, run_id],
                )? == 1)
            })
            .map_err(Into::into)
    }

    /// Persist the conversation selected by a routine fire before dispatch so
    /// startup recovery can exclude it from the generic chat-run replayer.
    pub fn bind_run_conversation(
        &self,
        run_id: &str,
        conversation_id: &str,
    ) -> Result<(), RoutineError> {
        self.db.transaction(|tx| {
            let stored: Option<Option<String>> = tx
                .query_row(
                    "SELECT conversation_id FROM state_routine_runs WHERE id=?1 AND status='Pending'",
                    [run_id],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(stored) = stored else {
                return Err(DbError::Invariant(format!("pending routine run was not found: {run_id}")));
            };
            if stored.as_deref().is_some_and(|existing| existing != conversation_id) {
                return Err(DbError::Invariant("routine run was retried with a different conversation".into()));
            }
            tx.execute(
                "UPDATE state_routine_runs SET conversation_id=?2 WHERE id=?1 AND status='Pending'",
                params![run_id, conversation_id],
            )?;
            Ok(())
        }).map_err(Into::into)
    }

    /// Conversations owned by pending routine fires that must be resumed by
    /// the routine scheduler, not the generic controller chat replayer.
    pub fn pending_run_conversations(
        &self,
    ) -> Result<std::collections::HashSet<String>, RoutineError> {
        self.db
            .with_conn(|connection| {
                let mut statement = connection.prepare_cached(
                    "SELECT conversation_id FROM state_routine_runs \
                 WHERE status='Pending' AND conversation_id IS NOT NULL",
                )?;
                let rows = statement.query_map([], |row| row.get(0))?;
                rows.collect::<Result<std::collections::HashSet<_>, _>>()
                    .map_err(DbError::from)
            })
            .map_err(Into::into)
    }

    /// Move a run from Pending → terminal status. The runner calls
    /// this when the dispatched turn finishes (or fails).
    pub fn finish_run(
        &self,
        run_id: &str,
        status: RoutineRunStatus,
        finished_at: i64,
        error: Option<&str>,
        conversation_id: Option<&str>,
    ) -> Result<(), RoutineError> {
        let id_owned = run_id.to_owned();
        let err_owned = error.map(|s| s.to_owned());
        let cid_owned = conversation_id.map(|s| s.to_owned());
        self.db
            .with_conn(|c| {
                c.execute(
                    "UPDATE state_routine_runs \
                 SET status = ?1, finished_at = ?2, error = ?3, conversation_id = ?4 \
                 WHERE id = ?5",
                    params![status.as_str(), finished_at, err_owned, cid_owned, id_owned],
                )?;
                Ok(())
            })
            .map_err(RoutineError::from)
    }

    /// Purge run-history rows older than `cutoff_unix`. Returns the
    /// number deleted. Only runs in a terminal status are eligible —
    /// a `Pending` row that's been hanging for longer than retention
    /// is preserved so the operator can still see it (and so a
    /// crash-mid-fire row doesn't silently disappear). The retention
    /// sweeper at boot calls this on the configured cadence.
    pub fn purge_runs_older_than(&self, cutoff_unix: i64) -> Result<usize, RoutineError> {
        let n = self.db.with_conn(|c| {
            let n = c.execute(
                "DELETE FROM state_routine_runs \
                 WHERE fired_at < ?1 AND status != 'Pending'",
                params![cutoff_unix],
            )?;
            Ok(n)
        })?;
        Ok(n)
    }

    /// Run history for a routine, most-recent first. Capped to
    /// `limit` so a deep history doesn't dump a million rows.
    pub fn list_runs(
        &self,
        routine_id: &str,
        limit: u32,
    ) -> Result<Vec<RoutineRunRow>, RoutineError> {
        let routine_id_owned = routine_id.to_owned();
        let lim = limit.min(500) as i64;
        let rows = self.db.with_conn(|c| {
            let mut stmt = c.prepare(
                "SELECT id, routine_id, fired_at, started_at, finished_at, \
                        status, error, conversation_id, occurrence_at \
                 FROM state_routine_runs \
                 WHERE routine_id = ?1 \
                 ORDER BY fired_at DESC \
                 LIMIT ?2",
            )?;
            let rows = stmt
                .query_map(params![routine_id_owned, lim], row_to_run)?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })?;
        Ok(rows)
    }
}

fn row_to_routine(row: &rusqlite::Row<'_>) -> rusqlite::Result<RoutineRow> {
    let status_str: Option<String> = row.get(8)?;
    let last_run_status = status_str.and_then(|s| RoutineRunStatus::parse(&s));
    let enabled: i64 = row.get(6)?;
    Ok(RoutineRow {
        id: row.get(0)?,
        name: row.get(1)?,
        schedule_cron: row.get(2)?,
        timezone: row.get(3)?,
        prompt: row.get(4)?,
        target_conversation_id: row.get(5)?,
        enabled: enabled != 0,
        last_run_at: row.get(7)?,
        last_run_status,
        next_run_at: row.get(9)?,
        created_at: row.get(10)?,
        updated_at: row.get(11)?,
        completion_contract: row
            .get::<_, Option<String>>(12)?
            .map(|json| serde_json::from_str(&json).map_err(|_| rusqlite::Error::InvalidQuery))
            .transpose()?,
        missed_run_policy: MissedRunPolicy::parse(&row.get::<_, String>(13)?).unwrap_or_default(),
        missed_run_limit: row.get::<_, u32>(14)?.clamp(1, 100),
        overlap_policy: RoutineOverlapPolicy::parse(&row.get::<_, String>(15)?).unwrap_or_default(),
    })
}

fn row_to_run(row: &rusqlite::Row<'_>) -> rusqlite::Result<RoutineRunRow> {
    let status_str: String = row.get(5)?;
    let status = RoutineRunStatus::parse(&status_str).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            5,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("unknown routine run status: {status_str}"),
            )),
        )
    })?;
    Ok(RoutineRunRow {
        id: row.get(0)?,
        routine_id: row.get(1)?,
        fired_at: row.get(2)?,
        started_at: row.get(3)?,
        finished_at: row.get(4)?,
        status,
        error: row.get(6)?,
        conversation_id: row.get(7)?,
        occurrence_at: row.get(8)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Database, DbConfig};
    use crate::migrations::MigrationRunner;

    fn fresh_db() -> Database {
        let db = Database::open(&DbConfig::in_memory_unencrypted()).unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        db
    }

    fn upsert(name: &str, cron: &str) -> RoutineUpsert {
        RoutineUpsert {
            id: None,
            name: name.into(),
            schedule_cron: cron.into(),
            timezone: "UTC".into(),
            prompt: "do the thing".into(),
            target_conversation_id: None,
            enabled: true,
            completion_contract: None,
            missed_run_policy: MissedRunPolicy::Skip,
            missed_run_limit: 1,
            overlap_policy: RoutineOverlapPolicy::Forbid,
        }
    }

    #[test]
    fn parse_cron_accepts_standard_5_field() {
        assert!(parse_cron("0 8 * * 1-5").is_ok());
        assert!(parse_cron("*/15 * * * *").is_ok());
        assert!(parse_cron("0 0 1 1 *").is_ok());
    }

    #[test]
    fn parse_cron_rejects_non_5_field_forms() {
        // 6-field with seconds — legal in `cron` crate but rejected here.
        assert!(parse_cron("0 0 8 * * 1-5").is_err());
        // 4-field — short.
        assert!(parse_cron("0 8 * *").is_err());
        // Empty.
        assert!(parse_cron("").is_err());
        // Garbage.
        assert!(parse_cron("not a cron").is_err());
    }

    #[test]
    fn next_fire_after_respects_timezone() {
        let sched = parse_cron("0 8 * * *").unwrap();
        // 2026-04-25 03:00 UTC → next 8am NY (= 12:00 UTC) the same day.
        let after = Utc.with_ymd_and_hms(2026, 4, 25, 3, 0, 0).unwrap();
        let tz = parse_timezone("America/New_York").unwrap();
        let next = next_fire_after(&sched, tz, after).expect("has a fire");
        assert_eq!(next, Utc.with_ymd_and_hms(2026, 4, 25, 12, 0, 0).unwrap());
    }

    #[test]
    fn upsert_creates_then_updates_with_next_run_at() {
        let db = fresh_db();
        let store = RoutineStore::new(&db);
        let now = Utc
            .with_ymd_and_hms(2026, 4, 25, 3, 0, 0)
            .unwrap()
            .timestamp();
        let r1 = store.upsert(&upsert("morning", "0 8 * * *"), now).unwrap();
        assert!(!r1.id.is_empty());
        assert!(r1.next_run_at.is_some());

        // Update via id round-trip.
        let mut p = upsert("morning v2", "0 9 * * *");
        p.id = Some(r1.id.clone());
        let r2 = store.upsert(&p, now).unwrap();
        assert_eq!(r2.id, r1.id);
        assert_eq!(r2.name, "morning v2");
        // The schedule changed, so next_run_at should differ.
        assert_ne!(r1.next_run_at, r2.next_run_at);
    }

    #[test]
    fn routine_fire_freezes_completion_contract_before_definition_changes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("routine-contract.db");
        let db = Database::open(&DbConfig {
            path: path.clone(),
            key: None,
        })
        .unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        let store = RoutineStore::new(&db);
        let now = Utc
            .with_ymd_and_hms(2026, 4, 25, 3, 0, 0)
            .unwrap()
            .timestamp();
        let mut definition = upsert("verified morning", "0 8 * * *");
        let contract = RunCompletionContractDraft {
            acceptance_criteria: vec![crate::runs::AcceptanceCriterion {
                criterion_id: "tests".into(),
                description: "Required tests pass".into(),
                required: true,
                verifier: None,
            }],
            required_artifacts: Vec::new(),
            delivery_required: false,
        };
        definition.completion_contract = Some(contract.clone());
        let routine = store.upsert(&definition, now).unwrap();
        let fire_id = store.insert_run_pending(&routine.id, now).unwrap();
        definition.id = Some(routine.id.clone());
        definition.completion_contract = None;
        store.upsert(&definition, now + 1).unwrap();
        drop(store);
        drop(db);
        let reopened = Database::open(&DbConfig { path, key: None }).unwrap();
        let store = RoutineStore::new(&reopened);
        assert_eq!(
            store.run_completion_contract(&fire_id).unwrap(),
            Some(contract)
        );
        assert!(
            store
                .get(&routine.id)
                .unwrap()
                .unwrap()
                .completion_contract
                .is_none()
        );
    }

    #[test]
    fn upsert_rejects_schedule_that_fires_more_than_a_year_out() {
        let db = fresh_db();
        let store = RoutineStore::new(&db);
        // April 25 2026 → next Feb 29 = Feb 29 2028 = ~22 months out.
        // That's the typo-catching case: legitimate "every leap-day"
        // schedules are vanishingly rare, so the guardrail prefers
        // rejecting on the assumption it's a fat-fingered cron.
        let now = Utc
            .with_ymd_and_hms(2026, 4, 25, 3, 0, 0)
            .unwrap()
            .timestamp();
        let err = store
            .upsert(&upsert("leap-only", "0 0 29 2 *"), now)
            .unwrap_err();
        assert!(matches!(err, RoutineError::Invalid(_)));
    }

    #[test]
    fn upsert_accepts_yearly_schedule_when_next_fire_is_within_a_year() {
        let db = fresh_db();
        let store = RoutineStore::new(&db);
        // Dec 1 2026 → next Jan 1 = Jan 1 2027 = ~31 days out → fine.
        let now = Utc
            .with_ymd_and_hms(2026, 12, 1, 3, 0, 0)
            .unwrap()
            .timestamp();
        let r = store
            .upsert(&upsert("yearly-but-soon", "0 0 1 1 *"), now)
            .unwrap();
        assert!(r.next_run_at.is_some());
    }

    #[test]
    fn upsert_rejects_invalid_cron_and_tz_and_empty_name() {
        let db = fresh_db();
        let store = RoutineStore::new(&db);
        let now = 0;
        assert!(store.upsert(&upsert("bad-cron", "lol"), now).is_err());

        let mut p = upsert("bad-tz", "0 8 * * *");
        p.timezone = "Mars/Olympus_Mons".into();
        assert!(store.upsert(&p, now).is_err());

        let mut p = upsert("", "0 8 * * *");
        p.name = "".into();
        assert!(store.upsert(&p, now).is_err());
    }

    #[test]
    fn list_due_returns_only_enabled_with_elapsed_next_run_at() {
        let db = fresh_db();
        let store = RoutineStore::new(&db);
        let now = Utc
            .with_ymd_and_hms(2026, 4, 25, 3, 0, 0)
            .unwrap()
            .timestamp();

        let due = store.upsert(&upsert("due", "0 8 * * *"), now).unwrap();
        // Force its next_run_at into the past.
        store
            .record_run(&due.id, RoutineRunStatus::Success, now - 600, Some(now - 1))
            .unwrap();

        // Disabled routine, also overdue — should NOT appear.
        let mut disabled_p = upsert("disabled", "0 8 * * *");
        disabled_p.enabled = false;
        let disabled = store.upsert(&disabled_p, now).unwrap();
        store
            .record_run(
                &disabled.id,
                RoutineRunStatus::Success,
                now - 600,
                Some(now - 1),
            )
            .unwrap();

        let due_rows = store.list_due(now).unwrap();
        assert_eq!(due_rows.len(), 1);
        assert_eq!(due_rows[0].id, due.id);
    }

    #[test]
    fn run_history_round_trip() {
        let db = fresh_db();
        let store = RoutineStore::new(&db);
        let now = Utc
            .with_ymd_and_hms(2026, 4, 25, 3, 0, 0)
            .unwrap()
            .timestamp();
        let r = store.upsert(&upsert("test", "0 8 * * *"), now).unwrap();

        let run_id = store.insert_run_pending(&r.id, now).unwrap();
        store
            .finish_run(
                &run_id,
                RoutineRunStatus::Success,
                now + 5,
                None,
                Some("conv-123"),
            )
            .unwrap();

        let runs = store.list_runs(&r.id, 50).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].status, RoutineRunStatus::Success);
        assert_eq!(runs[0].finished_at, Some(now + 5));
        assert_eq!(runs[0].conversation_id.as_deref(), Some("conv-123"));
    }

    #[test]
    fn pending_routine_run_claim_is_restored_after_restart() {
        let db = fresh_db();
        let store = RoutineStore::new(&db);
        let now = Utc
            .with_ymd_and_hms(2026, 4, 25, 3, 0, 0)
            .unwrap()
            .timestamp();
        let routine = store.upsert(&upsert("recover", "0 8 * * *"), now).unwrap();
        let run_id = store.insert_run_pending(&routine.id, now).unwrap();
        assert!(store.claim_pending_run(&run_id, now + 1).unwrap());
        assert!(store.list_pending_run_ids(10).unwrap().is_empty());

        assert_eq!(store.reset_pending_run_claims().unwrap(), 1);
        assert_eq!(
            store.list_pending_run_ids(10).unwrap(),
            vec![(run_id.clone(), routine.id.clone())]
        );
        assert!(store.claim_pending_run(&run_id, now + 2).unwrap());
        assert!(!store.claim_pending_run(&run_id, now + 3).unwrap());
    }

    #[test]
    fn routine_claim_serializes_distinct_occurrences() {
        let db = fresh_db();
        let store = RoutineStore::new(&db);
        let routine = store.upsert(&upsert("serial", "0 8 * * *"), 0).unwrap();
        let first = store
            .insert_run_for_occurrence(&routine.id, 100, 100)
            .unwrap();
        let second = store
            .insert_run_for_occurrence(&routine.id, 200, 200)
            .unwrap();
        assert!(store.claim_pending_run(&first, 101).unwrap());
        assert!(!store.claim_pending_run(&second, 201).unwrap());
        store
            .finish_run(&first, RoutineRunStatus::Success, 300, None, None)
            .unwrap();
        assert!(store.claim_pending_run(&second, 301).unwrap());
    }

    #[test]
    fn editing_a_routine_cancels_unstarted_queued_occurrences() {
        let db = fresh_db();
        let store = RoutineStore::new(&db);
        let routine = store
            .upsert(&upsert("before edit", "0 8 * * *"), 0)
            .unwrap();
        let queued = store
            .insert_run_for_occurrence(&routine.id, 100, 101)
            .unwrap();
        let mut edited = upsert("after edit", "0 9 * * *");
        edited.id = Some(routine.id.clone());
        store.upsert(&edited, 1).unwrap();
        let runs = store.list_runs(&routine.id, 10).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].id, queued);
        assert_eq!(runs[0].status, RoutineRunStatus::Skipped);
        assert!(
            runs[0]
                .error
                .as_deref()
                .unwrap()
                .contains("definition changed")
        );
    }

    #[test]
    fn delete_then_list_runs_is_empty_via_fk_cascade() {
        let db = fresh_db();
        let store = RoutineStore::new(&db);
        let now = Utc
            .with_ymd_and_hms(2026, 4, 25, 3, 0, 0)
            .unwrap()
            .timestamp();
        let r = store.upsert(&upsert("test", "0 8 * * *"), now).unwrap();
        store.insert_run_pending(&r.id, now).unwrap();

        assert!(store.delete(&r.id).unwrap());
        let runs = store.list_runs(&r.id, 50).unwrap();
        assert!(runs.is_empty(), "cascade should drop the run history");
    }

    #[test]
    fn next_n_fires_returns_n_distinct_future_times() {
        let sched = parse_cron("0 8 * * *").unwrap();
        let tz = parse_timezone("UTC").unwrap();
        let after = Utc.with_ymd_and_hms(2026, 4, 25, 3, 0, 0).unwrap();
        let fires = next_n_fires(&sched, tz, after, 3);
        assert_eq!(fires.len(), 3);
        // Each is strictly after the previous.
        assert!(fires[0] < fires[1]);
        assert!(fires[1] < fires[2]);
    }

    #[test]
    fn cron_preview_is_stable_through_daylight_saving_gap_and_fold() {
        let tz = parse_timezone("America/Los_Angeles").unwrap();
        let fold = parse_cron("30 1 * * *").unwrap();
        let after_first_0130 = Utc.with_ymd_and_hms(2026, 11, 1, 8, 45, 0).unwrap();
        assert_eq!(
            next_fire_after(&fold, tz, after_first_0130).unwrap(),
            Utc.with_ymd_and_hms(2026, 11, 2, 9, 30, 0).unwrap()
        );

        let gap = parse_cron("30 2 * * *").unwrap();
        let after_spring_transition = Utc.with_ymd_and_hms(2026, 3, 8, 10, 0, 0).unwrap();
        assert_eq!(
            next_fire_after(&gap, tz, after_spring_transition).unwrap(),
            Utc.with_ymd_and_hms(2026, 3, 9, 9, 30, 0).unwrap()
        );
    }

    #[test]
    fn missed_occurrence_policies_are_bounded_and_deterministic() {
        let occurrences = [100, 200, 300, 400];
        assert_eq!(
            plan_due_occurrences(MissedRunPolicy::Skip, 1, 500, &occurrences),
            DueOccurrencePlan {
                execute: vec![],
                skip: vec![100, 200, 300, 400]
            }
        );
        assert_eq!(
            plan_due_occurrences(MissedRunPolicy::Coalesce, 1, 500, &occurrences),
            DueOccurrencePlan {
                execute: vec![400],
                skip: vec![100, 200, 300]
            }
        );
        assert_eq!(
            plan_due_occurrences(MissedRunPolicy::Coalesce, 1, 500, &[100, 480]),
            DueOccurrencePlan {
                execute: vec![480],
                skip: vec![100]
            }
        );
        assert_eq!(
            plan_due_occurrences(MissedRunPolicy::CatchUp, 2, 500, &occurrences),
            DueOccurrencePlan {
                execute: vec![100, 200],
                skip: vec![300, 400]
            }
        );
        assert_eq!(
            plan_due_occurrences(MissedRunPolicy::Skip, 1, 460, &occurrences),
            DueOccurrencePlan {
                execute: vec![400],
                skip: vec![100, 200, 300]
            }
        );
    }

    #[test]
    fn scheduled_occurrence_identity_is_idempotent_and_policy_round_trips() {
        let db = fresh_db();
        let store = RoutineStore::new(&db);
        let mut definition = upsert("idempotent", "0 8 * * *");
        definition.missed_run_policy = MissedRunPolicy::CatchUp;
        definition.missed_run_limit = 3;
        definition.overlap_policy = RoutineOverlapPolicy::Queue;
        let row = store.upsert(&definition, 0).unwrap();
        assert_eq!(row.missed_run_policy, MissedRunPolicy::CatchUp);
        assert_eq!(row.missed_run_limit, 3);
        assert_eq!(row.overlap_policy, RoutineOverlapPolicy::Queue);
        let first = store
            .insert_run_for_occurrence(&row.id, 1234, 1300)
            .unwrap();
        let second = store
            .insert_run_for_occurrence(&row.id, 1234, 1400)
            .unwrap();
        assert_eq!(first, second);
        assert_eq!(store.list_runs(&row.id, 10).unwrap().len(), 1);
        assert_eq!(
            store.list_runs(&row.id, 10).unwrap()[0].occurrence_at,
            Some(1234)
        );
    }
}
