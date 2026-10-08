//! Durable definitions, mailbox, runs, and checkpoints for always-on agents.

use crate::agent_contract::{AgentOverlapPolicy, AgentTriggerSpec};
use crate::db::{Database, DbError};
use crate::runs::{
    ArtifactVerification, CriterionVerification, RunCompletionContract, RunCompletionContractDraft,
    RunCompletionReport, VerificationStatus, build_completion_report,
};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRow {
    pub id: String,
    pub name: String,
    pub role_prompt: String,
    pub model: Option<String>,
    pub backend_purpose: String,
    pub tools: Vec<String>,
    pub trust_policy: serde_json::Value,
    pub interval_secs: u32,
    pub token_budget: u32,
    pub max_runtime_secs: u32,
    pub concurrency_limit: u32,
    pub enabled: bool,
    pub paused: bool,
    pub next_run_at: Option<i64>,
    pub last_run_at: Option<i64>,
    pub last_run_status: Option<String>,
    pub last_error: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub trigger: serde_json::Value,
    pub reply_mode: String,
    pub definition_version: u32,
    pub schedule_next_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplyDraftRow {
    pub id: String,
    pub agent_id: String,
    pub conversation_id: String,
    pub channel: String,
    pub recipient: String,
    pub inbound_text: String,
    pub draft_text: String,
    pub status: String,
    pub created_at: i64,
    pub reviewed_at: Option<i64>,
    pub sent_at: Option<i64>,
    pub review_note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRunRow {
    pub id: String,
    pub agent_id: String,
    pub status: String,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub tokens_used: Option<u32>,
    pub checkpoint: serde_json::Value,
    pub output_text: Option<String>,
    pub error: Option<String>,
    pub completion: Option<RunCompletionReport>,
    pub mailbox_id: Option<String>,
    pub definition_version: Option<u32>,
    pub outcome_kind: Option<String>,
}

/// Durable receipt for a calendar fire, including skipped-policy reasons.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentScheduleFireRow {
    pub agent_id: String,
    pub due_at: i64,
    pub status: String,
    pub reason: Option<String>,
    pub mailbox_id: Option<String>,
    pub recorded_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentMessageRow {
    pub id: String,
    pub agent_id: String,
    pub parent_agent_id: Option<String>,
    pub direction: String,
    pub content: String,
    pub created_at: i64,
    pub delivered_at: Option<i64>,
    pub result_run_id: Option<String>,
    pub source_kind: String,
    pub source_event_id: Option<String>,
    pub source_occurred_at: Option<i64>,
    pub conversation_id: Option<String>,
    pub recipient: Option<String>,
    pub definition_version: Option<u32>,
    pub available_at: i64,
}

/// Terminal run update committed with mailbox acknowledgment in one transaction.
pub struct AgentRunCompletion<'a> {
    pub agent_id: &'a str,
    pub run_id: &'a str,
    pub status: &'a str,
    pub outcome_kind: Option<&'a str>,
    pub now: i64,
    pub next_run_at: Option<i64>,
    pub tokens: Option<u32>,
    pub output: Option<&'a str>,
    pub error: Option<&'a str>,
    pub checkpoint: &'a serde_json::Value,
    pub mailbox_ids: &'a [String],
}

/// Provenance and content for one idempotent external mailbox admission.
pub struct AgentSourceEvent<'a> {
    pub source_kind: &'a str,
    pub source_event_id: &'a str,
    pub occurred_at: i64,
    pub conversation_id: &'a str,
    pub recipient: &'a str,
    pub content: &'a str,
}

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error(transparent)]
    Db(#[from] DbError),
    #[error("agent not found: {0}")]
    NotFound(String),
    #[error("invalid agent: {0}")]
    Invalid(String),
    #[error("encoding: {0}")]
    Encoding(String),
}

#[derive(Debug, Clone)]
pub struct AgentUpsert {
    pub id: Option<String>,
    pub name: String,
    pub role_prompt: String,
    pub model: Option<String>,
    pub backend_purpose: String,
    pub tools: Vec<String>,
    pub trust_policy: serde_json::Value,
    pub interval_secs: u32,
    pub token_budget: u32,
    pub max_runtime_secs: u32,
    pub concurrency_limit: u32,
    pub enabled: bool,
    pub trigger: serde_json::Value,
    pub reply_mode: String,
}

#[derive(Clone)]
pub struct AgentStore {
    db: Database,
}

impl AgentStore {
    pub fn new(db: &Database) -> Self {
        Self { db: db.clone() }
    }

    pub fn list(&self) -> Result<Vec<AgentRow>, AgentError> {
        self.db.with_conn(|c| {
            let mut stmt = c.prepare("SELECT id,name,role_prompt,model,backend_purpose,tools_json,trust_policy_json,interval_secs,token_budget,max_runtime_secs,concurrency_limit,enabled,paused,next_run_at,last_run_at,last_run_status,last_error,created_at,updated_at,trigger_json,reply_mode,definition_version,schedule_next_at FROM config_agents ORDER BY name")?;
            let rows = stmt.query_map([], map_agent)?;
            rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
        }).map_err(Into::into)
    }

    pub fn get(&self, id: &str) -> Result<Option<AgentRow>, AgentError> {
        self.db.with_conn(|c| Ok(c.query_row("SELECT id,name,role_prompt,model,backend_purpose,tools_json,trust_policy_json,interval_secs,token_budget,max_runtime_secs,concurrency_limit,enabled,paused,next_run_at,last_run_at,last_run_status,last_error,created_at,updated_at,trigger_json,reply_mode,definition_version,schedule_next_at FROM config_agents WHERE id=?1", [id], map_agent).optional()?)).map_err(Into::into)
    }

    /// Read the immutable definition used by a queued mailbox event.
    pub fn definition_snapshot(
        &self,
        id: &str,
        version: u32,
    ) -> Result<Option<serde_json::Value>, AgentError> {
        let raw: Option<String> = self.db.with_conn(|connection| Ok(connection.query_row(
            "SELECT definition_json FROM config_agent_definition_revisions WHERE agent_id=?1 AND version=?2",
            params![id,version], |row| row.get(0)).optional()?))?;
        raw.map(|raw| {
            serde_json::from_str(&raw).map_err(|error| AgentError::Encoding(error.to_string()))
        })
        .transpose()
    }

    pub fn upsert(&self, input: &AgentUpsert, now: i64) -> Result<AgentRow, AgentError> {
        if input.name.trim().is_empty() || input.role_prompt.trim().is_empty() {
            return Err(AgentError::Invalid(
                "name and role_prompt are required".into(),
            ));
        }
        if input.interval_secs == 0
            || input.token_budget == 0
            || input.max_runtime_secs == 0
            || input.concurrency_limit == 0
        {
            return Err(AgentError::Invalid(
                "budgets and concurrency must be greater than zero".into(),
            ));
        }
        if !matches!(input.reply_mode.as_str(), "draft" | "automatic") {
            return Err(AgentError::Invalid(
                "reply_mode must be draft or automatic".into(),
            ));
        }
        let trigger_spec =
            AgentTriggerSpec::from_value(&input.trigger).map_err(AgentError::Invalid)?;
        if input
            .tools
            .iter()
            .any(|tool| !matches!(tool.as_str(), "read" | "search"))
        {
            return Err(AgentError::Invalid(
                "always-on agents currently support only scoped read and search capabilities"
                    .into(),
            ));
        }
        let schedule_next_at = trigger_spec
            .schedule
            .as_ref()
            .map(|schedule| schedule.next_fire_after(now))
            .transpose()
            .map_err(AgentError::Invalid)?
            .flatten();
        let id = input
            .id
            .clone()
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        let tools =
            serde_json::to_string(&input.tools).map_err(|e| AgentError::Encoding(e.to_string()))?;
        let trust = serde_json::to_string(&input.trust_policy)
            .map_err(|e| AgentError::Encoding(e.to_string()))?;
        let definition_json = serde_json::to_string(&serde_json::json!({
            "name": input.name, "role_prompt": input.role_prompt, "model": input.model,
            "backend_purpose": input.backend_purpose, "tools": input.tools,
            "trust_policy": input.trust_policy, "trigger": input.trigger,
            "reply_mode": input.reply_mode, "token_budget": input.token_budget,
            "max_runtime_secs": input.max_runtime_secs,
            "interval_secs": input.interval_secs, "concurrency_limit": input.concurrency_limit,
        }))
        .map_err(|e| AgentError::Encoding(e.to_string()))?;
        self.db.transaction(|c| {
            let trigger = serde_json::to_string(&input.trigger).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
            c.execute("INSERT INTO config_agents (id,name,role_prompt,model,backend_purpose,tools_json,trust_policy_json,interval_secs,token_budget,max_runtime_secs,concurrency_limit,enabled,paused,next_run_at,created_at,updated_at,trigger_json,reply_mode) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,0,?13,?13,?13,?14,?15) ON CONFLICT(id) DO UPDATE SET name=excluded.name,role_prompt=excluded.role_prompt,model=excluded.model,backend_purpose=excluded.backend_purpose,tools_json=excluded.tools_json,trust_policy_json=excluded.trust_policy_json,interval_secs=excluded.interval_secs,token_budget=excluded.token_budget,max_runtime_secs=excluded.max_runtime_secs,concurrency_limit=excluded.concurrency_limit,enabled=excluded.enabled,trigger_json=excluded.trigger_json,reply_mode=excluded.reply_mode,definition_version=config_agents.definition_version+1,next_run_at=COALESCE(config_agents.next_run_at, excluded.next_run_at),updated_at=excluded.updated_at", params![id,input.name,input.role_prompt,input.model,input.backend_purpose,tools,trust,input.interval_secs,input.token_budget,input.max_runtime_secs,input.concurrency_limit,input.enabled as i64,now,trigger,input.reply_mode])?;
            let version: i64 = c.query_row("SELECT definition_version FROM config_agents WHERE id=?1", [&id], |row| row.get(0))?;
            c.execute("INSERT INTO config_agent_definition_revisions(agent_id,version,definition_json,created_at) VALUES (?1,?2,?3,?4)", params![id,version,definition_json,now])?;
            c.execute("UPDATE config_agents SET schedule_next_at=?2 WHERE id=?1", params![id,schedule_next_at])?;
            if trigger_is_event_only(&input.trigger) {
                c.execute("UPDATE config_agents SET next_run_at=NULL WHERE id=?1", [&id])?;
            }
            Ok(())
        })?;
        self.get(&id)?.ok_or(AgentError::NotFound(id))
    }

    pub fn set_state(
        &self,
        id: &str,
        enabled: Option<bool>,
        paused: Option<bool>,
        next_run_at: Option<i64>,
    ) -> Result<(), AgentError> {
        self.db.with_conn(|c| { c.execute("UPDATE config_agents SET enabled=COALESCE(?1,enabled), paused=COALESCE(?2,paused), next_run_at=COALESCE(?3,next_run_at), updated_at=strftime('%s','now') WHERE id=?4", params![enabled.map(|v| v as i64), paused.map(|v| v as i64), next_run_at, id])?; Ok(()) }).map_err(Into::into)
    }

    pub fn delete(&self, id: &str) -> Result<bool, AgentError> {
        self.db
            .with_conn(|c| Ok(c.execute("DELETE FROM config_agents WHERE id=?1", [id])? > 0))
            .map_err(Into::into)
    }

    pub fn claim_due(&self, id: &str, now: i64) -> Result<Option<AgentRow>, AgentError> {
        self.db.with_conn(|c| { let n = c.execute("UPDATE config_agents SET next_run_at=?1, last_run_status='running', last_error=NULL, updated_at=?1 WHERE id=?2 AND enabled=1 AND paused=0 AND (next_run_at IS NULL OR next_run_at<=?1) AND NOT EXISTS (SELECT 1 FROM state_agent_runs WHERE agent_id=?2 AND status='running')", params![now.saturating_add(1),id])?; if n == 0 { return Ok(None); } Ok(Some(c.query_row("SELECT id,name,role_prompt,model,backend_purpose,tools_json,trust_policy_json,interval_secs,token_budget,max_runtime_secs,concurrency_limit,enabled,paused,next_run_at,last_run_at,last_run_status,last_error,created_at,updated_at,trigger_json,reply_mode,definition_version,schedule_next_at FROM config_agents WHERE id=?1", [id], map_agent)?)) }).map_err(Into::into)
    }

    /// Mark run rows left in `running` by a dead server as interrupted and
    /// make eligible agents due again. Agent inbox messages are acknowledged
    /// only after execution succeeds, so the supervisor will replay the same
    /// mailbox input under a new run id after restart.
    pub fn reconcile_interrupted_runs(&self, now: i64) -> Result<usize, AgentError> {
        self.db.transaction(|tx| {
            let changed = tx.execute(
                "UPDATE state_agent_runs SET status='interrupted',finished_at=?1, \
                 error=COALESCE(error,'server restarted during agent execution') \
                 WHERE status='running'",
                [now],
            )?;
            tx.execute(
                "UPDATE config_agents SET last_run_status='interrupted', \
                 last_error='server restarted during agent execution', \
                 next_run_at=CASE WHEN enabled=1 AND paused=0 THEN ?1 ELSE next_run_at END, \
                 updated_at=?1 WHERE EXISTS (SELECT 1 FROM state_agent_runs r \
                 WHERE r.agent_id=config_agents.id AND r.status='interrupted' AND r.finished_at=?1)",
                [now],
            )?;
            Ok(changed)
        }).map_err(Into::into)
    }

    pub fn finish(
        &self,
        agent_id: &str,
        run_id: &str,
        status: &str,
        now: i64,
        next_run_at: Option<i64>,
        tokens: Option<u32>,
        output: Option<&str>,
        error: Option<&str>,
        checkpoint: &serde_json::Value,
    ) -> Result<(), AgentError> {
        self.complete_mailbox_run(&AgentRunCompletion {
            agent_id,
            run_id,
            status,
            outcome_kind: None,
            now,
            next_run_at,
            tokens,
            output,
            error,
            checkpoint,
            mailbox_ids: &[],
        })
    }

    /// Finish a run and acknowledge its mailbox items atomically.
    pub fn complete_mailbox_run(
        &self,
        completion: &AgentRunCompletion<'_>,
    ) -> Result<(), AgentError> {
        let checkpoint = serde_json::to_string(completion.checkpoint)
            .map_err(|error| AgentError::Encoding(error.to_string()))?;
        self.db.transaction(|tx| {
            let changed = tx.execute(
                "UPDATE state_agent_runs SET status=?1,finished_at=?2,tokens_used=?3,output_text=?4,error=?5,checkpoint_json=?6,outcome_kind=?7 WHERE id=?8 AND agent_id=?9 AND status='running'",
                params![completion.status, completion.now, completion.tokens, completion.output, completion.error, checkpoint, completion.outcome_kind, completion.run_id, completion.agent_id],
            )?;
            if changed != 1 { return Err(DbError::Invariant("agent run is no longer running".into())); }
            for message_id in completion.mailbox_ids {
                let changed = tx.execute(
                    "UPDATE state_agent_messages SET delivered_at=?1,result_run_id=?2 WHERE id=?3 AND agent_id=?4 AND delivered_at IS NULL",
                    params![completion.now, completion.run_id, message_id, completion.agent_id],
                )?;
                if changed != 1 { return Err(DbError::Invariant(format!("agent mailbox item was already consumed: {message_id}"))); }
            }
            tx.execute("UPDATE config_agents SET last_run_at=?1,last_run_status=?2,last_error=?3,next_run_at=COALESCE(?4,(SELECT MIN(available_at) FROM state_agent_messages WHERE agent_id=?5 AND delivered_at IS NULL AND superseded_by IS NULL)),updated_at=?1 WHERE id=?5",
                params![completion.now,completion.status,completion.error,completion.next_run_at,completion.agent_id])?;
            Ok(())
        })?;
        Ok(())
    }

    pub fn insert_run(
        &self,
        agent_id: &str,
        now: i64,
        checkpoint: &serde_json::Value,
    ) -> Result<String, AgentError> {
        self.insert_run_for_mailbox(agent_id, None, None, now, checkpoint)
    }

    /// Start a run against one immutable mailbox item and definition version.
    pub fn insert_run_for_mailbox(
        &self,
        agent_id: &str,
        mailbox_id: Option<&str>,
        definition_version: Option<u32>,
        now: i64,
        checkpoint: &serde_json::Value,
    ) -> Result<String, AgentError> {
        let id = Uuid::new_v4().to_string();
        let cp =
            serde_json::to_string(checkpoint).map_err(|e| AgentError::Encoding(e.to_string()))?;
        self.db.transaction(|tx| {
            tx.execute(
                "INSERT INTO state_agent_runs (id,agent_id,status,started_at,checkpoint_json,mailbox_id,definition_version) \
                 VALUES (?1,?2,'running',?3,?4,?5,?6)",
                params![id, agent_id, now, cp, mailbox_id, definition_version],
            )?;
            let contract: Option<(String, String, bool)> = tx
                .query_row(
                    "SELECT acceptance_criteria_json,required_artifacts_json,delivery_required \
                     FROM config_agent_completion_contracts WHERE agent_id=?1",
                    [agent_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()?;
            if let Some((criteria, artifacts, delivery_required)) = contract {
                tx.execute(
                    "INSERT INTO state_agent_run_completion_contracts \
                     (run_id,acceptance_criteria_json,required_artifacts_json,delivery_required,created_at,updated_at) \
                     VALUES (?1,?2,?3,?4,?5,?5)",
                    params![id, criteria, artifacts, delivery_required, now],
                )?;
            }
            Ok(())
        })?;
        Ok(id)
    }

    /// Store immutable task acceptance requirements for future agent runs.
    pub fn set_completion_contract(
        &self,
        agent_id: &str,
        contract: Option<&RunCompletionContractDraft>,
        now: i64,
    ) -> Result<(), AgentError> {
        if let Some(contract) = contract {
            contract
                .validate()
                .map_err(|error| AgentError::Invalid(error.to_string()))?;
            if contract.acceptance_criteria.iter().any(|criterion| {
                criterion
                    .verifier
                    .as_ref()
                    .is_some_and(|verifier| verifier.step_id != "agent:output")
            }) {
                return Err(AgentError::Invalid(
                    "agent-run verifiers may inspect only agent:output".into(),
                ));
            }
        }
        self.db.transaction(|tx| {
            let exists: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM config_agents WHERE id=?1)",
                [agent_id],
                |row| row.get(0),
            )?;
            if !exists {
                return Err(DbError::Invariant(format!("agent not found: {agent_id}")));
            }
            match contract {
                Some(contract) => {
                    let criteria = serde_json::to_string(&contract.acceptance_criteria)
                        .map_err(|error| DbError::Invariant(error.to_string()))?;
                    let artifacts = serde_json::to_string(&contract.required_artifacts)
                        .map_err(|error| DbError::Invariant(error.to_string()))?;
                    tx.execute(
                        "INSERT INTO config_agent_completion_contracts \
                         (agent_id,acceptance_criteria_json,required_artifacts_json,delivery_required,updated_at) \
                         VALUES (?1,?2,?3,?4,?5) ON CONFLICT(agent_id) DO UPDATE SET \
                         acceptance_criteria_json=excluded.acceptance_criteria_json, \
                         required_artifacts_json=excluded.required_artifacts_json, \
                         delivery_required=excluded.delivery_required,updated_at=excluded.updated_at",
                        params![agent_id, criteria, artifacts, contract.delivery_required, now],
                    )?;
                }
                None => {
                    tx.execute(
                        "DELETE FROM config_agent_completion_contracts WHERE agent_id=?1",
                        [agent_id],
                    )?;
                }
            }
            Ok(())
        })?;
        Ok(())
    }

    /// Return the default acceptance requirements used by new runs of one agent.
    pub fn completion_contract(
        &self,
        agent_id: &str,
    ) -> Result<Option<RunCompletionContractDraft>, AgentError> {
        self.db
            .with_conn(|connection| {
                connection
                    .query_row(
                        "SELECT acceptance_criteria_json,required_artifacts_json,delivery_required \
                     FROM config_agent_completion_contracts WHERE agent_id=?1",
                        [agent_id],
                        |row| {
                            let criteria: String = row.get(0)?;
                            let artifacts: String = row.get(1)?;
                            Ok(RunCompletionContractDraft {
                                acceptance_criteria: serde_json::from_str(&criteria)
                                    .map_err(|_| rusqlite::Error::InvalidQuery)?,
                                required_artifacts: serde_json::from_str(&artifacts)
                                    .map_err(|_| rusqlite::Error::InvalidQuery)?,
                                delivery_required: row.get(2)?,
                            })
                        },
                    )
                    .optional()
                    .map_err(Into::into)
            })
            .map_err(Into::into)
    }

    /// Read evidence and the deterministic completion state for an agent run.
    pub fn completion_report(
        &self,
        run_id: &str,
    ) -> Result<Option<RunCompletionReport>, AgentError> {
        self.db.with_conn(|connection| {
            let stored: Option<(String, String, bool, bool, Option<String>, i64)> = connection
                .query_row(
                    "SELECT acceptance_criteria_json,required_artifacts_json,delivery_required, \
                     delivery_confirmed,delivery_evidence_ref,created_at \
                     FROM state_agent_run_completion_contracts WHERE run_id=?1",
                    [run_id],
                    |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?)),
                )
                .optional()?;
            let Some((criteria_json, artifacts_json, delivery_required, delivered, delivery_ref, created_at)) = stored else {
                return Ok(None);
            };
            let contract = RunCompletionContract {
                run_id: run_id.to_owned(),
                acceptance_criteria: serde_json::from_str(&criteria_json).map_err(|_| rusqlite::Error::InvalidQuery)?,
                required_artifacts: serde_json::from_str(&artifacts_json).map_err(|_| rusqlite::Error::InvalidQuery)?,
                delivery_required,
                created_at,
            };
            let mut verifications: Vec<CriterionVerification> = {
                let mut statement = connection.prepare("SELECT criterion_id,status,evidence_refs_json,detail,verified_at FROM state_agent_run_completion_verifications WHERE run_id=?1 ORDER BY criterion_id")?;
                statement.query_map([run_id], |row| {
                    let status: String = row.get(1)?;
                    let status = VerificationStatus::parse(&status).ok_or(rusqlite::Error::InvalidQuery)?;
                    let refs: String = row.get(2)?;
                    Ok(CriterionVerification {
                        criterion_id: row.get(0)?,
                        status,
                        evidence_refs: serde_json::from_str(&refs).map_err(|_| rusqlite::Error::InvalidQuery)?,
                        detail: row.get(3)?,
                        verified_at: row.get(4)?,
                    })
                })?.collect::<Result<Vec<_>,_>>()?
            };
            let artifacts = {
                let mut statement = connection.prepare("SELECT artifact_id,present,evidence_ref,detail,checked_at FROM state_agent_run_completion_artifacts WHERE run_id=?1 ORDER BY artifact_id")?;
                statement.query_map([run_id], |row| Ok(ArtifactVerification {
                    artifact_id: row.get(0)?,
                    present: row.get(1)?,
                    evidence_ref: row.get(2)?,
                    detail: row.get(3)?,
                    checked_at: row.get(4)?,
                }))?.collect::<Result<Vec<_>,_>>()?
            };
            let (executor_status, output_text, finished_at): (String, Option<String>, Option<i64>) = connection.query_row(
                "SELECT status,output_text,finished_at FROM state_agent_runs WHERE id=?1",
                [run_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
            for criterion in &contract.acceptance_criteria {
                let Some(verifier) = criterion.verifier.as_ref() else {
                    continue;
                };
                let (status, detail, evidence_refs) = match output_text.as_deref() {
                    None => (VerificationStatus::Pending, "agent output is not committed", Vec::new()),
                    Some(text) => {
                        let mut output = serde_json::json!({"text": text});
                        if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(text) {
                            output["json"] = parsed;
                        }
                        if output.pointer(&verifier.json_pointer) == Some(&verifier.expected) {
                            (
                                VerificationStatus::Passed,
                                "committed agent output matched",
                                vec![format!("agent-run:{run_id}/output")],
                            )
                        } else {
                            (VerificationStatus::Failed, "committed agent output did not match", Vec::new())
                        }
                    }
                };
                verifications.retain(|item| item.criterion_id != criterion.criterion_id);
                verifications.push(CriterionVerification {
                    criterion_id: criterion.criterion_id.clone(),
                    status,
                    evidence_refs,
                    detail: Some(detail.into()),
                    verified_at: finished_at.unwrap_or(0),
                });
            }
            for criterion in &contract.acceptance_criteria {
                if criterion.verifier.is_some() {
                    continue;
                }
                if let Some(record) = verifications.iter_mut().find(|record| record.criterion_id == criterion.criterion_id)
                    && record.status == VerificationStatus::Passed
                {
                    let row_id = format!("{run_id}/{}", criterion.criterion_id);
                    let mut valid = false;
                    for reference in &record.evidence_refs {
                        if crate::completion_evidence::attestation_exists(
                            connection,
                            reference,
                            "agent_run_completion_verification",
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
            let mut artifacts = artifacts;
            for artifact in &mut artifacts {
                if artifact.present
                    && !crate::completion_evidence::agent_output_exists(
                        connection,
                        run_id,
                        artifact.evidence_ref.as_deref().unwrap_or_default(),
                    )?
                {
                    artifact.present = false;
                    artifact.detail = Some("agent output is missing or invalid".into());
                }
            }
            let delivery_proven = if delivered {
                crate::completion_evidence::attestation_exists(
                    connection,
                    delivery_ref.as_deref().unwrap_or_default(),
                    "agent_run_completion_delivery",
                    run_id,
                    "confirmed",
                    None,
                )?
            } else {
                false
            };
            let mut report = build_completion_report(contract, verifications, artifacts, delivery_proven, delivery_ref);
            match executor_status.as_str() {
                "success" => {}
                "failed" | "interrupted" | "cancelled" => {
                    report.status = crate::runs::RunCompletionStatus::Blocked;
                    report.unfinished.push(format!("Agent run {executor_status}"));
                }
                _ if report.status == crate::runs::RunCompletionStatus::VerifiedComplete => {
                    report.status = crate::runs::RunCompletionStatus::Incomplete;
                    report.unfinished.push("Agent run has not completed".into());
                }
                _ => {}
            }
            Ok(Some(report))
        }).map_err(Into::into)
    }

    /// Record a verifier result against an immutable agent-run criterion.
    pub fn record_completion_verification(
        &self,
        run_id: &str,
        verification: &CriterionVerification,
    ) -> Result<(), AgentError> {
        crate::runs::validate_reference_list(&verification.evidence_refs)
            .map_err(|error| AgentError::Invalid(error.to_string()))?;
        if verification.status == VerificationStatus::Passed
            && verification.evidence_refs.is_empty()
        {
            return Err(AgentError::Invalid(
                "a passing verification requires evidence references".into(),
            ));
        }
        if verification
            .detail
            .as_ref()
            .is_some_and(|detail| detail.len() > 2_000)
        {
            return Err(AgentError::Invalid(
                "verification detail exceeds 2000 bytes".into(),
            ));
        }
        let report = self
            .completion_report(run_id)?
            .ok_or_else(|| AgentError::NotFound(run_id.into()))?;
        let criterion = report
            .contract
            .acceptance_criteria
            .iter()
            .find(|criterion| criterion.criterion_id == verification.criterion_id);
        let Some(criterion) = criterion else {
            return Err(AgentError::NotFound(verification.criterion_id.clone()));
        };
        if criterion.verifier.is_some() {
            return Err(AgentError::Invalid(
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
                        "agent_run_completion_verification",
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
                return Err(AgentError::Invalid(
                    "a manual passing result requires a scoped Controller attestation".into(),
                ));
            }
        }
        let refs = serde_json::to_string(&verification.evidence_refs)
            .map_err(|error| AgentError::Encoding(error.to_string()))?;
        self.db.with_conn(|connection| {
            connection.execute(
                "INSERT INTO state_agent_run_completion_verifications(run_id,criterion_id,status,evidence_refs_json,detail,verified_at) \
                 VALUES (?1,?2,?3,?4,?5,?6) ON CONFLICT(run_id,criterion_id) DO UPDATE SET \
                 status=excluded.status,evidence_refs_json=excluded.evidence_refs_json,detail=excluded.detail,verified_at=excluded.verified_at",
                params![run_id,verification.criterion_id,verification.status.as_str(),refs,verification.detail,verification.verified_at],
            )?;
            Ok(())
        })?;
        Ok(())
    }

    /// Record evidence for one required agent-run artifact.
    pub fn record_artifact_verification(
        &self,
        run_id: &str,
        verification: &ArtifactVerification,
    ) -> Result<(), AgentError> {
        let report = self
            .completion_report(run_id)?
            .ok_or_else(|| AgentError::NotFound(run_id.into()))?;
        if !report
            .contract
            .required_artifacts
            .iter()
            .any(|artifact| artifact.artifact_id == verification.artifact_id)
        {
            return Err(AgentError::NotFound(verification.artifact_id.clone()));
        }
        if verification.present
            && verification
                .evidence_ref
                .as_deref()
                .is_none_or(str::is_empty)
        {
            return Err(AgentError::Invalid(
                "a present artifact requires an evidence reference".into(),
            ));
        }
        if verification
            .evidence_ref
            .as_ref()
            .is_some_and(|reference| reference.len() > 512)
            || verification
                .detail
                .as_ref()
                .is_some_and(|detail| detail.len() > 2_000)
        {
            return Err(AgentError::Invalid(
                "artifact evidence fields exceed their bounds".into(),
            ));
        }
        if verification.present {
            let valid = self.db.with_conn(|connection| {
                crate::completion_evidence::agent_output_exists(
                    connection,
                    run_id,
                    verification.evidence_ref.as_deref().unwrap_or_default(),
                )
                .map_err(DbError::from)
            })?;
            if !valid {
                return Err(AgentError::Invalid(
                    "artifact reference has no committed output in this agent run".into(),
                ));
            }
        }
        self.db.with_conn(|connection| {
            connection.execute(
                "INSERT INTO state_agent_run_completion_artifacts(run_id,artifact_id,present,evidence_ref,detail,checked_at) \
                 VALUES (?1,?2,?3,?4,?5,?6) ON CONFLICT(run_id,artifact_id) DO UPDATE SET \
                 present=excluded.present,evidence_ref=excluded.evidence_ref,detail=excluded.detail,checked_at=excluded.checked_at",
                params![run_id,verification.artifact_id,verification.present,verification.evidence_ref,verification.detail,verification.checked_at],
            )?;
            Ok(())
        })?;
        Ok(())
    }

    /// Confirm externally required delivery for an agent run.
    pub fn confirm_completion_delivery(
        &self,
        run_id: &str,
        evidence_ref: &str,
        now: i64,
    ) -> Result<(), AgentError> {
        if evidence_ref.trim().is_empty() || evidence_ref.len() > 512 {
            return Err(AgentError::Invalid(
                "delivery evidence must be 1 to 512 bytes".into(),
            ));
        }
        let report = self
            .completion_report(run_id)?
            .ok_or_else(|| AgentError::NotFound(run_id.into()))?;
        if !report.contract.delivery_required {
            return Err(AgentError::Invalid(
                "delivery is not required by this run contract".into(),
            ));
        }
        let valid = self.db.with_conn(|connection| {
            crate::completion_evidence::attestation_exists(
                connection,
                evidence_ref,
                "agent_run_completion_delivery",
                run_id,
                "confirmed",
                None,
            )
            .map_err(DbError::from)
        })?;
        if !valid {
            return Err(AgentError::Invalid(
                "delivery reference has no scoped Controller attestation".into(),
            ));
        }
        self.db.with_conn(|connection| {
            connection.execute("UPDATE state_agent_run_completion_contracts SET delivery_confirmed=1,delivery_evidence_ref=?2,updated_at=?3 WHERE run_id=?1",params![run_id,evidence_ref,now])?;
            Ok(())
        })?;
        Ok(())
    }

    pub fn pending_messages(
        &self,
        agent_id: &str,
        limit: u32,
    ) -> Result<Vec<AgentMessageRow>, AgentError> {
        self.pending_messages_at(agent_id, limit, chrono::Utc::now().timestamp())
    }

    /// Read only due mailbox items at a caller-supplied clock instant.
    pub fn pending_messages_at(
        &self,
        agent_id: &str,
        limit: u32,
        now: i64,
    ) -> Result<Vec<AgentMessageRow>, AgentError> {
        self.db.with_conn(|c| { let mut s=c.prepare("SELECT id,agent_id,parent_agent_id,direction,content,created_at,delivered_at,result_run_id,source_kind,source_event_id,source_occurred_at,conversation_id,recipient,definition_version,available_at FROM state_agent_messages WHERE agent_id=?1 AND delivered_at IS NULL AND superseded_by IS NULL AND available_at<=?3 ORDER BY created_at,id LIMIT ?2")?; let rows=s.query_map(params![agent_id,limit,now],map_message)?; rows.collect::<Result<Vec<_>,_>>().map_err(Into::into) }).map_err(Into::into)
    }

    /// Read one due recipient batch, preventing event coalescing from mixing
    /// conversations or destinations in one agent run.
    pub fn pending_messages_for_scope(
        &self,
        agent_id: &str,
        conversation_id: &str,
        recipient: &str,
        definition_version: Option<u32>,
        limit: u32,
    ) -> Result<Vec<AgentMessageRow>, AgentError> {
        self.db.with_conn(|connection| {
            let mut statement = connection.prepare(
                "SELECT id,agent_id,parent_agent_id,direction,content,created_at,delivered_at,result_run_id,
                        source_kind,source_event_id,source_occurred_at,conversation_id,recipient,definition_version,available_at
                 FROM state_agent_messages WHERE agent_id=?1 AND delivered_at IS NULL AND superseded_by IS NULL AND available_at<=?4
                   AND conversation_id=?2 AND recipient=?3 AND definition_version IS ?5 ORDER BY created_at,id LIMIT ?6",
            )?;
            let rows = statement.query_map(
                params![agent_id, conversation_id, recipient, chrono::Utc::now().timestamp(), definition_version, limit.min(100)],
                map_message,
            )?;
            rows.collect::<Result<Vec<_>, _>>().map_err(Into::into)
        }).map_err(Into::into)
    }

    pub fn enqueue(
        &self,
        agent_id: &str,
        parent_agent_id: Option<&str>,
        content: &str,
        now: i64,
    ) -> Result<String, AgentError> {
        if content.trim().is_empty() {
            return Err(AgentError::Invalid("message is empty".into()));
        }
        let id = Uuid::new_v4().to_string();
        self.db.with_conn(|c|{c.execute("INSERT INTO state_agent_messages (id,agent_id,parent_agent_id,direction,content,created_at,available_at) VALUES (?1,?2,?3,'inbound',?4,?5,?5)",params![id,agent_id,parent_agent_id,content,now])?;Ok(())})?;
        Ok(id)
    }

    pub fn enqueue_triggered(
        &self,
        agent_id: &str,
        content: &str,
        now: i64,
    ) -> Result<String, AgentError> {
        let id = self.enqueue(agent_id, None, content, now)?;
        self.db.with_conn(|c| {
            c.execute(
                "UPDATE config_agents SET next_run_at=?1, updated_at=?1 WHERE id=?2",
                params![now, agent_id],
            )?;
            Ok(())
        })?;
        Ok(id)
    }

    /// Queue one external event once, keyed by its source and stable event ID.
    /// Repeated webhook deliveries return the original mailbox ID.
    pub fn enqueue_triggered_event(
        &self,
        agent_id: &str,
        event: &AgentSourceEvent<'_>,
        now: i64,
    ) -> Result<(String, bool), AgentError> {
        self.enqueue_triggered_event_at(agent_id, event, now, now)
    }

    /// Queue one source event for a durable debounce deadline. The event is
    /// stored immediately for attribution; only its work eligibility is delayed.
    pub fn enqueue_triggered_event_at(
        &self,
        agent_id: &str,
        event: &AgentSourceEvent<'_>,
        available_at: i64,
        now: i64,
    ) -> Result<(String, bool), AgentError> {
        if event.source_kind.trim().is_empty()
            || event.source_event_id.trim().is_empty()
            || event.conversation_id.trim().is_empty()
            || event.recipient.trim().is_empty()
            || event.content.trim().is_empty()
        {
            return Err(AgentError::Invalid(
                "source, event ID, conversation, recipient, and content are required".into(),
            ));
        }
        let id = Uuid::new_v4().to_string();
        let result = self.db.transaction(|tx| {
            let version: i64 = tx.query_row("SELECT definition_version FROM config_agents WHERE id=?1", [agent_id], |row| row.get(0))?;
            let inserted = tx.execute(
                "INSERT INTO state_agent_messages (id,agent_id,direction,content,created_at,source_kind,source_event_id,source_occurred_at,conversation_id,recipient,definition_version,available_at) VALUES (?1,?2,'inbound',?3,?4,?5,?6,?7,?8,?9,?10,?11) ON CONFLICT DO NOTHING",
                params![id,agent_id,event.content,now,event.source_kind,event.source_event_id,event.occurred_at,event.conversation_id,event.recipient,version,available_at.max(now)],
            )? == 1;
            let mailbox_id = if inserted { id.clone() } else {
                tx.query_row("SELECT id FROM state_agent_messages WHERE agent_id=?1 AND source_kind=?2 AND source_event_id=?3", params![agent_id,event.source_kind,event.source_event_id], |row| row.get(0))?
            };
            if inserted {
                tx.execute("UPDATE config_agents SET next_run_at=CASE WHEN next_run_at IS NULL OR next_run_at>?1 THEN ?1 ELSE next_run_at END,updated_at=?2 WHERE id=?3", params![available_at.max(now),now,agent_id])?;
            }
            Ok((mailbox_id, inserted))
        })?;
        Ok(result)
    }

    /// Atomically mark a stale source proposal superseded and enqueue its
    /// correction. The old mailbox row remains available for audit.
    pub fn enqueue_triggered_correction(
        &self,
        agent_id: &str,
        event: &AgentSourceEvent<'_>,
        supersedes_event_id: &str,
        available_at: i64,
        now: i64,
    ) -> Result<(String, bool), AgentError> {
        if event.source_kind.trim().is_empty()
            || event.source_event_id.trim().is_empty()
            || event.conversation_id.trim().is_empty()
            || event.recipient.trim().is_empty()
            || event.content.trim().is_empty()
            || supersedes_event_id.trim().is_empty()
        {
            return Err(AgentError::Invalid(
                "correction identity and content are required".into(),
            ));
        }
        let new_id = Uuid::new_v4().to_string();
        let result = self.db.transaction(|tx| {
            let version: i64 = tx.query_row("SELECT definition_version FROM config_agents WHERE id=?1", [agent_id], |row| row.get(0))?;
            let inserted = tx.execute(
                "INSERT INTO state_agent_messages(id,agent_id,direction,content,created_at,source_kind,source_event_id,source_occurred_at,conversation_id,recipient,definition_version,available_at)
                 VALUES (?1,?2,'inbound',?3,?4,?5,?6,?7,?8,?9,?10,?11) ON CONFLICT DO NOTHING",
                params![new_id, agent_id, event.content, now, event.source_kind, event.source_event_id, event.occurred_at, event.conversation_id, event.recipient, version, available_at.max(now)],
            )? == 1;
            if inserted {
                tx.execute(
                    "UPDATE state_agent_messages SET superseded_by=?1
                     WHERE agent_id=?2 AND source_kind=?3 AND source_event_id=?4
                       AND delivered_at IS NULL AND superseded_by IS NULL",
                    params![new_id, agent_id, event.source_kind, supersedes_event_id],
                )?;
                tx.execute("UPDATE config_agents SET next_run_at=CASE WHEN next_run_at IS NULL OR next_run_at>?1 THEN ?1 ELSE next_run_at END,updated_at=?2 WHERE id=?3", params![available_at.max(now), now, agent_id])?;
            }
            let id = if inserted { new_id.clone() } else {
                tx.query_row("SELECT id FROM state_agent_messages WHERE agent_id=?1 AND source_kind=?2 AND source_event_id=?3", params![agent_id,event.source_kind,event.source_event_id], |row| row.get(0))?
            };
            Ok((id, inserted))
        })?;
        Ok(result)
    }

    /// Advance one calendar fire and either queue one mailbox item or record why it was skipped.
    pub fn fire_due_schedule(
        &self,
        agent: &AgentRow,
        now: i64,
    ) -> Result<Option<String>, AgentError> {
        let trigger = AgentTriggerSpec::from_value(&agent.trigger).map_err(AgentError::Invalid)?;
        let Some(schedule) = trigger.schedule else {
            return Ok(None);
        };
        let Some(due) = agent.schedule_next_at.filter(|due| *due <= now) else {
            return Ok(None);
        };
        let next = schedule.next_fire_after(due).map_err(AgentError::Invalid)?;
        let quiet = schedule.is_quiet_at(due).map_err(AgentError::Invalid)?;
        let stale = now.saturating_sub(due) > schedule.catchup_secs as i64;
        let id = Uuid::new_v4().to_string();
        let content = serde_json::json!({
            "kind": "schedule_fire", "scheduled_at": due,
            "definition_version": agent.definition_version,
            "channel": "web", "recipient": "controller",
            "conversation_id": schedule.target_conversation_id.clone(),
        })
        .to_string();
        let result = self.db.transaction(|tx| {
            let current: Option<i64> = tx.query_row("SELECT schedule_next_at FROM config_agents WHERE id=?1 AND enabled=1 AND paused=0", [&agent.id], |row| row.get(0)).optional()?.flatten();
            if current != Some(due) { return Ok(None); }
            let running: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM state_agent_runs WHERE agent_id=?1 AND status='running')", [&agent.id], |row| row.get(0))?;
            let pending: i64 = tx.query_row("SELECT COUNT(*) FROM state_agent_messages WHERE agent_id=?1 AND source_kind='schedule' AND delivered_at IS NULL AND superseded_by IS NULL", [&agent.id], |row| row.get(0))?;
            let overlap = match schedule.overlap {
                AgentOverlapPolicy::Skip => running || pending > 0,
                AgentOverlapPolicy::BufferOne => pending > 0,
            };
            let reason = if stale { Some("catchup_expired") } else if quiet { Some("quiet_hours") } else if overlap { Some("overlap") } else { None };
            let queued = if reason.is_none() {
                tx.execute("INSERT INTO state_agent_messages(id,agent_id,direction,content,created_at,source_kind,source_event_id,source_occurred_at,conversation_id,recipient,definition_version,available_at) VALUES (?1,?2,'inbound',?3,?4,'schedule',?5,?6,?7,'controller',?8,?4)",
                    params![id,agent.id,content,now,due.to_string(),due,schedule.target_conversation_id,agent.definition_version])?;
                Some(id.clone())
            } else { None };
            tx.execute("INSERT INTO state_agent_schedule_fires(agent_id,due_at,status,reason,mailbox_id,recorded_at) VALUES (?1,?2,?3,?4,?5,?6)",
                params![agent.id,due,if queued.is_some() { "queued" } else { "skipped" },reason,queued,now])?;
            tx.execute("UPDATE config_agents SET schedule_next_at=?2,next_run_at=CASE WHEN ?3=1 THEN ?4 ELSE next_run_at END,updated_at=?4 WHERE id=?1 AND schedule_next_at=?5",
                params![agent.id,next,queued.is_some() as i64,now,due])?;
            Ok(queued)
        })?;
        Ok(result)
    }

    /// Read recent calendar-fire receipts for the Controller's run history.
    pub fn schedule_fires(
        &self,
        agent_id: &str,
        limit: u32,
    ) -> Result<Vec<AgentScheduleFireRow>, AgentError> {
        self.db.with_conn(|connection| {
            let mut stmt = connection.prepare("SELECT agent_id,due_at,status,reason,mailbox_id,recorded_at FROM state_agent_schedule_fires WHERE agent_id=?1 ORDER BY due_at DESC LIMIT ?2")?;
            Ok(stmt.query_map(params![agent_id,limit.min(200)], |row| Ok(AgentScheduleFireRow {
                agent_id: row.get(0)?, due_at: row.get(1)?, status: row.get(2)?, reason: row.get(3)?,
                mailbox_id: row.get(4)?, recorded_at: row.get(5)?,
            }))?.collect::<Result<Vec<_>,_>>()?)
        }).map_err(Into::into)
    }

    pub fn clear_event_only_due(&self, agent_id: &str) -> Result<(), AgentError> {
        self.db
            .with_conn(|c| {
                c.execute(
                    "UPDATE config_agents SET next_run_at=NULL, updated_at=strftime('%s','now') WHERE id=?1",
                    [agent_id],
                )?;
                Ok(())
            })
            .map_err(Into::into)
    }

    pub fn deliver(&self, ids: &[String], run_id: &str, now: i64) -> Result<(), AgentError> {
        self.db.with_conn(|c|{for id in ids{c.execute("UPDATE state_agent_messages SET delivered_at=?1,result_run_id=?2 WHERE id=?3",params![now,run_id,id])?;}Ok(())}).map_err(Into::into)
    }

    pub fn runs(&self, agent_id: &str, limit: u32) -> Result<Vec<AgentRunRow>, AgentError> {
        let mut runs = self.db.with_conn(|c| {
            let mut statement = c.prepare("SELECT id,agent_id,status,started_at,finished_at,tokens_used,checkpoint_json,output_text,error,mailbox_id,definition_version,outcome_kind FROM state_agent_runs WHERE agent_id=?1 ORDER BY started_at DESC LIMIT ?2")?;
            let rows = statement.query_map(params![agent_id,limit],map_run)?;
            Ok(rows.collect::<Result<Vec<_>, _>>()?)
        })?;
        for run in &mut runs {
            run.completion = self.completion_report(&run.id)?;
        }
        Ok(runs)
    }

    /// Check whether a persisted run belongs to a specific agent definition.
    pub fn run_belongs_to_agent(&self, agent_id: &str, run_id: &str) -> Result<bool, AgentError> {
        self.db
            .with_conn(|connection| {
                Ok(connection.query_row(
                    "SELECT EXISTS(SELECT 1 FROM state_agent_runs WHERE agent_id=?1 AND id=?2)",
                    params![agent_id, run_id],
                    |row| row.get(0),
                )?)
            })
            .map_err(Into::into)
    }
}

/// Returns whether this agent runs only when an external event enqueues it.
pub fn trigger_is_event_only(trigger: &serde_json::Value) -> bool {
    trigger
        .get("event_only")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
        || trigger
            .get("schedule")
            .is_some_and(|value| !value.is_null())
}

fn map_agent(r: &rusqlite::Row<'_>) -> rusqlite::Result<AgentRow> {
    let tools: String = r.get(5)?;
    let trust: String = r.get(6)?;
    Ok(AgentRow {
        id: r.get(0)?,
        name: r.get(1)?,
        role_prompt: r.get(2)?,
        model: r.get(3)?,
        backend_purpose: r.get(4)?,
        tools: serde_json::from_str(&tools).unwrap_or_default(),
        trust_policy: serde_json::from_str(&trust).unwrap_or_else(|_| serde_json::json!({})),
        interval_secs: r.get(7)?,
        token_budget: r.get(8)?,
        max_runtime_secs: r.get(9)?,
        concurrency_limit: r.get(10)?,
        enabled: r.get::<_, i64>(11)? != 0,
        paused: r.get::<_, i64>(12)? != 0,
        next_run_at: r.get(13)?,
        last_run_at: r.get(14)?,
        last_run_status: r.get(15)?,
        last_error: r.get(16)?,
        created_at: r.get(17)?,
        updated_at: r.get(18)?,
        trigger: serde_json::from_str::<serde_json::Value>(&r.get::<_, String>(19)?)
            .unwrap_or_else(|_| serde_json::json!({})),
        reply_mode: r.get(20)?,
        definition_version: r.get(21)?,
        schedule_next_at: r.get(22)?,
    })
}
fn map_run(r: &rusqlite::Row<'_>) -> rusqlite::Result<AgentRunRow> {
    let cp: String = r.get(6)?;
    Ok(AgentRunRow {
        id: r.get(0)?,
        agent_id: r.get(1)?,
        status: r.get(2)?,
        started_at: r.get(3)?,
        finished_at: r.get(4)?,
        tokens_used: r.get(5)?,
        checkpoint: serde_json::from_str(&cp).unwrap_or_else(|_| serde_json::json!({})),
        output_text: r.get(7)?,
        error: r.get(8)?,
        completion: None,
        mailbox_id: r.get(9)?,
        definition_version: r.get(10)?,
        outcome_kind: r.get(11)?,
    })
}
fn map_message(r: &rusqlite::Row<'_>) -> rusqlite::Result<AgentMessageRow> {
    Ok(AgentMessageRow {
        id: r.get(0)?,
        agent_id: r.get(1)?,
        parent_agent_id: r.get(2)?,
        direction: r.get(3)?,
        content: r.get(4)?,
        created_at: r.get(5)?,
        delivered_at: r.get(6)?,
        result_run_id: r.get(7)?,
        source_kind: r.get(8)?,
        source_event_id: r.get(9)?,
        source_occurred_at: r.get(10)?,
        conversation_id: r.get(11)?,
        recipient: r.get(12)?,
        definition_version: r.get(13)?,
        available_at: r.get(14)?,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit::AuditStore;
    use crate::{DbConfig, MigrationRunner};
    #[test]
    fn agent_round_trip() {
        let db = Database::open(&DbConfig::in_memory_unencrypted()).unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        let s = AgentStore::new(&db);
        let a = s
            .upsert(
                &AgentUpsert {
                    id: None,
                    name: "watcher".into(),
                    role_prompt: "Watch mailbox".into(),
                    model: None,
                    backend_purpose: "standard".into(),
                    tools: vec!["search".into()],
                    trust_policy: serde_json::json!({"trust":"Controller"}),
                    interval_secs: 60,
                    token_budget: 100,
                    max_runtime_secs: 30,
                    concurrency_limit: 1,
                    enabled: true,
                    trigger: serde_json::json!({}),
                    reply_mode: "draft".into(),
                },
                1,
            )
            .unwrap();
        let m = s.enqueue(&a.id, None, "hello", 2).unwrap();
        assert_eq!(s.pending_messages(&a.id, 10).unwrap()[0].id, m);
        let run = s
            .insert_run(&a.id, 3, &serde_json::json!({"cursor":m}))
            .unwrap();
        s.deliver(&[m], &run, 4).unwrap();
        s.finish(
            &a.id,
            &run,
            "success",
            5,
            Some(65),
            Some(3),
            Some("done"),
            None,
            &serde_json::json!({"cursor":"done"}),
        )
        .unwrap();
        assert_eq!(s.runs(&a.id, 10).unwrap()[0].status, "success");
    }

    #[test]
    fn source_identity_and_terminal_ack_are_atomic() {
        let db = Database::open(&DbConfig::in_memory_unencrypted()).unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        let store = AgentStore::new(&db);
        let agent = store
            .upsert(
                &AgentUpsert {
                    id: Some("camper".into()),
                    name: "camper".into(),
                    role_prompt: "Draft".into(),
                    model: None,
                    backend_purpose: "standard".into(),
                    tools: vec!["search".into()],
                    trust_policy: serde_json::json!({}),
                    interval_secs: 60,
                    token_budget: 100,
                    max_runtime_secs: 30,
                    concurrency_limit: 1,
                    enabled: true,
                    trigger: serde_json::json!({"event_only":true,"channel":"whatsapp"}),
                    reply_mode: "draft".into(),
                },
                10,
            )
            .unwrap();
        let event = AgentSourceEvent {
            source_kind: "transport:whatsapp",
            source_event_id: "upstream-1",
            occurred_at: 11,
            conversation_id: "chat",
            recipient: "group@g.us",
            content: "{\"text\":\"camper\"}",
        };
        let (message, inserted) = store.enqueue_triggered_event("camper", &event, 12).unwrap();
        assert!(inserted);
        let (duplicate, inserted) = store.enqueue_triggered_event("camper", &event, 13).unwrap();
        assert!(!inserted);
        assert_eq!(message, duplicate);
        let run = store
            .insert_run_for_mailbox(
                "camper",
                Some(&message),
                Some(agent.definition_version),
                14,
                &serde_json::json!({}),
            )
            .unwrap();
        store
            .complete_mailbox_run(&AgentRunCompletion {
                agent_id: "camper",
                run_id: &run,
                status: "success",
                outcome_kind: Some("draft_ready"),
                now: 15,
                next_run_at: None,
                tokens: Some(5),
                output: Some("draft"),
                error: None,
                checkpoint: &serde_json::json!({}),
                mailbox_ids: &[message.clone()],
            })
            .unwrap();
        assert!(
            store
                .pending_messages_at("camper", 10, 16)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            store.runs("camper", 10).unwrap()[0].outcome_kind.as_deref(),
            Some("draft_ready")
        );
        assert!(
            store
                .complete_mailbox_run(&AgentRunCompletion {
                    agent_id: "camper",
                    run_id: &run,
                    status: "success",
                    outcome_kind: Some("draft_ready"),
                    now: 16,
                    next_run_at: None,
                    tokens: None,
                    output: None,
                    error: None,
                    checkpoint: &serde_json::json!({}),
                    mailbox_ids: &[message]
                })
                .is_err()
        );
    }

    #[test]
    fn mailbox_definition_version_survives_later_agent_edits() {
        let db = Database::open(&DbConfig::in_memory_unencrypted()).unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        let store = AgentStore::new(&db);
        let mut input = AgentUpsert {
            id: Some("versioned".into()),
            name: "versioned".into(),
            role_prompt: "Original prompt".into(),
            model: None,
            backend_purpose: "standard".into(),
            tools: Vec::new(),
            trust_policy: serde_json::json!({}),
            interval_secs: 60,
            token_budget: 100,
            max_runtime_secs: 30,
            concurrency_limit: 1,
            enabled: true,
            trigger: serde_json::json!({"event_only":true,"channel":"whatsapp"}),
            reply_mode: "draft".into(),
        };
        let first = store.upsert(&input, 10).unwrap();
        let (message, _) = store
            .enqueue_triggered_event(
                "versioned",
                &AgentSourceEvent {
                    source_kind: "transport:whatsapp",
                    source_event_id: "msg-1",
                    occurred_at: 10,
                    conversation_id: "chat",
                    recipient: "group@g.us",
                    content: "{}",
                },
                10,
            )
            .unwrap();
        input.role_prompt = "Updated prompt".into();
        let second = store.upsert(&input, 11).unwrap();
        assert_eq!(second.definition_version, first.definition_version + 1);
        assert_eq!(
            store.pending_messages_at("versioned", 1, 12).unwrap()[0].definition_version,
            Some(first.definition_version)
        );
        assert_eq!(
            store
                .definition_snapshot("versioned", first.definition_version)
                .unwrap()
                .unwrap()["role_prompt"],
            "Original prompt"
        );
        assert_eq!(
            store
                .definition_snapshot("versioned", second.definition_version)
                .unwrap()
                .unwrap()["role_prompt"],
            "Updated prompt"
        );
        assert!(!message.is_empty());
    }

    #[test]
    fn calendar_fire_records_one_due_mailbox_item() {
        let db = Database::open(&DbConfig::in_memory_unencrypted()).unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        let store = AgentStore::new(&db);
        let now = 1_790_000_000;
        let agent = store.upsert(&AgentUpsert {
            id: Some("morning".into()), name: "morning".into(), role_prompt: "Report".into(),
            model: None, backend_purpose: "standard".into(), tools: Vec::new(),
            trust_policy: serde_json::json!({}), interval_secs: 60, token_budget: 100,
            max_runtime_secs: 30, concurrency_limit: 1, enabled: true,
            trigger: serde_json::json!({"schedule":{"cron":"0 8 * * *","timezone":"UTC","overlap":"skip","catchup_secs":3600}}),
            reply_mode: "draft".into(),
        }, now).unwrap();
        let due = agent.schedule_next_at.unwrap();
        assert!(store.fire_due_schedule(&agent, due).unwrap().is_some());
        assert!(store.fire_due_schedule(&agent, due).unwrap().is_none());
        let pending = store.pending_messages_at("morning", 10, due).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].source_kind, "schedule");
        assert_eq!(
            store.schedule_fires("morning", 10).unwrap()[0].status,
            "queued"
        );
    }

    #[test]
    fn event_only_agent_waits_for_a_triggered_message() {
        let db = Database::open(&DbConfig::in_memory_unencrypted()).unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        let s = AgentStore::new(&db);
        let a = s
            .upsert(
                &AgentUpsert {
                    id: None,
                    name: "camper".into(),
                    role_prompt: "Draft replies".into(),
                    model: None,
                    backend_purpose: "standard".into(),
                    tools: Vec::new(),
                    trust_policy: serde_json::json!({}),
                    interval_secs: 300,
                    token_budget: 100,
                    max_runtime_secs: 30,
                    concurrency_limit: 1,
                    enabled: true,
                    trigger: serde_json::json!({"channel":"whatsapp","event_only":true}),
                    reply_mode: "draft".into(),
                },
                1,
            )
            .unwrap();
        assert_eq!(a.next_run_at, None);
        s.enqueue_triggered(&a.id, "new WhatsApp message", 2)
            .unwrap();
        assert_eq!(s.get(&a.id).unwrap().unwrap().next_run_at, Some(2));
    }

    #[test]
    fn rejects_unknown_reply_mode() {
        let db = Database::open(&DbConfig::in_memory_unencrypted()).unwrap();
        let error = AgentStore::new(&db)
            .upsert(
                &AgentUpsert {
                    id: None,
                    name: "invalid-mode".into(),
                    role_prompt: "Draft replies".into(),
                    model: None,
                    backend_purpose: "standard".into(),
                    tools: Vec::new(),
                    trust_policy: serde_json::json!({}),
                    interval_secs: 300,
                    token_budget: 100,
                    max_runtime_secs: 30,
                    concurrency_limit: 1,
                    enabled: true,
                    trigger: serde_json::json!({}),
                    reply_mode: "unattended".into(),
                },
                1,
            )
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("reply_mode must be draft or automatic")
        );
    }

    #[test]
    fn agent_run_snapshots_contract_and_requires_all_evidence() {
        let db = Database::open(&DbConfig::in_memory_unencrypted()).unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        let store = AgentStore::new(&db);
        let agent = store
            .upsert(
                &AgentUpsert {
                    id: None,
                    name: "contract-agent".into(),
                    role_prompt: "Complete a checked task".into(),
                    model: None,
                    backend_purpose: "standard".into(),
                    tools: Vec::new(),
                    trust_policy: serde_json::json!({}),
                    interval_secs: 60,
                    token_budget: 200,
                    max_runtime_secs: 60,
                    concurrency_limit: 1,
                    enabled: true,
                    trigger: serde_json::json!({}),
                    reply_mode: "draft".into(),
                },
                10,
            )
            .unwrap();
        let draft = RunCompletionContractDraft {
            acceptance_criteria: vec![crate::runs::AcceptanceCriterion {
                criterion_id: "verified-output".into(),
                description: "Output matches the requested format".into(),
                required: true,
                verifier: None,
            }],
            required_artifacts: vec![crate::runs::RequiredRunArtifact {
                artifact_id: "report".into(),
                description: "A reviewable report".into(),
            }],
            delivery_required: true,
        };
        store
            .set_completion_contract(&agent.id, Some(&draft), 11)
            .unwrap();
        let run_id = store
            .insert_run(&agent.id, 12, &serde_json::json!({}))
            .unwrap();
        store.set_completion_contract(&agent.id, None, 13).unwrap();

        let pending = store.completion_report(&run_id).unwrap().unwrap();
        assert_eq!(pending.status, crate::runs::RunCompletionStatus::Incomplete);
        assert_eq!(
            pending.contract.acceptance_criteria,
            draft.acceptance_criteria
        );

        assert!(
            store
                .record_completion_verification(
                    &run_id,
                    &CriterionVerification {
                        criterion_id: "verified-output".into(),
                        status: VerificationStatus::Passed,
                        evidence_refs: vec!["trace:12".into()],
                        detail: None,
                        verified_at: 14,
                    },
                )
                .is_err()
        );
        let criterion_audit = AuditStore::new(&db)
            .insert(
                "controller",
                "agent_run_completion_verification",
                &format!("{run_id}/verified-output"),
                None,
                Some(
                    &serde_json::json!({"status":"passed","submitted_evidence_refs":["trace:12"]}),
                ),
            )
            .unwrap();

        store
            .record_completion_verification(
                &run_id,
                &CriterionVerification {
                    criterion_id: "verified-output".into(),
                    status: VerificationStatus::Passed,
                    evidence_refs: vec![
                        "trace:12".into(),
                        format!("attestation:{criterion_audit}"),
                    ],
                    detail: None,
                    verified_at: 14,
                },
            )
            .unwrap();
        assert_eq!(
            store.completion_report(&run_id).unwrap().unwrap().status,
            crate::runs::RunCompletionStatus::Incomplete
        );
        assert!(
            store
                .record_artifact_verification(
                    &run_id,
                    &ArtifactVerification {
                        artifact_id: "report".into(),
                        present: true,
                        evidence_ref: Some("attachment:report-id".into()),
                        detail: None,
                        checked_at: 15,
                    },
                )
                .is_err()
        );
        assert_eq!(
            store.completion_report(&run_id).unwrap().unwrap().status,
            crate::runs::RunCompletionStatus::Incomplete
        );
        assert!(
            store
                .confirm_completion_delivery(&run_id, "receipt:outbox-1", 16)
                .is_err()
        );
        let delivery_audit = AuditStore::new(&db)
            .insert(
                "controller",
                "agent_run_completion_delivery",
                &run_id,
                None,
                Some(&serde_json::json!({"status":"confirmed","submitted_evidence_ref":"receipt:outbox-1"})),
            )
            .unwrap();
        store
            .confirm_completion_delivery(&run_id, &format!("attestation:{delivery_audit}"), 16)
            .unwrap();
        assert_eq!(
            store.completion_report(&run_id).unwrap().unwrap().status,
            crate::runs::RunCompletionStatus::Incomplete
        );
        store
            .finish(
                &agent.id,
                &run_id,
                "success",
                17,
                None,
                Some(10),
                Some("verified output"),
                None,
                &serde_json::json!({}),
            )
            .unwrap();
        assert_eq!(
            store.completion_report(&run_id).unwrap().unwrap().status,
            crate::runs::RunCompletionStatus::Incomplete,
            "the required output artifact remains unverified"
        );
        store
            .record_artifact_verification(
                &run_id,
                &ArtifactVerification {
                    artifact_id: "report".into(),
                    present: true,
                    evidence_ref: Some(format!("agent-run:{run_id}/output")),
                    detail: None,
                    checked_at: 18,
                },
            )
            .unwrap();
        assert_eq!(
            store.completion_report(&run_id).unwrap().unwrap().status,
            crate::runs::RunCompletionStatus::VerifiedComplete
        );
        assert_eq!(
            store.runs(&agent.id, 10).unwrap()[0]
                .completion
                .as_ref()
                .unwrap()
                .status,
            crate::runs::RunCompletionStatus::VerifiedComplete
        );
    }

    #[test]
    fn agent_output_verifier_reads_committed_output_and_rejects_manual_pass() {
        let db = Database::open(&DbConfig::in_memory_unencrypted()).unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        let store = AgentStore::new(&db);
        let agent = store
            .upsert(
                &AgentUpsert {
                    id: None,
                    name: "structured-agent".into(),
                    role_prompt: "Return structured output".into(),
                    model: None,
                    backend_purpose: "standard".into(),
                    tools: Vec::new(),
                    trust_policy: serde_json::json!({}),
                    interval_secs: 60,
                    token_budget: 200,
                    max_runtime_secs: 60,
                    concurrency_limit: 1,
                    enabled: true,
                    trigger: serde_json::json!({}),
                    reply_mode: "draft".into(),
                },
                10,
            )
            .unwrap();
        let contract = RunCompletionContractDraft {
            acceptance_criteria: vec![crate::runs::AcceptanceCriterion {
                criterion_id: "format".into(),
                description: "Output has the requested format".into(),
                required: true,
                verifier: Some(crate::runs::RunStepVerifier {
                    step_id: "agent:output".into(),
                    json_pointer: "/json/format".into(),
                    expected: serde_json::json!("ok"),
                }),
            }],
            required_artifacts: Vec::new(),
            delivery_required: false,
        };
        store
            .set_completion_contract(&agent.id, Some(&contract), 11)
            .unwrap();
        let passing = store
            .insert_run(&agent.id, 12, &serde_json::json!({}))
            .unwrap();
        assert!(matches!(
            store.record_completion_verification(
                &passing,
                &CriterionVerification {
                    criterion_id: "format".into(),
                    status: VerificationStatus::Passed,
                    evidence_refs: vec!["fabricated:pass".into()],
                    detail: None,
                    verified_at: 13,
                },
            ),
            Err(AgentError::Invalid(_))
        ));
        assert_eq!(
            store.completion_report(&passing).unwrap().unwrap().status,
            crate::runs::RunCompletionStatus::Incomplete
        );
        store
            .finish(
                &agent.id,
                &passing,
                "success",
                14,
                None,
                None,
                Some("{\"format\":\"ok\"}"),
                None,
                &serde_json::json!({}),
            )
            .unwrap();
        assert_eq!(
            store.completion_report(&passing).unwrap().unwrap().status,
            crate::runs::RunCompletionStatus::VerifiedComplete
        );
        let failing = store
            .insert_run(&agent.id, 15, &serde_json::json!({}))
            .unwrap();
        store
            .finish(
                &agent.id,
                &failing,
                "success",
                16,
                None,
                None,
                Some("{\"format\":\"wrong\"}"),
                None,
                &serde_json::json!({}),
            )
            .unwrap();
        assert_eq!(
            store.completion_report(&failing).unwrap().unwrap().status,
            crate::runs::RunCompletionStatus::Blocked
        );
    }

    #[test]
    fn thousand_event_burst_is_attributed_batched_and_corrections_supersede() {
        let directory = tempfile::tempdir().unwrap();
        let config = DbConfig {
            path: directory.path().join("burst.db"),
            key: None,
        };
        let db = Database::open(&config).unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        let store = AgentStore::new(&db);
        let agent = store.upsert(&AgentUpsert {
            id: Some("burst".into()), name: "burst".into(), role_prompt: "summarize".into(),
            model: None, backend_purpose: "standard".into(), tools: Vec::new(), trust_policy: serde_json::json!({}),
            interval_secs: 60, token_budget: 1000, max_runtime_secs: 60, concurrency_limit: 1,
            enabled: true, trigger: serde_json::json!({"event_only":true,"debounce_secs":60,"max_batch_size":25}),
            reply_mode: "draft".into(),
        }, 1).unwrap();
        for index in 0..1000 {
            let id = format!("event-{index}");
            let content =
                serde_json::json!({"source_event_id":id,"text":format!("message {index}")})
                    .to_string();
            store
                .enqueue_triggered_event_at(
                    &agent.id,
                    &AgentSourceEvent {
                        source_kind: "transport:test",
                        source_event_id: &id,
                        occurred_at: index,
                        conversation_id: "conversation",
                        recipient: "peer",
                        content: &content,
                    },
                    1100,
                    1000,
                )
                .unwrap();
        }
        assert!(
            store
                .pending_messages_at(&agent.id, 100, 1099)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            store
                .pending_messages_for_scope(&agent.id, "conversation", "peer", Some(1), 25)
                .unwrap()
                .len(),
            25
        );
        let count: i64 = db.with_conn(|connection| Ok(connection.query_row(
            "SELECT COUNT(*) FROM state_agent_messages WHERE agent_id='burst' AND source_kind='transport:test'", [], |row| row.get(0)
        )?)).unwrap();
        assert_eq!(count, 1000, "every original event remains attributable");
        drop(store);
        drop(db);
        let db = Database::open(&config).unwrap();
        let store = AgentStore::new(&db);
        assert_eq!(
            store
                .pending_messages_at(&agent.id, 100, 1099)
                .unwrap()
                .len(),
            0
        );
        assert_eq!(
            store
                .pending_messages_at(&agent.id, 1001, 1100)
                .unwrap()
                .len(),
            1000
        );
        let corrected = r#"{"source_event_id":"correction-999","text":"corrected message 999"}"#;
        store
            .enqueue_triggered_correction(
                &agent.id,
                &AgentSourceEvent {
                    source_kind: "transport:test",
                    source_event_id: "correction-999",
                    occurred_at: 1101,
                    conversation_id: "conversation",
                    recipient: "peer",
                    content: corrected,
                },
                "event-999",
                1101,
                1001,
            )
            .unwrap();
        assert!(
            store
                .pending_messages_at(&agent.id, 100, 1100)
                .unwrap()
                .iter()
                .all(|row| row.source_event_id.as_deref() != Some("event-999"))
        );
        assert!(
            store
                .pending_messages_at(&agent.id, 1001, 1200)
                .unwrap()
                .iter()
                .any(|row| row.source_event_id.as_deref() == Some("correction-999"))
        );
    }
}
