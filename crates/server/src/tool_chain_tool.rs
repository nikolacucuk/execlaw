//! Built-in tools for the phase-2 tool-chain runtime.
//!
//! The manifest in `plugins/tool-chain/plugin.toml` declares these
//! names with `host_implemented = true`; dispatch lands here while the
//! plugin's enable/disable state remains the coarse ON/OFF switch.

use async_trait::async_trait;
use execlaw_core::Database;
use execlaw_core::conversation::ConversationStore;
use execlaw_core::ids::{ConversationId, EventSeq, IdempotencyKey, TurnSeq};
use execlaw_core::outbox::{OutboxRow, OutboxStatus, OutboxStore};
use execlaw_core::tool::{ToolCtx, ToolDescriptor, ToolImpl, ToolLatency, ToolOutcome, ToolSource};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;

const TOOL_CHAIN_PLUGIN_ID: &str = "tool-chain";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredStep {
    step_index: u32,
    label: String,
    effect_kind: Option<String>,
    payload: Value,
    #[serde(default)]
    resource_preconditions: Vec<execlaw_core::resource_versions::ResourceVersionPrecondition>,
    #[serde(default)]
    compensation: Option<CompensationSpec>,
    #[serde(default)]
    idempotency_key_override: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredPlan {
    objective: String,
    constraints: Vec<String>,
    steps: Vec<StoredStep>,
    #[serde(default)]
    compensation_for: Option<CompensationOrigin>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CompensationSpec {
    label: String,
    effect_kind: String,
    payload: Value,
    #[serde(default)]
    resource_keys: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CompensationOrigin {
    original_run_id: String,
    original_step_index: u32,
}

#[derive(Debug, Deserialize)]
struct PlanInputStep {
    label: String,
    #[serde(default)]
    effect_kind: Option<String>,
    #[serde(default)]
    payload: Option<Value>,
    #[serde(default)]
    resource_keys: Vec<String>,
    #[serde(default)]
    compensation: Option<CompensationSpec>,
}

#[derive(Debug, Deserialize)]
struct ChainPlanArgs {
    objective: String,
    #[serde(default)]
    constraints: Vec<String>,
    #[serde(default)]
    max_steps: Option<u32>,
    #[serde(default)]
    steps: Option<Vec<PlanInputStep>>,
}

#[derive(Debug, Deserialize)]
struct ChainPreviewArgs {
    plan_id: String,
}

#[derive(Debug, Deserialize)]
struct ChainExecuteArgs {
    plan_id: String,
    #[serde(default)]
    allow_external_effects: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ResumeDecision {
    Approve,
    Deny,
}

#[derive(Debug, Clone, Copy)]
pub enum ChainApprovalDecision {
    Approve,
    Deny,
}

#[derive(Debug, Deserialize)]
struct ChainResumeArgs {
    approval_id: String,
    decision: ResumeDecision,
}

#[derive(Debug, Clone)]
struct ChainRuntime {
    db: Database,
}

fn approval_effect_hash(plan: &StoredPlan) -> String {
    let effects: Vec<&StoredStep> = plan
        .steps
        .iter()
        .filter(|step| step.effect_kind.is_some())
        .collect();
    execlaw_core::tool::tool_schema_hash(&json!(effects))
}

impl ChainRuntime {
    fn new(db: Database) -> Self {
        Self { db }
    }

    fn plugin_enabled(&self) -> Result<bool, String> {
        self.db
            .with_conn(|c| {
                let enabled: Option<i64> = c
                    .query_row(
                        "SELECT enabled FROM state_plugins WHERE plugin_id = ?1",
                        params![TOOL_CHAIN_PLUGIN_ID],
                        |r| r.get(0),
                    )
                    .optional()?;
                Ok(enabled.unwrap_or(0) != 0)
            })
            .map_err(|e| format!("plugin toggle read failed: {e}"))
    }

    fn create_plan(
        &self,
        cid: &ConversationId,
        caller_trust: &str,
        args: ChainPlanArgs,
        now: i64,
    ) -> Result<Value, String> {
        let objective = args.objective.trim().to_string();
        if objective.is_empty() {
            return Err("objective must be non-empty".to_string());
        }
        let max_steps = args.max_steps.unwrap_or(6).clamp(1, 12) as usize;

        let mut steps: Vec<StoredStep> = Vec::new();
        match args.steps {
            Some(custom) if !custom.is_empty() => {
                let versions = execlaw_core::resource_versions::ResourceVersionStore::new(&self.db);
                for (index, step) in custom.into_iter().take(max_steps).enumerate() {
                    if step.compensation.as_ref().is_some_and(|compensation| {
                        compensation.label.trim().is_empty()
                            || compensation.effect_kind.trim().is_empty()
                    }) {
                        return Err("compensation label and effect_kind are required".into());
                    }
                    if step.compensation.is_some() && step.effect_kind.is_none() {
                        return Err("only effectful steps may declare compensation".into());
                    }
                    let resource_preconditions = versions
                        .capture(&step.resource_keys)
                        .map_err(|error| format!("capture resource versions: {error}"))?;
                    steps.push(StoredStep {
                        step_index: index as u32,
                        label: step.label,
                        effect_kind: step.effect_kind,
                        payload: step.payload.unwrap_or(Value::Null),
                        resource_preconditions,
                        compensation: step.compensation,
                        idempotency_key_override: None,
                    });
                }
            }
            _ => steps.push(StoredStep {
                step_index: 0,
                label: "analyze objective".to_string(),
                effect_kind: None,
                payload: json!({"objective": objective}),
                resource_preconditions: Vec::new(),
                compensation: None,
                idempotency_key_override: None,
            }),
        }
        for (idx, s) in steps.iter_mut().enumerate() {
            s.step_index = idx as u32;
        }
        let has_external_effects = steps.iter().any(|s| s.effect_kind.is_some());

        let plan = StoredPlan {
            objective: objective.clone(),
            constraints: args.constraints.clone(),
            steps,
            compensation_for: None,
        };
        let plan_json = serde_json::to_vec(&plan).map_err(|e| format!("serialize plan: {e}"))?;
        let constraints_json = serde_json::to_string(&args.constraints)
            .map_err(|e| format!("serialize constraints: {e}"))?;
        let plan_id = uuid::Uuid::new_v4().to_string();

        self.db
            .with_conn(|c| {
                c.execute(
                    "INSERT INTO state_chain_plans \
                     (id, conversation_id, objective, constraints_json, plan_json, \
                      has_external_effects, created_by_trust, created_at, updated_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    params![
                        plan_id,
                        cid.as_str(),
                        objective,
                        constraints_json,
                        plan_json,
                        if has_external_effects { 1 } else { 0 },
                        caller_trust,
                        now,
                        now,
                    ],
                )?;
                Ok(())
            })
            .map_err(|e| format!("insert plan failed: {e}"))?;

        Ok(json!({
            "plan_id": plan_id,
            "status": "planned",
            "has_external_effects": has_external_effects,
            "step_count": plan.steps.len(),
            "steps": plan.steps,
        }))
    }

    fn load_plan(&self, plan_id: &str) -> Result<(StoredPlan, bool, String), String> {
        let row = self
            .db
            .with_conn(|c| {
                let row: Option<(Vec<u8>, i64, String)> = c
                    .query_row(
                        "SELECT plan_json, has_external_effects, conversation_id \
                         FROM state_chain_plans WHERE id = ?1",
                        params![plan_id],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                    )
                    .optional()?;
                Ok(row)
            })
            .map_err(|e| format!("load plan failed: {e}"))?;
        let Some((blob, fx, conv)) = row else {
            return Err("plan not found".to_string());
        };
        let plan: StoredPlan =
            serde_json::from_slice(&blob).map_err(|e| format!("decode stored plan: {e}"))?;
        Ok((plan, fx != 0, conv))
    }

    fn preview_plan(&self, plan_id: &str, conversation_id: &str) -> Result<Value, String> {
        let (plan, has_external_effects, owner) = self.load_plan(plan_id)?;
        if owner != conversation_id {
            return Err("plan belongs to a different conversation".into());
        }
        let store = execlaw_core::resource_versions::ResourceVersionStore::new(&self.db);
        let mut stale = false;
        let mut unsupported_conditional_update = false;
        let mut steps = Vec::with_capacity(plan.steps.len());
        for step in &plan.steps {
            let checks = store
                .check(&step.resource_preconditions)
                .map_err(|error| format!("check resource versions: {error}"))?;
            stale |= checks.iter().any(|check| !check.matches);
            unsupported_conditional_update |= step.effect_kind.is_some()
                && checks
                    .iter()
                    .any(|check| check.matches && !check.conditional_updates);
            steps.push(json!({
                "step_index": step.step_index,
                "label": step.label,
                "effect_kind": step.effect_kind,
                "preconditions": checks,
            }));
        }
        let status = if stale {
            "stale"
        } else if unsupported_conditional_update {
            "conditional_update_unsupported"
        } else {
            "ready"
        };
        Ok(json!({
            "plan_id": plan_id,
            "status": status,
            "has_external_effects": has_external_effects,
            "steps": steps,
        }))
    }

    fn create_run(
        &self,
        plan_id: &str,
        cid: &ConversationId,
        status: &str,
        now: i64,
    ) -> Result<(String, i64), String> {
        self.db
            .transaction(|tx| {
                let next_seq: i64 = tx.query_row(
                    "SELECT COALESCE(MAX(run_seq), 0) + 1 FROM state_chain_runs WHERE conversation_id = ?1",
                    params![cid.as_str()],
                    |r| r.get(0),
                )?;
                let run_id = uuid::Uuid::new_v4().to_string();
                tx.execute(
                    "INSERT INTO state_chain_runs \
                     (id, plan_id, conversation_id, run_seq, status, next_step_index, created_at, updated_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6, ?7)",
                    params![run_id, plan_id, cid.as_str(), next_seq, status, now, now],
                )?;
                Ok((run_id, next_seq))
            })
            .map_err(|e| format!("create run failed: {e}"))
    }

    fn mark_run_waiting_approval(
        &self,
        run_id: &str,
        effect_hash: &str,
        now: i64,
    ) -> Result<String, String> {
        let approval_id = uuid::Uuid::new_v4().to_string();
        self.db
            .with_conn(|c| {
                c.execute(
                    "UPDATE state_chain_runs \
                     SET status = 'awaiting_approval', approval_id = ?1, approval_effect_hash = ?2, updated_at = ?3 \
                     WHERE id = ?4",
                    params![approval_id, effect_hash, now, run_id],
                )?;
                Ok(())
            })
            .map_err(|e| format!("mark awaiting approval failed: {e}"))?;
        Ok(approval_id)
    }

    fn resolve_run_for_approval(
        &self,
        approval_id: &str,
    ) -> Result<Option<(String, String, String, i64, String, Option<String>)>, String> {
        self.db
            .with_conn(|c| {
                let row: Option<(String, String, String, i64, String, Option<String>)> = c
                    .query_row(
                        "SELECT id, plan_id, conversation_id, run_seq, status, approval_effect_hash \
                         FROM state_chain_runs WHERE approval_id = ?1",
                        params![approval_id],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
                    )
                    .optional()?;
                Ok(row)
            })
            .map_err(|e| format!("approval lookup failed: {e}"))
    }

    fn write_step_row(
        &self,
        run_id: &str,
        step: &StoredStep,
        status: &str,
        result_json: Option<&Value>,
        error_text: Option<&str>,
        outbox_key: Option<&str>,
        now: i64,
    ) -> Result<(), String> {
        let args_json =
            serde_json::to_string(&step.payload).map_err(|e| format!("step payload json: {e}"))?;
        let result_json = result_json
            .map(serde_json::to_string)
            .transpose()
            .map_err(|e| format!("step result json: {e}"))?;
        self.db
            .with_conn(|c| {
                c.execute(
                    "INSERT INTO state_chain_run_steps \
                     (run_id, step_index, kind, status, tool_name, effect_kind, args_json, \
                      result_json, error_text, outbox_idempotency_key, created_at, updated_at) \
                     VALUES (?1, ?2, ?3, ?4, NULL, ?5, ?6, ?7, ?8, ?9, ?10, ?11) \
                     ON CONFLICT(run_id, step_index) DO UPDATE SET \
                       status = excluded.status, \
                       result_json = excluded.result_json, \
                       error_text = excluded.error_text, \
                       outbox_idempotency_key = excluded.outbox_idempotency_key, \
                       updated_at = excluded.updated_at",
                    params![
                        run_id,
                        step.step_index as i64,
                        if step.effect_kind.is_some() {
                            "effect"
                        } else {
                            "analysis"
                        },
                        status,
                        step.effect_kind,
                        args_json,
                        result_json,
                        error_text,
                        outbox_key,
                        now,
                        now,
                    ],
                )?;
                Ok(())
            })
            .map_err(|e| format!("write step row failed: {e}"))
    }

    fn outbox_already_has_key(&self, key: &IdempotencyKey) -> Result<bool, String> {
        self.db
            .with_conn(|c| {
                let n: i64 = c.query_row(
                    "SELECT COUNT(*) FROM state_outbox WHERE idempotency_key = ?1",
                    params![key.as_str()],
                    |r| r.get(0),
                )?;
                Ok(n > 0)
            })
            .map_err(|e| format!("outbox key lookup failed: {e}"))
    }

    fn compensation_review_for_origin(
        &self,
        original_run_id: &str,
        original_step_index: u32,
    ) -> Result<Option<(String, String)>, String> {
        self.db
            .with_conn(|connection| {
                connection
                    .query_row(
                        "SELECT c.id,r.approval_id FROM state_chain_compensations c \
                         JOIN state_chain_runs r ON r.id=c.approval_run_id \
                         WHERE c.original_run_id=?1 AND c.original_step_index=?2",
                        params![original_run_id, original_step_index],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .optional()
                    .map_err(execlaw_core::DbError::from)
            })
            .map_err(|error| format!("load compensation review: {error}"))
    }

    fn create_compensation_review(
        &self,
        original_run_id: &str,
        original_step: &StoredStep,
        original_outbox_key: &str,
        spec: &CompensationSpec,
        now: i64,
    ) -> Result<(String, String), String> {
        if let Some((compensation_id, approval_id)) =
            self.compensation_review_for_origin(original_run_id, original_step.step_index)?
        {
            return Ok((compensation_id, approval_id));
        }
        let conversation_id: String = self
            .db
            .with_conn(|connection| {
                connection
                    .query_row(
                        "SELECT conversation_id FROM state_chain_runs WHERE id=?1",
                        [original_run_id],
                        |row| row.get(0),
                    )
                    .map_err(execlaw_core::DbError::from)
            })
            .map_err(|error| format!("load original run for compensation: {error}"))?;
        let cid = ConversationId::from(conversation_id.clone());
        let preconditions = execlaw_core::resource_versions::ResourceVersionStore::new(&self.db)
            .capture(&spec.resource_keys)
            .map_err(|error| format!("capture compensation resource versions: {error}"))?;
        let compensation_key = IdempotencyKey::from_string(format!(
            "chain-compensation:{original_run_id}:{}",
            original_step.step_index
        ));
        let compensation_plan = StoredPlan {
            objective: format!(
                "Compensate step {} from run {original_run_id}",
                original_step.step_index
            ),
            constraints: vec![
                "Compensation is a separate effect and requires a fresh approval.".into(),
            ],
            steps: vec![StoredStep {
                step_index: 0,
                label: spec.label.clone(),
                effect_kind: Some(spec.effect_kind.clone()),
                payload: spec.payload.clone(),
                resource_preconditions: preconditions,
                compensation: None,
                idempotency_key_override: Some(compensation_key.as_str().to_owned()),
            }],
            compensation_for: Some(CompensationOrigin {
                original_run_id: original_run_id.to_owned(),
                original_step_index: original_step.step_index,
            }),
        };
        let compensation_plan_id = uuid::Uuid::new_v4().to_string();
        let plan_json = serde_json::to_vec(&compensation_plan)
            .map_err(|error| format!("serialize compensation plan: {error}"))?;
        let constraints_json = serde_json::to_string(&compensation_plan.constraints)
            .map_err(|error| format!("serialize compensation constraints: {error}"))?;
        self.db
            .with_conn(|connection| {
                connection.execute(
                    "INSERT INTO state_chain_plans \
                     (id,conversation_id,objective,constraints_json,plan_json,has_external_effects,created_by_trust,created_at,updated_at) \
                     VALUES (?1,?2,?3,?4,?5,1,'Controller',?6,?6)",
                    params![
                        compensation_plan_id,
                        conversation_id,
                        compensation_plan.objective,
                        constraints_json,
                        plan_json,
                        now,
                    ],
                )?;
                Ok(())
            })
            .map_err(|error| format!("store compensation plan: {error}"))?;
        let (approval_run_id, _) = self.create_run(&compensation_plan_id, &cid, "running", now)?;
        let approval_id = self.mark_run_waiting_approval(
            &approval_run_id,
            &approval_effect_hash(&compensation_plan),
            now,
        )?;
        let compensation_id = uuid::Uuid::new_v4().to_string();
        self.db
            .with_conn(|connection| {
                connection.execute(
                    "INSERT INTO state_chain_compensations \
                     (id,original_run_id,original_step_index,original_outbox_key,compensation_plan_id,approval_run_id,status,created_at,updated_at) \
                     VALUES (?1,?2,?3,?4,?5,?6,'awaiting_approval',?7,?7)",
                    params![
                        compensation_id,
                        original_run_id,
                        original_step.step_index,
                        original_outbox_key,
                        compensation_plan_id,
                        approval_run_id,
                        now,
                    ],
                )?;
                Ok(())
            })
            .map_err(|error| format!("record compensation review: {error}"))?;
        Ok((compensation_id, approval_id))
    }

    fn mark_compensation_review_status(
        &self,
        approval_run_id: &str,
        status: &str,
        now: i64,
    ) -> Result<(), String> {
        self.db
            .with_conn(|connection| {
                connection.execute(
                    "UPDATE state_chain_compensations SET status=?2,updated_at=?3 WHERE approval_run_id=?1 AND status='awaiting_approval'",
                    params![approval_run_id, status, now],
                )?;
                Ok(())
            })
            .map_err(|error| format!("update compensation review: {error}"))
    }

    fn partial_failure_report(
        &self,
        run_id: &str,
        error: &str,
        plan: &StoredPlan,
        now: i64,
    ) -> Result<Value, String> {
        let completed: Vec<(u32, Option<String>)> = self
            .db
            .with_conn(|connection| {
                let mut statement = connection.prepare(
                    "SELECT step_index,outbox_idempotency_key FROM state_chain_run_steps \
                     WHERE run_id=?1 AND kind='effect' AND status='completed' ORDER BY step_index",
                )?;
                statement
                    .query_map([run_id], |row| Ok((row.get::<_, u32>(0)?, row.get(1)?)))?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(execlaw_core::DbError::from)
            })
            .map_err(|error| format!("load partial chain history: {error}"))?;
        let mut approvals = Vec::new();
        let mut residual = Vec::new();
        for (step_index, outbox_key) in &completed {
            let Some(outbox_key) = outbox_key else {
                residual.push(json!({"step_index":step_index,"status":"effect_identity_missing","reversible":false}));
                continue;
            };
            let outbox_status: Option<String> = self
                .db
                .with_conn(|connection| {
                    connection
                        .query_row(
                            "SELECT status FROM state_outbox WHERE idempotency_key=?1",
                            [outbox_key],
                            |row| row.get(0),
                        )
                        .optional()
                        .map_err(execlaw_core::DbError::from)
                })
                .map_err(|error| format!("read original effect status: {error}"))?;
            let Some(step) = plan
                .steps
                .iter()
                .find(|step| step.step_index == *step_index)
            else {
                residual.push(json!({"step_index":step_index,"status":"step_definition_missing","reversible":false}));
                continue;
            };
            if outbox_status.as_deref() != Some("delivered") {
                residual.push(json!({
                    "step_index": step_index,
                    "outbox_status": outbox_status.unwrap_or_else(|| "missing".into()),
                    "reversible": step.compensation.is_some(),
                    "consequence": "original delivery is not confirmed; compensation was not attempted"
                }));
                continue;
            }
            if let Some(spec) = &step.compensation {
                let (compensation_id, approval_id) =
                    self.create_compensation_review(run_id, step, outbox_key, spec, now)?;
                approvals.push(json!({
                    "compensation_id": compensation_id,
                    "approval_id": approval_id,
                    "original_step_index": step_index,
                    "status": "awaiting_approval"
                }));
                residual.push(json!({
                    "step_index": step_index,
                    "outbox_status": "delivered",
                    "consequence": "original effect remains recorded; a separate compensating effect is awaiting approval"
                }));
            } else {
                residual.push(json!({
                    "step_index": step_index,
                    "outbox_status": "delivered",
                    "reversible": false,
                    "consequence": "original effect remains; no compensation was declared"
                }));
            }
        }
        Ok(json!({
            "status": if completed.is_empty() { "failed" } else { "partially_failed" },
            "run_id": run_id,
            "error": error,
            "completed_effect_steps": completed.iter().map(|(step, _)| step).collect::<Vec<_>>(),
            "compensation_approvals": approvals,
            "residual_consequences": residual,
        }))
    }

    fn conversation_last_seq(&self, cid: &ConversationId) -> EventSeq {
        ConversationStore::new(&self.db)
            .get(cid)
            .ok()
            .flatten()
            .map(|r| r.last_seq)
            .unwrap_or(EventSeq(0))
    }

    fn execute_run_steps(
        &self,
        run_id: &str,
        run_seq: i64,
        cid: &ConversationId,
        plan: &StoredPlan,
        now: i64,
    ) -> Result<(usize, usize), String> {
        let outbox = OutboxStore::new(&self.db);
        let enqueued_seq = self.conversation_last_seq(cid);
        let mut executed = 0usize;
        let mut effects = 0usize;

        for step in &plan.steps {
            let current = execlaw_core::resource_versions::ResourceVersionStore::new(&self.db)
                .check(&step.resource_preconditions)
                .map_err(|error| format!("check resource versions: {error}"))?;
            if current.iter().any(|check| !check.matches) {
                return Err(format!(
                    "resource_precondition_conflict at step {}: preview is stale; prepare and preview a new plan",
                    step.step_index
                ));
            }
            if let Some(effect_kind) = &step.effect_kind {
                if current.iter().any(|check| !check.conditional_updates) {
                    return Err(format!(
                        "conditional_update_unsupported at step {}: the provider cannot enforce resource versions; no effect was enqueued",
                        step.step_index
                    ));
                }
                let key = step
                    .idempotency_key_override
                    .as_ref()
                    .map(|key| IdempotencyKey::from_string(key.clone()))
                    .unwrap_or_else(|| {
                        IdempotencyKey::mint(cid, TurnSeq(run_seq), step.step_index)
                    });
                let key_s = key.as_str().to_string();
                if !self.outbox_already_has_key(&key)? {
                    let payload = rmp_serde::to_vec_named(&json!({
                        "run_id": run_id,
                        "step_index": step.step_index,
                        "label": step.label,
                        "payload": step.payload,
                        "resource_preconditions": step.resource_preconditions,
                    }))
                    .map_err(|e| format!("encode outbox payload failed: {e}"))?;
                    outbox
                        .enqueue(&OutboxRow {
                            id: None,
                            idempotency_key: key,
                            conversation_id: cid.clone(),
                            effect_kind: effect_kind.clone(),
                            payload,
                            status: OutboxStatus::Pending,
                            attempts: 0,
                            next_attempt_at: None,
                            last_error: None,
                            enqueued_seq,
                        })
                        .map_err(|e| format!("enqueue outbox failed: {e}"))?;
                }
                self.write_step_row(
                    run_id,
                    step,
                    "completed",
                    Some(&json!({"enqueued": true})),
                    None,
                    Some(&key_s),
                    now,
                )?;
                effects += 1;
            } else {
                self.write_step_row(
                    run_id,
                    step,
                    "completed",
                    Some(&json!({"note": "non-effect step recorded"})),
                    None,
                    None,
                    now,
                )?;
            }
            executed += 1;
        }
        if plan.compensation_for.is_some() {
            self.mark_compensation_review_status(run_id, "enqueued", now)?;
        }
        Ok((executed, effects))
    }

    fn mark_run_terminal(
        &self,
        run_id: &str,
        status: &str,
        error_text: Option<&str>,
        now: i64,
    ) -> Result<(), String> {
        self.db
            .with_conn(|c| {
                c.execute(
                    "UPDATE state_chain_runs \
                     SET status = ?1, error_text = ?2, finished_at = ?3, updated_at = ?4 \
                     WHERE id = ?5",
                    params![status, error_text, now, now, run_id],
                )?;
                Ok(())
            })
            .map_err(|e| format!("mark run terminal failed: {e}"))
    }

    fn mark_run_status(&self, run_id: &str, status: &str, now: i64) -> Result<(), String> {
        self.db
            .with_conn(|c| {
                c.execute(
                    "UPDATE state_chain_runs SET status = ?1, updated_at = ?2 WHERE id = ?3",
                    params![status, now, run_id],
                )?;
                Ok(())
            })
            .map_err(|e| format!("mark run status failed: {e}"))
    }
}

pub struct ChainPlanTool {
    descriptor: ToolDescriptor,
    runtime: ChainRuntime,
}

impl ChainPlanTool {
    pub fn new(db: Database) -> Self {
        Self {
            descriptor: ToolDescriptor {
                name: "chain.plan".to_string(),
                description:
                    "Create and persist a deterministic chain plan for a multi-step objective."
                        .to_string(),
                schema: json!({
                    "type": "object",
                    "properties": {
                        "objective": { "type": "string", "minLength": 1 },
                        "constraints": { "type": "array", "items": { "type": "string" }, "default": [] },
                        "max_steps": { "type": "integer", "minimum": 1, "maximum": 12, "default": 6 },
                        "steps": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "label": { "type": "string", "minLength": 1 },
                                    "effect_kind": { "type": "string" },
                                    "payload": {},
                                    "resource_keys": {
                                        "type": "array",
                                        "items": {"type": "string", "minLength": 1, "maxLength": 256},
                                        "maxItems": 64,
                                        "description": "Resource keys whose provider versions must still match the prepared proposal before execution."
                                    },
                                    "compensation": {
                                        "type": "object",
                                        "properties": {
                                            "label": {"type": "string", "minLength": 1},
                                            "effect_kind": {"type": "string", "minLength": 1},
                                            "payload": {},
                                            "resource_keys": {
                                                "type": "array",
                                                "items": {"type": "string", "minLength": 1, "maxLength": 256},
                                                "maxItems": 64
                                            }
                                        },
                                        "required": ["label", "effect_kind"],
                                        "additionalProperties": false
                                    }
                                },
                                "required": ["label"],
                                "additionalProperties": false
                            }
                        }
                    },
                    "required": ["objective"],
                    "additionalProperties": false
                }),
                source: ToolSource::Builtin,
                latency: ToolLatency::Low,
                capabilities: vec![],
                default_allowed_classes: vec![
                    "Controller".to_string(),
                    "Delegated".to_string(),
                    "KnownTrusted".to_string(),
                    "KnownLimited".to_string(),
                ],
                sensitive: false,
            },
            runtime: ChainRuntime::new(db),
        }
    }
}

#[async_trait]
impl ToolImpl for ChainPlanTool {
    fn descriptor(&self) -> &ToolDescriptor {
        &self.descriptor
    }

    async fn invoke(&self, ctx: ToolCtx, args: Value) -> ToolOutcome {
        match self.runtime.plugin_enabled() {
            Ok(true) => {}
            Ok(false) => {
                return ToolOutcome::denied("tool-chain plugin is OFF in Settings -> Plugins");
            }
            Err(e) => return ToolOutcome::err("storage_error", e),
        }

        let parsed: ChainPlanArgs = match serde_json::from_value(args) {
            Ok(v) => v,
            Err(e) => return ToolOutcome::err("invalid_argument", e.to_string()),
        };
        match self.runtime.create_plan(
            &ctx.conversation_id,
            &ctx.caller_trust,
            parsed,
            ctx.clock.now_unix(),
        ) {
            Ok(v) => ToolOutcome::ok(v),
            Err(e) => ToolOutcome::err("plan_failed", e),
        }
    }
}

pub struct ChainPreviewTool {
    descriptor: ToolDescriptor,
    runtime: ChainRuntime,
}

impl ChainPreviewTool {
    pub fn new(db: Database) -> Self {
        Self {
            descriptor: ToolDescriptor {
                name: "chain.preview".to_string(),
                description: "Check a prepared plan's resource versions and conditional-update support without creating effects.".to_string(),
                schema: json!({
                    "type": "object",
                    "properties": {"plan_id": {"type": "string", "minLength": 1}},
                    "required": ["plan_id"],
                    "additionalProperties": false
                }),
                source: ToolSource::Builtin,
                latency: ToolLatency::Low,
                capabilities: vec![],
                default_allowed_classes: vec!["Controller".to_string()],
                sensitive: true,
            },
            runtime: ChainRuntime::new(db),
        }
    }
}

#[async_trait]
impl ToolImpl for ChainPreviewTool {
    fn descriptor(&self) -> &ToolDescriptor {
        &self.descriptor
    }

    async fn invoke(&self, ctx: ToolCtx, args: Value) -> ToolOutcome {
        match self.runtime.plugin_enabled() {
            Ok(true) => {}
            Ok(false) => {
                return ToolOutcome::denied("tool-chain plugin is OFF in Settings -> Plugins");
            }
            Err(error) => return ToolOutcome::err("storage_error", error),
        }
        let parsed: ChainPreviewArgs = match serde_json::from_value(args) {
            Ok(value) => value,
            Err(error) => return ToolOutcome::err("invalid_argument", error.to_string()),
        };
        match self
            .runtime
            .preview_plan(&parsed.plan_id, ctx.conversation_id.as_str())
        {
            Ok(preview) => ToolOutcome::ok(preview),
            Err(error) if error == "plan not found" => ToolOutcome::err("not_found", error),
            Err(error) if error == "plan belongs to a different conversation" => {
                ToolOutcome::denied(error)
            }
            Err(error) => ToolOutcome::err("preview_failed", error),
        }
    }
}

pub struct ChainExecuteTool {
    descriptor: ToolDescriptor,
    runtime: ChainRuntime,
}

impl ChainExecuteTool {
    pub fn new(db: Database) -> Self {
        Self {
            descriptor: ToolDescriptor {
                name: "chain.execute".to_string(),
                description: "Start execution of a persisted chain plan. Effectful plans halt for approval before outbox enqueue.".to_string(),
                schema: json!({
                    "type": "object",
                    "properties": {
                        "plan_id": { "type": "string", "minLength": 1 },
                        "allow_external_effects": { "type": "boolean", "default": false }
                    },
                    "required": ["plan_id"],
                    "additionalProperties": false
                }),
                source: ToolSource::Builtin,
                latency: ToolLatency::High,
                capabilities: vec![],
                default_allowed_classes: vec!["Controller".to_string()],
                sensitive: false,
            },
            runtime: ChainRuntime::new(db),
        }
    }
}

#[async_trait]
impl ToolImpl for ChainExecuteTool {
    fn descriptor(&self) -> &ToolDescriptor {
        &self.descriptor
    }

    async fn invoke(&self, ctx: ToolCtx, args: Value) -> ToolOutcome {
        match self.runtime.plugin_enabled() {
            Ok(true) => {}
            Ok(false) => {
                return ToolOutcome::denied("tool-chain plugin is OFF in Settings -> Plugins");
            }
            Err(e) => return ToolOutcome::err("storage_error", e),
        }

        let parsed: ChainExecuteArgs = match serde_json::from_value(args) {
            Ok(v) => v,
            Err(e) => return ToolOutcome::err("invalid_argument", e.to_string()),
        };

        let now = ctx.clock.now_unix();
        let (plan, has_external_effects, conv_for_plan) =
            match self.runtime.load_plan(&parsed.plan_id) {
                Ok(v) => v,
                Err(e) => return ToolOutcome::err("not_found", e),
            };

        if conv_for_plan != ctx.conversation_id.as_str() {
            return ToolOutcome::denied("plan belongs to a different conversation");
        }

        let preview = match self
            .runtime
            .preview_plan(&parsed.plan_id, ctx.conversation_id.as_str())
        {
            Ok(preview) => preview,
            Err(error) => return ToolOutcome::err("preview_failed", error),
        };
        if preview["status"] != "ready" {
            return ToolOutcome::err("resource_precondition_conflict", preview.to_string());
        }

        let allow_external = parsed.allow_external_effects.unwrap_or(false);
        if has_external_effects && !allow_external {
            return ToolOutcome::err(
                "approval_required",
                "plan contains external effects; set allow_external_effects=true to request approval flow",
            );
        }

        let (run_id, run_seq) =
            match self
                .runtime
                .create_run(&parsed.plan_id, &ctx.conversation_id, "running", now)
            {
                Ok(v) => v,
                Err(e) => return ToolOutcome::err("storage_error", e),
            };

        if has_external_effects {
            match self
                .runtime
                .mark_run_waiting_approval(&run_id, &approval_effect_hash(&plan), now)
            {
                Ok(approval_id) => {
                    return ToolOutcome::ok(json!({
                        "status": "awaiting_approval",
                        "plan_id": parsed.plan_id,
                        "run_id": run_id,
                        "approval_id": approval_id,
                        "resume_tool": "chain.resume"
                    }));
                }
                Err(e) => return ToolOutcome::err("storage_error", e),
            }
        }

        match self
            .runtime
            .execute_run_steps(&run_id, run_seq, &ctx.conversation_id, &plan, now)
        {
            Ok((executed, effects)) => {
                if let Err(e) = self
                    .runtime
                    .mark_run_terminal(&run_id, "completed", None, now)
                {
                    return ToolOutcome::err("storage_error", e);
                }
                ToolOutcome::ok(json!({
                    "status": "completed",
                    "plan_id": parsed.plan_id,
                    "run_id": run_id,
                    "executed_steps": executed,
                    "effectful_steps": effects
                }))
            }
            Err(e) => {
                let _ = self
                    .runtime
                    .mark_run_terminal(&run_id, "failed", Some(&e), now);
                match self.runtime.partial_failure_report(&run_id, &e, &plan, now) {
                    Ok(report) => ToolOutcome::ok(report),
                    Err(report_error) => ToolOutcome::err(
                        "execute_failed",
                        format!("{e}; partial report unavailable: {report_error}"),
                    ),
                }
            }
        }
    }
}

pub struct ChainResumeTool {
    descriptor: ToolDescriptor,
    runtime: ChainRuntime,
}

impl ChainResumeTool {
    pub fn new(db: Database) -> Self {
        Self {
            descriptor: ToolDescriptor {
                name: "chain.resume".to_string(),
                description: "Resolve an effectful chain approval and resume or deny execution."
                    .to_string(),
                schema: json!({
                    "type": "object",
                    "properties": {
                        "approval_id": { "type": "string", "minLength": 1 },
                        "decision": { "type": "string", "enum": ["approve", "deny"] }
                    },
                    "required": ["approval_id", "decision"],
                    "additionalProperties": false
                }),
                source: ToolSource::Builtin,
                latency: ToolLatency::Medium,
                capabilities: vec![],
                default_allowed_classes: vec!["Controller".to_string()],
                sensitive: false,
            },
            runtime: ChainRuntime::new(db),
        }
    }
}

#[async_trait]
impl ToolImpl for ChainResumeTool {
    fn descriptor(&self) -> &ToolDescriptor {
        &self.descriptor
    }

    async fn invoke(&self, _ctx: ToolCtx, args: Value) -> ToolOutcome {
        match self.runtime.plugin_enabled() {
            Ok(true) => {}
            Ok(false) => {
                return ToolOutcome::denied("tool-chain plugin is OFF in Settings -> Plugins");
            }
            Err(e) => return ToolOutcome::err("storage_error", e),
        }

        let parsed: ChainResumeArgs = match serde_json::from_value(args) {
            Ok(v) => v,
            Err(e) => return ToolOutcome::err("invalid_argument", e.to_string()),
        };
        let decision = match parsed.decision {
            ResumeDecision::Approve => ChainApprovalDecision::Approve,
            ResumeDecision::Deny => ChainApprovalDecision::Deny,
        };
        match resolve_chain_approval(
            &self.runtime,
            &parsed.approval_id,
            decision,
            chrono::Utc::now().timestamp(),
        ) {
            Ok(v) => ToolOutcome::ok(v),
            Err(e) if e == "approval_not_found" => {
                ToolOutcome::err("not_found", "approval not found")
            }
            Err(e) => ToolOutcome::err("storage_error", e),
        }
    }
}

fn resolve_chain_approval(
    runtime: &ChainRuntime,
    approval_id: &str,
    decision: ChainApprovalDecision,
    now: i64,
) -> Result<Value, String> {
    let Some((run_id, plan_id, conv_id, run_seq, status, stored_effect_hash)) =
        runtime.resolve_run_for_approval(approval_id)?
    else {
        return Err("approval_not_found".to_string());
    };

    if status != "awaiting_approval" {
        return Ok(json!({
            "status": "already_resolved",
            "run_id": run_id,
            "approval_id": approval_id,
            "conversation_id": conv_id,
        }));
    }

    match decision {
        ChainApprovalDecision::Deny => {
            runtime.mark_run_terminal(&run_id, "denied", Some("denied by controller"), now)?;
            runtime.mark_compensation_review_status(&run_id, "denied", now)?;
            Ok(json!({
                "status": "denied",
                "run_id": run_id,
                "approval_id": approval_id,
                "conversation_id": conv_id,
            }))
        }
        ChainApprovalDecision::Approve => {
            let (plan, _fx, _cid) = match runtime.load_plan(&plan_id) {
                Ok(v) => v,
                Err(e) => {
                    let _ = runtime.mark_run_terminal(&run_id, "failed", Some(&e), now);
                    let _ = runtime.mark_compensation_review_status(&run_id, "failed", now);
                    return Err(e);
                }
            };
            if stored_effect_hash.as_deref() != Some(approval_effect_hash(&plan).as_str()) {
                runtime.mark_run_terminal(
                    &run_id,
                    "failed",
                    Some("approved effect no longer matches the pending approval"),
                    now,
                )?;
                runtime.mark_compensation_review_status(&run_id, "failed", now)?;
                return Err("approval_effect_mismatch".to_string());
            }
            runtime.mark_run_status(&run_id, "running", now)?;
            let cid = ConversationId::from(conv_id.clone());
            match runtime.execute_run_steps(&run_id, run_seq, &cid, &plan, now) {
                Ok((executed, effects)) => {
                    runtime.mark_run_terminal(&run_id, "completed", None, now)?;
                    let is_compensation = plan.compensation_for.is_some();
                    Ok(json!({
                        "status": if is_compensation { "compensation_enqueued" } else { "completed" },
                        "run_id": run_id,
                        "approval_id": approval_id,
                        "conversation_id": conv_id,
                        "executed_steps": executed,
                        "effectful_steps": effects,
                        "original_effect_status": if is_compensation { "remains_recorded" } else { "not_applicable" },
                        "reversal_confirmed": false
                    }))
                }
                Err(e) => {
                    let _ = runtime.mark_run_terminal(&run_id, "failed", Some(&e), now);
                    runtime.mark_compensation_review_status(&run_id, "failed", now)?;
                    runtime.partial_failure_report(&run_id, &e, &plan, now)
                }
            }
        }
    }
}

pub fn resolve_chain_approval_http(
    db: &Database,
    approval_id: &str,
    decision: ChainApprovalDecision,
    now: i64,
) -> Result<Value, String> {
    let runtime = ChainRuntime::new(db.clone());
    resolve_chain_approval(&runtime, approval_id, decision, now)
}

pub fn tool_chain_tools(db: Database) -> Vec<Arc<dyn ToolImpl>> {
    vec![
        Arc::new(ChainPlanTool::new(db.clone())),
        Arc::new(ChainPreviewTool::new(db.clone())),
        Arc::new(ChainExecuteTool::new(db.clone())),
        Arc::new(ChainResumeTool::new(db)),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use execlaw_core::conversation::{ConversationKind, ConversationRow, Modality, Phase};
    use execlaw_core::db::DbConfig;
    use execlaw_core::migrations::MigrationRunner;
    use execlaw_core::tool::{SystemClock, ToolCtx};

    fn fresh_db() -> Database {
        let db = Database::open(&DbConfig::in_memory_unencrypted()).unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        db
    }

    fn enable_tool_chain_plugin(db: &Database) {
        db.with_conn(|c| {
            c.execute(
                "INSERT OR REPLACE INTO state_plugins \
                 (plugin_id, version, manifest_toml, stage_path, enabled, installed_at, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, 1, ?5, ?6)",
                params![
                    TOOL_CHAIN_PLUGIN_ID,
                    "0.2.0",
                    "[plugin]\nid='tool-chain'\n",
                    "stage://tool-chain",
                    chrono::Utc::now().timestamp(),
                    chrono::Utc::now().timestamp(),
                ],
            )?;
            Ok(())
        })
        .unwrap();
    }

    fn seed_conversation(db: &Database, cid: &ConversationId) {
        let row = ConversationRow {
            conversation_id: cid.clone(),
            kind: ConversationKind::ControllerDM,
            last_seq: EventSeq(7),
            phase: Phase::Idle,
            controller_id: Some("controller".into()),
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
        };
        ConversationStore::new(db).upsert(&row).unwrap();
    }

    fn controller_ctx(cid: &ConversationId) -> ToolCtx {
        ToolCtx::empty(cid.clone(), "Controller", Arc::new(SystemClock))
    }

    fn tool_by_name(tools: &[Arc<dyn ToolImpl>], name: &str) -> Arc<dyn ToolImpl> {
        tools
            .iter()
            .find(|t| t.descriptor().name == name)
            .expect("tool present")
            .clone()
    }

    #[tokio::test]
    async fn effectful_chain_requires_approval_and_can_be_denied() {
        let db = fresh_db();
        enable_tool_chain_plugin(&db);
        let cid = ConversationId::from("conv-chain-approval");
        seed_conversation(&db, &cid);

        let tools = tool_chain_tools(db.clone());
        let planner = tool_by_name(&tools, "chain.plan");
        let executor = tool_by_name(&tools, "chain.execute");
        let resume = tool_by_name(&tools, "chain.resume");

        let plan = planner
            .invoke(
                controller_ctx(&cid),
                json!({
                    "objective": "send report",
                    "steps": [
                        {"label": "deliver report", "effect_kind": "transport.send", "payload": {"text": "hello"}}
                    ]
                }),
            )
            .await;
        let plan_id = match plan {
            ToolOutcome::Ok(v) => v["plan_id"].as_str().unwrap().to_string(),
            other => panic!("unexpected plan outcome: {other:?}"),
        };

        let started = executor
            .invoke(
                controller_ctx(&cid),
                json!({"plan_id": plan_id, "allow_external_effects": true}),
            )
            .await;
        let approval_id = match started {
            ToolOutcome::Ok(v) => {
                assert_eq!(v["status"], "awaiting_approval");
                v["approval_id"].as_str().unwrap().to_string()
            }
            other => panic!("unexpected execute outcome: {other:?}"),
        };

        let denied = resume
            .invoke(
                controller_ctx(&cid),
                json!({"approval_id": approval_id, "decision": "deny"}),
            )
            .await;
        match denied {
            ToolOutcome::Ok(v) => assert_eq!(v["status"], "denied"),
            other => panic!("unexpected resume outcome: {other:?}"),
        }
    }

    #[tokio::test]
    async fn effectful_chain_approval_uses_deterministic_outbox_idempotency_key() {
        let db = fresh_db();
        enable_tool_chain_plugin(&db);
        let cid = ConversationId::from("conv-chain-idempotency");
        seed_conversation(&db, &cid);

        let tools = tool_chain_tools(db.clone());
        let planner = tool_by_name(&tools, "chain.plan");
        let executor = tool_by_name(&tools, "chain.execute");
        let resume = tool_by_name(&tools, "chain.resume");

        let plan_id = match planner
            .invoke(
                controller_ctx(&cid),
                json!({
                    "objective": "effectful run",
                    "steps": [
                        {"label": "send", "effect_kind": "transport.send", "payload": {"text": "hi"}}
                    ]
                }),
            )
            .await
        {
            ToolOutcome::Ok(v) => v["plan_id"].as_str().unwrap().to_string(),
            other => panic!("unexpected plan outcome: {other:?}"),
        };

        let approval_id = match executor
            .invoke(
                controller_ctx(&cid),
                json!({"plan_id": plan_id, "allow_external_effects": true}),
            )
            .await
        {
            ToolOutcome::Ok(v) => v["approval_id"].as_str().unwrap().to_string(),
            other => panic!("unexpected execute outcome: {other:?}"),
        };

        let approved = resume
            .invoke(
                controller_ctx(&cid),
                json!({"approval_id": approval_id, "decision": "approve"}),
            )
            .await;
        match approved {
            ToolOutcome::Ok(v) => assert_eq!(v["status"], "completed"),
            other => panic!("unexpected approved outcome: {other:?}"),
        }

        let (run_seq, actual_key): (i64, String) = db
            .with_conn(|c| {
                Ok(c.query_row(
                    "SELECT r.run_seq, s.outbox_idempotency_key \
                     FROM state_chain_runs r \
                     JOIN state_chain_run_steps s ON s.run_id = r.id \
                     WHERE s.step_index = 0",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?)
            })
            .unwrap();

        let expected = IdempotencyKey::mint(&cid, TurnSeq(run_seq), 0);
        assert_eq!(actual_key, expected.as_str());

        let outbox_count: i64 = db
            .with_conn(|c| Ok(c.query_row("SELECT COUNT(*) FROM state_outbox", [], |r| r.get(0))?))
            .unwrap();
        assert_eq!(outbox_count, 1);

        let second = resume
            .invoke(
                controller_ctx(&cid),
                json!({"approval_id": approval_id, "decision": "approve"}),
            )
            .await;
        match second {
            ToolOutcome::Ok(v) => assert_eq!(v["status"], "already_resolved"),
            other => panic!("unexpected second resume outcome: {other:?}"),
        }

        let outbox_count_after: i64 = db
            .with_conn(|c| Ok(c.query_row("SELECT COUNT(*) FROM state_outbox", [], |r| r.get(0))?))
            .unwrap();
        assert_eq!(outbox_count_after, 1);
    }

    #[tokio::test]
    async fn stale_resource_preview_blocks_execution_before_creating_a_run_or_effect() {
        let db = fresh_db();
        enable_tool_chain_plugin(&db);
        let cid = ConversationId::from("conv-chain-precondition");
        seed_conversation(&db, &cid);
        let versions = execlaw_core::resource_versions::ResourceVersionStore::new(&db);
        versions.observe("record/7", "rev-1", true, 10).unwrap();

        let tools = tool_chain_tools(db.clone());
        let planner = tool_by_name(&tools, "chain.plan");
        let preview_tool = tool_by_name(&tools, "chain.preview");
        let executor = tool_by_name(&tools, "chain.execute");
        let plan_id = match planner
            .invoke(
                controller_ctx(&cid),
                json!({
                    "objective": "update the record if its version is unchanged",
                    "steps": [{
                        "label": "update record",
                        "effect_kind": "record.update",
                        "payload": {"id": 7, "name": "new"},
                        "resource_keys": ["record/7"]
                    }]
                }),
            )
            .await
        {
            ToolOutcome::Ok(value) => value["plan_id"].as_str().unwrap().to_owned(),
            other => panic!("unexpected plan outcome: {other:?}"),
        };
        assert_eq!(
            match preview_tool
                .invoke(controller_ctx(&cid), json!({"plan_id": plan_id}))
                .await
            {
                ToolOutcome::Ok(value) => value["status"].as_str().unwrap().to_owned(),
                other => panic!("unexpected preview outcome: {other:?}"),
            },
            "ready"
        );

        versions.observe("record/7", "rev-2", true, 11).unwrap();
        let stale = preview_tool
            .invoke(controller_ctx(&cid), json!({"plan_id": plan_id}))
            .await;
        match stale {
            ToolOutcome::Ok(value) => assert_eq!(value["status"], "stale"),
            other => panic!("unexpected stale preview outcome: {other:?}"),
        }
        match executor
            .invoke(
                controller_ctx(&cid),
                json!({"plan_id": plan_id, "allow_external_effects": true}),
            )
            .await
        {
            ToolOutcome::Err { code, .. } => assert_eq!(code, "resource_precondition_conflict"),
            other => panic!("stale plan was not rejected: {other:?}"),
        }
        let outbox_count: i64 = db
            .with_conn(|connection| {
                Ok(connection
                    .query_row("SELECT COUNT(*) FROM state_outbox", [], |row| row.get(0))?)
            })
            .unwrap();
        let run_count: i64 = db
            .with_conn(|connection| {
                Ok(
                    connection.query_row("SELECT COUNT(*) FROM state_chain_runs", [], |row| {
                        row.get(0)
                    })?,
                )
            })
            .unwrap();
        assert_eq!(outbox_count, 0);
        assert_eq!(run_count, 0);

        versions.observe("recipient/2", "rev-1", false, 12).unwrap();
        let unsupported_plan = match planner
            .invoke(
                controller_ctx(&cid),
                json!({
                    "objective": "send only if the recipient is still current",
                    "steps": [{
                        "label": "send",
                        "effect_kind": "transport.send",
                        "payload": {"text": "hello"},
                        "resource_keys": ["recipient/2"]
                    }]
                }),
            )
            .await
        {
            ToolOutcome::Ok(value) => value["plan_id"].as_str().unwrap().to_owned(),
            other => panic!("unexpected unsupported plan outcome: {other:?}"),
        };
        match preview_tool
            .invoke(
                controller_ctx(&cid),
                json!({"plan_id": unsupported_plan.clone()}),
            )
            .await
        {
            ToolOutcome::Ok(value) => {
                assert_eq!(value["status"], "conditional_update_unsupported")
            }
            other => panic!("unexpected unsupported preview outcome: {other:?}"),
        }
        match executor
            .invoke(
                controller_ctx(&cid),
                json!({"plan_id": unsupported_plan, "allow_external_effects": true}),
            )
            .await
        {
            ToolOutcome::Err { code, .. } => assert_eq!(code, "resource_precondition_conflict"),
            other => panic!("unsupported conditional update was allowed: {other:?}"),
        }
    }

    #[tokio::test]
    async fn partial_chain_failure_requires_approved_idempotent_compensation() {
        let db = fresh_db();
        enable_tool_chain_plugin(&db);
        let cid = ConversationId::from("conv-chain-compensation");
        seed_conversation(&db, &cid);
        db.with_conn(|connection| {
            connection.execute_batch(
                "CREATE TRIGGER accept_first_chain_effect AFTER INSERT ON state_outbox \
                 WHEN NEW.idempotency_key LIKE '%:0' BEGIN \
                   UPDATE state_outbox SET status='delivered' WHERE id=NEW.id; END; \
                 CREATE TRIGGER fail_second_chain_effect BEFORE INSERT ON state_outbox \
                 WHEN NEW.idempotency_key LIKE '%:1' BEGIN \
                   SELECT RAISE(ABORT,'injected second effect failure'); END;",
            )?;
            Ok(())
        })
        .unwrap();

        let tools = tool_chain_tools(db.clone());
        let planner = tool_by_name(&tools, "chain.plan");
        let executor = tool_by_name(&tools, "chain.execute");
        let resume = tool_by_name(&tools, "chain.resume");
        let plan_id = match planner
            .invoke(
                controller_ctx(&cid),
                json!({
                    "objective": "send two messages with a reviewed compensation",
                    "steps": [
                        {
                            "label": "send first",
                            "effect_kind": "transport.send",
                            "payload": {"text": "first"},
                            "compensation": {
                                "label": "retract first",
                                "effect_kind": "transport.retract",
                                "payload": {"message_id": "first"}
                            }
                        },
                        {"label": "send second", "effect_kind": "transport.send", "payload": {"text": "second"}}
                    ]
                }),
            )
            .await
        {
            ToolOutcome::Ok(value) => value["plan_id"].as_str().unwrap().to_owned(),
            other => panic!("unexpected plan outcome: {other:?}"),
        };
        let original_approval = match executor
            .invoke(
                controller_ctx(&cid),
                json!({"plan_id": plan_id, "allow_external_effects": true}),
            )
            .await
        {
            ToolOutcome::Ok(value) => value["approval_id"].as_str().unwrap().to_owned(),
            other => panic!("unexpected execute outcome: {other:?}"),
        };
        let partial = match resume
            .invoke(
                controller_ctx(&cid),
                json!({"approval_id": original_approval, "decision": "approve"}),
            )
            .await
        {
            ToolOutcome::Ok(value) => value,
            other => panic!("expected partial completion report: {other:?}"),
        };
        assert_eq!(partial["status"], "partially_failed");
        assert_eq!(partial["completed_effect_steps"], json!([0]));
        assert_eq!(
            partial["compensation_approvals"].as_array().unwrap().len(),
            1
        );
        assert!(
            partial["residual_consequences"]
                .to_string()
                .contains("remains recorded")
        );

        let compensation_approval = partial["compensation_approvals"][0]["approval_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let compensation = match resume
            .invoke(
                controller_ctx(&cid),
                json!({"approval_id": compensation_approval, "decision": "approve"}),
            )
            .await
        {
            ToolOutcome::Ok(value) => value,
            other => panic!("unexpected compensation outcome: {other:?}"),
        };
        assert_eq!(compensation["status"], "compensation_enqueued");
        assert_eq!(compensation["reversal_confirmed"], false);
        let ledger_status: String = db
            .with_conn(|connection| {
                Ok(connection.query_row(
                    "SELECT status FROM state_chain_compensations",
                    [],
                    |row| row.get(0),
                )?)
            })
            .unwrap();
        assert_eq!(ledger_status, "enqueued");
        match resume
            .invoke(
                controller_ctx(&cid),
                json!({"approval_id": compensation_approval, "decision": "approve"}),
            )
            .await
        {
            ToolOutcome::Ok(value) => assert_eq!(value["status"], "already_resolved"),
            other => panic!("unexpected duplicate compensation response: {other:?}"),
        }
        let outbox_count: i64 = db
            .with_conn(|connection| {
                Ok(connection
                    .query_row("SELECT COUNT(*) FROM state_outbox", [], |row| row.get(0))?)
            })
            .unwrap();
        assert_eq!(outbox_count, 2);
    }
}
