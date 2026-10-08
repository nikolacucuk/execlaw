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
use sha2::{Digest, Sha256};
use thiserror::Error;

const MAX_CHILD_TASKS_PER_PARENT: usize = 128;
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

/// Durable, typed handoff from a parent run to one child run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildTaskRecord {
    pub child_run_id: String,
    pub parent_run_id: String,
    pub task: serde_json::Value,
    pub task_hash: String,
    pub trust_ceiling: serde_json::Value,
    pub budget_tokens: u32,
    pub tokens_used: Option<u32>,
    pub budget_time_ms: u64,
    pub time_used_ms: Option<u64>,
    pub budget_retries: u32,
    pub retries_used: Option<u32>,
    pub budget_effects: u32,
    pub effects_used: Option<u32>,
    pub dependencies: Vec<String>,
    pub result_artifact_id: Option<String>,
    pub status: RunStatus,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Per-child share of its parent's durable execution budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChildExecutionBudget {
    pub tokens: u32,
    pub time_ms: u64,
    pub retries: u32,
    pub effects: u32,
}

/// Durable run-wide limits and current consumption counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunExecutionBudget {
    pub time_limit_ms: u64,
    pub deadline_at_ms: i64,
    pub retry_limit: u32,
    pub retries_used: u32,
    pub effect_limit: u32,
    pub effects_used: u32,
}

/// Error returned when persisted UTC observations show that wall time moved backward.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum RunClockError {
    #[error(
        "wall clock moved backward from {last_observed_ms} to {now_ms}; timer validity is unverifiable"
    )]
    Rollback { last_observed_ms: i64, now_ms: i64 },
}

/// Reject a timer decision when UTC moved backward relative to its durable observation.
pub fn validate_run_clock(last_observed_ms: i64, now_ms: i64) -> Result<(), RunClockError> {
    if now_ms < last_observed_ms {
        Err(RunClockError::Rollback {
            last_observed_ms,
            now_ms,
        })
    } else {
        Ok(())
    }
}

/// Versioned hashes of the effective prompt and model settings plus the
/// tool catalog snapshot used by a turn for consented offline replay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunInputManifest {
    pub input_version: u32,
    pub prompt_hash: String,
    pub model_settings_hash: String,
    pub tool_catalog_hash: String,
    pub tool_catalog_snapshot_json: Option<String>,
    pub recorded_at: i64,
}

/// Exact local plugin implementation and schema admitted to one durable run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunToolImplementationPin {
    pub runtime_kind: String,
    pub plugin_id: String,
    pub plugin_version: String,
    pub tool_name: String,
    pub artifact_sha256: String,
    #[serde(default)]
    pub input_schema_sha256: Option<String>,
    #[serde(default)]
    pub result_schema_sha256: Option<String>,
    #[serde(default)]
    pub server_identity: Option<String>,
}

fn validate_implementation_pins(pins: &[RunToolImplementationPin]) -> Result<(), RunStoreError> {
    let mut tools = std::collections::HashSet::new();
    for pin in pins {
        let valid_digest = |digest: &str| {
            digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
        };
        if !matches!(pin.runtime_kind.as_str(), "plugin" | "mcp")
            || pin.plugin_id.trim().is_empty()
            || pin.plugin_id.len() > 128
            || pin.plugin_version.trim().is_empty()
            || pin.plugin_version.len() > 128
            || pin.tool_name.trim().is_empty()
            || pin.tool_name.len() > 128
            || !valid_digest(&pin.artifact_sha256)
            || pin
                .input_schema_sha256
                .as_deref()
                .is_some_and(|hash| !valid_digest(hash))
            || pin
                .result_schema_sha256
                .as_deref()
                .is_some_and(|hash| !valid_digest(hash))
            || (pin.runtime_kind == "mcp"
                && pin.server_identity.as_deref().is_none_or(str::is_empty))
            || (pin.runtime_kind == "plugin" && pin.server_identity.is_some())
            || !tools.insert(pin.tool_name.as_str())
        {
            return Err(RunStoreError::Conflict(
                "implementation pins contain invalid or duplicate tool identities".into(),
            ));
        }
    }
    Ok(())
}

/// User-authored acceptance check attached to a durable run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct AcceptanceCriterion {
    pub criterion_id: String,
    pub description: String,
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verifier: Option<RunStepVerifier>,
}

/// Compare a durable step's checkpoint output with an expected JSON value.
/// Scheduled agent runs may use `agent:output` to inspect their persisted
/// output text (`/text`) or parsed JSON (`/json/...`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct RunStepVerifier {
    pub step_id: String,
    pub json_pointer: String,
    pub expected: serde_json::Value,
}

/// Artifact that must be present and verified before a run can be complete.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct RequiredRunArtifact {
    pub artifact_id: String,
    pub description: String,
}

/// User-authored requirements supplied when a durable task is started.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct RunCompletionContractDraft {
    pub acceptance_criteria: Vec<AcceptanceCriterion>,
    pub required_artifacts: Vec<RequiredRunArtifact>,
    pub delivery_required: bool,
}

impl RunCompletionContractDraft {
    /// Validate the immutable requirements before starting task execution.
    pub fn validate(&self) -> Result<(), RunStoreError> {
        validate_completion_contract(&RunCompletionContract {
            run_id: "validation".into(),
            acceptance_criteria: self.acceptance_criteria.clone(),
            required_artifacts: self.required_artifacts.clone(),
            delivery_required: self.delivery_required,
            created_at: 0,
        })
    }
}

/// Immutable completion contract for a durable run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunCompletionContract {
    pub run_id: String,
    pub acceptance_criteria: Vec<AcceptanceCriterion>,
    pub required_artifacts: Vec<RequiredRunArtifact>,
    pub delivery_required: bool,
    pub created_at: i64,
}

/// Deterministic result recorded by a verifier for one acceptance criterion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationStatus {
    Pending,
    Passed,
    Failed,
    Blocked,
}

impl VerificationStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::Blocked => "blocked",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "pending" => Some(Self::Pending),
            "passed" => Some(Self::Passed),
            "failed" => Some(Self::Failed),
            "blocked" => Some(Self::Blocked),
            _ => None,
        }
    }
}

/// Recorded verifier result and links to its supporting evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CriterionVerification {
    pub criterion_id: String,
    pub status: VerificationStatus,
    pub evidence_refs: Vec<String>,
    pub detail: Option<String>,
    pub verified_at: i64,
}

/// Result of checking one required artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactVerification {
    pub artifact_id: String,
    pub present: bool,
    pub evidence_ref: Option<String>,
    pub detail: Option<String>,
    pub checked_at: i64,
}

/// A completion result derived from persisted checks, not from a model reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunCompletionStatus {
    Incomplete,
    Partial,
    Blocked,
    VerifiedComplete,
}

/// Reviewable evidence and the current contract outcome for a run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunCompletionReport {
    pub contract: RunCompletionContract,
    pub verifications: Vec<CriterionVerification>,
    pub artifacts: Vec<ArtifactVerification>,
    pub delivery_confirmed: bool,
    pub delivery_evidence_ref: Option<String>,
    pub status: RunCompletionStatus,
    pub unfinished: Vec<String>,
}

/// Recovery decision for the run's current cursor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", content = "step", rename_all = "snake_case")]
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

/// A nonterminal run and the next durable transition its checkpoint state permits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunRecoveryCandidate {
    pub run: RunRecord,
    pub next_action: NextSafeAction,
}

/// A no-effect cursor transition completed automatically during recovery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunRecoveryTransition {
    pub run_id: String,
    pub action: &'static str,
    pub from_cursor: i64,
    pub to_cursor: i64,
    pub resulting_status: RunStatus,
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

fn child_dependency_cycle(
    graph: &std::collections::HashMap<String, Vec<String>>,
) -> Option<Vec<String>> {
    fn visit(
        node: &str,
        graph: &std::collections::HashMap<String, Vec<String>>,
        active: &mut Vec<String>,
        complete: &mut std::collections::HashSet<String>,
    ) -> Option<Vec<String>> {
        if let Some(position) = active.iter().position(|entry| entry == node) {
            let mut cycle = active[position..].to_vec();
            cycle.push(node.to_owned());
            return Some(cycle);
        }
        if complete.contains(node) {
            return None;
        }
        active.push(node.to_owned());
        if let Some(dependencies) = graph.get(node) {
            for dependency in dependencies {
                if graph.contains_key(dependency)
                    && let Some(cycle) = visit(dependency, graph, active, complete)
                {
                    return Some(cycle);
                }
            }
        }
        active.pop();
        complete.insert(node.to_owned());
        None
    }

    let mut complete = std::collections::HashSet::new();
    for node in graph.keys() {
        if let Some(cycle) = visit(node, graph, &mut Vec::new(), &mut complete) {
            return Some(cycle);
        }
    }
    None
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

    /// Atomically persist a child task contract and reserve its share of the
    /// parent's aggregate token budget. Repeating an identical child contract
    /// is idempotent; changed task authority or budget is a conflict.
    pub fn reserve_child_task(
        &self,
        parent_run_id: &str,
        child_run_id: &str,
        task: &serde_json::Value,
        task_hash: &str,
        trust_ceiling: &serde_json::Value,
        budget_tokens: u32,
        aggregate_limit_tokens: u32,
        dependencies: &[String],
        now: i64,
    ) -> Result<bool, RunStoreError> {
        let task_budget = ChildExecutionBudget {
            tokens: budget_tokens,
            time_ms: 120_000,
            retries: 0,
            effects: 0,
        };
        let aggregate_budget = ChildExecutionBudget {
            tokens: aggregate_limit_tokens,
            time_ms: 3_600_000,
            retries: 64,
            effects: 0,
        };
        self.reserve_child_task_with_budgets(
            parent_run_id,
            child_run_id,
            task,
            task_hash,
            trust_ceiling,
            task_budget,
            aggregate_budget,
            dependencies,
            now,
        )
    }

    /// Atomically reserve token, time, retry, and effect capacity for a child.
    /// Matching replays are idempotent; changed task or budget values conflict.
    pub fn reserve_child_task_with_budgets(
        &self,
        parent_run_id: &str,
        child_run_id: &str,
        task: &serde_json::Value,
        task_hash: &str,
        trust_ceiling: &serde_json::Value,
        task_budget: ChildExecutionBudget,
        aggregate_budget: ChildExecutionBudget,
        dependencies: &[String],
        now: i64,
    ) -> Result<bool, RunStoreError> {
        if task_budget.tokens == 0
            || aggregate_budget.tokens == 0
            || task_budget.tokens > aggregate_budget.tokens
            || task_budget.time_ms == 0
            || aggregate_budget.time_ms == 0
            || task_budget.time_ms > aggregate_budget.time_ms
            || task_budget.retries > aggregate_budget.retries
            || task_budget.effects > aggregate_budget.effects
        {
            return Err(RunStoreError::Conflict(
                "child budgets must be positive and fit the parent aggregate limits".into(),
            ));
        }
        if dependencies.len() > 64
            || dependencies.iter().any(|dependency| dependency.len() > 128)
            || dependencies
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len()
                != dependencies.len()
        {
            return Err(RunStoreError::Conflict(
                "child dependency list exceeds its bounds or repeats an edge".into(),
            ));
        }
        let task_json = serde_json::to_string(task)
            .map_err(|error| RunStoreError::Conflict(format!("encode child task: {error}")))?;
        let trust_json = serde_json::to_string(trust_ceiling).map_err(|error| {
            RunStoreError::Conflict(format!("encode child trust ceiling: {error}"))
        })?;
        let dependencies_json = serde_json::to_string(dependencies).map_err(|error| {
            RunStoreError::Conflict(format!("encode child dependencies: {error}"))
        })?;
        if task_json.len() > 64 * 1024 || trust_json.len() > 8 * 1024 || task_hash.len() != 64 {
            return Err(RunStoreError::Conflict(
                "child task contract is malformed or oversized".into(),
            ));
        }
        let task_time_ms = i64::try_from(task_budget.time_ms)
            .map_err(|_| RunStoreError::Conflict("child time budget is too large".into()))?;
        let aggregate_time_ms = i64::try_from(aggregate_budget.time_ms)
            .map_err(|_| RunStoreError::Conflict("parent time budget is too large".into()))?;
        self.db.transaction(|tx| {
            let parent_exists: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM state_runs WHERE run_id=?1)", [parent_run_id], |row| row.get(0))?;
            let child_parent: Option<String> = tx.query_row("SELECT parent_run_id FROM state_runs WHERE run_id=?1", [child_run_id], |row| row.get(0)).optional()?;
            if !parent_exists || child_parent.as_deref() != Some(parent_run_id) {
                return Err(DbError::Invariant("child contract must reference an existing child and parent run".into()));
            }
            let mut dependency_graph = std::collections::HashMap::<String, Vec<String>>::new();
            let mut dependency_statement = tx.prepare(
                "SELECT child_run_id, dependencies_json FROM state_run_child_tasks \
                 WHERE parent_run_id = ?1 ORDER BY child_run_id LIMIT ?2",
            )?;
            let dependency_rows = dependency_statement.query_map(
                params![parent_run_id, MAX_CHILD_TASKS_PER_PARENT as i64 + 1],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )?;
            for row in dependency_rows {
                let (run_id, encoded) = row?;
                let dependencies: Vec<String> = serde_json::from_str(&encoded).map_err(|error| {
                    DbError::Invariant(format!("stored child dependency list is invalid: {error}"))
                })?;
                dependency_graph.insert(run_id, dependencies);
            }
            if dependency_graph.len() >= MAX_CHILD_TASKS_PER_PARENT {
                return Err(DbError::Invariant(format!(
                    "parent child-task limit ({MAX_CHILD_TASKS_PER_PARENT}) reached"
                )));
            }
            dependency_graph.insert(child_run_id.to_owned(), dependencies.to_vec());
            if let Some(cycle) = child_dependency_cycle(&dependency_graph) {
                return Err(DbError::Invariant(format!(
                    "child dependency cycle rejected: {}",
                    cycle.join(" -> ")
                )));
            }
            tx.execute(
                "INSERT OR IGNORE INTO state_run_child_budgets(
                    parent_run_id,token_limit,tokens_reserved,updated_at,time_limit_ms,
                    time_reserved_ms,retry_limit,retries_reserved,effect_limit,effects_reserved
                 ) VALUES (?1,?2,0,?3,?4,0,?5,0,?6,0)",
                params![parent_run_id, aggregate_budget.tokens, now, aggregate_time_ms, aggregate_budget.retries, aggregate_budget.effects],
            )?;
            let limits: (u32, u32, i64, i64, u32, u32, u32, u32) = tx.query_row(
                "SELECT token_limit,tokens_reserved,time_limit_ms,time_reserved_ms,
                        retry_limit,retries_reserved,effect_limit,effects_reserved
                 FROM state_run_child_budgets WHERE parent_run_id=?1",
                [parent_run_id],
                |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?,row.get(7)?)),
            )?;
            if limits.0 != aggregate_budget.tokens || limits.2 != aggregate_time_ms
                || limits.4 != aggregate_budget.retries || limits.6 != aggregate_budget.effects
            {
                return Err(DbError::Invariant("parent aggregate child budget changed during a run".into()));
            }
            let existing: Option<(String, String, String, u32, String, i64, u32, u32)> = tx.query_row(
                "SELECT task_json,task_hash,trust_ceiling_json,budget_tokens,dependencies_json,
                        budget_time_ms,budget_retries,budget_effects
                 FROM state_run_child_tasks WHERE child_run_id=?1",
                [child_run_id],
                |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?,row.get(7)?)),
            ).optional()?;
            if let Some(existing) = existing {
                if existing == (task_json.clone(), task_hash.to_owned(), trust_json.clone(), task_budget.tokens, dependencies_json.clone(), task_time_ms, task_budget.retries, task_budget.effects) { return Ok(false); }
                return Err(DbError::Invariant("child run id was reused with a different task contract or budget".into()));
            }
            if limits.1.saturating_add(task_budget.tokens) > limits.0
                || limits.3.saturating_add(task_time_ms) > limits.2
                || limits.5.saturating_add(task_budget.retries) > limits.4
                || limits.7.saturating_add(task_budget.effects) > limits.6
            {
                return Err(DbError::Invariant("aggregate child execution budget exhausted".into()));
            }
            tx.execute(
                "INSERT INTO state_run_child_tasks(
                    child_run_id,parent_run_id,task_json,task_hash,trust_ceiling_json,
                    budget_tokens,dependencies_json,created_at,updated_at,budget_time_ms,
                    budget_retries,budget_effects
                 ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?8,?9,?10,?11)",
                params![child_run_id,parent_run_id,task_json,task_hash,trust_json,task_budget.tokens,dependencies_json,now,task_time_ms,task_budget.retries,task_budget.effects],
            )?;
            tx.execute(
                "UPDATE state_run_child_budgets SET tokens_reserved=tokens_reserved+?2,
                    time_reserved_ms=time_reserved_ms+?3,retries_reserved=retries_reserved+?4,
                    effects_reserved=effects_reserved+?5,updated_at=?6 WHERE parent_run_id=?1",
                params![parent_run_id,task_budget.tokens,task_time_ms,task_budget.retries,task_budget.effects,now],
            )?;
            Ok(true)
        }).map_err(RunStoreError::from)
    }

    /// Settle a child reservation from measured usage and attach an optional
    /// run-scoped result artifact. Missing usage leaves the full reservation
    /// charged so an unmeasured child cannot exceed the parent's budget.
    pub fn settle_child_task(
        &self,
        parent_run_id: &str,
        child_run_id: &str,
        tokens_used: Option<u32>,
        result_artifact_id: Option<&str>,
        now: i64,
    ) -> Result<(), RunStoreError> {
        self.settle_child_task_with_usage(
            parent_run_id,
            child_run_id,
            tokens_used,
            None,
            None,
            None,
            result_artifact_id,
            now,
        )
    }

    /// Settle all child reservations from measured use. Missing measurements
    /// charge the full reservation so an interrupted child cannot create
    /// capacity by disappearing before it reports consumption.
    pub fn settle_child_task_with_usage(
        &self,
        parent_run_id: &str,
        child_run_id: &str,
        tokens_used: Option<u32>,
        time_used_ms: Option<u64>,
        retries_used: Option<u32>,
        effects_used: Option<u32>,
        result_artifact_id: Option<&str>,
        now: i64,
    ) -> Result<(), RunStoreError> {
        let measured_time = time_used_ms
            .map(i64::try_from)
            .transpose()
            .map_err(|_| RunStoreError::Conflict("child time usage is too large".into()))?;
        self.db.transaction(|tx| {
            let row: Option<(u32,Option<u32>,i64,Option<i64>,u32,Option<u32>,u32,Option<u32>,bool)> = tx.query_row(
                "SELECT budget_tokens,tokens_used,budget_time_ms,time_used_ms,budget_retries,
                        retries_used,budget_effects,effects_used,budget_settled
                 FROM state_run_child_tasks WHERE child_run_id=?1 AND parent_run_id=?2",
                params![child_run_id,parent_run_id],
                |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?,row.get(7)?,row.get::<_,i64>(8)? != 0)),
            ).optional()?;
            let Some((token_budget,prior_tokens,time_budget,prior_time,retry_budget,prior_retries,effect_budget,prior_effects,settled)) = row else { return Err(DbError::Invariant("child task contract was not found".into())); };
            if settled { return Ok(()); }
            let token_charge = tokens_used.unwrap_or(token_budget);
            let time_charge = measured_time.unwrap_or(time_budget);
            let retry_charge = retries_used.unwrap_or(retry_budget);
            let effect_charge = effects_used.unwrap_or(effect_budget);
            tx.execute(
                "UPDATE state_run_child_budgets SET
                    tokens_reserved=MAX(0,tokens_reserved-?2+?3),
                    time_reserved_ms=MAX(0,time_reserved_ms-?4+?5),
                    retries_reserved=MAX(0,retries_reserved-?6+?7),
                    effects_reserved=MAX(0,effects_reserved-?8+?9),updated_at=?10
                 WHERE parent_run_id=?1",
                params![parent_run_id,token_budget,token_charge,time_budget,time_charge,retry_budget,retry_charge,effect_budget,effect_charge,now],
            )?;
            tx.execute(
                "UPDATE state_run_child_tasks SET tokens_used=COALESCE(?3,tokens_used),
                    time_used_ms=COALESCE(?4,time_used_ms),retries_used=COALESCE(?5,retries_used),
                    effects_used=COALESCE(?6,effects_used),result_artifact_id=COALESCE(?7,result_artifact_id),
                    budget_settled=1,updated_at=?8 WHERE child_run_id=?1 AND parent_run_id=?2",
                params![child_run_id,parent_run_id,tokens_used.or(prior_tokens),measured_time.or(prior_time),retries_used.or(prior_retries),effects_used.or(prior_effects),result_artifact_id,now],
            )?;
            Ok(())
        }).map_err(RunStoreError::from)
    }

    /// List typed child task contracts in creation order for the Agents and
    /// execution inspector views.
    pub fn list_child_tasks(
        &self,
        parent_run_id: &str,
    ) -> Result<Vec<ChildTaskRecord>, RunStoreError> {
        self.db.with_conn(|connection| {
            let mut statement = connection.prepare_cached("SELECT c.child_run_id,c.parent_run_id,c.task_json,c.task_hash,c.trust_ceiling_json,c.budget_tokens,c.tokens_used,c.dependencies_json,c.result_artifact_id,r.status,c.created_at,c.updated_at,c.budget_time_ms,c.time_used_ms,c.budget_retries,c.retries_used,c.budget_effects,c.effects_used FROM state_run_child_tasks c JOIN state_runs r ON r.run_id=c.child_run_id WHERE c.parent_run_id=?1 ORDER BY c.created_at,c.child_run_id")?;
            let rows = statement.query_map([parent_run_id], |row| {
                let task: String = row.get(2)?; let trust: String = row.get(4)?; let dependencies: String = row.get(7)?; let status: String = row.get(9)?;
                Ok(ChildTaskRecord { child_run_id:row.get(0)?,parent_run_id:row.get(1)?,task:serde_json::from_str(&task).unwrap_or(serde_json::Value::Null),task_hash:row.get(3)?,trust_ceiling:serde_json::from_str(&trust).unwrap_or(serde_json::Value::Null),budget_tokens:row.get(5)?,tokens_used:row.get(6)?,dependencies:serde_json::from_str(&dependencies).unwrap_or_default(),result_artifact_id:row.get(8)?,status:RunStatus::parse(&status).unwrap_or(RunStatus::Failed),created_at:row.get(10)?,updated_at:row.get(11)?,budget_time_ms:row.get::<_,i64>(12)?.max(0) as u64,time_used_ms:row.get::<_,Option<i64>>(13)?.map(|value| value.max(0) as u64),budget_retries:row.get(14)?,retries_used:row.get(15)?,budget_effects:row.get(16)?,effects_used:row.get(17)? })
            })?;
            rows.collect::<Result<Vec<_>,_>>().map_err(DbError::from)
        }).map_err(RunStoreError::from)
    }

    /// Create a pending run and return its generated identifier.
    pub fn create_run(&self, new_run: &NewRun) -> Result<String, RunStoreError> {
        let run_id = Uuid::new_v4().to_string();
        self.create_run_with_id(&run_id, new_run)?;
        Ok(run_id)
    }

    /// Create a fresh run fork linked to its source. The task's immutable
    /// acceptance requirements follow the fork, but verification and delivery
    /// evidence are reset; checkpoints, approvals, leases, and effect keys are
    /// deliberately not copied.
    pub fn fork_run(&self, source_run_id: &str, now: i64) -> Result<RunRecord, RunStoreError> {
        let source = self
            .get_run(source_run_id)?
            .ok_or_else(|| RunStoreError::NotFound {
                entity: "run",
                id: source_run_id.to_owned(),
            })?;
        let contract = self.completion_report(source_run_id)?;
        let new_run = NewRun {
            conversation_id: source.conversation_id.clone(),
            parent_run_id: Some(source.run_id.clone()),
            input_event_seq: source.input_event_seq,
            started_at: now,
            deadline_at: None,
        };
        let new_id = Uuid::new_v4().to_string();
        let new_contract = contract.map(|report| RunCompletionContract {
            run_id: new_id.clone(),
            acceptance_criteria: report.contract.acceptance_criteria,
            required_artifacts: report.contract.required_artifacts,
            delivery_required: report.contract.delivery_required,
            created_at: now,
        });
        if let Some(contract) = new_contract.as_ref() {
            validate_completion_contract(contract)?;
        }
        self.db.transaction(|tx| {
            tx.execute(
                "INSERT INTO state_runs \
                 (run_id, conversation_id, parent_run_id, status, cursor, input_event_seq, \
                  started_at, updated_at, deadline_at) \
                 VALUES (?1, ?2, ?3, 'pending', 0, ?4, ?5, ?5, NULL)",
                params![
                    new_id,
                    new_run.conversation_id.as_str(),
                    new_run.parent_run_id,
                    new_run.input_event_seq.0,
                    now,
                ],
            )?;
            if let Some(contract) = new_contract.as_ref() {
                let criteria_json = serde_json::to_string(&contract.acceptance_criteria)
                    .map_err(|error| DbError::Invariant(error.to_string()))?;
                let artifacts_json = serde_json::to_string(&contract.required_artifacts)
                    .map_err(|error| DbError::Invariant(error.to_string()))?;
                tx.execute(
                    "INSERT INTO state_run_completion_contracts \
                     (run_id, acceptance_criteria_json, required_artifacts_json, \
                      delivery_required, created_at, updated_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
                    params![
                        contract.run_id,
                        criteria_json,
                        artifacts_json,
                        contract.delivery_required,
                        now,
                    ],
                )?;
            }
            Ok(())
        })?;
        self.get_run(&new_id)?
            .ok_or_else(|| RunStoreError::NotFound {
                entity: "forked run",
                id: new_id,
            })
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
        let input = self.db.with_conn(|connection| {
            connection
                .query_row(
                    "SELECT e.payload,c.trust_class FROM state_conversations c \
                     LEFT JOIN state_events e ON e.conversation_id=c.conversation_id AND e.seq=?2 \
                     WHERE c.conversation_id=?1",
                    params![new_run.conversation_id.as_str(), new_run.input_event_seq.0],
                    |row| Ok((row.get::<_, Option<Vec<u8>>>(0)?, row.get::<_, String>(1)?)),
                )
                .optional()
                .map_err(DbError::from)
        })?;
        let (payload, trust_class) = input.unwrap_or_else(|| (None, "UnknownPending".into()));
        let source_bytes = payload.unwrap_or_else(|| run_id.as_bytes().to_vec());
        let subject = crate::information_store::InformationSubject {
            kind: "run".into(),
            id: run_id.to_owned(),
            sha256: hex::encode(Sha256::digest(&source_bytes)),
        };
        let labels = crate::information_store::InformationLabelStore::new(self.db);
        if labels.get(&subject)?.is_none() {
            let label = crate::information::InformationLabel::observed(
                crate::information::Sensitivity::Sensitive,
                Some(new_run.conversation_id.as_str().to_owned()),
                trust_class.clone(),
                "run_input",
                format!(
                    "{}:{}",
                    new_run.conversation_id.as_str(),
                    new_run.input_event_seq.0
                ),
                if matches!(
                    trust_class.as_str(),
                    "Controller" | "Delegated" | "KnownTrusted"
                ) {
                    vec!["transport:*".to_owned()]
                } else {
                    Vec::new()
                },
            );
            labels.observe(&subject, &label, "runs:store", new_run.started_at)?;
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

    /// Find the durable run created from one conversation input event.
    pub fn for_input_event(
        &self,
        conversation_id: &ConversationId,
        input_event_seq: EventSeq,
    ) -> Result<Option<RunRecord>, RunStoreError> {
        self.db
            .with_conn(|connection| {
                connection
                    .query_row(
                        "SELECT run_id, conversation_id, parent_run_id, status, cursor, \
                                input_event_seq, started_at, updated_at, deadline_at \
                         FROM state_runs WHERE conversation_id = ?1 AND input_event_seq = ?2 \
                         ORDER BY started_at DESC, run_id DESC LIMIT 1",
                        params![conversation_id.as_str(), input_event_seq.0],
                        row_to_run,
                    )
                    .optional()
                    .map_err(DbError::from)
            })
            .map_err(RunStoreError::from)
    }

    /// Create or load immutable run-wide time, retry, and effect limits.
    pub fn ensure_execution_budget(
        &self,
        run_id: &str,
        time_limit_ms: u64,
        retry_limit: u32,
        effect_limit: u32,
        now_ms: i64,
    ) -> Result<RunExecutionBudget, RunStoreError> {
        if time_limit_ms == 0 {
            return Err(RunStoreError::Conflict(
                "run time budget must be positive".into(),
            ));
        }
        let time_limit_ms = i64::try_from(time_limit_ms)
            .map_err(|_| RunStoreError::Conflict("run time budget is too large".into()))?;
        let deadline_at_ms = now_ms.saturating_add(time_limit_ms);
        self.db.with_conn(|connection| {
            connection.execute(
                "INSERT OR IGNORE INTO state_run_execution_budgets(
                    run_id,time_limit_ms,deadline_at_ms,retry_limit,effect_limit,updated_at_ms
                 ) VALUES (?1,?2,?3,?4,?5,?6)",
                params![run_id,time_limit_ms,deadline_at_ms,retry_limit,effect_limit,now_ms],
            )?;
            let stored: (i64,i64,u32,u32,u32,u32) = connection.query_row(
                "SELECT time_limit_ms,deadline_at_ms,retry_limit,retries_used,effect_limit,effects_used
                 FROM state_run_execution_budgets WHERE run_id=?1",
                [run_id],
                |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?)),
            )?;
            if stored.0 != time_limit_ms || stored.2 != retry_limit || stored.4 != effect_limit {
                return Err(DbError::Invariant(
                    "run execution budget changed during a durable run".into(),
                ));
            }
            connection.execute(
                "UPDATE state_runs SET deadline_at=COALESCE(deadline_at,?2) WHERE run_id=?1",
                params![run_id, stored.1.div_euclid(1_000)],
            )?;
            Ok(RunExecutionBudget {
                time_limit_ms: stored.0 as u64,
                deadline_at_ms: stored.1,
                retry_limit: stored.2,
                retries_used: stored.3,
                effect_limit: stored.4,
                effects_used: stored.5,
            })
        })
        .map_err(RunStoreError::from)
    }

    /// Return the saved execution budget for a durable run.
    pub fn execution_budget(
        &self,
        run_id: &str,
    ) -> Result<Option<RunExecutionBudget>, RunStoreError> {
        self.db.with_conn(|connection| {
            connection.query_row(
                "SELECT time_limit_ms,deadline_at_ms,retry_limit,retries_used,effect_limit,effects_used
                 FROM state_run_execution_budgets WHERE run_id=?1",
                [run_id],
                |row| Ok(RunExecutionBudget {
                    time_limit_ms: row.get::<_,i64>(0)?.max(0) as u64,
                    deadline_at_ms: row.get(1)?,
                    retry_limit: row.get(2)?,
                    retries_used: row.get(3)?,
                    effect_limit: row.get(4)?,
                    effects_used: row.get(5)?,
                }),
            ).optional().map_err(DbError::from)
        }).map_err(RunStoreError::from)
    }

    /// Observe UTC before making a timer decision; rollback since the previous
    /// durable observation invalidates the remaining-time calculation.
    pub fn execution_budget_at(
        &self,
        run_id: &str,
        now_ms: i64,
    ) -> Result<Option<RunExecutionBudget>, RunStoreError> {
        self.db.transaction(|tx| {
            let changed = tx.execute(
                "UPDATE state_run_execution_budgets SET updated_at_ms=?2 WHERE run_id=?1 AND updated_at_ms<=?2",
                params![run_id, now_ms],
            )?;
            if changed == 0 {
                let exists: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM state_run_execution_budgets WHERE run_id=?1)",
                    [run_id],
                    |row| row.get(0),
                )?;
                if exists {
                    let last_observed: i64 = tx.query_row(
                        "SELECT updated_at_ms FROM state_run_execution_budgets WHERE run_id=?1",
                        [run_id],
                        |row| row.get(0),
                    )?;
                    validate_run_clock(last_observed, now_ms)
                        .map_err(|error| DbError::Invariant(error.to_string()))?;
                }
                return Ok(None);
            }
            tx.query_row(
                "SELECT time_limit_ms,deadline_at_ms,retry_limit,retries_used,effect_limit,effects_used \
                 FROM state_run_execution_budgets WHERE run_id=?1",
                [run_id],
                |row| Ok(RunExecutionBudget {
                    time_limit_ms: row.get::<_, i64>(0)?.max(0) as u64,
                    deadline_at_ms: row.get(1)?,
                    retry_limit: row.get(2)?,
                    retries_used: row.get(3)?,
                    effect_limit: row.get(4)?,
                    effects_used: row.get(5)?,
                }),
            ).optional().map_err(DbError::from)
        }).map_err(RunStoreError::from)
    }

    // Step leases use whole UTC seconds. Preserve a newer millisecond observation
    // within that same second while still rejecting a rollback to an earlier second.
    fn observe_execution_budget_at_second(
        &self,
        run_id: &str,
        now: i64,
    ) -> Result<(), RunStoreError> {
        self.db.transaction(|tx| {
            let last_observed: Option<i64> = tx
                .query_row(
                    "SELECT updated_at_ms FROM state_run_execution_budgets WHERE run_id=?1",
                    [run_id],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(last_observed) = last_observed {
                if now < last_observed.div_euclid(1_000) {
                    return Err(DbError::Invariant(
                        RunClockError::Rollback {
                            last_observed_ms: last_observed,
                            now_ms: now.saturating_mul(1_000),
                        }
                        .to_string(),
                    ));
                }
                tx.execute(
                    "UPDATE state_run_execution_budgets SET updated_at_ms=MAX(updated_at_ms, ?2) WHERE run_id=?1",
                    params![run_id, now.saturating_mul(1_000)],
                )?;
            }
            Ok(())
        })
        .map_err(RunStoreError::from)
    }

    /// Consume one run-wide retry reservation if the deadline and quota allow it.
    pub fn consume_execution_retry(
        &self,
        run_id: &str,
        now_ms: i64,
    ) -> Result<bool, RunStoreError> {
        self.db
            .with_conn(|connection| {
                let changed = connection.execute(
                "UPDATE state_run_execution_budgets SET retries_used=retries_used+1,updated_at_ms=?2
                 WHERE run_id=?1 AND updated_at_ms<=?2 AND retries_used<retry_limit AND deadline_at_ms>?2",
                params![run_id,now_ms],
            )?;
                Ok(changed == 1)
            })
            .map_err(RunStoreError::from)
    }

    /// Claim one unique tool step against the durable effect quota.
    /// Reopening the same checkpoint does not consume the reservation twice.
    pub fn claim_execution_effect(
        &self,
        run_id: &str,
        step_id: &str,
        now_ms: i64,
    ) -> Result<bool, RunStoreError> {
        self.db.transaction(|tx| {
            let last_observed: Option<i64> = tx.query_row(
                "SELECT updated_at_ms FROM state_run_execution_budgets WHERE run_id=?1",
                [run_id],
                |row| row.get(0),
            ).optional()?;
            if let Some(last_observed) = last_observed {
                validate_run_clock(last_observed, now_ms).map_err(|error| DbError::Invariant(error.to_string()))?;
            }
            let claimed: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM state_run_effect_claims WHERE run_id=?1 AND step_id=?2)",
                params![run_id,step_id],
                |row| row.get(0),
            )?;
            if claimed {
                tx.execute(
                    "UPDATE state_run_execution_budgets SET updated_at_ms=?2 WHERE run_id=?1 AND updated_at_ms<=?2",
                    params![run_id, now_ms],
                )?;
                return Ok(true);
            }
            let changed = tx.execute(
                "UPDATE state_run_execution_budgets SET effects_used=effects_used+1,updated_at_ms=?2
                 WHERE run_id=?1 AND updated_at_ms<=?2 AND effects_used<effect_limit AND deadline_at_ms>?2",
                params![run_id,now_ms],
            )?;
            if changed != 1 {
                return Ok(false);
            }
            tx.execute(
                "INSERT INTO state_run_effect_claims(run_id,step_id,claimed_at_ms) VALUES (?1,?2,?3)",
                params![run_id,step_id,now_ms],
            )?;
            Ok(true)
        }).map_err(RunStoreError::from)
    }

    /// List recent child tasks across parents for the Agents overview.
    pub fn list_recent_child_tasks(
        &self,
        limit: usize,
    ) -> Result<Vec<ChildTaskRecord>, RunStoreError> {
        self.db.with_conn(|connection| {
            let mut statement = connection.prepare_cached("SELECT c.child_run_id,c.parent_run_id,c.task_json,c.task_hash,c.trust_ceiling_json,c.budget_tokens,c.tokens_used,c.dependencies_json,c.result_artifact_id,r.status,c.created_at,c.updated_at,c.budget_time_ms,c.time_used_ms,c.budget_retries,c.retries_used,c.budget_effects,c.effects_used FROM state_run_child_tasks c JOIN state_runs r ON r.run_id=c.child_run_id ORDER BY c.created_at DESC,c.child_run_id DESC LIMIT ?1")?;
            let rows = statement.query_map([limit.clamp(1, 100)], |row| {
                let task: String = row.get(2)?; let trust: String = row.get(4)?; let dependencies: String = row.get(7)?; let status: String = row.get(9)?;
                Ok(ChildTaskRecord { child_run_id:row.get(0)?,parent_run_id:row.get(1)?,task:serde_json::from_str(&task).unwrap_or(serde_json::Value::Null),task_hash:row.get(3)?,trust_ceiling:serde_json::from_str(&trust).unwrap_or(serde_json::Value::Null),budget_tokens:row.get(5)?,tokens_used:row.get(6)?,dependencies:serde_json::from_str(&dependencies).unwrap_or_default(),result_artifact_id:row.get(8)?,status:RunStatus::parse(&status).unwrap_or(RunStatus::Failed),created_at:row.get(10)?,updated_at:row.get(11)?,budget_time_ms:row.get::<_,i64>(12)?.max(0) as u64,time_used_ms:row.get::<_,Option<i64>>(13)?.map(|value| value.max(0) as u64),budget_retries:row.get(14)?,retries_used:row.get(15)?,budget_effects:row.get(16)?,effects_used:row.get(17)? })
            })?;
            rows.collect::<Result<Vec<_>,_>>().map_err(DbError::from)
        }).map_err(RunStoreError::from)
    }

    /// Record the immutable input fingerprint for a turn. Reopening a run
    /// with changed prompt, model settings, or catalog is reported as drift.
    pub fn record_input_manifest(
        &self,
        run_id: &str,
        manifest: &RunInputManifest,
    ) -> Result<(), RunStoreError> {
        let mut implementation_pins = Vec::new();
        if let Some(snapshot_json) = manifest.tool_catalog_snapshot_json.as_deref() {
            if snapshot_json.len() > 1024 * 1024 {
                return Err(RunStoreError::Conflict(
                    "tool catalog snapshot exceeds the 1 MiB storage limit".into(),
                ));
            }
            let snapshot: serde_json::Value =
                serde_json::from_str(snapshot_json).map_err(|error| {
                    RunStoreError::Conflict(format!("invalid tool catalog snapshot: {error}"))
                })?;
            implementation_pins = snapshot
                .get("implementation_pins")
                .map(|value| {
                    serde_json::from_value::<Vec<RunToolImplementationPin>>(value.clone()).map_err(
                        |error| {
                            RunStoreError::Conflict(format!(
                                "invalid implementation pins in tool snapshot: {error}"
                            ))
                        },
                    )
                })
                .transpose()?
                .unwrap_or_default();
            validate_implementation_pins(&implementation_pins)?;
            let tools = snapshot
                .get("tools")
                .and_then(serde_json::Value::as_array)
                .ok_or_else(|| {
                    RunStoreError::Conflict("tool catalog snapshot omitted tools".into())
                })?;
            let discoverable_tools = snapshot
                .get("discoverable_tools")
                .and_then(serde_json::Value::as_array)
                .ok_or_else(|| {
                    RunStoreError::Conflict(
                        "tool catalog snapshot omitted discoverable tools".into(),
                    )
                })?;
            let mut pinned = discoverable_tools.clone();
            pinned.extend(tools.iter().cloned());
            if crate::tool::tool_schema_hash(&serde_json::Value::Array(pinned))
                != manifest.tool_catalog_hash
            {
                return Err(RunStoreError::Conflict(
                    "tool catalog snapshot does not match its recorded hash".into(),
                ));
            }
        }
        self.db.transaction(|tx| {
            tx.execute(
                "INSERT OR IGNORE INTO state_run_input_manifests
                 (run_id, input_version, prompt_hash, model_settings_hash,
                  tool_catalog_hash, recorded_at, tool_catalog_snapshot_json)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    run_id,
                    i64::from(manifest.input_version),
                    manifest.prompt_hash,
                    manifest.model_settings_hash,
                    manifest.tool_catalog_hash,
                    manifest.recorded_at,
                    manifest.tool_catalog_snapshot_json,
                ],
            )?;
            let stored: (i64, String, String, String, Option<String>) = tx.query_row(
                "SELECT input_version, prompt_hash, model_settings_hash, tool_catalog_hash,
                        tool_catalog_snapshot_json
                 FROM state_run_input_manifests WHERE run_id = ?1",
                [run_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
            )?;
            if stored.0 != i64::from(manifest.input_version)
                || stored.1 != manifest.prompt_hash
                || stored.2 != manifest.model_settings_hash
                || stored.3 != manifest.tool_catalog_hash
            {
                return Err(DbError::Invariant(format!(
                    "run '{run_id}' was reopened with changed turn inputs (version={}, prompt={}, model_settings={}, tool_catalog={})",
                    stored.0 != i64::from(manifest.input_version),
                    stored.1 != manifest.prompt_hash,
                    stored.2 != manifest.model_settings_hash,
                    stored.3 != manifest.tool_catalog_hash,
                )));
            }
            match (
                stored.4.as_deref(),
                manifest.tool_catalog_snapshot_json.as_deref(),
            ) {
                (Some(stored_json), Some(new_json)) => {
                    let stored_value: serde_json::Value = serde_json::from_str(stored_json)
                        .map_err(|error| DbError::Serde(format!("stored tool catalog snapshot: {error}")))?;
                    let new_value: serde_json::Value = serde_json::from_str(new_json)
                        .map_err(|error| DbError::Serde(format!("new tool catalog snapshot: {error}")))?;
                    if stored_value != new_value {
                        return Err(DbError::Invariant(format!(
                            "run '{run_id}' was reopened with a different tool catalog snapshot"
                        )));
                    }
                }
                (None, Some(snapshot_json)) => {
                    tx.execute(
                        "UPDATE state_run_input_manifests SET tool_catalog_snapshot_json = ?1 \
                         WHERE run_id = ?2 AND tool_catalog_snapshot_json IS NULL",
                        params![snapshot_json, run_id],
                    )?;
                }
                _ => {}
            }
            for pin in &implementation_pins {
                let mutation_locked: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM state_plugin_mutation_leases WHERE plugin_id=?1)",
                    [&pin.plugin_id],
                    |row| row.get::<_, i64>(0).map(|value| value != 0),
                )?;
                if mutation_locked {
                    return Err(DbError::Invariant(format!(
                        "plugin '{}' is changing while run '{run_id}' pins its tool surface",
                        pin.plugin_id
                    )));
                }
                tx.execute(
                    "INSERT OR IGNORE INTO state_run_tool_implementation_pins
                     (run_id,runtime_kind,plugin_id,plugin_version,tool_name,artifact_sha256,
                      input_schema_sha256,result_schema_sha256,server_identity,created_at)
                     VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                    params![
                        run_id,
                        pin.runtime_kind,
                        pin.plugin_id,
                        pin.plugin_version,
                        pin.tool_name,
                        pin.artifact_sha256,
                        pin.input_schema_sha256,
                        pin.result_schema_sha256,
                        pin.server_identity,
                        manifest.recorded_at,
                    ],
                )?;
                let stored: (String, String, String, String, Option<String>, Option<String>, Option<String>) = tx.query_row(
                    "SELECT runtime_kind,plugin_id,plugin_version,artifact_sha256,input_schema_sha256,result_schema_sha256,server_identity
                     FROM state_run_tool_implementation_pins WHERE run_id=?1 AND tool_name=?2",
                    params![run_id, pin.tool_name],
                    |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?)),
                )?;
                if stored != (
                    pin.runtime_kind.clone(),
                    pin.plugin_id.clone(),
                    pin.plugin_version.clone(),
                    pin.artifact_sha256.clone(),
                    pin.input_schema_sha256.clone(),
                    pin.result_schema_sha256.clone(),
                    pin.server_identity.clone(),
                ) {
                    return Err(DbError::Invariant(format!(
                        "run '{run_id}' tool '{}' implementation pin changed",
                        pin.tool_name
                    )));
                }
            }
            Ok(())
        })?;
        Ok(())
    }

    /// Load immutable implementation pins recorded for a run.
    pub fn implementation_pins(
        &self,
        run_id: &str,
    ) -> Result<Vec<RunToolImplementationPin>, RunStoreError> {
        self.db
            .with_conn(|conn| {
                let mut statement = conn.prepare(
                    "SELECT runtime_kind,plugin_id,plugin_version,tool_name,artifact_sha256,
                            input_schema_sha256,result_schema_sha256,server_identity
                     FROM state_run_tool_implementation_pins WHERE run_id=?1 ORDER BY tool_name",
                )?;
                let rows = statement.query_map([run_id], |row| {
                    Ok(RunToolImplementationPin {
                        runtime_kind: row.get(0)?,
                        plugin_id: row.get(1)?,
                        plugin_version: row.get(2)?,
                        tool_name: row.get(3)?,
                        artifact_sha256: row.get(4)?,
                        input_schema_sha256: row.get(5)?,
                        result_schema_sha256: row.get(6)?,
                        server_identity: row.get(7)?,
                    })
                })?;
                rows.collect::<Result<Vec<_>, _>>().map_err(DbError::from)
            })
            .map_err(RunStoreError::from)
    }

    /// Number of pending, running, or approval-waiting runs pinned to this plugin.
    pub fn active_runs_pinning_plugin(&self, plugin_id: &str) -> Result<u64, RunStoreError> {
        self.db
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT COUNT(DISTINCT p.run_id)
                     FROM state_run_tool_implementation_pins p
                     JOIN state_runs r ON r.run_id=p.run_id
                     WHERE p.runtime_kind='plugin' AND p.plugin_id=?1 AND r.status IN ('pending','running','waiting')",
                    [plugin_id],
                    |row| row.get::<_, i64>(0),
                )
                .map(|count| count.max(0) as u64)
                .map_err(DbError::from)
            })
            .map_err(RunStoreError::from)
    }

    /// Load the immutable input fingerprint associated with a run.
    pub fn input_manifest(&self, run_id: &str) -> Result<Option<RunInputManifest>, RunStoreError> {
        self.db
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT input_version, prompt_hash, model_settings_hash,
                            tool_catalog_hash, recorded_at, tool_catalog_snapshot_json
                     FROM state_run_input_manifests WHERE run_id = ?1",
                    [run_id],
                    |row| {
                        let version: i64 = row.get(0)?;
                        Ok(RunInputManifest {
                            input_version: u32::try_from(version).unwrap_or_default(),
                            prompt_hash: row.get(1)?,
                            model_settings_hash: row.get(2)?,
                            tool_catalog_hash: row.get(3)?,
                            recorded_at: row.get(4)?,
                            tool_catalog_snapshot_json: row.get(5)?,
                        })
                    },
                )
                .optional()
                .map_err(DbError::from)
            })
            .map_err(RunStoreError::from)
    }

    /// Store a user acceptance contract once. Retrying the same definition is
    /// idempotent; changing criteria requires a new run so prior evidence
    /// cannot silently certify a different request.
    pub fn set_completion_contract(
        &self,
        contract: &RunCompletionContract,
    ) -> Result<(), RunStoreError> {
        validate_completion_contract(contract)?;
        if contract.acceptance_criteria.iter().any(|criterion| {
            criterion
                .verifier
                .as_ref()
                .is_some_and(|verifier| verifier.step_id == "agent:output")
        }) {
            return Err(RunStoreError::Corrupt(
                "agent:output verifiers require an agent run".into(),
            ));
        }
        let criteria_json = serde_json::to_string(&contract.acceptance_criteria)
            .map_err(|error| RunStoreError::Corrupt(error.to_string()))?;
        let artifacts_json = serde_json::to_string(&contract.required_artifacts)
            .map_err(|error| RunStoreError::Corrupt(error.to_string()))?;
        self.db.transaction(|tx| {
            tx.execute(
                "INSERT OR IGNORE INTO state_run_completion_contracts
                 (run_id, acceptance_criteria_json, required_artifacts_json,
                  delivery_required, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
                params![
                    contract.run_id,
                    criteria_json,
                    artifacts_json,
                    contract.delivery_required,
                    contract.created_at,
                ],
            )?;
            let stored: (String, String, bool) = tx.query_row(
                "SELECT acceptance_criteria_json, required_artifacts_json,
                        delivery_required
                 FROM state_run_completion_contracts WHERE run_id = ?1",
                [&contract.run_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
            if stored.0 != criteria_json
                || stored.1 != artifacts_json
                || stored.2 != contract.delivery_required
            {
                return Err(DbError::Invariant(format!(
                    "run '{}' already has a different completion contract",
                    contract.run_id
                )));
            }
            Ok(())
        })?;
        Ok(())
    }

    /// Record deterministic verifier output. A passing result must link to at
    /// least one evidence reference; model text alone is not verifier evidence.
    pub fn record_completion_verification(
        &self,
        run_id: &str,
        verification: &CriterionVerification,
    ) -> Result<(), RunStoreError> {
        validate_reference_list(&verification.evidence_refs)?;
        if verification.status == VerificationStatus::Passed
            && verification.evidence_refs.is_empty()
        {
            return Err(RunStoreError::Corrupt(
                "a passing verification requires evidence references".into(),
            ));
        }
        if verification
            .detail
            .as_ref()
            .is_some_and(|detail| detail.len() > 2_000)
        {
            return Err(RunStoreError::Corrupt(
                "verification detail exceeds 2000 bytes".into(),
            ));
        }
        let report = self
            .completion_report(run_id)?
            .ok_or_else(|| RunStoreError::NotFound {
                entity: "completion contract",
                id: run_id.to_owned(),
            })?;
        let criterion = report
            .contract
            .acceptance_criteria
            .iter()
            .find(|criterion| criterion.criterion_id == verification.criterion_id);
        let Some(criterion) = criterion else {
            return Err(RunStoreError::NotFound {
                entity: "acceptance criterion",
                id: verification.criterion_id.clone(),
            });
        };
        if criterion.verifier.is_some() {
            return Err(RunStoreError::Conflict(
                "a deterministic criterion cannot be overridden by a manual result".into(),
            ));
        }
        if verification.status == VerificationStatus::Passed {
            let row_id = format!("{run_id}/{}", verification.criterion_id);
            let valid = self.db.with_conn(|connection| {
                for reference in &verification.evidence_refs {
                    if crate::completion_evidence::attestation_exists(
                        connection,
                        reference,
                        "run_completion_verification",
                        &row_id,
                        "passed",
                        Some(&verification.evidence_refs),
                    )? {
                        return Ok(true);
                    }
                }
                Ok(false)
            })?;
            if !valid {
                return Err(RunStoreError::Corrupt(
                    "a manual passing result requires a scoped Controller attestation".into(),
                ));
            }
        }
        let evidence_json = serde_json::to_string(&verification.evidence_refs)
            .map_err(|error| RunStoreError::Corrupt(error.to_string()))?;
        self.db.transaction(|tx| {
            tx.execute(
                "INSERT INTO state_run_completion_verifications
                 (run_id, criterion_id, status, evidence_refs_json, detail, verified_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(run_id, criterion_id) DO UPDATE SET
                   status = excluded.status,
                   evidence_refs_json = excluded.evidence_refs_json,
                   detail = excluded.detail,
                   verified_at = excluded.verified_at",
                params![
                    run_id,
                    verification.criterion_id,
                    verification.status.as_str(),
                    evidence_json,
                    verification.detail,
                    verification.verified_at,
                ],
            )?;
            Ok(())
        })?;
        Ok(())
    }

    /// Record whether a required artifact was found and verified.
    pub fn record_artifact_verification(
        &self,
        run_id: &str,
        verification: &ArtifactVerification,
    ) -> Result<(), RunStoreError> {
        if verification.present
            && verification
                .evidence_ref
                .as_deref()
                .is_none_or(str::is_empty)
        {
            return Err(RunStoreError::Corrupt(
                "a present artifact requires an evidence reference".into(),
            ));
        }
        if verification
            .detail
            .as_ref()
            .is_some_and(|detail| detail.len() > 2_000)
            || verification
                .evidence_ref
                .as_ref()
                .is_some_and(|reference| reference.len() > 512)
        {
            return Err(RunStoreError::Corrupt(
                "artifact evidence fields exceed their size limits".into(),
            ));
        }
        let report = self
            .completion_report(run_id)?
            .ok_or_else(|| RunStoreError::NotFound {
                entity: "completion contract",
                id: run_id.to_owned(),
            })?;
        if !report
            .contract
            .required_artifacts
            .iter()
            .any(|artifact| artifact.artifact_id == verification.artifact_id)
        {
            return Err(RunStoreError::NotFound {
                entity: "required artifact",
                id: verification.artifact_id.clone(),
            });
        }
        if verification.present {
            let valid = self.db.with_conn(|connection| {
                crate::completion_evidence::run_artifact_exists(
                    connection,
                    run_id,
                    verification.evidence_ref.as_deref().unwrap_or_default(),
                    chrono::Utc::now().timestamp(),
                )
                .map_err(DbError::from)
            })?;
            if !valid {
                return Err(RunStoreError::Corrupt(
                    "artifact reference has no intact producer in this run".into(),
                ));
            }
        }
        self.db.transaction(|tx| {
            tx.execute(
                "INSERT INTO state_run_completion_artifacts
                 (run_id, artifact_id, present, evidence_ref, detail, checked_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(run_id, artifact_id) DO UPDATE SET
                   present = excluded.present,
                   evidence_ref = excluded.evidence_ref,
                   detail = excluded.detail,
                   checked_at = excluded.checked_at",
                params![
                    run_id,
                    verification.artifact_id,
                    verification.present,
                    verification.evidence_ref,
                    verification.detail,
                    verification.checked_at,
                ],
            )?;
            Ok(())
        })?;
        Ok(())
    }

    /// Confirm delivery only after a durable transport receipt or equivalent
    /// evidence exists. The evidence reference is retained in the report.
    pub fn confirm_run_delivery(
        &self,
        run_id: &str,
        evidence_ref: &str,
        confirmed_at: i64,
    ) -> Result<(), RunStoreError> {
        if evidence_ref.trim().is_empty() || evidence_ref.len() > 512 {
            return Err(RunStoreError::Corrupt(
                "delivery confirmation requires a bounded evidence reference".into(),
            ));
        }
        let valid = self.db.with_conn(|connection| {
            Ok(
                crate::completion_evidence::run_delivery_exists(connection, run_id, evidence_ref)?
                    || crate::completion_evidence::attestation_exists(
                        connection,
                        evidence_ref,
                        "run_completion_delivery",
                        run_id,
                        "confirmed",
                        None,
                    )?,
            )
        })?;
        if !valid {
            return Err(RunStoreError::Corrupt(
                "delivery reference has no scoped receipt or Controller attestation".into(),
            ));
        }
        let changed = self.db.with_conn(|connection| {
            connection
                .execute(
                    "UPDATE state_run_completion_contracts
                     SET delivery_confirmed = 1, delivery_evidence_ref = ?2, updated_at = ?3
                     WHERE run_id = ?1 AND delivery_required = 1",
                    params![run_id, evidence_ref, confirmed_at],
                )
                .map_err(DbError::from)
        })?;
        if changed != 1 {
            return Err(RunStoreError::Conflict(format!(
                "run '{run_id}' has no pending delivery requirement"
            )));
        }
        Ok(())
    }

    /// Derive a reviewable task outcome from persisted checks and delivery
    /// evidence. Executor completion never sets this outcome by itself.
    pub fn completion_report(
        &self,
        run_id: &str,
    ) -> Result<Option<RunCompletionReport>, RunStoreError> {
        self.db
            .with_conn(|connection| {
                let stored = connection
                    .query_row(
                        "SELECT acceptance_criteria_json, required_artifacts_json,
                                delivery_required, delivery_confirmed,
                                delivery_evidence_ref, created_at
                         FROM state_run_completion_contracts WHERE run_id = ?1",
                        [run_id],
                        |row| {
                            Ok((
                                row.get::<_, String>(0)?,
                                row.get::<_, String>(1)?,
                                row.get::<_, bool>(2)?,
                                row.get::<_, bool>(3)?,
                                row.get::<_, Option<String>>(4)?,
                                row.get::<_, i64>(5)?,
                            ))
                        },
                    )
                    .optional()?;
                let Some((
                    criteria_json,
                    artifacts_json,
                    delivery_required,
                    delivery_confirmed,
                    delivery_ref,
                    created_at,
                )) = stored
                else {
                    return Ok(None);
                };
                let contract = RunCompletionContract {
                    run_id: run_id.to_owned(),
                    acceptance_criteria: serde_json::from_str(&criteria_json).map_err(|error| {
                        DbError::Invariant(format!("invalid completion criteria: {error}"))
                    })?,
                    required_artifacts: serde_json::from_str(&artifacts_json).map_err(|error| {
                        DbError::Invariant(format!("invalid required artifacts: {error}"))
                    })?,
                    delivery_required,
                    created_at,
                };
                let mut verifications: Vec<CriterionVerification> = {
                    let mut statement = connection.prepare(
                        "SELECT criterion_id, status, evidence_refs_json, detail, verified_at
                         FROM state_run_completion_verifications WHERE run_id = ?1
                         ORDER BY criterion_id",
                    )?;
                    statement
                        .query_map([run_id], |row| {
                            let raw_status: String = row.get(1)?;
                            let status =
                                VerificationStatus::parse(&raw_status).ok_or_else(|| {
                                    rusqlite::Error::InvalidColumnType(
                                        1,
                                        "status".into(),
                                        rusqlite::types::Type::Text,
                                    )
                                })?;
                            let raw_refs: String = row.get(2)?;
                            let evidence_refs = serde_json::from_str(&raw_refs).map_err(|_| {
                                rusqlite::Error::InvalidColumnType(
                                    2,
                                    "evidence_refs_json".into(),
                                    rusqlite::types::Type::Text,
                                )
                            })?;
                            Ok(CriterionVerification {
                                criterion_id: row.get(0)?,
                                status,
                                evidence_refs,
                                detail: row.get(3)?,
                                verified_at: row.get(4)?,
                            })
                        })?
                        .collect::<Result<Vec<_>, _>>()?
                };
                for criterion in &contract.acceptance_criteria {
                    if let Some(verifier) = &criterion.verifier {
                        let step: Option<(String, String, Option<String>, Option<i64>)> = connection
                            .query_row(
                                "SELECT kind, status, output_ref, completed_at FROM state_run_steps \
                                 WHERE run_id = ?1 AND step_id = ?2",
                                params![run_id, verifier.step_id],
                                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                            )
                            .optional()?;
                        let (status, detail, verified_at) = match step {
                            None => (VerificationStatus::Pending, "step has not started", 0),
                            Some((kind, _, _, at))
                                if kind != "tool_dispatch"
                                    && kind != "artifact_publish"
                                    && kind != "deterministic_compute" => {
                                (VerificationStatus::Failed, "verifier step is not a host tool", at.unwrap_or(0))
                            }
                            Some((_, status, _, _))
                                if status == "pending" || status == "running" || status == "waiting" => {
                                (VerificationStatus::Pending, "step has not completed", 0)
                            }
                            Some((_, status, _, at)) if status != "completed" => {
                                (VerificationStatus::Failed, "step did not complete", at.unwrap_or(0))
                            }
                            Some((_, _, output, at)) => {
                                let actual = output
                                    .as_deref()
                                    .and_then(|output| output.strip_prefix("json:"))
                                    .and_then(|json| serde_json::from_str::<serde_json::Value>(json).ok())
                                    .and_then(|json| json.pointer(&verifier.json_pointer).cloned());
                                if actual.as_ref() == Some(&verifier.expected) {
                                    (VerificationStatus::Passed, "checkpoint value matched", at.unwrap_or(0))
                                } else {
                                    (VerificationStatus::Failed, "checkpoint value did not match", at.unwrap_or(0))
                                }
                            }
                        };
                        let evidence_refs = if status == VerificationStatus::Passed {
                            vec![format!("run:{run_id}/step:{}", verifier.step_id)]
                        } else {
                            Vec::new()
                        };
                        verifications.retain(|item| item.criterion_id != criterion.criterion_id);
                        verifications.push(CriterionVerification {
                            criterion_id: criterion.criterion_id.clone(),
                            status,
                            evidence_refs,
                            detail: Some(detail.into()),
                            verified_at,
                        });
                    } else if let Some(record) = verifications
                        .iter_mut()
                        .find(|record| record.criterion_id == criterion.criterion_id)
                        && record.status == VerificationStatus::Passed
                    {
                        let row_id = format!("{run_id}/{}", criterion.criterion_id);
                        let mut valid = false;
                        for reference in &record.evidence_refs {
                            if crate::completion_evidence::attestation_exists(
                                connection,
                                reference,
                                "run_completion_verification",
                                &row_id,
                                "passed",
                                Some(&record.evidence_refs),
                            )? {
                                valid = true;
                                break;
                            }
                        }
                        if !valid {
                            record.status = VerificationStatus::Blocked;
                            record.detail = Some("Controller attestation is missing or invalid".into());
                        }
                    }
                }
                let mut artifacts: Vec<ArtifactVerification> = {
                    let mut statement = connection.prepare(
                        "SELECT artifact_id, present, evidence_ref, detail, checked_at
                         FROM state_run_completion_artifacts WHERE run_id = ?1
                         ORDER BY artifact_id",
                    )?;
                    statement
                        .query_map([run_id], |row| {
                            Ok(ArtifactVerification {
                                artifact_id: row.get(0)?,
                                present: row.get(1)?,
                                evidence_ref: row.get(2)?,
                                detail: row.get(3)?,
                                checked_at: row.get(4)?,
                            })
                        })?
                        .collect::<Result<Vec<_>, _>>()?
                };
                for artifact in &mut artifacts {
                    if artifact.present
                        && !crate::completion_evidence::run_artifact_exists(
                            connection,
                            run_id,
                            artifact.evidence_ref.as_deref().unwrap_or_default(),
                            chrono::Utc::now().timestamp(),
                        )?
                    {
                        artifact.present = false;
                        artifact.detail = Some("produced artifact is missing or invalid".into());
                    }
                }
                let delivery_proven = if delivery_confirmed {
                    let reference = delivery_ref.as_deref().unwrap_or_default();
                    crate::completion_evidence::run_delivery_exists(connection, run_id, reference)?
                        || crate::completion_evidence::attestation_exists(
                            connection,
                            reference,
                            "run_completion_delivery",
                            run_id,
                            "confirmed",
                            None,
                        )?
                } else {
                    false
                };
                let mut report = build_completion_report(
                    contract,
                    verifications,
                    artifacts,
                    delivery_proven,
                    delivery_ref,
                );
                let executor_status: String = connection.query_row(
                    "SELECT status FROM state_runs WHERE run_id = ?1",
                    [run_id],
                    |row| row.get(0),
                )?;
                match executor_status.as_str() {
                    "completed" => {}
                    "failed" | "cancelled" => {
                        report.status = RunCompletionStatus::Blocked;
                        report.unfinished.push(format!("Executor run {executor_status}"));
                    }
                    _ if report.status == RunCompletionStatus::VerifiedComplete => {
                        report.status = RunCompletionStatus::Incomplete;
                        report.unfinished.push("Executor run has not completed".into());
                    }
                    _ => {}
                }
                Ok(Some(report))
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

    /// Describe recovery candidates without claiming leases or dispatching work.
    pub fn recovery_candidates(
        &self,
        now: i64,
        limit: usize,
    ) -> Result<Vec<RunRecoveryCandidate>, RunStoreError> {
        let runs = self.db.with_conn(|conn| {
            let mut statement = conn.prepare_cached(
                "SELECT run_id, conversation_id, parent_run_id, status, cursor, input_event_seq, \
                        started_at, updated_at, deadline_at FROM state_runs \
                 WHERE status IN ('pending', 'running', 'waiting') \
                 ORDER BY started_at, run_id LIMIT ?1",
            )?;
            statement
                .query_map([limit.clamp(1, 500)], row_to_run)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(DbError::from)
        })?;
        runs.into_iter()
            .map(|run| {
                let next_action = self.next_safe_action(&run.run_id, now)?;
                Ok(RunRecoveryCandidate { run, next_action })
            })
            .collect()
    }

    /// Advance already-completed checkpoints and empty terminal cursors without
    /// redispatching model, tool, approval, or external-effect work.
    pub fn recover_completed_transitions(
        &self,
        now: i64,
        candidate_limit: usize,
        transition_limit: usize,
    ) -> Result<Vec<RunRecoveryTransition>, RunStoreError> {
        let candidates = self.recovery_candidates(now, candidate_limit)?;
        let mut transitions = Vec::new();
        let transition_limit = transition_limit.clamp(1, 2_000);
        for candidate in candidates {
            let mut run = candidate.run;
            let mut action = candidate.next_action;
            loop {
                if transitions.len() >= transition_limit {
                    return Ok(transitions);
                }
                match action {
                    NextSafeAction::AdvanceCursor(_) => {
                        let from_cursor = run.cursor;
                        run = self.advance_cursor(&run.run_id, from_cursor, now)?;
                        transitions.push(RunRecoveryTransition {
                            run_id: run.run_id.clone(),
                            action: "advance_completed_checkpoint",
                            from_cursor,
                            to_cursor: run.cursor,
                            resulting_status: run.status,
                        });
                        action = self.next_safe_action(&run.run_id, now)?;
                    }
                    NextSafeAction::CompleteRun { cursor } => {
                        run = self.complete_run(&run.run_id, cursor, now)?;
                        transitions.push(RunRecoveryTransition {
                            run_id: run.run_id.clone(),
                            action: "complete_empty_cursor",
                            from_cursor: cursor,
                            to_cursor: cursor,
                            resulting_status: run.status,
                        });
                        break;
                    }
                    _ => break,
                }
            }
        }
        Ok(transitions)
    }

    /// List recent runs for the operator inspector in stable newest-first order.
    pub fn list_recent(
        &self,
        before_started_at: Option<i64>,
        before_run_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<RunRecord>, RunStoreError> {
        self.db.with_conn(|conn| {
            let mut statement = conn.prepare_cached(
                "SELECT run_id, conversation_id, parent_run_id, status, cursor, input_event_seq, started_at, updated_at, deadline_at \
                 FROM state_runs WHERE (?1 IS NULL OR started_at < ?1 OR (started_at=?1 AND run_id<?2)) \
                 ORDER BY started_at DESC, run_id DESC LIMIT ?3"
            )?;
            statement.query_map(params![before_started_at, before_run_id, limit.clamp(1, 200)], row_to_run)?
                .collect::<Result<Vec<_>, _>>().map_err(DbError::from)
        }).map_err(RunStoreError::from)
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

    /// Load every checkpoint in ordinal order for durable replay and audit.
    pub fn list_steps(&self, run_id: &str) -> Result<Vec<RunStepRecord>, RunStoreError> {
        self.db
            .with_conn(|connection| {
                let mut statement = connection.prepare_cached(
                    "SELECT run_id, step_id, ordinal, kind, status, attempt, input_hash, \
                        output_ref, approval_id, outbox_idempotency_key, lease_owner, \
                        lease_expires_at, started_at, completed_at \
                 FROM state_run_steps WHERE run_id = ?1 ORDER BY ordinal ASC",
                )?;
                let rows = statement.query_map([run_id], row_to_step)?;
                rows.collect::<Result<Vec<_>, _>>().map_err(DbError::from)
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
        self.observe_execution_budget_at_second(run_id, now)?;
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

    /// Start a persisted inference attempt on a currently leased model step.
    /// A prior `started` row indicates interruption and is closed as `retrying`
    /// before the new attempt is inserted.
    pub fn start_inference_attempt(
        &self,
        run_id: &str,
        step_id: &str,
        lease_owner: &str,
        started_at: i64,
    ) -> Result<u32, RunStoreError> {
        self.db
            .transaction(|tx| {
                let owns_lease: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM state_run_steps \
                 WHERE run_id = ?1 AND step_id = ?2 AND kind = 'model_request' \
                   AND status = 'running' AND lease_owner = ?3)",
                    params![run_id, step_id, lease_owner],
                    |row| row.get(0),
                )?;
                if !owns_lease {
                    return Err(DbError::Invariant(format!(
                        "worker does not own inference step '{run_id}/{step_id}'"
                    )));
                }
                tx.execute(
                    "UPDATE state_run_inference_attempts \
                 SET status = 'retrying', finished_at = ?3, error_class = 'interrupted' \
                 WHERE run_id = ?1 AND step_id = ?2 AND status = 'started'",
                    params![run_id, step_id, started_at],
                )?;
                let next: i64 = tx.query_row(
                    "SELECT COALESCE(MAX(attempt_no), 0) + 1 \
                 FROM state_run_inference_attempts WHERE run_id = ?1 AND step_id = ?2",
                    params![run_id, step_id],
                    |row| row.get(0),
                )?;
                tx.execute(
                    "INSERT INTO state_run_inference_attempts \
                 (run_id, step_id, attempt_no, status, started_at) \
                 VALUES (?1, ?2, ?3, 'started', ?4)",
                    params![run_id, step_id, next, started_at],
                )?;
                u32::try_from(next)
                    .map_err(|_| DbError::Invariant("inference attempt counter overflow".into()))
            })
            .map_err(RunStoreError::from)
    }

    /// Finish an inference attempt while retaining its retry classification.
    pub fn finish_inference_attempt(
        &self,
        run_id: &str,
        step_id: &str,
        attempt_no: u32,
        succeeded: bool,
        error_class: Option<&str>,
        finished_at: i64,
    ) -> Result<(), RunStoreError> {
        let status = if succeeded { "succeeded" } else { "failed" };
        let changed = self.db.with_conn(|connection| {
            connection
                .execute(
                    "UPDATE state_run_inference_attempts \
                 SET status = ?4, error_class = ?5, finished_at = ?6 \
                                 WHERE run_id = ?1 AND step_id = ?2 AND attempt_no = ?3 \
                                     AND status IN ('started', 'retrying')",
                    params![
                        run_id,
                        step_id,
                        attempt_no,
                        status,
                        error_class,
                        finished_at
                    ],
                )
                .map_err(DbError::from)
        })?;
        if changed != 1 {
            return Err(RunStoreError::Conflict(format!(
                "inference attempt '{run_id}/{step_id}/{attempt_no}' is not active"
            )));
        }
        Ok(())
    }

    pub fn mark_inference_attempt_retrying(
        &self,
        run_id: &str,
        step_id: &str,
        attempt_no: u32,
        error_class: &str,
        finished_at: i64,
    ) -> Result<(), RunStoreError> {
        let changed = self.db.with_conn(|connection| {
            connection
                .execute(
                    "UPDATE state_run_inference_attempts \
                 SET status = 'retrying', error_class = ?4, finished_at = ?5 \
                 WHERE run_id = ?1 AND step_id = ?2 AND attempt_no = ?3 AND status = 'started'",
                    params![run_id, step_id, attempt_no, error_class, finished_at],
                )
                .map_err(DbError::from)
        })?;
        if changed != 1 {
            return Err(RunStoreError::Conflict(format!(
                "inference attempt '{run_id}/{step_id}/{attempt_no}' is not active"
            )));
        }
        Ok(())
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

    /// Cancel an active run and its active descendants, failing their
    /// unfinished checkpoints so recovery cannot mistake an interrupted
    /// inference wait for live work.
    pub fn cancel_run(&self, run_id: &str, cancelled_at: i64) -> Result<bool, RunStoreError> {
        self.db.transaction(|tx| {
            let prior_status: Option<String> = tx
                .query_row(
                    "SELECT status FROM state_runs WHERE run_id=?1",
                    [run_id],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(prior_status) = prior_status else {
                return Ok(false);
            };
            let changed = tx.execute(
                "UPDATE state_runs SET status='cancelled',updated_at=?2 WHERE run_id=?1 AND status IN ('pending','running','waiting')",
                params![run_id,cancelled_at],
            )?;
            if changed == 0 && prior_status != "cancelled" {
                return Ok(false);
            }
            tx.execute(
                "WITH RECURSIVE descendants(run_id) AS (\
                    SELECT run_id FROM state_runs WHERE parent_run_id=?1 \
                    UNION \
                    SELECT child.run_id FROM state_runs child \
                    JOIN descendants parent ON child.parent_run_id=parent.run_id \
                 ) \
                 UPDATE state_runs SET status='cancelled',updated_at=?2 \
                 WHERE run_id IN (SELECT run_id FROM descendants) \
                   AND status IN ('pending','running','waiting')",
                params![run_id, cancelled_at],
            )?;
            tx.execute(
                "WITH RECURSIVE cancelled_runs(run_id) AS (\
                    SELECT ?1 \
                    UNION \
                    SELECT child.run_id FROM state_runs child \
                    JOIN cancelled_runs parent ON child.parent_run_id=parent.run_id \
                 ) \
                 UPDATE state_run_steps SET status='failed',output_ref='error:cancelled', \
                    lease_owner=NULL,lease_expires_at=NULL,completed_at=?2 \
                 WHERE run_id IN (SELECT run_id FROM cancelled_runs) \
                   AND status IN ('pending','running','waiting')",
                params![run_id,cancelled_at],
            )?;
            Ok(changed == 1)
        }).map_err(RunStoreError::from)
    }

    /// Fail one active run and close its unfinished checkpoints after a
    /// terminal runner or inference error. Completed evidence is retained.
    pub fn fail_run(&self, run_id: &str, failed_at: i64) -> Result<bool, RunStoreError> {
        self.db
            .transaction(|tx| {
                let changed = tx.execute(
                    "UPDATE state_runs SET status='failed',updated_at=?2
                 WHERE run_id=?1 AND status IN ('pending','running','waiting')",
                    params![run_id, failed_at],
                )?;
                if changed == 0 {
                    return Ok(false);
                }
                tx.execute(
                    "UPDATE state_run_steps SET status='failed',
                    output_ref=COALESCE(output_ref,'error:run_failed'),
                    lease_owner=NULL,lease_expires_at=NULL,completed_at=?2
                 WHERE run_id=?1 AND status IN ('pending','running','waiting')",
                    params![run_id, failed_at],
                )?;
                Ok(true)
            })
            .map_err(RunStoreError::from)
    }

    /// Fail an unfinished run only when its persisted execution deadline has
    /// expired. The deadline check and status change share one transaction so
    /// startup recovery cannot race a new claim against a stale read.
    pub fn fail_run_if_budget_expired(
        &self,
        run_id: &str,
        now_ms: i64,
    ) -> Result<bool, RunStoreError> {
        let _ = self.execution_budget_at(run_id, now_ms)?;
        self.db
            .transaction(|tx| {
                let changed = tx.execute(
                    "UPDATE state_runs SET status='failed',updated_at=?2
                     WHERE run_id=?1 AND status IN ('pending','running','waiting')
                       AND EXISTS (SELECT 1 FROM state_run_execution_budgets
                                   WHERE run_id=?1 AND deadline_at_ms<=?3)",
                    params![run_id, now_ms.div_euclid(1_000), now_ms],
                )?;
                if changed == 0 {
                    return Ok(false);
                }
                tx.execute(
                    "UPDATE state_run_steps SET status='failed',
                        output_ref=COALESCE(output_ref,'error:run_budget_expired'),
                        lease_owner=NULL,lease_expires_at=NULL,completed_at=?2
                     WHERE run_id=?1 AND status IN ('pending','running','waiting')",
                    params![run_id, now_ms.div_euclid(1_000)],
                )?;
                Ok(true)
            })
            .map_err(RunStoreError::from)
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

/// Validate a task acceptance contract before persisting it.
pub fn validate_completion_contract(contract: &RunCompletionContract) -> Result<(), RunStoreError> {
    if contract.run_id.trim().is_empty()
        || contract.acceptance_criteria.len() > 100
        || contract.required_artifacts.len() > 100
        || (contract.acceptance_criteria.is_empty() && contract.required_artifacts.is_empty())
        || (!contract
            .acceptance_criteria
            .iter()
            .any(|criterion| criterion.required)
            && contract.required_artifacts.is_empty())
    {
        return Err(RunStoreError::Corrupt(
            "a completion contract requires at least one required criterion or artifact".into(),
        ));
    }
    let mut criterion_ids = std::collections::HashSet::new();
    for criterion in &contract.acceptance_criteria {
        if criterion.criterion_id.trim().is_empty()
            || criterion.criterion_id.len() > 128
            || criterion.description.trim().is_empty()
            || criterion.description.len() > 2_000
            || !criterion_ids.insert(&criterion.criterion_id)
        {
            return Err(RunStoreError::Corrupt(
                "acceptance criteria need unique bounded ids and descriptions".into(),
            ));
        }
        if let Some(verifier) = &criterion.verifier {
            if verifier.step_id.trim().is_empty()
                || verifier.step_id.len() > 128
                || !verifier.json_pointer.starts_with('/')
                || verifier.json_pointer.len() > 256
                || serde_json::to_vec(&verifier.expected).is_ok_and(|encoded| encoded.len() > 4096)
            {
                return Err(RunStoreError::Corrupt(
                    "step verifier needs a bounded step id, JSON pointer, and expected value"
                        .into(),
                ));
            }
        }
    }
    let mut artifact_ids = std::collections::HashSet::new();
    for artifact in &contract.required_artifacts {
        if artifact.artifact_id.trim().is_empty()
            || artifact.artifact_id.len() > 128
            || artifact.description.trim().is_empty()
            || artifact.description.len() > 2_000
            || !artifact_ids.insert(&artifact.artifact_id)
        {
            return Err(RunStoreError::Corrupt(
                "required artifacts need unique bounded ids and descriptions".into(),
            ));
        }
    }
    Ok(())
}

/// Validate bounded evidence references used by run and agent verifiers.
pub fn validate_reference_list(references: &[String]) -> Result<(), RunStoreError> {
    if references.len() > 32
        || references
            .iter()
            .any(|reference| reference.trim().is_empty() || reference.len() > 512)
    {
        return Err(RunStoreError::Corrupt(
            "evidence references must be non-empty and bounded".into(),
        ));
    }
    Ok(())
}

/// Compute a reviewable completion report from persisted contract evidence.
pub fn build_completion_report(
    contract: RunCompletionContract,
    verifications: Vec<CriterionVerification>,
    artifacts: Vec<ArtifactVerification>,
    delivery_confirmed: bool,
    delivery_evidence_ref: Option<String>,
) -> RunCompletionReport {
    let verification_by_id: std::collections::HashMap<_, _> = verifications
        .into_iter()
        .map(|verification| (verification.criterion_id.clone(), verification))
        .collect();
    let verifications: Vec<_> = contract
        .acceptance_criteria
        .iter()
        .map(|criterion| {
            verification_by_id
                .get(&criterion.criterion_id)
                .cloned()
                .unwrap_or_else(|| CriterionVerification {
                    criterion_id: criterion.criterion_id.clone(),
                    status: VerificationStatus::Pending,
                    evidence_refs: Vec::new(),
                    detail: None,
                    verified_at: 0,
                })
        })
        .collect();
    let artifact_by_id: std::collections::HashMap<_, _> = artifacts
        .into_iter()
        .map(|artifact| (artifact.artifact_id.clone(), artifact))
        .collect();
    let artifacts: Vec<_> = contract
        .required_artifacts
        .iter()
        .map(|artifact| {
            artifact_by_id
                .get(&artifact.artifact_id)
                .cloned()
                .unwrap_or_else(|| ArtifactVerification {
                    artifact_id: artifact.artifact_id.clone(),
                    present: false,
                    evidence_ref: None,
                    detail: None,
                    checked_at: 0,
                })
        })
        .collect();
    let mut blocked = false;
    let mut incomplete = false;
    let mut partial = false;
    let mut unfinished = Vec::new();
    for (criterion, result) in contract.acceptance_criteria.iter().zip(&verifications) {
        match result.status {
            VerificationStatus::Passed => {}
            VerificationStatus::Failed | VerificationStatus::Blocked if criterion.required => {
                blocked = true;
                unfinished.push(result.detail.clone().unwrap_or_else(|| {
                    format!(
                        "Required criterion '{}' did not pass",
                        criterion.description
                    )
                }));
            }
            VerificationStatus::Failed | VerificationStatus::Blocked => {
                partial = true;
                unfinished.push(result.detail.clone().unwrap_or_else(|| {
                    format!(
                        "Optional criterion '{}' did not pass",
                        criterion.description
                    )
                }));
            }
            VerificationStatus::Pending if criterion.required => {
                incomplete = true;
                unfinished.push(format!(
                    "Required criterion '{}' is not verified",
                    criterion.description
                ));
            }
            VerificationStatus::Pending => {
                partial = true;
                unfinished.push(format!(
                    "Optional criterion '{}' is not verified",
                    criterion.description
                ));
            }
        }
    }
    for (requirement, result) in contract.required_artifacts.iter().zip(&artifacts) {
        if result.checked_at == 0 {
            incomplete = true;
            unfinished.push(format!(
                "Required artifact '{}' has not been checked",
                requirement.description
            ));
        } else if !result.present {
            blocked = true;
            unfinished.push(result.detail.clone().unwrap_or_else(|| {
                format!("Required artifact '{}' is missing", requirement.description)
            }));
        }
    }
    if contract.delivery_required && !delivery_confirmed {
        incomplete = true;
        unfinished.push("Required delivery has no durable confirmation".into());
    }
    let status = if blocked {
        RunCompletionStatus::Blocked
    } else if incomplete {
        RunCompletionStatus::Incomplete
    } else if partial {
        RunCompletionStatus::Partial
    } else {
        RunCompletionStatus::VerifiedComplete
    };
    RunCompletionReport {
        contract,
        verifications,
        artifacts,
        delivery_confirmed,
        delivery_evidence_ref,
        status,
        unfinished,
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
    use crate::attachments::AttachmentStore;
    use crate::audit::AuditStore;
    use crate::db::DbConfig;
    use crate::ids::IdempotencyKey;
    use crate::migrations::MigrationRunner;
    use crate::outbox::OutboxStatus;

    #[test]
    fn child_dependency_cycles_are_reported_with_an_actionable_path() {
        let graph = std::collections::HashMap::from([
            ("child-a".to_owned(), vec!["child-b".to_owned()]),
            ("child-b".to_owned(), vec!["child-c".to_owned()]),
            ("child-c".to_owned(), vec!["child-a".to_owned()]),
        ]);
        let cycle = child_dependency_cycle(&graph).unwrap();
        assert!(cycle.len() >= 4);
        assert_eq!(cycle.first(), cycle.last());
    }

    #[test]
    fn child_task_reservation_rejects_a_cycle_inside_its_transaction() {
        let db = fresh_db();
        let store = RunStore::new(&db);
        let parent = create_run(&store, None);
        let first = create_run(&store, Some(parent.clone()));
        let second = create_run(&store, Some(parent.clone()));
        let first_dependencies = vec![second.clone()];
        let second_dependencies = vec![first.clone()];
        store
            .reserve_child_task(
                &parent,
                &first,
                &serde_json::json!({"task":"first"}),
                &"a".repeat(64),
                &serde_json::json!({}),
                100,
                1_000,
                &first_dependencies,
                1,
            )
            .unwrap();
        let error = store
            .reserve_child_task(
                &parent,
                &second,
                &serde_json::json!({"task":"second"}),
                &"b".repeat(64),
                &serde_json::json!({}),
                100,
                1_000,
                &second_dependencies,
                2,
            )
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("child dependency cycle rejected")
        );
        let persisted: i64 = db
            .with_conn(|connection| {
                Ok(connection.query_row(
                    "SELECT COUNT(*) FROM state_run_child_tasks WHERE child_run_id=?1",
                    [&second],
                    |row| row.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(persisted, 0);
    }

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

    fn attest_criterion(db: &Database, run_id: &str, criterion_id: &str, evidence: &str) -> String {
        let id = AuditStore::new(db)
            .insert(
                "controller",
                "run_completion_verification",
                &format!("{run_id}/{criterion_id}"),
                None,
                Some(&serde_json::json!({"status":"passed","submitted_evidence_refs":[evidence]})),
            )
            .unwrap();
        format!("attestation:{id}")
    }

    fn attest_delivery(db: &Database, run_id: &str) -> String {
        let id = AuditStore::new(db)
            .insert(
                "controller",
                "run_completion_delivery",
                run_id,
                None,
                Some(&serde_json::json!({"status":"confirmed","submitted_evidence_ref":"sink receipt reviewed"})),
            )
            .unwrap();
        format!("attestation:{id}")
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
    fn expired_execution_budget_closes_running_step_once() {
        let db = fresh_db();
        let store = RunStore::new(&db);
        let run_id = create_run(&store, None);
        store
            .add_step(&run_id, &step("model", 0, RunStepKind::ModelRequest))
            .unwrap();
        store
            .ensure_execution_budget(&run_id, 1_000, 2, 1, 10_000)
            .unwrap();
        store
            .claim_step(&run_id, "model", "old-worker", 10, 30)
            .unwrap();

        assert!(!store.fail_run_if_budget_expired(&run_id, 10_999).unwrap());
        assert!(store.fail_run_if_budget_expired(&run_id, 11_000).unwrap());
        assert!(!store.fail_run_if_budget_expired(&run_id, 11_001).unwrap());
        assert!(matches!(
            store.next_safe_action(&run_id, 11).unwrap(),
            NextSafeAction::RunFailed
        ));
        let closed = store.get_step(&run_id, "model").unwrap().unwrap();
        assert_eq!(closed.status, RunStepStatus::Failed);
        assert_eq!(
            closed.output_ref.as_deref(),
            Some("error:run_budget_expired")
        );
        assert!(closed.lease_owner.is_none());
    }

    #[test]
    fn execution_budget_clock_rollback_is_rejected_and_deadline_is_immutable() {
        let db = fresh_db();
        let store = RunStore::new(&db);
        let run_id = create_run(&store, None);
        let budget = store
            .ensure_execution_budget(&run_id, 5_000, 2, 1, 10_000)
            .unwrap();
        assert_eq!(budget.deadline_at_ms, 15_000);
        let observed = store.execution_budget_at(&run_id, 12_000).unwrap().unwrap();
        assert_eq!(observed.deadline_at_ms, 15_000);
        assert!(matches!(
            store.execution_budget_at(&run_id, 11_999),
            Err(RunStoreError::Db(DbError::Invariant(message))) if message.contains("moved backward")
        ));
        assert_eq!(
            store
                .execution_budget_at(&run_id, 12_500)
                .unwrap()
                .unwrap()
                .deadline_at_ms,
            15_000
        );
    }

    #[test]
    fn second_precision_step_claim_preserves_same_second_budget_observation() {
        let db = fresh_db();
        let store = RunStore::new(&db);
        let run_id = create_run(&store, None);
        store
            .add_step(&run_id, &step("model", 0, RunStepKind::ModelRequest))
            .unwrap();
        store
            .ensure_execution_budget(&run_id, 10_000, 2, 1, 10_000)
            .unwrap();
        store.execution_budget_at(&run_id, 10_037).unwrap();
        store
            .claim_step(&run_id, "model", "worker", 10, 20)
            .unwrap();
        assert!(matches!(
            store.execution_budget_at(&run_id, 10_036),
            Err(RunStoreError::Db(DbError::Invariant(message))) if message.contains("moved backward")
        ));
        assert!(matches!(
            store.claim_step(&run_id, "model", "worker", 9, 20),
            Err(RunStoreError::Db(DbError::Invariant(message))) if message.contains("moved backward")
        ));
    }

    #[test]
    fn plugin_and_research_artifacts_need_a_run_producer_and_intact_bytes() {
        use crate::events::{EventKind, EventLog, PendingEvent, ToolResultPayload, ToolUsePayload};
        use crate::ids::{AttachmentId, ResearchJobId};
        use crate::research::{ResearchJobStatus, ResearchJobStore};
        use sha2::Digest;

        let db = fresh_db();
        let store = RunStore::new(&db);
        let run_id = create_run(&store, None);
        let cid = ConversationId::from("conversation-1");
        let dir = tempfile::tempdir().unwrap();
        let now = chrono::Utc::now().timestamp();
        let plugin = AttachmentStore::new(&db)
            .insert_plugin_artifact(
                dir.path(),
                "test-plugin",
                "report.json",
                "application/json",
                br#"{"ok":true}"#,
                Some(3_600),
                now,
            )
            .unwrap();
        let plugin_ref = format!("attachment:{}", plugin.attachment_id);
        let valid = |reference: &str| {
            db.with_conn(|connection| {
                crate::completion_evidence::run_artifact_exists(connection, &run_id, reference, now)
                    .map_err(DbError::from)
            })
            .unwrap()
        };
        assert!(!valid(&plugin_ref), "a file row alone is not run evidence");
        let log = EventLog::new(&db);
        let pair = |ordinal: u32, tool: &str, result: serde_json::Value| {
            vec![
                PendingEvent::encode(
                    EventKind::ToolUse,
                    &ToolUsePayload {
                        ordinal,
                        tool_name: tool.into(),
                        args_json: serde_json::json!({}),
                    },
                    Some("agent".into()),
                )
                .unwrap(),
                PendingEvent::encode(
                    EventKind::ToolResult,
                    &ToolResultPayload {
                        ordinal,
                        outcome: Ok(result),
                    },
                    Some("system".into()),
                )
                .unwrap(),
            ]
        };
        log.commit_turn(
            &cid,
            EventSeq(1),
            pair(
                0,
                "test-plugin.render",
                serde_json::json!({"attachment_id": plugin.attachment_id}),
            ),
        )
        .unwrap();
        assert!(valid(&plugin_ref));

        let research_id = ResearchJobId::from("research-qualification");
        ResearchJobStore::new(&db)
            .insert_pending(&research_id, &cid, "Find evidence", "Controller", None, now)
            .unwrap();
        let report_path = dir.path().join("research-report.md");
        std::fs::write(&report_path, b"verified research").unwrap();
        let report_id = AttachmentId::from("research-report-attachment");
        AttachmentStore::new(&db)
            .insert(&crate::attachments::AttachmentRow {
                id: report_id.clone(),
                conversation_id: cid.clone(),
                mime_type: "text/markdown".into(),
                path: report_path.to_string_lossy().into_owned(),
                sha256: format!("{:x}", sha2::Sha256::digest(b"verified research")),
                received_at: now,
                filename: Some("report.md".into()),
            })
            .unwrap();
        ResearchJobStore::new(&db)
            .finish(
                &research_id,
                ResearchJobStatus::Complete,
                None,
                Some(report_id.as_str()),
                now,
            )
            .unwrap();
        let research_ref = format!("attachment:{}", report_id.as_str());
        assert!(
            !valid(&research_ref),
            "a completed job needs a producer in this run"
        );
        log.commit_turn(
            &cid,
            EventSeq(3),
            pair(
                1,
                "research.start",
                serde_json::json!({"job": {"id": research_id.as_str()}}),
            ),
        )
        .unwrap();
        assert!(valid(&research_ref));
        std::fs::write(&report_path, b"tampered research").unwrap();
        assert!(
            !valid(&research_ref),
            "report reads must recheck the file hash"
        );
    }

    #[test]
    fn input_manifest_is_immutable_and_detects_backend_or_catalog_drift() {
        let db = fresh_db();
        let store = RunStore::new(&db);
        let run_id = create_run(&store, None);
        let run = store.get_run(&run_id).unwrap().unwrap();
        assert_eq!(
            store
                .for_input_event(&run.conversation_id, run.input_event_seq)
                .unwrap()
                .unwrap()
                .run_id,
            run_id
        );
        let original = RunInputManifest {
            input_version: 1,
            prompt_hash: "prompt-a".into(),
            model_settings_hash: "model-a".into(),
            tool_catalog_hash: "catalog-a".into(),
            tool_catalog_snapshot_json: None,
            recorded_at: 100,
        };
        store.record_input_manifest(&run_id, &original).unwrap();
        assert_eq!(
            store.input_manifest(&run_id).unwrap(),
            Some(original.clone())
        );

        let mut drifted = original;
        drifted.model_settings_hash = "model-b".into();
        assert!(matches!(
            store.record_input_manifest(&run_id, &drifted),
            Err(RunStoreError::Db(DbError::Invariant(_)))
        ));
    }

    #[test]
    fn input_manifest_persists_the_catalog_snapshot_for_local_replay() {
        let db = fresh_db();
        let store = RunStore::new(&db);
        let run_id = create_run(&store, None);
        let tool = serde_json::json!({
            "type":"function",
            "function":{
                "name":"fixture.echo",
                "description":"Synthetic fixture tool",
                "parameters":{"type":"object","properties":{}}
            }
        });
        let snapshot = serde_json::json!({
            "tools":[tool.clone()],
            "discoverable_tools":[],
            "implementation_pins":[{
                "runtime_kind":"plugin",
                "plugin_id":"fixture",
                "plugin_version":"1.2.3",
                "tool_name":"fixture.echo",
                "artifact_sha256":"a".repeat(64),
                "input_schema_sha256":null,
                "result_schema_sha256":null,
                "server_identity":null
            }]
        });
        let catalog_hash = crate::tool::tool_schema_hash(&serde_json::Value::Array(vec![tool]));
        let manifest = RunInputManifest {
            input_version: 1,
            prompt_hash: "prompt-fixture".into(),
            model_settings_hash: "model-fixture".into(),
            tool_catalog_hash: catalog_hash,
            tool_catalog_snapshot_json: Some(serde_json::to_string(&snapshot).unwrap()),
            recorded_at: 101,
        };
        store.record_input_manifest(&run_id, &manifest).unwrap();
        let pins = store.implementation_pins(&run_id).unwrap();
        assert_eq!(pins.len(), 1);
        assert_eq!(pins[0].plugin_version, "1.2.3");
        assert_eq!(store.active_runs_pinning_plugin("fixture").unwrap(), 1);
        assert_eq!(store.input_manifest(&run_id).unwrap(), Some(manifest));

        let mut mismatched = store.input_manifest(&run_id).unwrap().unwrap();
        let mut changed_snapshot: serde_json::Value =
            serde_json::from_str(mismatched.tool_catalog_snapshot_json.as_deref().unwrap())
                .unwrap();
        changed_snapshot["implementation_pins"][0]["artifact_sha256"] =
            serde_json::Value::String("b".repeat(64));
        mismatched.tool_catalog_snapshot_json = Some(changed_snapshot.to_string());
        assert!(matches!(
            store.record_input_manifest(&run_id, &mismatched),
            Err(RunStoreError::Db(DbError::Invariant(_)))
        ));
        store.complete_run(&run_id, 0, 102).unwrap();
        assert_eq!(store.active_runs_pinning_plugin("fixture").unwrap(), 0);
    }

    #[test]
    fn completion_contract_requires_verifier_artifact_and_delivery_evidence() {
        let db = fresh_db();
        let store = RunStore::new(&db);
        let run_id = create_run(&store, None);
        let contract = RunCompletionContract {
            run_id: run_id.clone(),
            acceptance_criteria: vec![AcceptanceCriterion {
                criterion_id: "tests-pass".into(),
                description: "Required workspace tests pass".into(),
                required: true,
                verifier: None,
            }],
            required_artifacts: vec![RequiredRunArtifact {
                artifact_id: "report".into(),
                description: "Review report".into(),
            }],
            delivery_required: true,
            created_at: 100,
        };
        store.set_completion_contract(&contract).unwrap();
        store.complete_run(&run_id, 0, 105).unwrap();

        assert_eq!(
            store.completion_report(&run_id).unwrap().unwrap().status,
            RunCompletionStatus::Incomplete
        );
        assert!(
            store
                .record_completion_verification(
                    &run_id,
                    &CriterionVerification {
                        criterion_id: "tests-pass".into(),
                        status: VerificationStatus::Passed,
                        evidence_refs: vec![],
                        detail: None,
                        verified_at: 110,
                    },
                )
                .is_err()
        );
        assert!(
            store
                .record_completion_verification(
                    &run_id,
                    &CriterionVerification {
                        criterion_id: "tests-pass".into(),
                        status: VerificationStatus::Passed,
                        evidence_refs: vec!["fabricated:pass".into()],
                        detail: None,
                        verified_at: 110,
                    },
                )
                .is_err()
        );
        store
            .record_completion_verification(
                &run_id,
                &CriterionVerification {
                    criterion_id: "tests-pass".into(),
                    status: VerificationStatus::Passed,
                    evidence_refs: vec![
                        "test-run:abc".into(),
                        attest_criterion(&db, &run_id, "tests-pass", "test-run:abc"),
                    ],
                    detail: None,
                    verified_at: 110,
                },
            )
            .unwrap();
        assert!(
            store
                .record_artifact_verification(
                    &run_id,
                    &ArtifactVerification {
                        artifact_id: "report".into(),
                        present: true,
                        evidence_ref: Some("attachment:nonexistent-report".into()),
                        detail: None,
                        checked_at: 120,
                    },
                )
                .is_err()
        );
        let artifacts_dir = tempfile::tempdir().unwrap();
        let produced = AttachmentStore::new(&db)
            .insert_tool_result_artifact(
                artifacts_dir.path(),
                &ConversationId::from("conversation-1"),
                &run_id,
                br#"{"report":true}"#,
                chrono::Utc::now().timestamp(),
            )
            .unwrap();
        store
            .record_artifact_verification(
                &run_id,
                &ArtifactVerification {
                    artifact_id: "report".into(),
                    present: true,
                    evidence_ref: Some(format!("attachment:{}", produced.attachment_id)),
                    detail: None,
                    checked_at: 120,
                },
            )
            .unwrap();
        assert_eq!(
            store.completion_report(&run_id).unwrap().unwrap().status,
            RunCompletionStatus::Incomplete,
            "a successful verifier and artifact do not imply delivered completion"
        );
        assert!(
            store
                .confirm_run_delivery(&run_id, "outbox:999999", 130)
                .is_err()
        );
        let delivery_attestation = attest_delivery(&db, &run_id);
        store
            .confirm_run_delivery(&run_id, &delivery_attestation, 130)
            .unwrap();
        let report = store.completion_report(&run_id).unwrap().unwrap();
        assert_eq!(report.status, RunCompletionStatus::VerifiedComplete);
        assert!(report.unfinished.is_empty());
        assert_eq!(
            report.delivery_evidence_ref.as_deref(),
            Some(delivery_attestation.as_str())
        );
        let path: String = db
            .with_conn(|connection| {
                Ok(connection.query_row(
                    "SELECT path FROM state_artifacts WHERE id=?1",
                    [&produced.attachment_id],
                    |row| row.get(0),
                )?)
            })
            .unwrap();
        std::fs::write(path, b"tampered").unwrap();
        assert_eq!(
            store.completion_report(&run_id).unwrap().unwrap().status,
            RunCompletionStatus::Blocked,
            "a purged or altered artifact must revoke verified completion"
        );
    }

    #[test]
    fn artifact_proof_is_scoped_to_the_producing_run_and_excludes_inbound_uploads() {
        let db = fresh_db();
        let store = RunStore::new(&db);
        let producer_run = create_run(&store, None);
        let review_run = create_run(&store, None);
        store
            .set_completion_contract(&RunCompletionContract {
                run_id: review_run.clone(),
                acceptance_criteria: Vec::new(),
                required_artifacts: vec![RequiredRunArtifact {
                    artifact_id: "report".into(),
                    description: "Produced report".into(),
                }],
                delivery_required: false,
                created_at: 10,
            })
            .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let produced = AttachmentStore::new(&db)
            .insert_tool_result_artifact(
                directory.path(),
                &ConversationId::from("conversation-1"),
                &producer_run,
                b"{}",
                chrono::Utc::now().timestamp(),
            )
            .unwrap();
        db.with_conn(|connection| {
            connection.execute(
                "INSERT INTO state_attachments(id,conversation_id,mime_type,path,sha256,received_at) \
                 VALUES('inbound-only','conversation-1','text/plain','inbound.txt','0000000000000000000000000000000000000000000000000000000000000000',10)",
                [],
            )?;
            Ok(())
        }).unwrap();
        for reference in [
            format!("attachment:{}", produced.attachment_id),
            "attachment:inbound-only".into(),
        ] {
            assert!(
                store
                    .record_artifact_verification(
                        &review_run,
                        &ArtifactVerification {
                            artifact_id: "report".into(),
                            present: true,
                            evidence_ref: Some(reference),
                            detail: None,
                            checked_at: 11,
                        },
                    )
                    .is_err()
            );
        }
    }

    #[test]
    fn delivery_proof_requires_a_delivered_transport_outbox_event_in_run_scope() {
        let db = fresh_db();
        let store = RunStore::new(&db);
        let run_id = create_run(&store, None);
        store
            .set_completion_contract(&RunCompletionContract {
                run_id: run_id.clone(),
                acceptance_criteria: vec![AcceptanceCriterion {
                    criterion_id: "review".into(),
                    description: "Reviewed".into(),
                    required: true,
                    verifier: None,
                }],
                required_artifacts: Vec::new(),
                delivery_required: true,
                created_at: 10,
            })
            .unwrap();
        let outbox_id = db.with_conn(|connection| {
            connection.execute(
                "INSERT INTO state_outbox(idempotency_key,conversation_id,effect_kind,payload,status,attempts,enqueued_seq) \
                 VALUES('delivery-proof','conversation-1','transport.send',X'00','pending',0,1)",
                [],
            )?;
            Ok(connection.last_insert_rowid())
        }).unwrap();
        let reference = format!("outbox:{outbox_id}");
        assert!(store.confirm_run_delivery(&run_id, &reference, 12).is_err());
        db.with_conn(|connection| {
            connection.execute("UPDATE state_outbox SET status='delivered' WHERE id=?1", [outbox_id])?;
            connection.execute(
                "INSERT INTO state_outbox_delivery_events(outbox_id,transition,occurred_at,attempt) \
                 VALUES(?1,'delivered',13,1)",
                [outbox_id],
            )?;
            Ok(())
        }).unwrap();
        store.confirm_run_delivery(&run_id, &reference, 14).unwrap();
        assert!(
            store
                .completion_report(&run_id)
                .unwrap()
                .unwrap()
                .delivery_confirmed
        );
    }

    #[test]
    fn step_verifier_derives_required_test_result_from_durable_checkpoint() {
        let db = fresh_db();
        let store = RunStore::new(&db);
        for (exit_code, step_kind, expected_status) in [
            (1, RunStepKind::ToolDispatch, RunCompletionStatus::Blocked),
            (
                0,
                RunStepKind::ToolDispatch,
                RunCompletionStatus::VerifiedComplete,
            ),
            (0, RunStepKind::ModelRequest, RunCompletionStatus::Blocked),
        ] {
            let run_id = create_run(&store, None);
            store
                .set_completion_contract(&RunCompletionContract {
                    run_id: run_id.clone(),
                    acceptance_criteria: vec![AcceptanceCriterion {
                        criterion_id: "required-tests".into(),
                        description: "Required tests exit successfully".into(),
                        required: true,
                        verifier: Some(RunStepVerifier {
                            step_id: "test-suite".into(),
                            json_pointer: "/exit_code".into(),
                            expected: serde_json::json!(0),
                        }),
                    }],
                    required_artifacts: Vec::new(),
                    delivery_required: false,
                    created_at: 100,
                })
                .unwrap();
            assert_eq!(
                store.completion_report(&run_id).unwrap().unwrap().status,
                RunCompletionStatus::Incomplete
            );
            assert!(matches!(
                store.record_completion_verification(
                    &run_id,
                    &CriterionVerification {
                        criterion_id: "required-tests".into(),
                        status: VerificationStatus::Passed,
                        evidence_refs: vec!["fabricated:pass".into()],
                        detail: None,
                        verified_at: 101,
                    },
                ),
                Err(RunStoreError::Conflict(_))
            ));
            store
                .add_step(&run_id, &step("test-suite", 0, step_kind))
                .unwrap();
            store
                .claim_step(&run_id, "test-suite", "worker", 102, 160)
                .unwrap();
            store
                .complete_step(
                    &run_id,
                    "test-suite",
                    "worker",
                    Some(&format!("json:{{\"exit_code\":{exit_code}}}")),
                    103,
                )
                .unwrap();
            if exit_code == 0 && step_kind == RunStepKind::ToolDispatch {
                assert_eq!(
                    store.completion_report(&run_id).unwrap().unwrap().status,
                    RunCompletionStatus::Incomplete,
                    "a passing tool checkpoint cannot complete a live run"
                );
            }
            store.advance_cursor(&run_id, 0, 104).unwrap();
            store.complete_run(&run_id, 1, 105).unwrap();
            let report = store.completion_report(&run_id).unwrap().unwrap();
            assert_eq!(report.status, expected_status);
            assert_eq!(
                report.verifications[0].status,
                if exit_code == 0 && step_kind == RunStepKind::ToolDispatch {
                    VerificationStatus::Passed
                } else {
                    VerificationStatus::Failed
                }
            );
        }
    }

    #[test]
    fn failed_required_check_blocks_completion_and_optional_pending_is_partial() {
        let db = fresh_db();
        let store = RunStore::new(&db);
        let blocked_run = create_run(&store, None);
        store
            .set_completion_contract(&RunCompletionContract {
                run_id: blocked_run.clone(),
                acceptance_criteria: vec![AcceptanceCriterion {
                    criterion_id: "required".into(),
                    description: "Required check".into(),
                    required: true,
                    verifier: None,
                }],
                required_artifacts: vec![],
                delivery_required: false,
                created_at: 100,
            })
            .unwrap();
        store
            .record_completion_verification(
                &blocked_run,
                &CriterionVerification {
                    criterion_id: "required".into(),
                    status: VerificationStatus::Failed,
                    evidence_refs: vec!["test-run:failed".into()],
                    detail: Some("one required test failed".into()),
                    verified_at: 110,
                },
            )
            .unwrap();
        assert_eq!(
            store
                .completion_report(&blocked_run)
                .unwrap()
                .unwrap()
                .status,
            RunCompletionStatus::Blocked
        );

        let partial_run = create_run(&store, None);
        store
            .set_completion_contract(&RunCompletionContract {
                run_id: partial_run.clone(),
                acceptance_criteria: vec![
                    AcceptanceCriterion {
                        criterion_id: "required".into(),
                        description: "Required check".into(),
                        required: true,
                        verifier: None,
                    },
                    AcceptanceCriterion {
                        criterion_id: "optional".into(),
                        description: "Optional polish".into(),
                        required: false,
                        verifier: None,
                    },
                ],
                required_artifacts: vec![],
                delivery_required: false,
                created_at: 100,
            })
            .unwrap();
        store
            .record_completion_verification(
                &partial_run,
                &CriterionVerification {
                    criterion_id: "required".into(),
                    status: VerificationStatus::Passed,
                    evidence_refs: vec![
                        "test-run:passed".into(),
                        attest_criterion(&db, &partial_run, "required", "test-run:passed"),
                    ],
                    detail: None,
                    verified_at: 110,
                },
            )
            .unwrap();
        assert_eq!(
            store
                .completion_report(&partial_run)
                .unwrap()
                .unwrap()
                .status,
            RunCompletionStatus::Partial
        );
    }

    #[test]
    fn interrupted_inference_attempt_is_recovered_and_history_is_durable() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("inference-attempts.db");
        let db = Database::open(&DbConfig {
            path: path.clone(),
            key: None,
        })
        .unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        seed_conversation(&db);
        let store = RunStore::new(&db);
        let run_id = create_run(&store, None);
        store
            .add_step(&run_id, &step("model", 0, RunStepKind::ModelRequest))
            .unwrap();
        store
            .claim_step(&run_id, "model", "worker-a", 100, 200)
            .unwrap();
        assert_eq!(
            store
                .start_inference_attempt(&run_id, "model", "worker-a", 110)
                .unwrap(),
            1
        );
        drop(db);

        let reopened = Database::open(&DbConfig { path, key: None }).unwrap();
        MigrationRunner::new(&reopened).apply_all().unwrap();
        let recovered = RunStore::new(&reopened);
        recovered
            .claim_step(&run_id, "model", "worker-b", 200, 300)
            .unwrap();
        assert_eq!(
            recovered
                .start_inference_attempt(&run_id, "model", "worker-b", 210)
                .unwrap(),
            2
        );
        recovered
            .finish_inference_attempt(&run_id, "model", 2, true, None, 240)
            .unwrap();

        let attempts: Vec<(i64, String, Option<String>)> = reopened
            .with_conn(|connection| {
                let mut statement = connection.prepare(
                    "SELECT attempt_no, status, error_class FROM state_run_inference_attempts \
                 WHERE run_id = ?1 AND step_id = 'model' ORDER BY attempt_no",
                )?;
                Ok(statement
                    .query_map([&run_id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
                    .collect::<Result<Vec<_>, _>>()?)
            })
            .unwrap();
        assert_eq!(
            attempts,
            vec![
                (1, "retrying".into(), Some("interrupted".into())),
                (2, "succeeded".into(), None),
            ]
        );
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
    fn completed_outbox_enqueue_remains_idempotent_after_database_reopen() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("durable-outbox.db");
        let db = Database::open(&DbConfig {
            path: path.clone(),
            key: None,
        })
        .unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        seed_conversation(&db);
        let store = RunStore::new(&db);
        let run_id = create_run(&store, None);
        let mut enqueue = step("enqueue", 0, RunStepKind::OutboxEnqueue);
        enqueue.outbox_idempotency_key = Some("restart-effect-key".into());
        store.add_step(&run_id, &enqueue).unwrap();
        store
            .claim_step(&run_id, "enqueue", "worker-before-restart", 100, 200)
            .unwrap();
        let outbox_row = OutboxRow {
            id: None,
            idempotency_key: IdempotencyKey::from_string("restart-effect-key"),
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
                "worker-before-restart",
                &outbox_row,
                Some("outbox:restart-effect-key"),
                120,
            )
            .unwrap();
        drop(db);

        let reopened = Database::open(&DbConfig { path, key: None }).unwrap();
        MigrationRunner::new(&reopened).apply_all().unwrap();
        let recovered = RunStore::new(&reopened);
        let step = recovered.get_step(&run_id, "enqueue").unwrap().unwrap();
        assert_eq!(step.status, RunStepStatus::Completed);
        recovered
            .enqueue_outbox_and_complete_step(
                &run_id,
                "enqueue",
                "worker-after-restart",
                &outbox_row,
                Some("outbox:restart-effect-key"),
                130,
            )
            .unwrap();
        let outbox_count: i64 = reopened
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM state_outbox WHERE idempotency_key = 'restart-effect-key'",
                    [],
                    |row| row.get(0),
                )
                .map_err(DbError::from)
            })
            .unwrap();
        assert_eq!(outbox_count, 1);
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
    fn aggregate_child_token_reservations_survive_restart_and_block_overcommit() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("parent-child-budget.db");
        let db = Database::open(&DbConfig {
            path: path.clone(),
            key: None,
        })
        .unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        seed_conversation(&db);
        let store = RunStore::new(&db);
        let parent = create_run(&store, None);
        let first = create_run(&store, Some(parent.clone()));
        let second = create_run(&store, Some(parent.clone()));
        let first_task = serde_json::json!({"task":"first child"});
        let second_task = serde_json::json!({"task":"second child"});
        assert!(
            store
                .reserve_child_task(
                    &parent,
                    &first,
                    &first_task,
                    &"1".repeat(64),
                    &serde_json::json!({}),
                    3_000,
                    6_000,
                    &[],
                    20,
                )
                .unwrap()
        );
        assert!(
            store
                .reserve_child_task(
                    &parent,
                    &second,
                    &second_task,
                    &"2".repeat(64),
                    &serde_json::json!({}),
                    3_000,
                    6_000,
                    &[],
                    21,
                )
                .unwrap()
        );
        drop(db);

        let reopened = Database::open(&DbConfig { path, key: None }).unwrap();
        MigrationRunner::new(&reopened).apply_all().unwrap();
        let recovered = RunStore::new(&reopened);
        let third = create_run(&recovered, Some(parent.clone()));
        assert!(
            recovered
                .reserve_child_task(
                    &parent,
                    &third,
                    &serde_json::json!({"task":"over budget"}),
                    &"3".repeat(64),
                    &serde_json::json!({}),
                    1,
                    6_000,
                    &[],
                    30,
                )
                .is_err()
        );

        assert!(
            !recovered
                .reserve_child_task(
                    &parent,
                    &first,
                    &first_task,
                    &"1".repeat(64),
                    &serde_json::json!({}),
                    3_000,
                    6_000,
                    &[],
                    31,
                )
                .unwrap()
        );
        recovered
            .settle_child_task(&parent, &first, Some(2_000), None, 32)
            .unwrap();
        assert!(
            recovered
                .reserve_child_task(
                    &parent,
                    &third,
                    &serde_json::json!({"task":"fits after settlement"}),
                    &"4".repeat(64),
                    &serde_json::json!({}),
                    1_000,
                    6_000,
                    &[],
                    33,
                )
                .unwrap()
        );
    }

    #[test]
    fn settling_usage_above_a_child_reservation_charges_the_actual_overage() {
        let db = fresh_db();
        let store = RunStore::new(&db);
        let parent = create_run(&store, None);
        let child = create_run(&store, Some(parent.clone()));
        assert!(
            store
                .reserve_child_task(
                    &parent,
                    &child,
                    &serde_json::json!({"task":"child"}),
                    &"a".repeat(64),
                    &serde_json::json!({}),
                    2_000,
                    5_000,
                    &[],
                    20,
                )
                .unwrap()
        );
        store
            .settle_child_task(&parent, &child, Some(4_000), None, 21)
            .unwrap();

        let next_child = create_run(&store, Some(parent.clone()));
        assert!(
            store
                .reserve_child_task(
                    &parent,
                    &next_child,
                    &serde_json::json!({"task":"would exceed aggregate"}),
                    &"b".repeat(64),
                    &serde_json::json!({}),
                    1_001,
                    5_000,
                    &[],
                    22,
                )
                .is_err()
        );
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
    fn startup_recovery_advances_completed_checkpoint_without_redispatch() {
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
        let transitions = store.recover_completed_transitions(121, 10, 10).unwrap();
        assert_eq!(
            transitions
                .iter()
                .map(|transition| transition.action)
                .collect::<Vec<_>>(),
            vec!["advance_completed_checkpoint", "complete_empty_cursor"]
        );
        let run = store.get_run(&run_id).unwrap().unwrap();
        assert_eq!(run.cursor, 1);
        assert_eq!(run.status, RunStatus::Completed);

        let interrupted_run = create_run(&store, None);
        store
            .add_step(
                &interrupted_run,
                &step("compute", 0, RunStepKind::DeterministicCompute),
            )
            .unwrap();
        store
            .add_step(
                &interrupted_run,
                &step("effect", 1, RunStepKind::ToolDispatch),
            )
            .unwrap();
        store
            .claim_step(&interrupted_run, "compute", "worker-a", 100, 200)
            .unwrap();
        store
            .complete_step(&interrupted_run, "compute", "worker-a", None, 120)
            .unwrap();
        assert_eq!(
            store
                .recover_completed_transitions(121, 10, 10)
                .unwrap()
                .len(),
            1
        );
        assert!(matches!(
            store.next_safe_action(&interrupted_run, 121).unwrap(),
            NextSafeAction::Claim(step) if step.step_id == "effect"
        ));
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

    #[test]
    fn cancelling_parent_stops_active_descendants_and_closes_their_steps() {
        let db = fresh_db();
        let store = RunStore::new(&db);
        let parent = create_run(&store, None);
        let child = create_run(&store, Some(parent.clone()));
        let grandchild = create_run(&store, Some(child.clone()));
        store
            .add_step(&child, &step("child-work", 0, RunStepKind::ModelRequest))
            .unwrap();
        store
            .add_step(
                &grandchild,
                &step("grandchild-work", 0, RunStepKind::ModelRequest),
            )
            .unwrap();
        store
            .claim_step(&child, "child-work", "worker-child", 10, 100)
            .unwrap();
        store
            .claim_step(&grandchild, "grandchild-work", "worker-grandchild", 10, 100)
            .unwrap();

        assert!(store.cancel_run(&parent, 20).unwrap());
        assert_eq!(
            store.get_run(&parent).unwrap().unwrap().status,
            RunStatus::Cancelled
        );
        assert_eq!(
            store.get_run(&child).unwrap().unwrap().status,
            RunStatus::Cancelled
        );
        assert_eq!(
            store.get_run(&grandchild).unwrap().unwrap().status,
            RunStatus::Cancelled
        );
        for (run_id, step_id) in [(&child, "child-work"), (&grandchild, "grandchild-work")] {
            let step = store.get_step(run_id, step_id).unwrap().unwrap();
            assert_eq!(step.status, RunStepStatus::Failed);
            assert_eq!(step.output_ref.as_deref(), Some("error:cancelled"));
            assert_eq!(step.lease_owner, None);
        }
        // Simulate a child made eligible by an older interrupted cascade.
        // Repeating the parent cancel reconciles descendants even though the
        // parent itself is already terminal.
        db.with_conn(|connection| {
            connection.execute(
                "UPDATE state_runs SET status='running' WHERE run_id=?1",
                [&child],
            )?;
            connection.execute(
                "UPDATE state_run_steps SET status='running',output_ref=NULL,completed_at=NULL,lease_owner='recovered-child',lease_expires_at=100 WHERE run_id=?1 AND step_id='child-work'",
                [&child],
            )?;
            connection.execute(
                "UPDATE state_runs SET status='running' WHERE run_id=?1",
                [&grandchild],
            )?;
            connection.execute(
                "UPDATE state_run_steps SET status='running',output_ref=NULL,completed_at=NULL,lease_owner='recovered-grandchild',lease_expires_at=100 WHERE run_id=?1 AND step_id='grandchild-work'",
                [&grandchild],
            )?;
            Ok(())
        })
        .unwrap();
        assert!(!store.cancel_run(&parent, 21).unwrap());
        assert_eq!(
            store.get_run(&child).unwrap().unwrap().status,
            RunStatus::Cancelled
        );
        assert_eq!(
            store.get_run(&grandchild).unwrap().unwrap().status,
            RunStatus::Cancelled
        );
        assert_eq!(
            store
                .get_step(&child, "child-work")
                .unwrap()
                .unwrap()
                .status,
            RunStepStatus::Failed
        );
        assert_eq!(
            store
                .get_step(&grandchild, "grandchild-work")
                .unwrap()
                .unwrap()
                .status,
            RunStepStatus::Failed
        );
    }

    #[test]
    fn fork_links_lineage_without_copying_steps_approvals_or_effect_keys() {
        let db = fresh_db();
        let store = RunStore::new(&db);
        let source = create_run(&store, None);
        store
            .add_step(
                &source,
                &NewRunStep {
                    step_id: "approved-send".into(),
                    ordinal: 0,
                    kind: RunStepKind::OutboxEnqueue,
                    input_hash: "send-hash".into(),
                    approval_id: Some("approval-source".into()),
                    outbox_idempotency_key: Some("effect-source".into()),
                },
            )
            .unwrap();

        let fork = store.fork_run(&source, 20).unwrap();
        assert_eq!(fork.parent_run_id.as_deref(), Some(source.as_str()));
        assert_eq!(fork.status, RunStatus::Pending);
        assert!(store.list_steps(&fork.run_id).unwrap().is_empty());
        assert_eq!(store.list_steps(&source).unwrap().len(), 1);
    }
}

#[cfg(test)]
mod run_clock_tests {
    use super::{RunClockError, validate_run_clock};

    #[test]
    fn persisted_wall_clock_observation_fails_closed_after_rollback() {
        assert_eq!(validate_run_clock(10_000, 10_000), Ok(()));
        assert_eq!(validate_run_clock(10_000, 10_001), Ok(()));
        assert_eq!(
            validate_run_clock(10_000, 9_999),
            Err(RunClockError::Rollback {
                last_observed_ms: 10_000,
                now_ms: 9_999
            })
        );
    }
}
