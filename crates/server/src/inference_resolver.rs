//! Inference-client resolution for the runner (Phase 12.E).
//!
//! Pre-Phase-12, `state.inference` was a single `InferenceClient`
//! built from `EXECLAW_INFERENCE_URL` at process start — operators
//! who wanted to swap backends had to restart execlaw. With managed
//! backends spawning their own containers + writing endpoints back
//! into `config_backends` (Phase 12.C), we need the runner to pick
//! up those URLs on every turn instead of the boot-time URL.
//!
//! `InferenceResolver` is the indirection that closes that loop.
//! Per turn, the runner calls `resolve(purpose)`:
//!
//!   1. Read the `config_backends` row for `purpose`.
//!   2. If it has a non-empty `endpoint`, build a fresh
//!      `InferenceClient` for that URL.
//!   3. If the row has no endpoint OR no row exists, fall through
//!      to the boot-time `bootstrap` client. (External rows that
//!      operators left unconfigured still work via the bootstrap.)
//!   4. If neither path produces a URL, return `None` and the
//!      caller falls back to the stub turn.
//!
//! No caching: `InferenceClient` is a thin wrapper around a base
//! URL + a `reqwest::Client`. Construction is sub-millisecond, and
//! avoiding a cache means the resolver is naturally hot-reloadable
//! — a Backends save updates the row, the *next* turn picks up the
//! new URL, no lock contention or invalidation dance.
//!
//! Scope of v1: every caller asks for `BackendPurpose::Standard`.
//! Per-purpose routing for Small / Voice* lands when the runner
//! grows modality-aware backend selection.

use execlaw_core::Database;
use execlaw_core::backends::{BackendMode, BackendPurpose, BackendStore};
use execlaw_inference_api::{InferenceClient, InferenceEngine};
use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex, Weak};
use std::time::Duration;
use thiserror::Error;
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};

const MAX_INFERENCE_CONCURRENCY: usize = 8;
const MAX_BACKGROUND_CONCURRENCY: usize = 6;
const MAX_MODEL_CONCURRENCY: usize = 2;
const MAX_BACKGROUND_PER_MODEL: usize = 1;
const MAX_CHILD_CONCURRENCY_PER_PARENT: usize = 2;
const MAX_QUEUED_INFERENCE: usize = 128;
const INFERENCE_QUEUE_TIMEOUT: Duration = Duration::from_secs(120);
const BACKGROUND_AGING: Duration = Duration::from_secs(20);

/// Workload class used for fair admission to a local model endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InferenceWorkload {
    Chat,
    Voice,
    Background,
    Research,
    Agent,
    Automation,
    Child,
}

impl InferenceWorkload {
    fn is_background(self) -> bool {
        matches!(
            self,
            Self::Background | Self::Research | Self::Agent | Self::Automation | Self::Child
        )
    }
}

/// Failure to obtain a bounded local-inference admission slot.
#[derive(Debug, Error)]
pub enum InferenceAdmissionError {
    #[error("inference queue is full")]
    QueueFull,
    #[error("inference admission queue timed out")]
    TimedOut,
    #[error("inference admission was cancelled")]
    Cancelled,
    #[error("inference admission semaphore is closed")]
    Closed,
    #[error("child inference budget scope is invalid")]
    InvalidBudgetScope,
}

#[derive(Debug, Clone)]
struct ModelPermits {
    total: Arc<Semaphore>,
    background: Arc<Semaphore>,
}

/// Shared bounded scheduler for requests sent to local inference backends.
///
/// It reserves global and per-model capacity for foreground chat and voice.
/// Background work uses a smaller lane initially, then ages into the shared
/// lane after waiting long enough. Dropping the returned permit releases all
/// reservations, including when a request is cancelled or times out.
#[derive(Debug, Clone)]
pub struct InferenceAdmission {
    total: Arc<Semaphore>,
    background: Arc<Semaphore>,
    models: Arc<Mutex<HashMap<String, ModelPermits>>>,
    parents: Arc<StdMutex<HashMap<String, Weak<Semaphore>>>>,
    queued: Arc<AtomicUsize>,
}

impl Default for InferenceAdmission {
    fn default() -> Self {
        Self::new()
    }
}

impl InferenceAdmission {
    /// Create a scheduler with foreground reserves, per-model limits, and a
    /// bounded queue. Production shares one instance through `InferenceResolver`.
    pub fn new() -> Self {
        Self {
            total: Arc::new(Semaphore::new(MAX_INFERENCE_CONCURRENCY)),
            background: Arc::new(Semaphore::new(MAX_BACKGROUND_CONCURRENCY)),
            models: Arc::new(Mutex::new(HashMap::new())),
            parents: Arc::new(StdMutex::new(HashMap::new())),
            queued: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Wait for an admission slot, returning an RAII permit that releases on drop.
    pub async fn acquire(
        &self,
        model: &str,
        workload: InferenceWorkload,
    ) -> Result<InferencePermit, InferenceAdmissionError> {
        self.acquire_scoped(model, workload, None).await
    }

    /// Wait for an admission slot while enforcing one durable parent's child limit.
    pub async fn acquire_scoped(
        &self,
        model: &str,
        workload: InferenceWorkload,
        budget_scope: Option<&str>,
    ) -> Result<InferencePermit, InferenceAdmissionError> {
        self.acquire_scoped_cancellable(model, workload, budget_scope, std::future::pending())
            .await
    }

    /// Wait for a slot while also observing caller cancellation.
    pub async fn acquire_cancellable<C>(
        &self,
        model: &str,
        workload: InferenceWorkload,
        cancelled: C,
    ) -> Result<InferencePermit, InferenceAdmissionError>
    where
        C: Future<Output = ()> + Send,
    {
        self.acquire_scoped_cancellable(model, workload, None, cancelled)
            .await
    }

    /// Wait for a slot in a durable parent scope while observing cancellation.
    pub async fn acquire_scoped_cancellable<C>(
        &self,
        model: &str,
        workload: InferenceWorkload,
        budget_scope: Option<&str>,
        cancelled: C,
    ) -> Result<InferencePermit, InferenceAdmissionError>
    where
        C: Future<Output = ()> + Send,
    {
        let parent_scope = if workload == InferenceWorkload::Child {
            let scope = budget_scope.unwrap_or("unscoped-child").trim();
            if scope.is_empty() || scope.len() > 256 {
                return Err(InferenceAdmissionError::InvalidBudgetScope);
            }
            Some(scope.to_owned())
        } else {
            None
        };
        let wait_started = tokio::time::Instant::now();
        let position = self.queued.fetch_add(1, Ordering::AcqRel);
        if position >= MAX_QUEUED_INFERENCE {
            self.queued.fetch_sub(1, Ordering::AcqRel);
            tracing::warn!(workload = ?workload, queue_limit = MAX_QUEUED_INFERENCE, "local inference admission queue is full");
            return Err(InferenceAdmissionError::QueueFull);
        }
        let _queue_slot = QueueSlot(self.queued.clone());
        let model_permits = {
            let key = model.trim().to_ascii_lowercase();
            let mut models = self.models.lock().await;
            models
                .entry(key)
                .or_insert_with(|| ModelPermits {
                    total: Arc::new(Semaphore::new(MAX_MODEL_CONCURRENCY)),
                    background: Arc::new(Semaphore::new(MAX_BACKGROUND_PER_MODEL)),
                })
                .clone()
        };

        let wait = async {
            let parent = if let Some(scope) = parent_scope.as_deref() {
                Some(self.acquire_parent(scope).await?)
            } else {
                None
            };
            let mut permit = if workload.is_background() {
                tokio::select! {
                    biased;
                    result = self.acquire_background(model_permits.clone()) => result?,
                    _ = tokio::time::sleep(BACKGROUND_AGING) => {
                        self.acquire_shared(model_permits).await?
                    }
                }
            } else {
                self.acquire_shared(model_permits).await?
            };
            permit._parent = parent;
            Ok(permit)
        };

        let result = tokio::select! {
            biased;
            _ = cancelled => Err(InferenceAdmissionError::Cancelled),
            result = tokio::time::timeout(INFERENCE_QUEUE_TIMEOUT, wait) => {
                result.unwrap_or(Err(InferenceAdmissionError::TimedOut))
            }
        };
        if result.is_ok() {
            tracing::debug!(
                model,
                workload = ?workload,
                queue_wait_ms = wait_started.elapsed().as_millis() as u64,
                queued = self.queued.load(Ordering::Relaxed).saturating_sub(1),
                "local inference admission granted"
            );
        }
        result
    }

    fn parent_semaphore(&self, scope: &str) -> Result<Arc<Semaphore>, InferenceAdmissionError> {
        let mut parents = self
            .parents
            .lock()
            .map_err(|_| InferenceAdmissionError::Closed)?;
        parents.retain(|_, semaphore| semaphore.strong_count() > 0);
        if let Some(semaphore) = parents.get(scope).and_then(Weak::upgrade) {
            return Ok(semaphore);
        }
        let semaphore = Arc::new(Semaphore::new(MAX_CHILD_CONCURRENCY_PER_PARENT));
        parents.insert(scope.to_owned(), Arc::downgrade(&semaphore));
        Ok(semaphore)
    }

    async fn acquire_parent(
        &self,
        scope: &str,
    ) -> Result<OwnedSemaphorePermit, InferenceAdmissionError> {
        self.parent_semaphore(scope)?
            .acquire_owned()
            .await
            .map_err(|_| InferenceAdmissionError::Closed)
    }

    async fn acquire_shared(
        &self,
        model: ModelPermits,
    ) -> Result<InferencePermit, InferenceAdmissionError> {
        let global = self
            .total
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| InferenceAdmissionError::Closed)?;
        let per_model = model
            .total
            .acquire_owned()
            .await
            .map_err(|_| InferenceAdmissionError::Closed)?;
        Ok(InferencePermit {
            _global: global,
            _model: per_model,
            _background_global: None,
            _background_model: None,
            _parent: None,
        })
    }

    async fn acquire_background(
        &self,
        model: ModelPermits,
    ) -> Result<InferencePermit, InferenceAdmissionError> {
        let background_global = self
            .background
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| InferenceAdmissionError::Closed)?;
        let background_model = model
            .background
            .acquire_owned()
            .await
            .map_err(|_| InferenceAdmissionError::Closed)?;
        let global = self
            .total
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| InferenceAdmissionError::Closed)?;
        let per_model = model
            .total
            .acquire_owned()
            .await
            .map_err(|_| InferenceAdmissionError::Closed)?;
        Ok(InferencePermit {
            _global: global,
            _model: per_model,
            _background_global: Some(background_global),
            _background_model: Some(background_model),
            _parent: None,
        })
    }
}

#[async_trait::async_trait]
impl execlaw_inference_api::InferenceRequestAdmission for InferenceAdmission {
    async fn acquire(
        &self,
        model: &str,
        workload: &str,
        budget_scope: Option<&str>,
    ) -> Result<Box<dyn execlaw_inference_api::InferenceAdmissionPermit>, String> {
        let workload = match workload {
            "chat" => InferenceWorkload::Chat,
            "voice" => InferenceWorkload::Voice,
            "background" => InferenceWorkload::Background,
            "research" => InferenceWorkload::Research,
            "agent" => InferenceWorkload::Agent,
            "automation" => InferenceWorkload::Automation,
            _ => InferenceWorkload::Child,
        };
        InferenceAdmission::acquire_scoped(self, model, workload, budget_scope)
            .await
            .map(|permit| {
                Box::new(permit) as Box<dyn execlaw_inference_api::InferenceAdmissionPermit>
            })
            .map_err(|error| error.to_string())
    }
}

struct QueueSlot(Arc<AtomicUsize>);

impl Drop for QueueSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Active admission reservation. Dropping it releases every semaphore permit.
pub struct InferencePermit {
    _global: OwnedSemaphorePermit,
    _model: OwnedSemaphorePermit,
    _background_global: Option<OwnedSemaphorePermit>,
    _background_model: Option<OwnedSemaphorePermit>,
    _parent: Option<OwnedSemaphorePermit>,
}

/// Fallback `model` id when the operator's backend row has no
/// `--model=…` arg AND there's no bootstrap-specified model. The
/// chat-completions request MUST send some non-empty `model`
/// string; this is a "best guess" placeholder. The right answer
/// is for the operator to set the model via Settings → Backends.
///
/// Pre-2026-05-13 this default was hardcoded into
/// `ServerConfig::default()` and read on every turn from
/// `state.config.model_id`. That created a second source of truth
/// that drifted from `config_backends.model_spec_json.args` — the
/// supervisor would spawn vLLM with model X, the chat path would
/// send model Y, vLLM 404'd. Single source of truth now: this
/// constant is ONLY reached if the operator's DB row is in an
/// unusable state.
pub const DEFAULT_FALLBACK_MODEL: &str = "QuantTrio/Qwen3.6-27B-AWQ";

/// Resolved inference target for one turn.
///
/// All four fields come from the same DB row read, so they
/// can't drift. Always carries a non-empty `model_id` (falls back
/// to [`DEFAULT_FALLBACK_MODEL`] when the row has no `--model=…`).
#[derive(Debug, Clone)]
pub struct ResolvedInference {
    pub client: Arc<InferenceClient>,
    pub model_id: String,
    pub endpoint: String,
    /// Whether the operator toggled "enable thinking" / reasoning
    /// on this backend row. Pre-2026-05-13 every chat call site did
    /// its OWN `BackendStore::get(...).ok().flatten().map(|r|
    /// r.reasoning_enabled).unwrap_or(false)` — a redundant second
    /// DB read that silently masked errors (`.ok()`) AND opened a
    /// drift window where the resolver got the row from `t0` and
    /// reasoning was read at `t1` after a config save. Now carried
    /// on the resolved struct so it's bound to the same row as the
    /// endpoint + model id.
    pub reasoning_enabled: bool,
    /// Optional OpenAI-compatible reasoning control from the same backend row.
    pub reasoning_effort: Option<String>,
    /// Explicit native Ollama context from the operator's model spec.
    pub ollama_context_tokens: Option<u32>,
    /// `"db"` when the resolution came from a backend row;
    /// `"bootstrap"` when it came from the boot-time URL.
    /// Surfaced for the turn-timing trace so the operator can
    /// confirm which path won.
    pub source: &'static str,
    /// Shared scheduler inherited from the owning resolver.
    pub admission: InferenceAdmission,
}

impl ResolvedInference {
    /// Return this resolved endpoint with an explicit scheduler workload.
    pub fn with_workload(mut self, workload: &'static str) -> Self {
        self.client = Arc::new(self.client.as_ref().clone().with_workload(workload));
        self
    }
}

#[derive(Debug, Clone)]
pub struct InferenceResolver {
    /// Boot-time fallback. Constructed from `--inference-url`
    /// when set; `None` when the operator hasn't configured a
    /// global URL and managed backends are the only source.
    pub bootstrap: Option<Arc<InferenceClient>>,
    /// Optional model id paired with the bootstrap client. When
    /// the bootstrap fires, we use this string instead of
    /// `DEFAULT_FALLBACK_MODEL` so an operator who set
    /// `--inference-url --inference-model` gets exactly what they
    /// asked for.
    pub bootstrap_model: Option<String>,
    /// One scheduler is shared across every purpose resolved by this object.
    pub admission: InferenceAdmission,
}

impl InferenceResolver {
    pub fn new(bootstrap: Option<Arc<InferenceClient>>) -> Self {
        Self {
            bootstrap,
            bootstrap_model: None,
            admission: InferenceAdmission::new(),
        }
    }

    pub fn with_bootstrap_model(mut self, model: Option<String>) -> Self {
        self.bootstrap_model = model;
        self
    }

    /// Pick the `(InferenceClient, model_id)` pair for the next
    /// turn. Bound together intentionally — pre-2026-05-13 these
    /// came from separate sources (`config_backends.endpoint` vs
    /// `state.config.model_id`) and drifted out of sync the moment
    /// an operator switched models without restarting; the
    /// chat-completions request would 404 with
    /// `The model X does not exist` because vLLM was loaded with
    /// model Y. One source of truth, one read, both fields atomic.
    ///
    /// Decision tree:
    ///   * Row has `endpoint` AND `--model=…` arg → use both.
    ///   * Row has `endpoint` but no model arg → use endpoint +
    ///     bootstrap_model (or `DEFAULT_FALLBACK_MODEL`).
    ///   * Row has no endpoint, `mode = managed` → `None` (the
    ///     supervisor hasn't come up yet; we don't silently route
    ///     to bootstrap because that's a different URL than the
    ///     operator chose).
    ///   * Row absent OR external+no-endpoint → bootstrap fallback.
    ///
    /// **Loud failure semantics**: a DB read error is logged at
    /// WARN to the `inference_resolver` target with the underlying
    /// `DbError` formatted in. Pre-2026-05-13 we silently
    /// swallowed via `.ok().flatten()`, which masked the
    /// "BLOB column got TEXT-overwritten by raw SQL" failure mode
    /// for hours of operator-time. Errors now surface
    /// unambiguously even if the return value is still `None`.
    pub fn resolve(&self, db: &Database, purpose: BackendPurpose) -> Option<ResolvedInference> {
        let store = BackendStore::new(db);
        let row = match store.get(purpose) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(
                    target: "inference_resolver",
                    purpose = ?purpose,
                    error = %e,
                    "config_backends read failed — falling through to bootstrap. \
                     Likely causes: BLOB column corrupted by a raw SQL UPDATE \
                     (use Settings → Backends instead), schema migration miss, \
                     or DB lock contention."
                );
                return self.bootstrap_resolved();
            }
        };
        match row {
            Some(r) => {
                let endpoint = r.endpoint.clone().filter(|s| !s.trim().is_empty());
                let row_model = extract_model_arg(&r.model_spec_json);
                let reasoning_enabled = r.reasoning_enabled;
                let reasoning_effort = r
                    .model_spec_json
                    .get("reasoning_effort")
                    .and_then(serde_json::Value::as_str)
                    .filter(|value| matches!(*value, "none" | "low" | "medium" | "high" | "max"))
                    .map(str::to_owned);
                let ollama_context_tokens = r
                    .model_spec_json
                    .get("context_tokens")
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|tokens| u32::try_from(tokens).ok())
                    .filter(|tokens| (4_096..=262_144).contains(tokens));
                // Apple-Silicon Ollama backends speak through
                // Ollama's native /api/chat endpoint rather than
                // the OpenAI-compat shim — the shim drops
                // tool_calls on small qwen quants. The
                // `binary_hint: "ollama"` marker on the
                // managed-row envelope (written by
                // `backend_presets::materialise_spec`) is the
                // discriminator.
                let engine = if is_ollama_binary_hint(&r.model_spec_json) {
                    InferenceEngine::Ollama
                } else {
                    InferenceEngine::OpenAICompat
                };
                match endpoint {
                    Some(url) => {
                        let endpoint_key = format!("inference:{}", purpose.as_str());
                        match crate::local_endpoint_policy::checked_inference_client(
                            db,
                            &endpoint_key,
                            &url,
                        ) {
                            Ok(client) => Some(ResolvedInference {
                                client: Arc::new({
                                    let client = client.with_engine(engine);
                                    let client = match reasoning_effort.as_deref() {
                                        Some(effort) => client.with_reasoning_effort(effort),
                                        None => client,
                                    };
                                    let client = match (engine, ollama_context_tokens) {
                                        (InferenceEngine::Ollama, Some(tokens)) => {
                                            client.with_ollama_context_tokens(tokens)
                                        }
                                        _ => client,
                                    };
                                    client.with_admission(
                                        Arc::new(self.admission.clone()),
                                        "background",
                                    )
                                }),
                                model_id: row_model
                                    .or_else(|| self.bootstrap_model.clone())
                                    .unwrap_or_else(|| DEFAULT_FALLBACK_MODEL.to_owned()),
                                endpoint: url,
                                reasoning_enabled,
                                reasoning_effort,
                                ollama_context_tokens,
                                source: "db",
                                admission: self.admission.clone(),
                            }),
                            Err(error) => {
                                tracing::warn!(
                                    target: "inference_resolver",
                                    purpose = ?purpose,
                                    %error,
                                    "configured inference endpoint denied by local-only policy"
                                );
                                None
                            }
                        }
                    }
                    None => {
                        if r.mode == BackendMode::Managed {
                            tracing::debug!(
                                target: "inference_resolver",
                                purpose = ?purpose,
                                "managed backend row has no endpoint yet — supervisor not ready; \
                                 NOT falling through to bootstrap so the operator sees the stub"
                            );
                            None
                        } else {
                            self.bootstrap_resolved()
                        }
                    }
                }
            }
            None => self.bootstrap_resolved(),
        }
    }

    fn bootstrap_resolved(&self) -> Option<ResolvedInference> {
        let client = self.bootstrap.clone()?;
        let client = Arc::new(
            client
                .as_ref()
                .clone()
                .with_admission(Arc::new(self.admission.clone()), "background"),
        );
        Some(ResolvedInference {
            endpoint: client.base_url.clone(),
            client,
            model_id: self
                .bootstrap_model
                .clone()
                .unwrap_or_else(|| DEFAULT_FALLBACK_MODEL.to_owned()),
            // Bootstrap has no row to read this from; default OFF.
            // Operators who want reasoning on must configure the
            // Standard backend row via Settings → Backends.
            reasoning_enabled: false,
            reasoning_effort: None,
            ollama_context_tokens: None,
            source: "bootstrap",
            admission: self.admission.clone(),
        })
    }
}

/// Pull the configured model id out of a backend row's
/// `model_spec_json`. Tries three locations in order:
///
///   1. A top-level `"model"` field. The Apple-Silicon Ollama
///      preset writes the tag here (see `materialise_spec` in
///      `backend_presets.rs` — Ollama's spawn args are just
///      `["serve"]`, the model isn't a CLI flag). Native engines
///      that ship after Ollama (MLX, llama-server) use the same
///      slot.
///   2. `args` containing `--model=X` (single-token form).
///   3. `args` containing `--model X` (two-token form, vLLM's
///      default invocation).
///
/// Returns `None` when none of the three are present so the
/// resolver falls back to the bootstrap or `DEFAULT_FALLBACK_MODEL`.
/// Without the top-level check, Ollama rows fell through to the
/// vLLM default ("QuantTrio/Qwen3.6-27B-AWQ") and Ollama 404'd at
/// chat time.
/// `true` when the row's `model_spec_json.binary_hint` declares
/// the native runtime is Ollama. Used by `resolve()` to pick the
/// Ollama-native client over the OpenAI-compat path. Case-
/// insensitive; missing field → `false`.
fn is_ollama_binary_hint(spec: &serde_json::Value) -> bool {
    spec.get("binary_hint")
        .and_then(|v| v.as_str())
        .map(|s| s.eq_ignore_ascii_case("ollama"))
        .unwrap_or(false)
}

fn extract_model_arg(spec: &serde_json::Value) -> Option<String> {
    // 1. Top-level `model` field (Ollama / future native engines).
    if let Some(s) = spec.get("model").and_then(|v| v.as_str()) {
        let trimmed = s.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_owned());
        }
    }
    let args = spec.get("args")?.as_array()?;
    // 2. `--model=X` form.
    for a in args {
        if let Some(s) = a.as_str() {
            if let Some(rest) = s.strip_prefix("--model=") {
                let trimmed = rest.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_owned());
                }
            }
        }
    }
    // 3. `--model` followed by a value.
    let mut iter = args.iter();
    while let Some(a) = iter.next() {
        if a.as_str() == Some("--model") {
            if let Some(v) = iter.next().and_then(|v| v.as_str()) {
                let trimmed = v.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_owned());
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use execlaw_core::MigrationRunner;
    use execlaw_core::backends::{BackendStore, BackendUpsert};
    use execlaw_core::db::DbConfig;

    fn fresh_db() -> Database {
        let db = Database::open(&DbConfig::in_memory_unencrypted()).unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        db
    }

    #[tokio::test]
    async fn sibling_children_share_a_parent_limit_without_blocking_chat() {
        let admission = InferenceAdmission::new();
        let child_one = admission
            .acquire_scoped("model-one", InferenceWorkload::Child, Some("run-parent"))
            .await
            .unwrap();
        let child_two = admission
            .acquire_scoped("model-two", InferenceWorkload::Child, Some("run-parent"))
            .await
            .unwrap();

        let waiting_admission = admission.clone();
        let waiting_child = tokio::spawn(async move {
            waiting_admission
                .acquire_scoped("model-three", InferenceWorkload::Child, Some("run-parent"))
                .await
        });
        tokio::time::timeout(Duration::from_millis(100), async {
            while admission.queued.load(Ordering::Acquire) < 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("third sibling enters the bounded queue");

        let foreground = tokio::time::timeout(
            Duration::from_millis(100),
            admission.acquire("model-one", InferenceWorkload::Chat),
        )
        .await
        .expect("foreground admission must not wait behind a saturated parent")
        .unwrap();
        drop(foreground);
        drop(child_one);
        drop(child_two);
        tokio::time::timeout(Duration::from_millis(100), waiting_child)
            .await
            .expect("released sibling reservation should wake queued work")
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn cancelling_queued_child_releases_queue_and_parent_reservations() {
        let admission = InferenceAdmission::new();
        let child_one = admission
            .acquire_scoped("model-one", InferenceWorkload::Child, Some("run-parent"))
            .await
            .unwrap();
        let child_two = admission
            .acquire_scoped("model-two", InferenceWorkload::Child, Some("run-parent"))
            .await
            .unwrap();
        let cancel = Arc::new(tokio::sync::Notify::new());
        let signal = cancel.clone();
        let waiting_admission = admission.clone();
        let waiter = tokio::spawn(async move {
            waiting_admission
                .acquire_scoped_cancellable(
                    "model-three",
                    InferenceWorkload::Child,
                    Some("run-parent"),
                    async move { signal.notified().await },
                )
                .await
        });
        tokio::time::timeout(Duration::from_millis(100), async {
            while admission.queued.load(Ordering::Acquire) < 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("child waiter enters the bounded queue");
        cancel.notify_one();
        assert!(matches!(
            waiter.await.unwrap(),
            Err(InferenceAdmissionError::Cancelled)
        ));
        assert_eq!(admission.queued.load(Ordering::Acquire), 0);

        drop(child_one);
        let replacement = tokio::time::timeout(
            Duration::from_millis(100),
            admission.acquire_scoped("model-three", InferenceWorkload::Child, Some("run-parent")),
        )
        .await
        .expect("cancelled sibling must not leak its parent reservation")
        .unwrap();
        drop(replacement);
        drop(child_two);
    }

    fn upsert_row(
        store: &BackendStore<'_>,
        purpose: BackendPurpose,
        mode: BackendMode,
        endpoint: Option<&str>,
    ) {
        upsert_row_with_spec(store, purpose, mode, endpoint, serde_json::json!({}));
    }

    fn upsert_row_with_spec(
        store: &BackendStore<'_>,
        purpose: BackendPurpose,
        mode: BackendMode,
        endpoint: Option<&str>,
        model_spec_json: serde_json::Value,
    ) {
        store
            .upsert(
                &BackendUpsert {
                    purpose,
                    inference_backend: "service-vllm".into(),
                    model_spec_json,
                    gpu_id: None,
                    endpoint: endpoint.map(String::from),
                    notes: None,
                    reasoning_enabled: false,
                    mode,
                },
                100,
            )
            .unwrap();
    }

    #[test]
    fn no_row_no_bootstrap_returns_none() {
        let db = fresh_db();
        let resolver = InferenceResolver::new(None);
        assert!(resolver.resolve(&db, BackendPurpose::Standard).is_none());
    }

    #[test]
    fn no_row_with_bootstrap_returns_bootstrap() {
        let db = fresh_db();
        let bootstrap = Arc::new(InferenceClient::new("http://boot:8000/v1"));
        let resolver = InferenceResolver::new(Some(bootstrap.clone()));
        let got = resolver.resolve(&db, BackendPurpose::Standard).unwrap();
        assert_eq!(got.endpoint, "http://boot:8000/v1");
        assert_eq!(got.source, "bootstrap");
        // No DB row, no bootstrap_model → default fallback.
        assert_eq!(got.model_id, DEFAULT_FALLBACK_MODEL);
    }

    #[test]
    fn external_row_with_endpoint_returns_row_url() {
        let db = fresh_db();
        execlaw_core::local_endpoint_policy::LocalEndpointPolicyStore::new(&db)
            .approve_for(
                execlaw_core::local_endpoint_policy::EndpointApprovalScope::LocalInference,
                execlaw_core::local_endpoint_policy::EndpointApprovalKind::Cidr,
                "192.168.1.0/24",
                chrono::Utc::now().timestamp(),
            )
            .unwrap();
        let store = BackendStore::new(&db);
        upsert_row(
            &store,
            BackendPurpose::Standard,
            BackendMode::External,
            Some("http://192.168.1.50:8000/v1"),
        );
        let bootstrap = Arc::new(InferenceClient::new("http://boot:8000/v1"));
        let resolver = InferenceResolver::new(Some(bootstrap));
        let got = resolver.resolve(&db, BackendPurpose::Standard).unwrap();
        assert_eq!(got.endpoint, "http://192.168.1.50:8000/v1");
        assert_eq!(got.source, "db");
    }

    #[test]
    fn external_ollama_row_uses_native_api_and_configured_tag() {
        let db = fresh_db();
        let store = BackendStore::new(&db);
        upsert_row_with_spec(
            &store,
            BackendPurpose::Standard,
            BackendMode::External,
            Some("http://127.0.0.1:11434"),
            serde_json::json!({ "binary_hint": "ollama", "model": "qwen3:8b", "context_tokens": 8_192 }),
        );
        let resolver = InferenceResolver::new(None);
        let got = resolver.resolve(&db, BackendPurpose::Standard).unwrap();
        assert_eq!(got.model_id, "qwen3:8b");
        assert_eq!(got.endpoint, "http://127.0.0.1:11434");
        assert_eq!(got.client.engine, InferenceEngine::Ollama);
        assert_eq!(got.ollama_context_tokens, Some(8_192));
    }

    #[test]
    fn external_openai_compatible_row_carries_reasoning_effort() {
        let db = fresh_db();
        let store = BackendStore::new(&db);
        upsert_row_with_spec(
            &store,
            BackendPurpose::Standard,
            BackendMode::External,
            Some("http://127.0.0.1:11434/v1"),
            serde_json::json!({"model":"qwen3:8b","reasoning_effort":"none"}),
        );
        let resolved = InferenceResolver::new(None)
            .resolve(&db, BackendPurpose::Standard)
            .unwrap();
        assert_eq!(resolved.client.engine, InferenceEngine::OpenAICompat);
        assert_eq!(resolved.reasoning_effort.as_deref(), Some("none"));
    }

    #[test]
    fn external_row_without_endpoint_falls_through_to_bootstrap() {
        // External rows that the operator typed an inference_backend
        // for but never set a URL on still work — the bootstrap
        // catches them. Pre-Phase-12 behaviour.
        let db = fresh_db();
        let store = BackendStore::new(&db);
        upsert_row(
            &store,
            BackendPurpose::Standard,
            BackendMode::External,
            None,
        );
        let bootstrap = Arc::new(InferenceClient::new("http://boot:8000/v1"));
        let resolver = InferenceResolver::new(Some(bootstrap));
        let got = resolver.resolve(&db, BackendPurpose::Standard).unwrap();
        assert_eq!(got.endpoint, "http://boot:8000/v1");
        assert_eq!(got.source, "bootstrap");
    }

    #[test]
    fn managed_row_without_endpoint_returns_none_even_with_bootstrap() {
        // Managed rows whose supervisor hasn't come up yet must NOT
        // silently fall through to a different URL — the operator
        // explicitly chose managed mode. Surfacing None drops to
        // the stub turn so the issue is visible.
        let db = fresh_db();
        let store = BackendStore::new(&db);
        upsert_row(&store, BackendPurpose::Standard, BackendMode::Managed, None);
        let bootstrap = Arc::new(InferenceClient::new("http://boot:8000/v1"));
        let resolver = InferenceResolver::new(Some(bootstrap));
        assert!(resolver.resolve(&db, BackendPurpose::Standard).is_none());
    }

    #[test]
    fn managed_row_with_endpoint_and_model_arg_returns_both_atomically() {
        // Steady-state happy path AND the regression case from the
        // 2026-05-13 hang: supervisor wrote http://127.0.0.1:8101/v1
        // back AND `--model=QuantTrio/Qwen3.6-27B-AWQ` is in the
        // args. Pre-rework these came from separate state
        // (config_backends.endpoint + state.config.model_id) and
        // drifted out of sync; the chat path sent model=Qwen3.5
        // while vLLM was loaded with Qwen3.6 and 404'd. Now they
        // come from the same row read.
        let db = fresh_db();
        let store = BackendStore::new(&db);
        upsert_row_with_spec(
            &store,
            BackendPurpose::Standard,
            BackendMode::Managed,
            Some("http://127.0.0.1:8101/v1"),
            serde_json::json!({
                "image": "vllm/vllm-openai:v0.20.2",
                "args": ["--model=QuantTrio/Qwen3.6-27B-AWQ"],
            }),
        );
        let resolver = InferenceResolver::new(None);
        let got = resolver.resolve(&db, BackendPurpose::Standard).unwrap();
        assert_eq!(got.endpoint, "http://127.0.0.1:8101/v1");
        assert_eq!(got.model_id, "QuantTrio/Qwen3.6-27B-AWQ");
        assert_eq!(got.source, "db");
    }

    #[test]
    fn extract_model_arg_handles_both_arg_shapes() {
        // Operators / wizard / supervisor write args in two forms:
        //   ["--model=X"]            (single token)
        //   ["--model", "X"]         (separate tokens)
        // Both must resolve to the same model id.
        assert_eq!(
            super::extract_model_arg(&serde_json::json!({
                "args": ["--model=Qwen3.6-27B"]
            })),
            Some("Qwen3.6-27B".to_owned())
        );
        assert_eq!(
            super::extract_model_arg(&serde_json::json!({
                "args": ["--model", "Qwen3.6-27B"]
            })),
            Some("Qwen3.6-27B".to_owned())
        );
        // Missing model arg → None (caller falls back).
        assert_eq!(
            super::extract_model_arg(&serde_json::json!({
                "args": ["--enable-prefix-caching"]
            })),
            None
        );
        // Empty args / no args → None.
        assert_eq!(
            super::extract_model_arg(&serde_json::json!({ "args": [] })),
            None
        );
        assert_eq!(super::extract_model_arg(&serde_json::json!({})), None);
        // `--model=` with empty value → None, not Some("").
        assert_eq!(
            super::extract_model_arg(&serde_json::json!({
                "args": ["--model="]
            })),
            None
        );
    }

    #[test]
    fn extract_model_arg_reads_top_level_model_field_first() {
        // The Apple-Silicon Ollama preset writes the tag at the
        // top level (`model_spec_json.model`) rather than as a
        // CLI arg — Ollama's `serve` invocation doesn't take a
        // --model flag. Without this branch the resolver fell
        // through to DEFAULT_FALLBACK_MODEL (a vLLM HF id) and
        // Ollama 404'd at chat time.
        assert_eq!(
            super::extract_model_arg(&serde_json::json!({
                "runtime": "native",
                "binary_hint": "ollama",
                "args": ["serve"],
                "model": "qwen2.5:14b-instruct-q4_K_M",
            })),
            Some("qwen2.5:14b-instruct-q4_K_M".to_owned())
        );
    }

    #[test]
    fn extract_model_arg_top_level_takes_precedence_over_model_arg() {
        // Defensive: a future row that has BOTH the top-level
        // field AND a stale `--model=` arg should resolve to the
        // top-level value (which is what the wizard is writing
        // today). Keeps the precedence stable if someone
        // hand-edits a row.
        assert_eq!(
            super::extract_model_arg(&serde_json::json!({
                "model": "qwen2.5:14b-instruct-q4_K_M",
                "args": ["--model=Qwen3.6-27B"],
            })),
            Some("qwen2.5:14b-instruct-q4_K_M".to_owned())
        );
    }

    #[test]
    fn extract_model_arg_skips_blank_top_level_model() {
        // An empty-string `model` field shouldn't trap the
        // resolver into sending the empty string — fall through
        // to args / bootstrap as if the field were absent.
        assert_eq!(
            super::extract_model_arg(&serde_json::json!({
                "model": "",
                "args": ["--model=Qwen3.6-27B"],
            })),
            Some("Qwen3.6-27B".to_owned())
        );
        assert_eq!(
            super::extract_model_arg(&serde_json::json!({
                "model": "   ",
            })),
            None
        );
    }

    #[test]
    fn reasoning_enabled_round_trips_from_row() {
        // Pre-2026-05-13 four separate chat sites each re-read this
        // bool via `.ok().flatten().map(|r| r.reasoning_enabled)`,
        // opening a drift window (resolver row at t0, reasoning row
        // at t1) AND silently masking BLOB-decode errors. The field
        // now rides on `ResolvedInference` from the same row read.
        let db = fresh_db();
        let store = BackendStore::new(&db);
        store
            .upsert(
                &BackendUpsert {
                    purpose: BackendPurpose::Standard,
                    inference_backend: "service-vllm".into(),
                    model_spec_json: serde_json::json!({ "args": ["--model=Q"] }),
                    gpu_id: None,
                    endpoint: Some("http://127.0.0.1:8101/v1".into()),
                    notes: None,
                    reasoning_enabled: true,
                    mode: BackendMode::External,
                },
                100,
            )
            .unwrap();
        let resolver = InferenceResolver::new(None);
        let got = resolver.resolve(&db, BackendPurpose::Standard).unwrap();
        assert!(
            got.reasoning_enabled,
            "row reasoning_enabled = true must reach the resolved struct"
        );

        // And toggle off: same flow, false comes through.
        store
            .upsert(
                &BackendUpsert {
                    purpose: BackendPurpose::Standard,
                    inference_backend: "service-vllm".into(),
                    model_spec_json: serde_json::json!({ "args": ["--model=Q"] }),
                    gpu_id: None,
                    endpoint: Some("http://127.0.0.1:8101/v1".into()),
                    notes: None,
                    reasoning_enabled: false,
                    mode: BackendMode::External,
                },
                101,
            )
            .unwrap();
        let got = resolver.resolve(&db, BackendPurpose::Standard).unwrap();
        assert!(
            !got.reasoning_enabled,
            "row reasoning_enabled = false must reach the resolved struct"
        );
    }

    #[test]
    fn bootstrap_resolution_defaults_reasoning_off() {
        // The bootstrap path has no row to read; reasoning defaults
        // OFF. Operators who want it on must configure the Standard
        // backend row.
        let db = fresh_db();
        let bootstrap = Arc::new(InferenceClient::new("http://boot:8000/v1"));
        let resolver = InferenceResolver::new(Some(bootstrap));
        let got = resolver.resolve(&db, BackendPurpose::Standard).unwrap();
        assert_eq!(got.source, "bootstrap");
        assert!(!got.reasoning_enabled);
    }

    #[test]
    fn empty_string_endpoint_treated_as_unset() {
        // Some upsert paths can leave endpoint = "" instead of
        // null. Treat both equivalently so a stale empty string
        // doesn't synthesize an InferenceClient with a useless URL.
        let db = fresh_db();
        let store = BackendStore::new(&db);
        upsert_row(
            &store,
            BackendPurpose::Standard,
            BackendMode::External,
            Some(""),
        );
        let bootstrap = Arc::new(InferenceClient::new("http://boot:8000/v1"));
        let resolver = InferenceResolver::new(Some(bootstrap));
        let got = resolver.resolve(&db, BackendPurpose::Standard).unwrap();
        assert_eq!(got.endpoint, "http://boot:8000/v1");
        assert_eq!(got.source, "bootstrap");
    }

    #[test]
    fn resolve_picks_up_supervisor_endpoint_after_set_endpoint_writeback() {
        // The whole point of Phase 12.E: a fresh row with no
        // endpoint resolves to None (managed) → supervisor calls
        // BackendStore::set_endpoint → next resolve picks up the
        // new URL without any cache invalidation.
        let db = fresh_db();
        let store = BackendStore::new(&db);
        upsert_row(&store, BackendPurpose::Standard, BackendMode::Managed, None);
        let resolver = InferenceResolver::new(None);
        assert!(resolver.resolve(&db, BackendPurpose::Standard).is_none());

        store
            .set_endpoint(
                BackendPurpose::Standard,
                Some("http://127.0.0.1:8101/v1"),
                200,
            )
            .unwrap();
        let got = resolver.resolve(&db, BackendPurpose::Standard).unwrap();
        assert_eq!(got.endpoint, "http://127.0.0.1:8101/v1");
        assert_eq!(got.source, "db");
    }

    #[test]
    fn bootstrap_model_override_wins_when_row_has_no_model_arg() {
        // Operator who passes --inference-url AND --inference-model
        // at boot wants those values used even when no DB row exists.
        let db = fresh_db();
        let bootstrap = Arc::new(InferenceClient::new("http://boot:8000/v1"));
        let resolver = InferenceResolver::new(Some(bootstrap))
            .with_bootstrap_model(Some("operator/Custom-7B".to_owned()));
        let got = resolver.resolve(&db, BackendPurpose::Standard).unwrap();
        assert_eq!(got.model_id, "operator/Custom-7B");
        assert_eq!(got.source, "bootstrap");
    }
}
