//! Supervisor for durable always-on child agents.

use crate::inference_resolver::InferenceResolver;
use execlaw_core::Database;
use execlaw_core::agents::{AgentRow, AgentStore};
use execlaw_core::backends::BackendPurpose;
use execlaw_inference_api::{ChatMessage, ChatRequest, ModelId};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, Notify, Semaphore};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

pub const DEFAULT_TICK_INTERVAL: Duration = Duration::from_secs(5);
static GLOBAL_WAKE: std::sync::OnceLock<Arc<Notify>> = std::sync::OnceLock::new();

#[derive(Clone)]
pub struct AgentSupervisor {
    db: Database,
    inference: Arc<InferenceResolver>,
    wake: Arc<Notify>,
    stop: CancellationToken,
    permits: Arc<Mutex<HashMap<String, Arc<Semaphore>>>>,
}

impl AgentSupervisor {
    pub fn new(db: Database, inference: Arc<InferenceResolver>) -> Self {
        let wake = Arc::new(Notify::new());
        let _ = GLOBAL_WAKE.set(wake.clone());
        Self {
            db,
            inference,
            wake,
            stop: CancellationToken::new(),
            permits: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn kick(&self) {
        self.wake.notify_one();
    }

    pub fn kick_global() {
        if let Some(wake) = GLOBAL_WAKE.get() {
            wake.notify_one();
        }
    }
    pub fn stop(&self) {
        self.stop.cancel();
    }

    pub fn spawn(&self) -> tokio::task::JoinHandle<()> {
        let this = self.clone();
        tokio::spawn(async move {
            info!("always-on agent supervisor running");
            loop {
                tokio::select! {
                    _ = this.stop.cancelled() => break,
                    _ = this.wake.notified() => {},
                    _ = tokio::time::sleep(DEFAULT_TICK_INTERVAL) => {},
                }
                if let Err(error) = this.tick_once().await {
                    warn!(%error, "agent supervisor tick failed");
                }
            }
        })
    }

    pub async fn tick_once(&self) -> Result<(), String> {
        let agents = AgentStore::new(&self.db)
            .list()
            .map_err(|e| e.to_string())?;
        for agent in agents.into_iter().filter(|a| a.enabled && !a.paused) {
            let db = self.db.clone();
            let inference = self.inference.clone();
            let permits = self.permits.clone();
            tokio::spawn(async move {
                let permit = {
                    let mut all = permits.lock().await;
                    all.entry(agent.id.clone())
                        .or_insert_with(|| {
                            Arc::new(Semaphore::new(agent.concurrency_limit as usize))
                        })
                        .clone()
                };
                if let Err(error) = run_agent(db, inference, agent, permit).await {
                    warn!(%error, "agent run failed");
                }
            });
        }
        Ok(())
    }
}

async fn run_agent(
    db: Database,
    inference: Arc<InferenceResolver>,
    agent: AgentRow,
    semaphore: Arc<Semaphore>,
) -> Result<(), String> {
    let store = AgentStore::new(&db);
    let claimed = store
        .claim_due(&agent.id, chrono::Utc::now().timestamp())
        .map_err(|e| e.to_string())?;
    let agent = match claimed {
        Some(agent) => agent,
        None => return Ok(()),
    };
    let _permit = semaphore.acquire().await.map_err(|e| e.to_string())?;
    let messages = store
        .pending_messages(&agent.id, 32)
        .map_err(|e| e.to_string())?;
    let mailbox = messages
        .iter()
        .map(|m| format!("[{}] {}", m.direction, m.content))
        .collect::<Vec<_>>()
        .join("\n");
    let prompt = if mailbox.is_empty() {
        "No new mailbox messages. Perform your scheduled responsibility and report only useful changes.".to_owned()
    } else {
        format!("Mailbox:\n{mailbox}\n\nProcess these messages and report the result.")
    };
    let resolved = inference
        .resolve(&db, parse_purpose(&agent.backend_purpose))
        .ok_or_else(|| "no inference backend configured".to_owned())?;
    let model = agent
        .model
        .clone()
        .unwrap_or_else(|| resolved.model_id.clone());
    let request = ChatRequest {
        model: ModelId(model),
        messages: vec![
            ChatMessage::system(&agent.role_prompt),
            ChatMessage::user(prompt),
        ],
        tools: None,
        stream: false,
        temperature: None,
        max_tokens: Some(agent.token_budget),
        chat_template_kwargs: None,
        tool_choice: None,
        guided_decoding_backend: None,
    };
    let run_id = store
        .insert_run(
            &agent.id,
            chrono::Utc::now().timestamp(),
            &serde_json::json!({"mailbox_count": messages.len()}),
        )
        .map_err(|e| e.to_string())?;
    let result = tokio::time::timeout(
        Duration::from_secs(agent.max_runtime_secs as u64),
        resolved.client.chat_completions(&request),
    )
    .await;
    match result {
        Ok(Ok(response)) => {
            let text = response
                .choices
                .first()
                .and_then(|c| c.message.content.as_ref())
                .map(|c| c.as_text())
                .unwrap_or_default();
            let tokens = response.usage.as_ref().map(|u| u.completion_tokens);
            store
                .deliver(
                    &messages.iter().map(|m| m.id.clone()).collect::<Vec<_>>(),
                    &run_id,
                    chrono::Utc::now().timestamp(),
                )
                .map_err(|e| e.to_string())?;
            let now = chrono::Utc::now().timestamp();
            store
                .finish(
                    &agent.id,
                    &run_id,
                    "success",
                    now,
                    now.saturating_add(agent.interval_secs as i64),
                    tokens,
                    Some(&text),
                    None,
                    &serde_json::json!({"mailbox_count": messages.len(), "last_output": text}),
                )
                .map_err(|e| e.to_string())?;
                for parent_id in messages.iter().filter_map(|m| m.parent_agent_id.as_deref()) {
                    store.enqueue(parent_id, Some(&agent.id), &text, now).map_err(|e| e.to_string())?;
                }
        }
        Ok(Err(error)) => finish_error(&store, &agent, &run_id, format!("inference: {error}"))?,
        Err(_) => finish_error(
            &store,
            &agent,
            &run_id,
            "runtime budget exceeded".to_owned(),
        )?,
    }
    Ok(())
}

fn finish_error(
    store: &AgentStore,
    agent: &AgentRow,
    run_id: &str,
    error: String,
) -> Result<(), String> {
    let now = chrono::Utc::now().timestamp();
    store
        .finish(
            &agent.id,
            run_id,
            "failed",
            now,
            now.saturating_add((agent.interval_secs as i64).saturating_mul(2)),
            None,
            None,
            Some(&error),
            &serde_json::json!({"error": error}),
        )
        .map_err(|e| e.to_string())
}

fn parse_purpose(value: &str) -> BackendPurpose {
    match value.to_ascii_lowercase().as_str() {
        "small" => BackendPurpose::Small,
        "vision" => BackendPurpose::Vision,
        "voice_stt" => BackendPurpose::VoiceStt,
        "voice_tts" => BackendPurpose::VoiceTts,
        _ => BackendPurpose::Standard,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use execlaw_core::{DbConfig, MigrationRunner};
    #[tokio::test]
    async fn supervisor_does_not_claim_future_agent() {
        let db = Database::open(&DbConfig::in_memory_unencrypted()).unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        let supervisor = AgentSupervisor::new(db, Arc::new(InferenceResolver::new(None)));
        supervisor.tick_once().await.unwrap();
    }
}
