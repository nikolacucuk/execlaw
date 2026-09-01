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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredPlan {
    objective: String,
    constraints: Vec<String>,
    steps: Vec<StoredStep>,
}

#[derive(Debug, Deserialize)]
struct PlanInputStep {
    label: String,
    #[serde(default)]
    effect_kind: Option<String>,
    #[serde(default)]
    payload: Option<Value>,
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

        let mut steps: Vec<StoredStep> = match args.steps {
            Some(custom) if !custom.is_empty() => custom
                .into_iter()
                .take(max_steps)
                .enumerate()
                .map(|(i, s)| StoredStep {
                    step_index: i as u32,
                    label: s.label,
                    effect_kind: s.effect_kind,
                    payload: s.payload.unwrap_or(Value::Null),
                })
                .collect(),
            _ => vec![StoredStep {
                step_index: 0,
                label: "analyze objective".to_string(),
                effect_kind: None,
                payload: json!({"objective": objective}),
            }],
        };
        for (idx, s) in steps.iter_mut().enumerate() {
            s.step_index = idx as u32;
        }
        let has_external_effects = steps.iter().any(|s| s.effect_kind.is_some());

        let plan = StoredPlan {
            objective: objective.clone(),
            constraints: args.constraints.clone(),
            steps,
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

    fn mark_run_waiting_approval(&self, run_id: &str, now: i64) -> Result<String, String> {
        let approval_id = uuid::Uuid::new_v4().to_string();
        self.db
            .with_conn(|c| {
                c.execute(
                    "UPDATE state_chain_runs \
                     SET status = 'awaiting_approval', approval_id = ?1, updated_at = ?2 \
                     WHERE id = ?3",
                    params![approval_id, now, run_id],
                )?;
                Ok(())
            })
            .map_err(|e| format!("mark awaiting approval failed: {e}"))?;
        Ok(approval_id)
    }

    fn resolve_run_for_approval(
        &self,
        approval_id: &str,
    ) -> Result<Option<(String, String, String, i64, String)>, String> {
        self.db
            .with_conn(|c| {
                let row: Option<(String, String, String, i64, String)> = c
                    .query_row(
                        "SELECT id, plan_id, conversation_id, run_seq, status \
                         FROM state_chain_runs WHERE approval_id = ?1",
                        params![approval_id],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
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
            if let Some(effect_kind) = &step.effect_kind {
                let key = IdempotencyKey::mint(cid, TurnSeq(run_seq), step.step_index);
                let key_s = key.as_str().to_string();
                if !self.outbox_already_has_key(&key)? {
                    let payload = rmp_serde::to_vec_named(&json!({
                        "run_id": run_id,
                        "step_index": step.step_index,
                        "label": step.label,
                        "payload": step.payload,
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
                                    "payload": {}
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
            match self.runtime.mark_run_waiting_approval(&run_id, now) {
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
                ToolOutcome::err("execute_failed", e)
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
    let Some((run_id, plan_id, conv_id, run_seq, status)) =
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
            Ok(json!({
                "status": "denied",
                "run_id": run_id,
                "approval_id": approval_id,
                "conversation_id": conv_id,
            }))
        }
        ChainApprovalDecision::Approve => {
            runtime.mark_run_status(&run_id, "running", now)?;
            let (plan, _fx, _cid) = match runtime.load_plan(&plan_id) {
                Ok(v) => v,
                Err(e) => {
                    let _ = runtime.mark_run_terminal(&run_id, "failed", Some(&e), now);
                    return Err(e);
                }
            };
            let cid = ConversationId::from(conv_id.clone());
            match runtime.execute_run_steps(&run_id, run_seq, &cid, &plan, now) {
                Ok((executed, effects)) => {
                    runtime.mark_run_terminal(&run_id, "completed", None, now)?;
                    Ok(json!({
                        "status": "completed",
                        "run_id": run_id,
                        "approval_id": approval_id,
                        "conversation_id": conv_id,
                        "executed_steps": executed,
                        "effectful_steps": effects
                    }))
                }
                Err(e) => {
                    let _ = runtime.mark_run_terminal(&run_id, "failed", Some(&e), now);
                    Err(e)
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
}
