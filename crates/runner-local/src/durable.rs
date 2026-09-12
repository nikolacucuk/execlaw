//! Durable checkpoint coordination shared by in-process and server runners.

use execlaw_core::Database;
use execlaw_core::ids::{ConversationId, EventSeq};
use execlaw_core::runs::{
    NewRun, NewRunStep, NextSafeAction, RunRecord, RunStatus, RunStepKind, RunStepRecord,
    RunStepStatus, RunStore, RunStoreError,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

const OUTPUT_PREFIX: &str = "json:";

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
        let run_id = run_id.into();
        let store = RunStore::new(db);
        store.create_run_with_id(
            &run_id,
            &NewRun {
                conversation_id,
                parent_run_id,
                input_event_seq,
                started_at: now,
                deadline_at: None,
            },
        )?;
        Ok(Self {
            store,
            run_id,
            worker_id: worker_id.into(),
            lease_seconds: 60,
        })
    }

    /// Override the default lease duration for long-running operations or tests.
    pub fn with_lease_seconds(mut self, lease_seconds: i64) -> Self {
        self.lease_seconds = lease_seconds.max(1);
        self
    }

    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    /// Whether this stable run already reached its terminal success state.
    pub fn is_completed(&self) -> Result<bool, RunStoreError> {
        Ok(self
            .store
            .get_run(&self.run_id)?
            .is_some_and(|run| run.status == RunStatus::Completed))
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
                    now.saturating_add(self.lease_seconds),
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
