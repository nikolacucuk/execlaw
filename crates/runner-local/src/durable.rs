//! Durable checkpoint coordination shared by in-process and server runners.

use execlaw_core::Database;
use execlaw_core::ids::{ConversationId, EventSeq};
use execlaw_core::runs::{
    NewRun, NewRunStep, NextSafeAction, RunCompletionContract, RunCompletionContractDraft,
    RunInputManifest, RunRecord, RunStatus, RunStepKind, RunStepRecord, RunStepStatus, RunStore,
    RunStoreError,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

const OUTPUT_PREFIX: &str = "json:";
const DEFAULT_RUN_TIME_BUDGET_MS: u64 = 60 * 60 * 1_000;
const DEFAULT_RUN_RETRY_BUDGET: u32 = 64;
const DEFAULT_RUN_EFFECT_BUDGET: u32 = 64;

/// Result of entering a durable step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StepDecision<T> {
    /// This worker owns the step lease and may execute the operation.
    Execute(RunStepRecord),
    /// The operation completed previously; use its checkpointed output.
    Replay(T),
    /// The step is durably parked on an approval.
    WaitForApproval { approval_id: String },
    /// Another live worker owns the step lease.
    Busy { lease_owner: Option<String> },
}

/// Run-scoped coordinator over [`RunStore`].
pub struct DurableRun<'db> {
    store: RunStore<'db>,
    run_id: String,
    worker_id: String,
    lease_seconds: i64,
    model_lease_seconds: i64,
}

impl<'db> DurableRun<'db> {
    /// Open or idempotently recreate a stable run for one input event.
    pub fn open(
        db: &'db Database,
        run_id: impl Into<String>,
        worker_id: impl Into<String>,
        conversation_id: ConversationId,
        input_event_seq: EventSeq,
        parent_run_id: Option<String>,
        now: i64,
    ) -> Result<Self, RunStoreError> {
        Self::open_with_execution_budget(
            db,
            run_id,
            worker_id,
            conversation_id,
            input_event_seq,
            parent_run_id,
            now,
            DEFAULT_RUN_TIME_BUDGET_MS,
            DEFAULT_RUN_RETRY_BUDGET,
            DEFAULT_RUN_EFFECT_BUDGET,
        )
    }

    /// Open a durable run with immutable wall-clock, retry, and effect quotas.
    pub fn open_with_execution_budget(
        db: &'db Database,
        run_id: impl Into<String>,
        worker_id: impl Into<String>,
        conversation_id: ConversationId,
        input_event_seq: EventSeq,
        parent_run_id: Option<String>,
        now: i64,
        time_limit_ms: u64,
        retry_limit: u32,
        effect_limit: u32,
    ) -> Result<Self, RunStoreError> {
        let run_id = run_id.into();
        let store = RunStore::new(db);
        let persisted_deadline = store.get_run(&run_id)?.and_then(|run| run.deadline_at);
        store.create_run_with_id(
            &run_id,
            &NewRun {
                conversation_id,
                parent_run_id,
                input_event_seq,
                started_at: now,
                deadline_at: persisted_deadline,
            },
        )?;
        store.ensure_execution_budget(
            &run_id,
            time_limit_ms,
            retry_limit,
            effect_limit,
            now.saturating_mul(1_000),
        )?;
        Ok(Self {
            store,
            run_id,
            worker_id: worker_id.into(),
            lease_seconds: 60,
            model_lease_seconds: 60,
        })
    }

    /// Override the default lease duration for long-running operations or tests.
    pub fn with_lease_seconds(mut self, lease_seconds: i64) -> Self {
        self.lease_seconds = lease_seconds.max(1);
        self.model_lease_seconds = self.lease_seconds;
        self
    }

    /// Give model requests a longer lease while leaving effect steps on the
    /// normal recovery timer. The lease must outlast the model retry deadline.
    ///
    /// ```ignore
    /// let durable = durable.with_model_lease_seconds(150);
    /// ```
    pub fn with_model_lease_seconds(mut self, lease_seconds: i64) -> Self {
        self.model_lease_seconds = lease_seconds.max(self.lease_seconds);
        self
    }

    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    /// Return the durable execution budget persisted for this run.
    pub fn execution_budget(
        &self,
    ) -> Result<execlaw_core::runs::RunExecutionBudget, RunStoreError> {
        self.store
            .execution_budget(&self.run_id)?
            .ok_or_else(|| RunStoreError::Corrupt("run execution budget is missing".into()))
    }

    /// Persist the task's immutable user-authored completion requirements.
    pub fn set_completion_contract(
        &self,
        draft: &RunCompletionContractDraft,
        created_at: i64,
    ) -> Result<(), RunStoreError> {
        self.store.set_completion_contract(&RunCompletionContract {
            run_id: self.run_id.clone(),
            acceptance_criteria: draft.acceptance_criteria.clone(),
            required_artifacts: draft.required_artifacts.clone(),
            delivery_required: draft.delivery_required,
            created_at,
        })
    }

    /// Record hashes of the effective turn inputs without persisting prompt
    /// text or tool arguments a second time.
    pub fn record_input_manifest(
        &self,
        prompt: &impl Serialize,
        model_settings: &impl Serialize,
        tool_catalog: &impl Serialize,
        recorded_at: i64,
    ) -> Result<(), RunStoreError> {
        self.store.record_input_manifest(
            &self.run_id,
            &RunInputManifest {
                input_version: 1,
                prompt_hash: hash_serializable(prompt)?,
                model_settings_hash: hash_serializable(model_settings)?,
                tool_catalog_hash: hash_serializable(tool_catalog)?,
                recorded_at,
            },
        )
    }

    /// Persist the start of one HTTP inference attempt under its leased model
    /// step. A cancelled process leaves a discoverable `started` record.
    pub fn start_inference_attempt(
        &self,
        step_id: &str,
        started_at: i64,
    ) -> Result<u32, RunStoreError> {
        self.store
            .start_inference_attempt(&self.run_id, step_id, &self.worker_id, started_at)
    }

    /// Persist the terminal outcome of a previously started inference attempt.
    pub fn finish_inference_attempt(
        &self,
        step_id: &str,
        attempt_no: u32,
        succeeded: bool,
        error_class: Option<&str>,
        finished_at: i64,
    ) -> Result<(), RunStoreError> {
        self.store.finish_inference_attempt(
            &self.run_id,
            step_id,
            attempt_no,
            succeeded,
            error_class,
            finished_at,
        )
    }

    pub fn mark_inference_attempt_retrying(
        &self,
        step_id: &str,
        attempt_no: u32,
        error_class: &str,
        finished_at: i64,
    ) -> Result<(), RunStoreError> {
        self.store.mark_inference_attempt_retrying(
            &self.run_id,
            step_id,
            attempt_no,
            error_class,
            finished_at,
        )
    }

    /// Whether this stable run already reached its terminal success state.
    pub fn is_completed(&self) -> Result<bool, RunStoreError> {
        Ok(self
            .store
            .get_run(&self.run_id)?
            .is_some_and(|run| run.status == RunStatus::Completed))
    }

    /// Load a durable step by id for restart reconciliation.
    pub fn get_step(&self, step_id: &str) -> Result<Option<RunStepRecord>, RunStoreError> {
        self.store.get_step(&self.run_id, step_id)
    }

    /// Load the ordered step ledger for restart planning.
    pub fn steps(&self) -> Result<Vec<RunStepRecord>, RunStoreError> {
        self.store.list_steps(&self.run_id)
    }

    /// Replay a completed step's checkpointed output without claiming or
    /// executing it again. Pending/running steps return `None` for the caller
    /// to enter through [`Self::begin`], which applies lease fencing.
    pub fn replay_completed<T: DeserializeOwned>(
        &self,
        step_id: &str,
    ) -> Result<Option<T>, RunStoreError> {
        let Some(step) = self.store.get_step(&self.run_id, step_id)? else {
            return Ok(None);
        };
        if step.status != RunStepStatus::Completed {
            return Ok(None);
        }
        decode_output(&step).map(Some)
    }

    /// Define a future step without claiming it.
    pub fn define(
        &self,
        step_id: impl Into<String>,
        ordinal: i64,
        kind: RunStepKind,
        input: &impl Serialize,
        approval_id: Option<String>,
        outbox_idempotency_key: Option<String>,
    ) -> Result<RunStepRecord, RunStoreError> {
        let input = serde_json::to_value(input)
            .map_err(|error| RunStoreError::Conflict(format!("serialize step input: {error}")))?;
        self.store.add_step(
            &self.run_id,
            &NewRunStep {
                step_id: step_id.into(),
                ordinal,
                kind,
                input_hash: stable_input_hash(&input),
                approval_id,
                outbox_idempotency_key,
            },
        )
    }

    /// Add and enter the current step using a canonical hash of its input.
    pub fn begin<T: DeserializeOwned>(
        &self,
        step_id: impl Into<String>,
        ordinal: i64,
        kind: RunStepKind,
        input: &impl Serialize,
        approval_id: Option<String>,
        outbox_idempotency_key: Option<String>,
        now: i64,
    ) -> Result<StepDecision<T>, RunStoreError> {
        let step_id = step_id.into();
        self.define(
            step_id.clone(),
            ordinal,
            kind,
            input,
            approval_id,
            outbox_idempotency_key,
        )?;

        if let Some(step) = self.store.get_step(&self.run_id, &step_id)?
            && step.status == RunStepStatus::Completed
        {
            return Ok(StepDecision::Replay(decode_output(&step)?));
        }

        match self.store.next_safe_action(&self.run_id, now)? {
            NextSafeAction::Claim(step) | NextSafeAction::ReclaimExpired(step)
                if step.step_id == step_id =>
            {
                let claimed = self.store.claim_step(
                    &self.run_id,
                    &step_id,
                    &self.worker_id,
                    now,
                    now.saturating_add(if kind == RunStepKind::ModelRequest {
                        self.model_lease_seconds
                    } else {
                        self.lease_seconds
                    }),
                )?;
                Ok(StepDecision::Execute(claimed))
            }
            NextSafeAction::WaitForLease(step) if step.step_id == step_id => {
                Ok(StepDecision::Busy {
                    lease_owner: step.lease_owner,
                })
            }
            NextSafeAction::WaitForApproval(step) if step.step_id == step_id => {
                Ok(StepDecision::WaitForApproval {
                    approval_id: step.approval_id.ok_or_else(|| {
                        RunStoreError::Corrupt("approval wait has no approval id".into())
                    })?,
                })
            }
            NextSafeAction::AdvanceCursor(step) if step.step_id == step_id => {
                let output = decode_output(&step)?;
                Ok(StepDecision::Replay(output))
            }
            action => Err(RunStoreError::Conflict(format!(
                "step '{step_id}' at ordinal {ordinal} is not the next safe action: {action:?}"
            ))),
        }
    }

    /// Checkpoint a leased step's output. Repeated completion returns the
    /// already-persisted value through [`Self::begin`].
    pub fn complete(
        &self,
        step_id: &str,
        output: &impl Serialize,
        completed_at: i64,
    ) -> Result<RunStepRecord, RunStoreError> {
        let json = serde_json::to_string(output)
            .map_err(|error| RunStoreError::Conflict(format!("serialize step output: {error}")))?;
        self.store.complete_step(
            &self.run_id,
            step_id,
            &self.worker_id,
            Some(&format!("{OUTPUT_PREFIX}{json}")),
            completed_at,
        )
    }

    /// Fail the active step and its run while retaining a bounded diagnostic
    /// reference for the run inspector.
    pub fn fail(
        &self,
        step_id: &str,
        detail: &str,
        failed_at: i64,
    ) -> Result<RunStepRecord, RunStoreError> {
        let detail = detail.chars().take(512).collect::<String>();
        self.store.fail_step(
            &self.run_id,
            step_id,
            &self.worker_id,
            Some(&format!("error:{detail}")),
            failed_at,
        )
    }

    /// Park the leased step until the named approval is resolved.
    pub fn wait_for_approval(
        &self,
        step_id: &str,
        approval_id: &str,
        updated_at: i64,
    ) -> Result<RunStepRecord, RunStoreError> {
        self.store.wait_step(
            &self.run_id,
            step_id,
            &self.worker_id,
            Some(approval_id),
            updated_at,
        )
    }

    /// Resume a parked approval step after the sideband decision is persisted.
    pub fn resume_approval(
        &self,
        step_id: &str,
        approval_id: &str,
        updated_at: i64,
    ) -> Result<RunStepRecord, RunStoreError> {
        self.store
            .resume_waiting_step(&self.run_id, step_id, Some(approval_id), updated_at)
    }

    /// Advance a completed cursor step. This never implicitly completes a run.
    pub fn advance(&self, ordinal: i64, updated_at: i64) -> Result<RunRecord, RunStoreError> {
        if let Some(run) = self.store.get_run(&self.run_id)?
            && run.cursor > ordinal
        {
            return Ok(run);
        }
        self.store.advance_cursor(&self.run_id, ordinal, updated_at)
    }

    /// Complete a run after its final completed step has been advanced.
    pub fn finish(&self, cursor: i64, completed_at: i64) -> Result<RunRecord, RunStoreError> {
        self.store.complete_run(&self.run_id, cursor, completed_at)
    }
}

/// SHA-256 over recursively canonicalized JSON.
pub fn stable_input_hash(value: &Value) -> String {
    execlaw_core::tool::tool_schema_hash(value)
}

fn hash_serializable(value: &impl Serialize) -> Result<String, RunStoreError> {
    let json = serde_json::to_value(value)
        .map_err(|error| RunStoreError::Conflict(format!("serialize turn input: {error}")))?;
    Ok(stable_input_hash(&json))
}

fn decode_output<T: DeserializeOwned>(step: &RunStepRecord) -> Result<T, RunStoreError> {
    let output = step.output_ref.as_deref().ok_or_else(|| {
        RunStoreError::Corrupt(format!("completed step '{}' has no output", step.step_id))
    })?;
    let json = output.strip_prefix(OUTPUT_PREFIX).ok_or_else(|| {
        RunStoreError::Corrupt(format!(
            "completed step '{}' has an unsupported output reference",
            step.step_id
        ))
    })?;
    serde_json::from_str(json).map_err(|error| {
        RunStoreError::Corrupt(format!(
            "completed step '{}' output is invalid JSON: {error}",
            step.step_id
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use execlaw_core::conversation::{
        ConversationKind, ConversationRow, ConversationStore, Modality, Phase,
    };
    use execlaw_core::db::DbConfig;
    use execlaw_core::events::{EventKind, EventLog, EventRecord};
    use execlaw_core::migrations::MigrationRunner;

    fn seed(db: &Database) -> ConversationId {
        let conversation_id = ConversationId::from("durable-turn");
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
                    &serde_json::json!({"text": "hello"}),
                    Some("operator".into()),
                )
                .unwrap(),
            )
            .unwrap();
        conversation_id
    }

    fn file_db(path: &std::path::Path) -> Database {
        let db = Database::open(&DbConfig {
            path: path.to_owned(),
            key: None,
        })
        .unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        db
    }

    #[test]
    fn long_model_lease_keeps_tool_effect_recovery_on_normal_timer() {
        let db = Database::open(&DbConfig::in_memory_unencrypted()).unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        let conversation_id = seed(&db);
        let run = DurableRun::open(
            &db,
            "turn:durable-turn:1",
            "worker-a",
            conversation_id,
            EventSeq(1),
            None,
            100,
        )
        .unwrap()
        .with_model_lease_seconds(150);
        let StepDecision::Execute(model) = run
            .begin::<Value>(
                "model:0",
                0,
                RunStepKind::ModelRequest,
                &serde_json::json!({"round":0}),
                None,
                None,
                100,
            )
            .unwrap()
        else {
            panic!("model request was not claimed");
        };
        assert_eq!(model.lease_expires_at, Some(250));
        run.complete("model:0", &serde_json::json!({"text":"ready"}), 101)
            .unwrap();
        run.advance(0, 102).unwrap();
        let StepDecision::Execute(tool) = run
            .begin::<Value>(
                "tool:1:0",
                1,
                RunStepKind::ToolDispatch,
                &serde_json::json!({"tool":"noop"}),
                None,
                None,
                103,
            )
            .unwrap()
        else {
            panic!("tool request was not claimed");
        };
        assert_eq!(tool.lease_expires_at, Some(163));
    }

    #[test]
    fn reopen_reclaims_before_and_after_claim_and_replays_after_completion() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("durable.db");
        let db = file_db(&path);
        let conversation_id = seed(&db);
        let run = DurableRun::open(
            &db,
            "turn:durable-turn:1",
            "worker-a",
            conversation_id.clone(),
            EventSeq(1),
            None,
            100,
        )
        .unwrap()
        .with_lease_seconds(10);
        assert!(matches!(
            run.begin::<Value>(
                "model:0",
                0,
                RunStepKind::ModelRequest,
                &serde_json::json!({"round": 0}),
                None,
                None,
                100
            )
            .unwrap(),
            StepDecision::Execute(_)
        ));
        drop(run);
        drop(db);

        let reopened = file_db(&path);
        let run = DurableRun::open(
            &reopened,
            "turn:durable-turn:1",
            "worker-b",
            conversation_id,
            EventSeq(1),
            None,
            111,
        )
        .unwrap();
        assert!(matches!(
            run.begin::<Value>(
                "model:0",
                0,
                RunStepKind::ModelRequest,
                &serde_json::json!({"round": 0}),
                None,
                None,
                111
            )
            .unwrap(),
            StepDecision::Execute(_)
        ));
        run.complete("model:0", &serde_json::json!({"text": "done"}), 112)
            .unwrap();
        drop(run);
        drop(reopened);

        let reopened = file_db(&path);
        let run = DurableRun::open(
            &reopened,
            "turn:durable-turn:1",
            "worker-c",
            ConversationId::from("durable-turn"),
            EventSeq(1),
            None,
            113,
        )
        .unwrap();
        assert_eq!(
            run.begin::<Value>(
                "model:0",
                0,
                RunStepKind::ModelRequest,
                &serde_json::json!({"round": 0}),
                None,
                None,
                113
            )
            .unwrap(),
            StepDecision::Replay(serde_json::json!({"text": "done"}))
        );
        run.advance(0, 114).unwrap();
        run.define(
            "model:1",
            1,
            RunStepKind::ModelRequest,
            &serde_json::json!({"round": 1}),
            None,
            None,
        )
        .unwrap();
        assert_eq!(
            run.begin::<Value>(
                "model:0",
                0,
                RunStepKind::ModelRequest,
                &serde_json::json!({"round": 0}),
                None,
                None,
                115
            )
            .unwrap(),
            StepDecision::Replay(serde_json::json!({"text": "done"}))
        );
        assert_eq!(run.advance(0, 116).unwrap().cursor, 1);
    }

    #[test]
    fn process_kill_after_tool_checkpoint_reopens_and_resumes_next_model() {
        use std::process::{Command, Stdio};
        use std::time::{Duration, Instant};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("runner-tool-crash.db");
        let ready = dir.path().join("checkpointed");
        let db = file_db(&path);
        let conversation_id = seed(&db);
        drop(db);

        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "durable::tests::runner_crash_child_holds_tool_checkpoint_until_killed",
                "--nocapture",
            ])
            .env("EXECLAW_RUNNER_CRASH_DB", &path)
            .env("EXECLAW_RUNNER_CRASH_READY", &ready)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !ready.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(ready.exists(), "child did not checkpoint tool completion");
        child.kill().unwrap();
        let _ = child.wait();

        let reopened = file_db(&path);
        let run = DurableRun::open(
            &reopened,
            "turn:durable-turn:1",
            "recovery-worker",
            conversation_id,
            EventSeq(1),
            None,
            200,
        )
        .unwrap();
        let checkpoint: execlaw_runner_protocol::ModelRoundCheckpoint = run
            .replay_completed("model:0")
            .unwrap()
            .expect("model response must replay");
        let outcome: execlaw_runner_protocol::ToolOutcome = run
            .replay_completed("tool:0:0")
            .unwrap()
            .expect("tool outcome must replay");
        assert_eq!(checkpoint.tool_calls[0].id, "call-1");
        assert!(matches!(
            outcome,
            execlaw_runner_protocol::ToolOutcome::Ok { .. }
        ));

        run.advance(0, 201).unwrap();
        run.advance(1, 202).unwrap();
        assert!(matches!(
            run.begin::<execlaw_runner_protocol::ModelRoundCheckpoint>(
                "model:1",
                2,
                RunStepKind::ModelRequest,
                &serde_json::json!({"previous_round": 0, "tool_call_ids": ["call-1"]}),
                None,
                None,
                203,
            )
            .unwrap(),
            StepDecision::Execute(_)
        ));
    }

    #[test]
    fn process_kill_during_active_inference_reclaims_only_after_lease_expiry() {
        use std::process::{Command, Stdio};
        use std::time::{Duration, Instant};

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("active-inference-crash.db");
        let ready = dir.path().join("inference-started");
        let db = file_db(&path);
        let conversation_id = seed(&db);
        drop(db);

        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "durable::tests::active_inference_crash_child_holds_lease_until_killed",
                "--nocapture",
            ])
            .env("EXECLAW_ACTIVE_INFERENCE_CRASH_DB", &path)
            .env("EXECLAW_ACTIVE_INFERENCE_CRASH_READY", &ready)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !ready.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(ready.exists(), "child did not start the inference attempt");
        child.kill().unwrap();
        let _ = child.wait();

        let reopened = file_db(&path);
        let run = DurableRun::open(
            &reopened,
            "turn:durable-turn:1",
            "recovery-worker",
            conversation_id,
            EventSeq(1),
            None,
            101,
        )
        .unwrap();
        let input = serde_json::json!({"round": 0});
        assert!(matches!(
            run.begin::<Value>(
                "model:0",
                0,
                RunStepKind::ModelRequest,
                &input,
                None,
                None,
                101
            )
            .unwrap(),
            StepDecision::Busy { .. }
        ));
        assert!(matches!(
            run.begin::<Value>(
                "model:0",
                0,
                RunStepKind::ModelRequest,
                &input,
                None,
                None,
                161
            )
            .unwrap(),
            StepDecision::Execute(_)
        ));
        assert_eq!(run.start_inference_attempt("model:0", 161).unwrap(), 2);
        run.finish_inference_attempt("model:0", 2, true, None, 162)
            .unwrap();
        run.complete("model:0", &serde_json::json!({"text": "recovered"}), 162)
            .unwrap();
        assert_eq!(
            run.replay_completed::<Value>("model:0").unwrap(),
            Some(serde_json::json!({"text": "recovered"}))
        );
        let attempts: Vec<(i64, String)> = reopened
            .with_conn(|conn| {
                let mut statement = conn.prepare(
                    "SELECT attempt_no, status FROM state_run_inference_attempts \
                     WHERE run_id = 'turn:durable-turn:1' AND step_id = 'model:0' \
                     ORDER BY attempt_no",
                )?;
                Ok(statement
                    .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
                    .collect::<Result<_, _>>()?)
            })
            .unwrap();
        assert_eq!(attempts, [(1, "retrying".into()), (2, "succeeded".into())]);
    }

    #[test]
    fn active_inference_crash_child_holds_lease_until_killed() {
        let (Some(path), Some(ready)) = (
            std::env::var_os("EXECLAW_ACTIVE_INFERENCE_CRASH_DB"),
            std::env::var_os("EXECLAW_ACTIVE_INFERENCE_CRASH_READY"),
        ) else {
            return;
        };
        let db = file_db(std::path::Path::new(&path));
        let run = DurableRun::open(
            &db,
            "turn:durable-turn:1",
            "killed-worker",
            ConversationId::from("durable-turn"),
            EventSeq(1),
            None,
            100,
        )
        .unwrap();
        assert!(matches!(
            run.begin::<Value>(
                "model:0",
                0,
                RunStepKind::ModelRequest,
                &serde_json::json!({"round": 0}),
                None,
                None,
                100,
            )
            .unwrap(),
            StepDecision::Execute(_)
        ));
        assert_eq!(run.start_inference_attempt("model:0", 100).unwrap(), 1);
        std::fs::write(ready, b"started").unwrap();
        std::thread::park();
    }

    #[test]
    fn runner_crash_child_holds_tool_checkpoint_until_killed() {
        let (Some(path), Some(ready)) = (
            std::env::var_os("EXECLAW_RUNNER_CRASH_DB"),
            std::env::var_os("EXECLAW_RUNNER_CRASH_READY"),
        ) else {
            return;
        };
        let db = file_db(std::path::Path::new(&path));
        let run = DurableRun::open(
            &db,
            "turn:durable-turn:1",
            "killed-worker",
            ConversationId::from("durable-turn"),
            EventSeq(1),
            None,
            100,
        )
        .unwrap();
        let checkpoint: execlaw_runner_protocol::ModelRoundCheckpoint =
            serde_json::from_value(serde_json::json!({
                "round": 0,
                "model": "local",
                "text": "",
                "finish_reason": "tool_calls",
                "tool_calls": [{
                    "id": "call-1",
                    "type": "function",
                    "function": {"name": "test.effect", "arguments": "{}"}
                }]
            }))
            .unwrap();
        assert!(matches!(
            run.begin::<execlaw_runner_protocol::ModelRoundCheckpoint>(
                "model:0",
                0,
                RunStepKind::ModelRequest,
                &serde_json::json!({"round":0}),
                None,
                None,
                100,
            )
            .unwrap(),
            StepDecision::Execute(_)
        ));
        run.complete("model:0", &checkpoint, 101).unwrap();
        run.advance(0, 101).unwrap();
        run.define(
            "tool:0:0",
            1,
            RunStepKind::ToolDispatch,
            &serde_json::json!({
                "call_id":"call-1",
                "tool_name":"test.effect",
                "arguments":{}
            }),
            None,
            None,
        )
        .unwrap();
        assert!(matches!(
            run.begin::<execlaw_runner_protocol::ToolOutcome>(
                "tool:0:0",
                1,
                RunStepKind::ToolDispatch,
                &serde_json::json!({
                    "call_id":"call-1",
                    "tool_name":"test.effect",
                    "arguments":{}
                }),
                None,
                None,
                102,
            )
            .unwrap(),
            StepDecision::Execute(_)
        ));
        run.complete(
            "tool:0:0",
            &execlaw_runner_protocol::ToolOutcome::Ok {
                value: serde_json::json!({"accepted":true}),
            },
            103,
        )
        .unwrap();
        std::fs::write(ready, b"checkpointed").unwrap();
        std::thread::park();
    }

    #[test]
    fn approval_wait_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("approval.db");
        let db = file_db(&path);
        let conversation_id = seed(&db);
        let run = DurableRun::open(
            &db,
            "turn:approval:1",
            "worker-a",
            conversation_id.clone(),
            EventSeq(1),
            None,
            10,
        )
        .unwrap();
        assert!(matches!(
            run.begin::<Value>(
                "approval:0",
                0,
                RunStepKind::ApprovalWait,
                &serde_json::json!({"reason": "sensitive"}),
                Some("approval-1".into()),
                None,
                10
            )
            .unwrap(),
            StepDecision::Execute(_)
        ));
        run.wait_for_approval("approval:0", "approval-1", 11)
            .unwrap();
        drop(run);
        drop(db);

        let reopened = file_db(&path);
        let run = DurableRun::open(
            &reopened,
            "turn:approval:1",
            "worker-b",
            conversation_id,
            EventSeq(1),
            None,
            12,
        )
        .unwrap();
        assert_eq!(
            run.begin::<Value>(
                "approval:0",
                0,
                RunStepKind::ApprovalWait,
                &serde_json::json!({"reason": "sensitive"}),
                Some("approval-1".into()),
                None,
                12
            )
            .unwrap(),
            StepDecision::WaitForApproval {
                approval_id: "approval-1".into()
            }
        );
    }
}
