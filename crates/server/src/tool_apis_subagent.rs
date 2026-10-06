//! Inference-backed implementation of [`execlaw_core::tool::SubagentApi`].
//!
//! Lives in `server` (not `core`) because it depends on
//! `execlaw_inference_api::InferenceClient` to make the child LLM
//! call. The dispatcher constructs one per turn via
//! `ChainedToolDispatch::with_inference(...)` so subagent calls
//! reach whichever Standard backend the operator's resolver
//! returned (managed-vLLM, OpenArc, etc.) — no fixed URL.
//!
//! In the persistent runner path, each child is linked to its parent run and
//! its spawn/model/join checkpoints are stored for inspection and restart
//! replay. A killed model attempt is reclaimed only after its lease expires.
//!
//! 2026-04-29.

use async_trait::async_trait;
use execlaw_core::Database;
use execlaw_core::events::{EventKind, EventLog, PendingEvent};
use execlaw_core::ids::ConversationId;
use execlaw_core::runs::{RunStepKind, RunStore};
use execlaw_core::tool::{ApiError, SubagentApi, SubagentRequest, SubagentResponse};
use execlaw_inference_api::{ChatMessage, ChatRequest, InferenceClient, ModelId};
use execlaw_runner_local::durable::{DurableRun, StepDecision};
use serde::{Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use uuid::Uuid;

const DEFAULT_MAX_TOKENS: u32 = 1024;
const HARD_CAP_TOKENS: u32 = 4096;
const PARENT_CHILD_TOKEN_BUDGET: u32 = 8192;
const CHILD_TIME_BUDGET_MS: u64 = 300_000;
const PARENT_CHILD_TIME_BUDGET_MS: u64 = 3_600_000;
const CHILD_RETRY_BUDGET: u32 = 0;
const PARENT_CHILD_RETRY_BUDGET: u32 = 64;
const CHILD_EFFECT_BUDGET: u32 = 0;
const PARENT_CHILD_EFFECT_BUDGET: u32 = 0;

fn bounded_max_tokens(requested: Option<u32>) -> u32 {
    requested.unwrap_or(DEFAULT_MAX_TOKENS).min(HARD_CAP_TOKENS)
}

/// System prompt the parent hands to the child. Deliberately
/// minimal: the parent's `task` + `context` is the entire substance
/// of the call.
const SUBAGENT_SYSTEM_PROMPT: &str = "You are a helper subagent invoked by another agent for a focused sub-task. \
Reply with exactly the requested output and nothing else — no preface, no \
disclaimer, no \"as an AI...\" framing. Keep replies tight and on-topic.";

#[derive(Debug, Serialize)]
struct SubagentStartedPayload {
    task_id: String,
    /// Truncated form of `task` so the typing-indicator pill has
    /// something compact to show without exposing 4 KB of prompt.
    task_preview: String,
}

#[derive(Debug, Serialize)]
struct SubagentCompletedPayload {
    task_id: String,
    tokens_used: Option<u32>,
    /// Whether the subagent succeeded; the SPA's pill flips back
    /// to the parent's typing state regardless, but operators
    /// auditing the log appreciate the explicit signal.
    succeeded: bool,
}

/// Inference-backed `SubagentApi`. Holds a cheap-clone
/// `Arc<InferenceClient>` + the model id; the dispatcher constructs
/// one per turn from the same resolver `chats.rs` already uses.
///
/// Each `delegate` call:
///   1. Mints a `task_id`, writes a `SubagentStarted` event
///   2. Makes a non-streaming chat-completion call (system + user)
///   3. Writes `SubagentCompleted` with token usage + status
///   4. Returns the text
///
/// The events flow through the existing WS event bus so the SPA's
/// typing-indicator pill subscribes once and renders the structured
/// detail line for any subagent any tool fires.
pub struct InferenceSubagentApi {
    client: Arc<InferenceClient>,
    model: String,
    db: Database,
    conversation_id: ConversationId,
    parent_run_id: Option<String>,
    artifact_root: Option<std::path::PathBuf>,
    cancel_flag: Option<Arc<AtomicBool>>,
    child_lease_seconds: i64,
}

impl InferenceSubagentApi {
    pub fn new(
        client: Arc<InferenceClient>,
        model: impl Into<String>,
        db: Database,
        conversation_id: ConversationId,
    ) -> Self {
        Self::new_with_parent(client, model, db, conversation_id, None)
    }

    /// Construct a delegate implementation linked to its durable parent run.
    pub fn new_with_parent(
        client: Arc<InferenceClient>,
        model: impl Into<String>,
        db: Database,
        conversation_id: ConversationId,
        parent_run_id: Option<String>,
    ) -> Self {
        let budget_scope = parent_run_id
            .clone()
            .unwrap_or_else(|| "unparented-subagent".to_owned());
        Self {
            client: Arc::new(
                client
                    .as_ref()
                    .clone()
                    .with_workload("child")
                    .with_budget_scope(budget_scope),
            ),
            model: model.into(),
            db,
            conversation_id,
            parent_run_id,
            artifact_root: None,
            cancel_flag: None,
            child_lease_seconds: 150,
        }
    }

    /// Use the managed content-addressed artifact directory for child results.
    pub fn with_artifact_root(mut self, root: impl Into<std::path::PathBuf>) -> Self {
        self.artifact_root = Some(root.into());
        self
    }

    /// Attach parent-turn cancellation to the running child inference.
    pub fn with_cancel_flag(mut self, flag: Arc<AtomicBool>) -> Self {
        self.cancel_flag = Some(flag);
        self
    }

    #[cfg(test)]
    fn with_child_lease_seconds(mut self, seconds: i64) -> Self {
        self.child_lease_seconds = seconds.max(1);
        self
    }

    fn stable_task_id(
        parent: Option<&str>,
        task: &str,
        context: Option<&str>,
        max_tokens: u32,
        dependencies: &[String],
    ) -> String {
        let mut hash = Sha256::new();
        hash.update(parent.unwrap_or("unparented"));
        hash.update([0]);
        hash.update(task.as_bytes());
        hash.update([0]);
        hash.update(context.unwrap_or_default().as_bytes());
        hash.update(max_tokens.to_be_bytes());
        for dependency in dependencies {
            hash.update([0]);
            hash.update(dependency.as_bytes());
        }
        hex::encode(hash.finalize())
    }

    fn open_durable_child(
        &self,
        task_id: &str,
        task: &str,
        context: Option<&str>,
        max_tokens: u32,
        dependencies: &[String],
        time_budget_ms: u64,
    ) -> Result<Option<DurableRun<'_>>, ApiError> {
        let Some(parent_run_id) = self.parent_run_id.as_deref() else {
            return Ok(None);
        };
        let store = RunStore::new(&self.db);
        let parent = store
            .get_run(parent_run_id)
            .map_err(|error| ApiError::Storage(format!("load parent run: {error}")))?
            .ok_or_else(|| {
                ApiError::Storage("durable parent run disappeared before child spawn".into())
            })?;
        if parent.conversation_id != self.conversation_id {
            return Err(ApiError::Storage(
                "child run conversation differs from its parent".into(),
            ));
        }
        let child_id = format!("child:{task_id}");
        let existing_budget = store
            .execution_budget(&child_id)
            .map_err(|error| ApiError::Storage(format!("load prior child budget: {error}")))?;
        let effective_time_budget_ms = existing_budget
            .as_ref()
            .map(|budget| budget.time_limit_ms)
            .unwrap_or(time_budget_ms);
        let worker_id = format!("subagent:{}", Uuid::new_v4());
        let child = DurableRun::open_with_execution_budget(
            &self.db,
            child_id,
            worker_id,
            self.conversation_id.clone(),
            parent.input_event_seq,
            Some(parent_run_id.to_owned()),
            chrono::Utc::now().timestamp(),
            effective_time_budget_ms,
            CHILD_RETRY_BUDGET,
            CHILD_EFFECT_BUDGET,
        )
        .map_err(|error| ApiError::Storage(format!("open durable child run: {error}")))?
        .with_lease_seconds(self.child_lease_seconds);
        let now = chrono::Utc::now().timestamp();
        let legacy_layout = existing_budget
            .as_ref()
            .is_some_and(|budget| budget.time_limit_ms == 120_000)
            || child
                .get_step("join")
                .map_err(|error| ApiError::Storage(format!("inspect child layout: {error}")))?
                .is_some_and(|step| step.ordinal == 1);
        if !legacy_layout {
            child
                .record_input_manifest(
                    &serde_json::json!({
                        "system": SUBAGENT_SYSTEM_PROMPT,
                        "task": task,
                        "context": context,
                        "dependencies": dependencies,
                    }),
                    &serde_json::json!({
                        "model": self.model,
                        "endpoint": self.client.base_url,
                        "engine": format!("{:?}", self.client.engine),
                        "max_tokens": max_tokens,
                    }),
                    &Vec::<String>::new(),
                    &serde_json::json!({
                        "tools": [],
                        "discoverable_tools": [],
                    }),
                    now,
                )
                .map_err(|error| {
                    ApiError::Storage(format!("record child input manifest: {error}"))
                })?;
        }
        let child_already_completed = child
            .is_completed()
            .map_err(|error| ApiError::Storage(format!("inspect child run: {error}")))?;
        if !child_already_completed {
            let spawn_input = serde_json::json!({"task_hash": task_id, "context_hash": context.map(|value| hex::encode(Sha256::digest(value.as_bytes()))), "max_tokens": max_tokens});
            match child
                .begin::<serde_json::Value>(
                    "spawn",
                    0,
                    RunStepKind::ChildRunSpawn,
                    &spawn_input,
                    None,
                    None,
                    now,
                )
                .map_err(|error| ApiError::Storage(format!("begin child spawn step: {error}")))?
            {
                StepDecision::Execute(_) => {
                    child
                        .complete("spawn", &serde_json::json!({"status":"started"}), now)
                        .map_err(|error| {
                            ApiError::Storage(format!("checkpoint child spawn: {error}"))
                        })?;
                }
                StepDecision::Replay(_) => {}
                other => {
                    return Err(ApiError::Storage(format!(
                        "child spawn step is not executable: {other:?}"
                    )));
                }
            }
            child
                .advance(0, now)
                .map_err(|error| ApiError::Storage(format!("advance child spawn: {error}")))?;
        }
        let task_contract = serde_json::json!({
            "version": if legacy_layout { 2 } else { 3 },
            "task": task,
            "context": context,
            "dependencies": dependencies,
            "result": {"type": "text", "max_bytes": 524_288},
            "budget": {
                "tokens": max_tokens,
                "time_ms": effective_time_budget_ms,
                "retries": CHILD_RETRY_BUDGET,
                "effects": CHILD_EFFECT_BUDGET,
            },
        });
        store
            .reserve_child_task_with_budgets(
                parent_run_id,
                &format!("child:{task_id}"),
                &task_contract,
                task_id,
                &serde_json::json!({"tool_capabilities": [], "delegation": false}),
                execlaw_core::runs::ChildExecutionBudget {
                    tokens: max_tokens,
                    time_ms: effective_time_budget_ms,
                    retries: CHILD_RETRY_BUDGET,
                    effects: CHILD_EFFECT_BUDGET,
                },
                execlaw_core::runs::ChildExecutionBudget {
                    tokens: PARENT_CHILD_TOKEN_BUDGET,
                    time_ms: PARENT_CHILD_TIME_BUDGET_MS,
                    retries: PARENT_CHILD_RETRY_BUDGET,
                    effects: PARENT_CHILD_EFFECT_BUDGET,
                },
                dependencies,
                now,
            )
            .map_err(|error| ApiError::Storage(format!("reserve durable child task: {error}")))?;
        Ok(Some(child))
    }

    fn truncate_for_preview(s: &str) -> String {
        const MAX: usize = 80;
        let trimmed = s.trim();
        if trimmed.chars().count() <= MAX {
            return trimmed.to_owned();
        }
        let mut buf: String = trimmed.chars().take(MAX - 1).collect();
        buf.push('…');
        buf
    }

    fn emit_event<P: Serialize>(&self, kind: EventKind, payload: &P) -> Result<(), ApiError> {
        let log = EventLog::new(&self.db);
        let base = log
            .last_seq(&self.conversation_id)
            .map_err(|e| ApiError::Storage(format!("last_seq: {e}")))?;
        let bytes =
            rmp_serde::to_vec(payload).map_err(|e| ApiError::Storage(format!("encode: {e}")))?;
        let pending = PendingEvent {
            kind,
            payload: bytes,
            actor: Some("system".into()),
        };
        log.commit_turn(&self.conversation_id, base, vec![pending])
            .map_err(|e| ApiError::Storage(format!("commit: {e}")))?;
        Ok(())
    }
}

#[async_trait]
impl SubagentApi for InferenceSubagentApi {
    async fn delegate(&self, req: &SubagentRequest) -> Result<SubagentResponse, ApiError> {
        if req.task.trim().is_empty() {
            return Err(ApiError::Validation("task is empty".into()));
        }
        if req.task.len() > 64 * 1024
            || req
                .context
                .as_ref()
                .is_some_and(|context| context.len() > 64 * 1024)
        {
            return Err(ApiError::Validation(
                "child task and context must each be at most 65536 bytes".into(),
            ));
        }
        if req.dependencies.len() > 16 {
            return Err(ApiError::Validation(
                "a child task may depend on at most 16 completed sibling tasks".into(),
            ));
        }
        let mut dependencies = req.dependencies.clone();
        dependencies.sort();
        dependencies.dedup();
        if dependencies.len() != req.dependencies.len()
            || dependencies
                .iter()
                .any(|id| id.is_empty() || id.len() > 128)
        {
            return Err(ApiError::Validation(
                "child dependencies must be unique non-empty task ids".into(),
            ));
        }
        let dependency_runs = if dependencies.is_empty() {
            Vec::new()
        } else {
            let parent_run_id = self.parent_run_id.as_deref().ok_or_else(|| {
                ApiError::Validation("dependencies require a durable parent run".into())
            })?;
            let tasks = RunStore::new(&self.db)
                .list_child_tasks(parent_run_id)
                .map_err(|error| ApiError::Storage(format!("load child dependencies: {error}")))?;
            let mut resolved = Vec::with_capacity(dependencies.len());
            for dependency in &dependencies {
                let run_id = if dependency.starts_with("child:") {
                    dependency.clone()
                } else {
                    format!("child:{dependency}")
                };
                let task = tasks
                    .iter()
                    .find(|task| task.child_run_id == run_id)
                    .ok_or_else(|| {
                        ApiError::Validation(format!(
                            "dependency '{dependency}' is not a child of this parent run"
                        ))
                    })?;
                if task.status != execlaw_core::runs::RunStatus::Completed {
                    return Err(ApiError::Validation(format!(
                        "dependency '{}' is not completed (status: {:?})",
                        task.child_run_id, task.status
                    )));
                }
                resolved.push(task.child_run_id.clone());
            }
            resolved
        };
        let task_id = if self.parent_run_id.is_some() {
            Self::stable_task_id(
                self.parent_run_id.as_deref(),
                &req.task,
                req.context.as_deref(),
                bounded_max_tokens(req.max_tokens),
                &dependencies,
            )
        } else {
            Uuid::new_v4().to_string()
        };
        let task_preview = Self::truncate_for_preview(&req.task);
        let max_tokens = bounded_max_tokens(req.max_tokens);
        let child_run = self.open_durable_child(
            &task_id,
            &req.task,
            req.context.as_deref(),
            max_tokens,
            &dependency_runs,
            CHILD_TIME_BUDGET_MS,
        )?;
        let (child_deadline_at_ms, child_time_budget_ms) = if let Some(child) = &child_run {
            let budget = child.execution_budget().map_err(|error| {
                ApiError::Storage(format!("load child execution budget: {error}"))
            })?;
            (budget.deadline_at_ms, budget.time_limit_ms)
        } else {
            (
                chrono::Utc::now()
                    .timestamp_millis()
                    .saturating_add(CHILD_TIME_BUDGET_MS.min(i64::MAX as u64) as i64),
                CHILD_TIME_BUDGET_MS,
            )
        };
        let mut legacy_join = false;
        let mut replayed_model = None;
        if let Some(child) = &child_run {
            if child
                .is_completed()
                .map_err(|error| ApiError::Storage(format!("inspect child completion: {error}")))?
            {
                return child
                    .replay_completed::<SubagentResponse>("join")
                    .map_err(|error| {
                        ApiError::Storage(format!("replay completed child join: {error}"))
                    })?
                    .ok_or_else(|| {
                        ApiError::Storage("completed child run has no join checkpoint".into())
                    });
            }
            legacy_join = child
                .execution_budget()
                .map_err(|error| ApiError::Storage(format!("inspect child budget: {error}")))?
                .time_limit_ms
                == 120_000
                || child
                    .get_step("join")
                    .map_err(|error| ApiError::Storage(format!("inspect child layout: {error}")))?
                    .is_some_and(|step| step.ordinal == 1);
            let step_id = if legacy_join { "join" } else { "model:0" };
            let kind = if legacy_join {
                RunStepKind::ChildRunJoin
            } else {
                RunStepKind::ModelRequest
            };
            let input = serde_json::json!({"task_hash": task_id});
            let decision = claim_child_step::<SubagentResponse>(
                &self.db,
                child,
                step_id,
                1,
                kind,
                &input,
                child_deadline_at_ms,
                self.cancel_flag.as_ref(),
            )
            .await?;
            match decision {
                StepDecision::Replay(response) if legacy_join => {
                    let now = chrono::Utc::now().timestamp();
                    child.advance(1, now).map_err(|error| {
                        ApiError::Storage(format!("advance replayed child: {error}"))
                    })?;
                    child.finish(2, now).map_err(|error| {
                        ApiError::Storage(format!("finish replayed child: {error}"))
                    })?;
                    return Ok(response);
                }
                StepDecision::Replay(response) => {
                    child
                        .advance(1, chrono::Utc::now().timestamp())
                        .map_err(|error| {
                            ApiError::Storage(format!("advance replayed child model: {error}"))
                        })?;
                    replayed_model = Some(response);
                }
                StepDecision::Execute(_) => {}
                other => {
                    return Err(ApiError::Storage(format!(
                        "durable child {step_id} is not executable: {other:?}"
                    )));
                }
            }
        }

        // Best-effort emit — DB hiccups shouldn't kill the subagent
        // call. The inference path is the load-bearing part.
        let mut response = if let Some(replayed) = replayed_model {
            replayed
        } else {
            let attempt_no = if let Some(child) = child_run.as_ref().filter(|_| !legacy_join) {
                Some(
                    child
                        .start_inference_attempt("model:0", chrono::Utc::now().timestamp_millis())
                        .map_err(|error| {
                            ApiError::Storage(format!("start child inference attempt: {error}"))
                        })?,
                )
            } else {
                None
            };
            let active_step = if legacy_join { "join" } else { "model:0" };
            if let Err(e) = self.emit_event(
                EventKind::SubagentStarted,
                &SubagentStartedPayload {
                    task_id: task_id.clone(),
                    task_preview: task_preview.clone(),
                },
            ) {
                tracing::warn!(?e, "subagent started-event emit failed; continuing");
            }

            // Build the child's prompt: system + (optional) context +
            // task. The child sees no parent history; that's the
            // point — context isolation is the whole reason for using
            // a subagent.
            let mut messages: Vec<ChatMessage> = vec![ChatMessage::system(SUBAGENT_SYSTEM_PROMPT)];
            if let Some(ctx) = req.context.as_ref().filter(|s| !s.trim().is_empty()) {
                messages.push(ChatMessage::user(format!("Context:\n{ctx}")));
            }
            messages.push(ChatMessage::user(req.task.clone()));

            let chat_req = ChatRequest {
                model: ModelId(self.model.clone()),
                messages,
                max_tokens: Some(max_tokens),
                temperature: None,
                stream: false,
                tools: None,
                chat_template_kwargs: Some(serde_json::json!({"enable_thinking": false})),
                tool_choice: None,
                response_format: None,
                guided_decoding_backend: None,
            };

            let adapter = execlaw_model_adapter::adapter_for(
                execlaw_model_adapter::ModelFamily::detect(&self.model),
            );
            // Subagent reply is consumed verbatim by the parent turn;
            // markdown hint = no fence stripping (parent may want code
            // blocks if the subagent emitted them).
            let chat = adapter.chat(
                &self.client,
                chat_req,
                execlaw_model_adapter::OutputHint::Markdown,
            );
            tokio::pin!(chat);
            let remaining_time_ms =
                child_deadline_at_ms.saturating_sub(chrono::Utc::now().timestamp_millis());
            if remaining_time_ms <= 0 {
                if let (Some(child), Some(attempt_no)) = (&child_run, attempt_no) {
                    let _ = child.finish_inference_attempt(
                        "model:0",
                        attempt_no,
                        false,
                        Some("time_budget_exhausted"),
                        chrono::Utc::now().timestamp_millis(),
                    );
                    let _ = child.fail(
                        active_step,
                        "time_budget_exhausted",
                        chrono::Utc::now().timestamp(),
                    );
                }
                return Err(ApiError::Storage("child time budget exhausted".into()));
            }
            let result = if let Some(cancel_flag) = self.cancel_flag.clone() {
                tokio::select! {
                    result = tokio::time::timeout(
                        std::time::Duration::from_millis(remaining_time_ms as u64),
                        &mut chat,
                    ) => Some(result),
                    _ = wait_for_cancel(cancel_flag) => None,
                }
            } else {
                Some(
                    tokio::time::timeout(
                        std::time::Duration::from_millis(remaining_time_ms as u64),
                        &mut chat,
                    )
                    .await,
                )
            };
            let Some(result) = result else {
                let now = chrono::Utc::now().timestamp();
                if let Some(child) = &child_run {
                    if let Some(attempt_no) = attempt_no {
                        let _ = child.finish_inference_attempt(
                            "model:0",
                            attempt_no,
                            false,
                            Some("cancelled"),
                            chrono::Utc::now().timestamp_millis(),
                        );
                    }
                    let _ = RunStore::new(&self.db).cancel_run(child.run_id(), now);
                    let _ = RunStore::new(&self.db).settle_child_task_with_usage(
                        self.parent_run_id.as_deref().unwrap_or_default(),
                        &format!("child:{task_id}"),
                        None,
                        Some(
                            child_time_budget_ms.saturating_sub(
                                u64::try_from(
                                    child_deadline_at_ms
                                        .saturating_sub(chrono::Utc::now().timestamp_millis()),
                                )
                                .unwrap_or(0),
                            ),
                        ),
                        Some(0),
                        Some(0),
                        None,
                        now,
                    );
                }
                let _ = self.emit_event(
                    EventKind::SubagentCompleted,
                    &SubagentCompletedPayload {
                        task_id: task_id.clone(),
                        tokens_used: None,
                        succeeded: false,
                    },
                );
                return Err(ApiError::Cancelled);
            };
            let elapsed_ms = child_time_budget_ms.saturating_sub(
                u64::try_from(
                    child_deadline_at_ms.saturating_sub(chrono::Utc::now().timestamp_millis()),
                )
                .unwrap_or(0),
            );
            let outcome = match result {
                Ok(outcome) => outcome,
                Err(_) => {
                    let now = chrono::Utc::now().timestamp();
                    if let Some(child) = &child_run {
                        if let Some(attempt_no) = attempt_no {
                            let _ = child.finish_inference_attempt(
                                "model:0",
                                attempt_no,
                                false,
                                Some("time_budget_exhausted"),
                                chrono::Utc::now().timestamp_millis(),
                            );
                        }
                        let _ = child.fail(active_step, "time_budget_exhausted", now);
                        RunStore::new(&self.db)
                            .settle_child_task_with_usage(
                                self.parent_run_id.as_deref().unwrap_or_default(),
                                &format!("child:{task_id}"),
                                None,
                                Some(elapsed_ms),
                                Some(0),
                                Some(0),
                                None,
                                now,
                            )
                            .map_err(|error| {
                                ApiError::Storage(format!("settle timed-out child budget: {error}"))
                            })?;
                    }
                    let _ = self.emit_event(
                        EventKind::SubagentCompleted,
                        &SubagentCompletedPayload {
                            task_id: task_id.clone(),
                            tokens_used: None,
                            succeeded: false,
                        },
                    );
                    return Err(ApiError::Storage("child time budget exhausted".into()));
                }
            };
            let (text, tokens_used, succeeded) = match outcome {
                Ok(adapted) => {
                    let tokens = adapted.usage.as_ref().map(total_usage_tokens);
                    (adapted.content, tokens, true)
                }
                Err(e) => {
                    if let Some(child) = &child_run {
                        if let Some(attempt_no) = attempt_no {
                            let _ = child.finish_inference_attempt(
                                "model:0",
                                attempt_no,
                                false,
                                Some("request_failed"),
                                chrono::Utc::now().timestamp_millis(),
                            );
                        }
                        let _ = child.fail(
                            active_step,
                            &format!("inference: {e}"),
                            chrono::Utc::now().timestamp(),
                        );
                        let _ = RunStore::new(&self.db).settle_child_task_with_usage(
                            self.parent_run_id.as_deref().unwrap_or_default(),
                            &format!("child:{task_id}"),
                            None,
                            Some(elapsed_ms),
                            Some(0),
                            Some(0),
                            None,
                            chrono::Utc::now().timestamp(),
                        );
                    }
                    let _ = self.emit_event(
                        EventKind::SubagentCompleted,
                        &SubagentCompletedPayload {
                            task_id: task_id.clone(),
                            tokens_used: None,
                            succeeded: false,
                        },
                    );
                    return Err(ApiError::Storage(format!("inference: {e}")));
                }
            };

            if let Err(e) = self.emit_event(
                EventKind::SubagentCompleted,
                &SubagentCompletedPayload {
                    task_id: task_id.clone(),
                    tokens_used,
                    succeeded,
                },
            ) {
                tracing::warn!(?e, "subagent completed-event emit failed; continuing");
            }

            let response = SubagentResponse {
                text,
                task_id,
                tokens_used,
                artifact_id: None,
            };
            if let (Some(child), Some(attempt_no)) = (&child_run, attempt_no) {
                let now = chrono::Utc::now().timestamp();
                child.complete("model:0", &response, now).map_err(|error| {
                    ApiError::Storage(format!("checkpoint child model: {error}"))
                })?;
                child
                    .finish_inference_attempt(
                        "model:0",
                        attempt_no,
                        true,
                        None,
                        chrono::Utc::now().timestamp_millis(),
                    )
                    .map_err(|error| {
                        ApiError::Storage(format!("finish child inference attempt: {error}"))
                    })?;
                child
                    .advance(1, now)
                    .map_err(|error| ApiError::Storage(format!("advance child model: {error}")))?;
            }
            response
        };
        if let Some(child) = &child_run {
            let join_ordinal = if legacy_join { 1 } else { 2 };
            if !legacy_join {
                let decision = claim_child_step::<SubagentResponse>(
                    &self.db,
                    child,
                    "join",
                    join_ordinal,
                    RunStepKind::ChildRunJoin,
                    &serde_json::json!({"task_hash": response.task_id}),
                    child_deadline_at_ms,
                    self.cancel_flag.as_ref(),
                )
                .await?;
                match decision {
                    StepDecision::Replay(saved) => {
                        let now = chrono::Utc::now().timestamp();
                        child.advance(join_ordinal, now).map_err(|error| {
                            ApiError::Storage(format!("advance replayed child join: {error}"))
                        })?;
                        child.finish(join_ordinal + 1, now).map_err(|error| {
                            ApiError::Storage(format!("finish replayed child join: {error}"))
                        })?;
                        return Ok(saved);
                    }
                    StepDecision::Execute(_) => {}
                    other => {
                        return Err(ApiError::Storage(format!(
                            "durable child join is not executable: {other:?}"
                        )));
                    }
                }
            }
            let now = chrono::Utc::now().timestamp();
            if let (Some(parent_run_id), Some(artifact_root)) =
                (&self.parent_run_id, &self.artifact_root)
            {
                let bytes = serde_json::to_vec(&serde_json::json!({"text": response.text, "task_id": response.task_id, "tokens_used": response.tokens_used}))
                    .map_err(|error| ApiError::Storage(format!("encode child result artifact: {error}")))?;
                let created = execlaw_core::attachments::AttachmentStore::new(&self.db)
                    .insert_tool_result_artifact_with_id(
                        artifact_root,
                        &self.conversation_id,
                        parent_run_id,
                        &format!("child-result-{}", response.task_id),
                        &bytes,
                        now,
                    )
                    .map_err(|error| {
                        ApiError::Storage(format!("persist child result artifact: {error}"))
                    })?;
                response.artifact_id = Some(created.attachment_id);
            }
            let elapsed_ms = child_time_budget_ms.saturating_sub(
                u64::try_from(
                    child_deadline_at_ms.saturating_sub(chrono::Utc::now().timestamp_millis()),
                )
                .unwrap_or(0),
            );
            RunStore::new(&self.db)
                .settle_child_task_with_usage(
                    self.parent_run_id.as_deref().unwrap_or_default(),
                    &format!("child:{}", response.task_id),
                    response.tokens_used,
                    Some(elapsed_ms),
                    Some(0),
                    Some(0),
                    response.artifact_id.as_deref(),
                    now,
                )
                .map_err(|error| {
                    ApiError::Storage(format!("settle child token budget: {error}"))
                })?;
            child.complete("join", &response, now).map_err(|error| {
                ApiError::Storage(format!("checkpoint durable child result: {error}"))
            })?;
            child.advance(join_ordinal, now).map_err(|error| {
                ApiError::Storage(format!("advance durable child result: {error}"))
            })?;
            child
                .finish(join_ordinal + 1, now)
                .map_err(|error| ApiError::Storage(format!("finish durable child run: {error}")))?;
        }
        Ok(response)
    }
}

async fn claim_child_step<T: DeserializeOwned>(
    db: &Database,
    child: &DurableRun<'_>,
    step_id: &str,
    ordinal: i64,
    kind: RunStepKind,
    input: &serde_json::Value,
    deadline_at_ms: i64,
    cancel_flag: Option<&Arc<AtomicBool>>,
) -> Result<StepDecision<T>, ApiError> {
    loop {
        if chrono::Utc::now().timestamp_millis() >= deadline_at_ms {
            return Err(ApiError::Storage(
                "child time budget expired while waiting for its durable lease".into(),
            ));
        }
        let decision = child
            .begin::<T>(
                step_id,
                ordinal,
                kind,
                input,
                None,
                None,
                chrono::Utc::now().timestamp(),
            )
            .map_err(|error| {
                ApiError::Storage(format!("begin durable child {step_id}: {error}"))
            })?;
        if !matches!(decision, StepDecision::Busy { .. }) {
            return Ok(decision);
        }
        if cancel_flag.is_some_and(|flag| flag.load(Ordering::SeqCst)) {
            let _ = RunStore::new(db).cancel_run(child.run_id(), chrono::Utc::now().timestamp());
            return Err(ApiError::Cancelled);
        }
        let step = child
            .get_step(step_id)
            .map_err(|error| ApiError::Storage(format!("read child {step_id} lease: {error}")))?
            .ok_or_else(|| ApiError::Storage(format!("child {step_id} lease disappeared")))?;
        let now = chrono::Utc::now().timestamp();
        let remaining_seconds = deadline_at_ms
            .saturating_sub(chrono::Utc::now().timestamp_millis())
            .div_euclid(1_000);
        let wait_seconds = step
            .lease_expires_at
            .unwrap_or(now + 1)
            .saturating_sub(now)
            .clamp(1, 2)
            .min(remaining_seconds.max(1));
        tokio::time::sleep(std::time::Duration::from_secs(wait_seconds as u64)).await;
    }
}

async fn wait_for_cancel(flag: Arc<AtomicBool>) {
    while !flag.load(Ordering::SeqCst) {
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
}

fn total_usage_tokens(usage: &execlaw_inference_api::Usage) -> u32 {
    let measured_components = usage.prompt_tokens.saturating_add(usage.completion_tokens);
    usage.total_tokens.max(measured_components)
}

#[cfg(test)]
mod tests {
    use super::*;
    use execlaw_core::conversation::{
        ConversationKind, ConversationRow, ConversationStore, Modality, Phase,
    };
    use execlaw_core::db::DbConfig;
    use execlaw_core::ids::EventSeq;
    use execlaw_core::migrations::MigrationRunner;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn fresh_db() -> Database {
        let db = Database::open(&DbConfig::in_memory_unencrypted()).unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        db
    }

    fn seed_conv(db: &Database, id: &str) -> ConversationId {
        let cid = ConversationId::from(id);
        ConversationStore::new(db)
            .upsert(&ConversationRow {
                conversation_id: cid.clone(),
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
        cid
    }

    #[test]
    fn child_budget_charges_prompt_and_completion_usage() {
        assert_eq!(
            total_usage_tokens(&execlaw_inference_api::Usage {
                prompt_tokens: 3_000,
                completion_tokens: 1_000,
                total_tokens: 0,
            }),
            4_000
        );
        assert_eq!(
            total_usage_tokens(&execlaw_inference_api::Usage {
                prompt_tokens: 3_000,
                completion_tokens: 1_000,
                total_tokens: 4_250,
            }),
            4_250
        );
    }

    /// End-to-end test: spin up an in-process HTTP mock that
    /// returns a canned chat-completion response, point an
    /// InferenceClient at it, fire `delegate`, verify the response
    /// text comes through and both `SubagentStarted` +
    /// `SubagentCompleted` events landed in the log.
    #[tokio::test]
    async fn delegate_round_trips_against_mock_inference_backend() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 8192];
            let _ = sock.read(&mut buf).await;
            let body = serde_json::json!({
                "id": "cmpl-test",
                "object": "chat.completion",
                "created": 1_700_000_000,
                "model": "test-model",
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": "ok"},
                    "finish_reason": "stop",
                }],
                "usage": {"prompt_tokens": 5, "completion_tokens": 1, "total_tokens": 6},
            })
            .to_string();
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            let _ = sock.write_all(resp.as_bytes()).await;
        });

        let client = Arc::new(InferenceClient::new(format!("http://{addr}/v1")));
        let db = fresh_db();
        let cid = seed_conv(&db, "c1");
        let api = InferenceSubagentApi::new(client, "test-model", db.clone(), cid.clone());

        let resp = api
            .delegate(&SubagentRequest {
                task: "say ok".into(),
                context: None,
                max_tokens: Some(64),
                dependencies: Vec::new(),
            })
            .await
            .unwrap();

        assert_eq!(resp.text, "ok");
        assert_eq!(resp.tokens_used, Some(6));
        assert!(!resp.task_id.is_empty());

        // Verify both lifecycle events committed.
        let events = EventLog::new(&db).replay_since(&cid, EventSeq(0)).unwrap();
        let kinds: Vec<_> = events.iter().map(|e| e.kind).collect();
        assert!(kinds.contains(&EventKind::SubagentStarted));
        assert!(kinds.contains(&EventKind::SubagentCompleted));
    }

    #[tokio::test]
    async fn child_crash_process_holds_inference() {
        let Ok(db_path) = std::env::var("EXECLAW_CHILD_CRASH_DB") else {
            return;
        };
        let endpoint = std::env::var("EXECLAW_CHILD_CRASH_URL").unwrap();
        let parent_run_id = std::env::var("EXECLAW_CHILD_CRASH_PARENT").unwrap();
        let artifact_root = std::env::var("EXECLAW_CHILD_CRASH_ARTIFACTS").unwrap();
        let db = Database::open(&DbConfig {
            path: db_path.into(),
            key: None,
        })
        .unwrap();
        let api = InferenceSubagentApi::new_with_parent(
            Arc::new(InferenceClient::new(endpoint)),
            "test-model",
            db,
            ConversationId::from("child-process-kill"),
            Some(parent_run_id),
        )
        .with_child_lease_seconds(2)
        .with_artifact_root(artifact_root);
        let _ = api
            .delegate(&SubagentRequest {
                task: "produce one durable result".into(),
                context: None,
                max_tokens: Some(64),
                dependencies: Vec::new(),
            })
            .await;
    }

    #[tokio::test]
    async fn killed_child_inference_reclaims_once_and_publishes_one_artifact() {
        use execlaw_core::runs::{NewRun, RunStore};
        use std::process::{Command, Stdio};
        use std::sync::atomic::{AtomicUsize, Ordering};

        let directory = tempfile::tempdir().unwrap();
        let db_path = directory.path().join("child-kill.db");
        let artifact_root = directory.path().join("artifacts");
        let db = Database::open(&DbConfig {
            path: db_path.clone(),
            key: None,
        })
        .unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        let cid = seed_conv(&db, "child-process-kill");
        EventLog::new(&db)
            .commit_turn(
                &cid,
                EventSeq(0),
                vec![
                    PendingEvent::encode(
                        EventKind::UserMsg,
                        &serde_json::json!({"text":"delegate"}),
                        Some("controller".into()),
                    )
                    .unwrap(),
                ],
            )
            .unwrap();
        let parent_run_id = RunStore::new(&db)
            .create_run(&NewRun {
                conversation_id: cid.clone(),
                parent_run_id: None,
                input_event_seq: EventSeq(1),
                started_at: chrono::Utc::now().timestamp(),
                deadline_at: None,
            })
            .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
        let hits = Arc::new(AtomicUsize::new(0));
        let server_hits = hits.clone();
        let server = tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let attempt = server_hits.fetch_add(1, Ordering::SeqCst) + 1;
                let mut buffer = [0_u8; 8_192];
                let _ = socket.read(&mut buffer).await;
                if attempt == 1 {
                    let _ = socket.read(&mut buffer).await;
                    continue;
                }
                let body = serde_json::json!({
                    "id":"child-retry","model":"test-model",
                    "choices":[{"index":0,"message":{"role":"assistant","content":"recovered once"},"finish_reason":"stop"}],
                    "usage":{"prompt_tokens":9,"completion_tokens":3,"total_tokens":12}
                }).to_string();
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len(),
                );
                let _ = socket.write_all(response.as_bytes()).await;
            }
        });
        let mut child = Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("tool_apis_subagent::tests::child_crash_process_holds_inference")
            .env("EXECLAW_CHILD_CRASH_DB", &db_path)
            .env("EXECLAW_CHILD_CRASH_URL", &endpoint)
            .env("EXECLAW_CHILD_CRASH_PARENT", &parent_run_id)
            .env("EXECLAW_CHILD_CRASH_ARTIFACTS", &artifact_root)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(15), async {
            loop {
                let started = db
                    .with_conn(|connection| {
                        Ok(connection.query_row(
                        "SELECT COUNT(*) FROM state_run_inference_attempts WHERE status='started'",
                        [],
                        |row| row.get::<_, i64>(0),
                    )?)
                    })
                    .unwrap_or(0);
                if started == 1 && hits.load(Ordering::SeqCst) == 1 {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("child process began the held inference request");
        child.kill().unwrap();
        child.wait().unwrap();

        let api = InferenceSubagentApi::new_with_parent(
            Arc::new(InferenceClient::new(endpoint)),
            "test-model",
            db.clone(),
            cid,
            Some(parent_run_id.clone()),
        )
        .with_child_lease_seconds(2)
        .with_artifact_root(&artifact_root);
        let response = tokio::time::timeout(
            std::time::Duration::from_secs(15),
            api.delegate(&SubagentRequest {
                task: "produce one durable result".into(),
                context: None,
                max_tokens: Some(64),
                dependencies: Vec::new(),
            }),
        )
        .await
        .expect("expired child lease was reclaimed")
        .unwrap();
        server.abort();
        assert_eq!(response.text, "recovered once");
        assert_eq!(hits.load(Ordering::SeqCst), 2);
        let child_run_id = format!("child:{}", response.task_id);
        let attempts: Vec<(i64, String)> = db.with_conn(|connection| {
            let mut statement = connection.prepare(
                "SELECT attempt_no,status FROM state_run_inference_attempts WHERE run_id=?1 ORDER BY attempt_no",
            )?;
            Ok(statement
                .query_map([&child_run_id], |row| Ok((row.get(0)?, row.get(1)?)))?
                .collect::<Result<Vec<_>, _>>()?)
        }).unwrap();
        assert_eq!(attempts, [(1, "retrying".into()), (2, "succeeded".into())]);
        let (artifacts, settled): (i64, i64) = db
            .with_conn(|connection| {
                let artifacts = connection.query_row(
                    "SELECT COUNT(*) FROM state_tool_result_artifacts WHERE run_id=?1",
                    [&parent_run_id],
                    |row| row.get(0),
                )?;
                let settled = connection.query_row(
                    "SELECT budget_settled FROM state_run_child_tasks WHERE child_run_id=?1",
                    [&child_run_id],
                    |row| row.get(0),
                )?;
                Ok((artifacts, settled))
            })
            .unwrap();
        assert_eq!(artifacts, 1);
        assert_eq!(settled, 1);
    }

    #[tokio::test]
    async fn completed_child_join_replays_without_another_inference_request() {
        use execlaw_core::runs::{NewRun, RunStore};
        use std::sync::atomic::{AtomicUsize, Ordering};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let request_count = Arc::new(AtomicUsize::new(0));
        let server_count = request_count.clone();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buffer = [0u8; 8192];
            let _ = socket.read(&mut buffer).await;
            server_count.fetch_add(1, Ordering::SeqCst);
            let body = serde_json::json!({
                "id": "child-recovery",
                "object": "chat.completion",
                "created": 1_700_000_000,
                "model": "test-model",
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": "recovered child result"},
                    "finish_reason": "stop",
                }],
                "usage": {"prompt_tokens": 9, "completion_tokens": 3, "total_tokens": 12},
            })
            .to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(response.as_bytes()).await;
        });

        let db = fresh_db();
        let conversation_id = seed_conv(&db, "child-recovery");
        EventLog::new(&db)
            .commit_turn(
                &conversation_id,
                EventSeq(0),
                vec![
                    PendingEvent::encode(
                        EventKind::UserMsg,
                        &serde_json::json!({"text":"delegate focused child task"}),
                        Some("controller".into()),
                    )
                    .unwrap(),
                ],
            )
            .unwrap();
        let parent_run_id = RunStore::new(&db)
            .create_run(&NewRun {
                conversation_id: conversation_id.clone(),
                parent_run_id: None,
                input_event_seq: EventSeq(1),
                started_at: chrono::Utc::now().timestamp(),
                deadline_at: None,
            })
            .unwrap();
        let api = InferenceSubagentApi::new_with_parent(
            Arc::new(InferenceClient::new(format!("http://{addr}/v1"))),
            "test-model",
            db,
            conversation_id,
            Some(parent_run_id),
        );
        let request = SubagentRequest {
            task: "produce a stable result".into(),
            context: Some("checkpoint this result".into()),
            max_tokens: Some(64),
            dependencies: Vec::new(),
        };

        let first = api.delegate(&request).await.unwrap();
        let replay = api.delegate(&request).await.unwrap();

        assert_eq!(first.text, "recovered child result");
        assert_eq!(replay.text, first.text);
        assert_eq!(replay.task_id, first.task_id);
        assert_eq!(request_count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn delegate_rejects_empty_task() {
        let client = Arc::new(InferenceClient::new("http://127.0.0.1:0/v1"));
        let db = fresh_db();
        let cid = seed_conv(&db, "c1");
        let api = InferenceSubagentApi::new(client, "m", db, cid);
        let err = api
            .delegate(&SubagentRequest {
                task: "   ".into(),
                context: None,
                max_tokens: None,
                dependencies: Vec::new(),
            })
            .await
            .unwrap_err();
        match err {
            ApiError::Validation(msg) => assert!(msg.contains("empty")),
            other => panic!("expected Validation, got {other:?}"),
        }
    }

    #[test]
    fn task_preview_truncates_long_prompts() {
        let long = "a".repeat(200);
        let preview = InferenceSubagentApi::truncate_for_preview(&long);
        assert!(preview.chars().count() <= 80);
        assert!(preview.ends_with('…'));
    }

    #[test]
    fn child_reply_tokens_respect_the_hard_cap() {
        assert_eq!(bounded_max_tokens(None), DEFAULT_MAX_TOKENS);
        assert_eq!(bounded_max_tokens(Some(65_536)), HARD_CAP_TOKENS);
        assert_eq!(bounded_max_tokens(Some(98_304)), HARD_CAP_TOKENS);
    }

    #[test]
    fn task_preview_passes_through_short_prompts_intact() {
        let short = "draft the email";
        assert_eq!(
            InferenceSubagentApi::truncate_for_preview(short),
            "draft the email"
        );
    }
}
