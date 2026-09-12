//! Durable evidence-backed memory extraction over committed turn ranges.

use async_trait::async_trait;
use execlaw_core::Database;
use execlaw_core::backends::BackendPurpose;
use execlaw_core::events::EventLog;
use execlaw_core::ids::{ConversationId, EventSeq};
use execlaw_core::memory_assertions::{
    MemoryAssertionStore, MemoryCandidate, MemoryExtractionPolicy, MemoryJob, MemoryJobKind,
    MemoryJobStore, NewMemoryJob,
};
use execlaw_inference_api::{ChatMessage, ChatRequest, ModelId};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;

const EXTRACTION_POLICY_VERSION: &str = "memory-extract-v1";
const MAX_EVENTS: usize = 32;
const MAX_PROMPT_BYTES: usize = 32 * 1024;
const MAX_CANDIDATES: usize = 8;
const MAX_OUTPUT_TOKENS: u32 = 1200;
const MAX_ATTEMPTS: i64 = 5;
const LEASE_SECS: i64 = 120;
const MAX_BACKOFF_SECS: i64 = 300;
const POLL_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Debug, Clone)]
pub struct MemoryExtractionRequest {
    pub conversation_id: ConversationId,
    pub event_start_seq: EventSeq,
    pub event_end_seq: EventSeq,
    pub run_id: String,
    pub authority_scope: String,
    pub authority_trust_class: String,
}

#[derive(Clone)]
pub struct MemoryExtractionSink {
    db: Option<Database>,
    wake: Option<Arc<Notify>>,
    inference: Option<Arc<crate::inference_resolver::InferenceResolver>>,
}

impl MemoryExtractionSink {
    pub fn noop() -> Self {
        Self {
            db: None,
            wake: None,
            inference: None,
        }
    }

    /// Enqueue an exact committed turn range with host-derived authority.
    pub fn enqueue(&self, request: MemoryExtractionRequest) -> bool {
        let (Some(db), Some(inference)) = (&self.db, &self.inference) else {
            return false;
        };
        let Some(resolved) = resolve_approved(inference, db) else {
            return false;
        };
        let now = chrono::Utc::now().timestamp();
        let job = NewMemoryJob {
            kind: MemoryJobKind::MemoryExtract,
            conversation_id: request.conversation_id,
            event_start_seq: request.event_start_seq,
            event_end_seq: request.event_end_seq,
            run_id: request.run_id,
            policy_hash: policy_hash(),
            model_hash: model_config_hash(&resolved),
            max_attempts: MAX_ATTEMPTS,
            now,
        };
        if MemoryJobStore::new(db)
            .enqueue_extraction(
                &job,
                &request.authority_scope,
                &request.authority_trust_class,
            )
            .is_err()
        {
            return false;
        }
        if let Some(wake) = &self.wake {
            wake.notify_one();
        }
        true
    }
}

#[derive(Debug, Serialize)]
struct PromptEvent {
    seq: i64,
    kind: String,
    payload: serde_json::Value,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExtractionOutput {
    candidates: Vec<MemoryCandidate>,
}

#[async_trait]
trait CandidateExtractor: Send + Sync {
    async fn extract(
        &self,
        job: &MemoryJob,
        events: &[PromptEvent],
    ) -> Result<Vec<MemoryCandidate>, String>;
}

struct InferenceCandidateExtractor {
    db: Database,
    inference: Arc<crate::inference_resolver::InferenceResolver>,
}

#[async_trait]
impl CandidateExtractor for InferenceCandidateExtractor {
    async fn extract(
        &self,
        job: &MemoryJob,
        events: &[PromptEvent],
    ) -> Result<Vec<MemoryCandidate>, String> {
        let resolved = resolve_approved(&self.inference, &self.db)
            .ok_or_else(|| "no approved local Small or Standard backend".to_string())?;
        if model_config_hash(&resolved) != job.model_hash {
            return Err("pinned extraction model configuration changed".into());
        }
        let event_json = serde_json::to_string(events).map_err(|error| error.to_string())?;
        if event_json.len() > MAX_PROMPT_BYTES {
            return Err("committed event range exceeds extraction prompt budget".into());
        }
        let system = concat!(
            "Extract durable memory candidates only from the supplied committed events. ",
            "Return strict JSON {\"candidates\":[...]}. Each candidate must contain kind, ",
            "subject, predicate, object, confidence, supersedes_id, and one or more evidence ",
            "objects with event_seq, payload_path, quote, quote_hash, evidence_kind. ",
            "payload_path must be a JSON pointer or $.field path. quote must exactly equal the ",
            "value at that path and quote_hash must be lowercase SHA-256 of quote UTF-8 bytes. ",
            "Never infer scope or trust. Use at most 8 candidates. Procedural knowledge may be ",
            "returned as kind procedural and will require separate approval."
        );
        let request = ChatRequest {
            model: ModelId(resolved.model_id.clone()),
            messages: vec![
                ChatMessage::system(system),
                ChatMessage::user(format!(
                    "policy_version={EXTRACTION_POLICY_VERSION}\ncommitted_events={event_json}"
                )),
            ],
            tools: None,
            stream: false,
            temperature: Some(0.0),
            max_tokens: Some(MAX_OUTPUT_TOKENS),
            chat_template_kwargs: None,
            tool_choice: None,
            guided_decoding_backend: None,
        };
        let adapter = execlaw_model_adapter::adapter_for(
            execlaw_model_adapter::ModelFamily::detect(&resolved.model_id),
        );
        let response = adapter
            .chat(
                &resolved.client,
                request,
                execlaw_model_adapter::OutputHint::StructuredJson,
            )
            .await
            .map_err(|error| format!("memory extraction inference failed: {error}"))?;
        let output: ExtractionOutput = serde_json::from_str(&response.content)
            .map_err(|error| format!("invalid memory extraction response: {error}"))?;
        if output.candidates.len() > MAX_CANDIDATES {
            return Err(format!(
                "memory extraction returned {} candidates; maximum is {MAX_CANDIDATES}",
                output.candidates.len()
            ));
        }
        Ok(output.candidates)
    }
}

pub struct MemoryExtractionWorker {
    db: Database,
    extractor: Arc<dyn CandidateExtractor>,
}

impl MemoryExtractionWorker {
    fn new(db: Database, extractor: Arc<dyn CandidateExtractor>) -> Self {
        Self { db, extractor }
    }

    async fn process_job(&self, job: &MemoryJob) -> Result<(), String> {
        if job.policy_hash != policy_hash() {
            return Err("pinned extraction policy version is unsupported".into());
        }
        let scope = job
            .authority_scope
            .as_deref()
            .ok_or_else(|| "memory extraction job has no host authority scope".to_string())?;
        let trust = job
            .authority_trust_class
            .as_deref()
            .ok_or_else(|| "memory extraction job has no host authority trust".to_string())?;
        let events = load_exact_events(&self.db, job)?;
        let candidates = self.extractor.extract(job, &events).await?;
        let policy = MemoryExtractionPolicy::default();
        for candidate in candidates {
            MemoryAssertionStore::new(&self.db)
                .persist_candidate(
                    job,
                    scope,
                    trust,
                    &candidate,
                    policy,
                    chrono::Utc::now().timestamp(),
                )
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }

    async fn run_once(&self, owner: &str, now: i64) -> Result<bool, String> {
        let Some(job) = MemoryJobStore::new(&self.db)
            .claim_next(MemoryJobKind::MemoryExtract, owner, now, now + LEASE_SECS)
            .map_err(|error| error.to_string())?
        else {
            return Ok(false);
        };
        let store = MemoryJobStore::new(&self.db);
        match self.process_job(&job).await {
            Ok(()) => {
                store
                    .complete(&job.job_id, owner, now)
                    .map_err(|error| error.to_string())?;
            }
            Err(error) => {
                let backoff = 2_i64
                    .saturating_pow(job.attempt.saturating_sub(1) as u32)
                    .min(MAX_BACKOFF_SECS);
                store
                    .retry(&job.job_id, owner, &error, now + backoff)
                    .map_err(|retry_error| retry_error.to_string())?;
            }
        }
        Ok(true)
    }

    fn spawn(self: Arc<Self>) -> (MemoryExtractionSink, tokio::task::JoinHandle<()>) {
        let wake = Arc::new(Notify::new());
        let sink = MemoryExtractionSink {
            db: Some(self.db.clone()),
            wake: Some(wake.clone()),
            inference: None,
        };
        let handle = tokio::spawn(async move {
            let owner = format!("memory-extract-{}", uuid::Uuid::new_v4());
            loop {
                match self.run_once(&owner, chrono::Utc::now().timestamp()).await {
                    Ok(true) => continue,
                    Ok(false) => {
                        tokio::select! {
                            _ = wake.notified() => {},
                            _ = tokio::time::sleep(POLL_INTERVAL) => {},
                        }
                    }
                    Err(error) => {
                        tracing::warn!(%error, "memory extraction worker iteration failed");
                        tokio::time::sleep(POLL_INTERVAL).await;
                    }
                }
            }
        });
        (sink, handle)
    }
}

pub fn spawn_memory_extraction_worker(
    db: Database,
    inference: Arc<crate::inference_resolver::InferenceResolver>,
) -> (MemoryExtractionSink, tokio::task::JoinHandle<()>) {
    let extractor: Arc<dyn CandidateExtractor> = Arc::new(InferenceCandidateExtractor {
        db: db.clone(),
        inference: inference.clone(),
    });
    let worker = Arc::new(MemoryExtractionWorker::new(db, extractor));
    let (mut sink, handle) = worker.spawn();
    sink.inference = Some(inference);
    (sink, handle)
}

fn resolve_approved(
    inference: &crate::inference_resolver::InferenceResolver,
    db: &Database,
) -> Option<crate::inference_resolver::ResolvedInference> {
    inference
        .resolve(db, BackendPurpose::Small)
        .filter(|resolved| resolved.source == "db")
        .or_else(|| {
            inference
                .resolve(db, BackendPurpose::Standard)
                .filter(|resolved| resolved.source == "db")
        })
}

fn model_config_hash(resolved: &crate::inference_resolver::ResolvedInference) -> String {
    hex::encode(Sha256::digest(
        format!(
            "{}\n{}\n{}",
            resolved.source, resolved.endpoint, resolved.model_id
        )
        .as_bytes(),
    ))
}

fn policy_hash() -> String {
    hex::encode(Sha256::digest(
        format!(
            "{EXTRACTION_POLICY_VERSION}:events={MAX_EVENTS}:bytes={MAX_PROMPT_BYTES}:candidates={MAX_CANDIDATES}:tokens={MAX_OUTPUT_TOKENS}:auto_approve=false"
        )
        .as_bytes(),
    ))
}

fn load_exact_events(db: &Database, job: &MemoryJob) -> Result<Vec<PromptEvent>, String> {
    let records = EventLog::new(db)
        .replay_since(
            &job.conversation_id,
            EventSeq(job.event_start_seq.0.saturating_sub(1)),
        )
        .map_err(|error| error.to_string())?
        .into_iter()
        .take_while(|event| event.seq.0 <= job.event_end_seq.0)
        .collect::<Vec<_>>();
    let expected = (job.event_end_seq.0 - job.event_start_seq.0 + 1) as usize;
    if records.len() != expected
        || records.first().map(|event| event.seq) != Some(job.event_start_seq)
        || records.last().map(|event| event.seq) != Some(job.event_end_seq)
        || records.len() > MAX_EVENTS
    {
        return Err(
            "memory extraction range is missing, non-contiguous, or exceeds event budget".into(),
        );
    }
    records
        .into_iter()
        .map(|event| {
            let payload = rmp_serde::from_slice(&event.payload)
                .map_err(|error| format!("decode committed event {}: {error}", event.seq.0))?;
            Ok(PromptEvent {
                seq: event.seq.0,
                kind: event.kind.as_str().to_owned(),
                payload,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use execlaw_core::backends::{BackendMode, BackendStore, BackendUpsert};
    use execlaw_core::db::DbConfig;
    use execlaw_core::events::{EventKind, EventRecord};
    use execlaw_core::migrations::MigrationRunner;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct FailOnceExtractor {
        calls: AtomicUsize,
    }

    #[test]
    fn production_sink_enqueues_deduplicated_authorized_range() {
        let db = Database::open(&DbConfig::in_memory_unencrypted()).unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        BackendStore::new(&db)
            .upsert(
                &BackendUpsert {
                    purpose: BackendPurpose::Small,
                    inference_backend: "service-vllm".into(),
                    model_spec_json: serde_json::json!({"model": "local-small"}),
                    gpu_id: None,
                    endpoint: Some("http://127.0.0.1:8101/v1".into()),
                    notes: None,
                    reasoning_enabled: false,
                    mode: BackendMode::External,
                },
                1,
            )
            .unwrap();
        let conversation_id = ConversationId::from("wired");
        let log = EventLog::new(&db);
        for seq in 1..=2 {
            log.append(
                &EventRecord::new(
                    conversation_id.clone(),
                    EventSeq(seq),
                    if seq == 1 {
                        EventKind::UserMsg
                    } else {
                        EventKind::ModelTurn
                    },
                    &serde_json::json!({"text": format!("event {seq}")}),
                    None,
                )
                .unwrap(),
            )
            .unwrap();
        }
        let sink = MemoryExtractionSink {
            db: Some(db.clone()),
            wake: None,
            inference: Some(Arc::new(crate::inference_resolver::InferenceResolver::new(
                None,
            ))),
        };
        let request = MemoryExtractionRequest {
            conversation_id,
            event_start_seq: EventSeq(1),
            event_end_seq: EventSeq(2),
            run_id: "turn-wired-2".into(),
            authority_scope: "principal:p1".into(),
            authority_trust_class: "KnownTrusted".into(),
        };
        assert!(sink.enqueue(request.clone()));
        assert!(sink.enqueue(request));
        let row: (i64, String, String) = db
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT COUNT(*), authority_scope, authority_trust_class FROM memory_jobs",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .map_err(execlaw_core::DbError::from)
            })
            .unwrap();
        assert_eq!(row, (1, "principal:p1".into(), "KnownTrusted".into()));
    }

    #[async_trait]
    impl CandidateExtractor for FailOnceExtractor {
        async fn extract(
            &self,
            _job: &MemoryJob,
            _events: &[PromptEvent],
        ) -> Result<Vec<MemoryCandidate>, String> {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                Err("transient local inference failure".into())
            } else {
                Ok(Vec::new())
            }
        }
    }

    #[tokio::test]
    async fn worker_retries_then_completes_durable_job() {
        let db = Database::open(&DbConfig::in_memory_unencrypted()).unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        let conversation_id = ConversationId::from("memory-worker");
        EventLog::new(&db)
            .append(
                &EventRecord::new(
                    conversation_id.clone(),
                    EventSeq(1),
                    EventKind::UserMsg,
                    &serde_json::json!({"text": "remember blue"}),
                    Some("p1".into()),
                )
                .unwrap(),
            )
            .unwrap();
        let job = NewMemoryJob {
            kind: MemoryJobKind::MemoryExtract,
            conversation_id,
            event_start_seq: EventSeq(1),
            event_end_seq: EventSeq(1),
            run_id: "turn-1".into(),
            policy_hash: policy_hash(),
            model_hash: "pinned-model".into(),
            max_attempts: 3,
            now: 10,
        };
        MemoryJobStore::new(&db)
            .enqueue_extraction(&job, "principal:p1", "KnownTrusted")
            .unwrap();
        let extractor = Arc::new(FailOnceExtractor {
            calls: AtomicUsize::new(0),
        });
        let worker = MemoryExtractionWorker::new(db.clone(), extractor);

        assert!(worker.run_once("worker", 10).await.unwrap());
        let status: String = db
            .with_conn(|conn| {
                conn.query_row("SELECT status FROM memory_jobs", [], |row| row.get(0))
                    .map_err(execlaw_core::DbError::from)
            })
            .unwrap();
        assert_eq!(status, "pending");
        assert!(worker.run_once("worker", 11).await.unwrap());
        let (status, attempts): (String, i64) = db
            .with_conn(|conn| {
                conn.query_row("SELECT status, attempt FROM memory_jobs", [], |row| {
                    Ok((row.get(0)?, row.get(1)?))
                })
                .map_err(execlaw_core::DbError::from)
            })
            .unwrap();
        assert_eq!(status, "completed");
        assert_eq!(attempts, 2);
    }
}
