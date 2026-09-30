//! Inference observability + per-consumer attribution (M5).
//!
//! Wraps async LLM calls so the `/admin/inference` page can answer
//! "is the model the bottleneck?" with numbers instead of vibes.
//! Tracks four things per consumer:
//!
//!   * `in_flight` — currently outstanding calls (peak observable
//!     via the snapshot endpoint).
//!   * `total_calls` — monotonic counter, lifetime of process.
//!   * `total_failures` — monotonic counter of `Err(_)` outcomes.
//!   * `last_durations_ms` — 256-deep ring buffer for p50/p95 stats.
//!
//! Per-consumer attribution: callers tag their wrapped call with
//! [`InferenceConsumer`] so the snapshot endpoint can slice by who's
//! driving the load (chat / routines / research / automations).
//!
//! Locking model: one `Mutex<HashMap<..>>` for the whole metrics
//! state. Lock is held briefly (microseconds per call) and the
//! contention budget is well under the inference call's own latency
//! (typically 100ms+ for LLM round-trips), so this is fine in
//! practice. If the lock ever shows up in a flamegraph, refactor to
//! per-consumer DashMap entries with atomic counters.

use serde::Serialize;
use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use utoipa::ToSchema;

/// Caller-supplied tag identifying who's driving the inference call.
/// Snapshot endpoint slices by this so an operator can tell whether
/// chat, research, or automations is the heavy hitter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum InferenceConsumer {
    /// The user-facing chat turn loop.
    Chat,
    /// Cron-scheduled routine fires.
    Routines,
    /// Deep-research subsystem (plan + gather + synthesize).
    Research,
    /// Automation `AskAgent` node dispatches.
    Automations,
    /// Skill-capture / reuse-update / other background workers that
    /// don't fit one of the named consumers.
    Other,
}

/// Stable phase labels for per-turn latency diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum InferencePhase {
    PromptAssembly,
    PrefillDecode,
    ToolWait,
    Retry,
    StreamDelay,
}

impl InferencePhase {
    fn as_str(self) -> &'static str {
        match self {
            Self::PromptAssembly => "prompt_assembly",
            Self::PrefillDecode => "prefill_decode",
            Self::ToolWait => "tool_wait",
            Self::Retry => "retry",
            Self::StreamDelay => "stream_delay",
        }
    }
}

impl InferenceConsumer {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Routines => "routines",
            Self::Research => "research",
            Self::Automations => "automations",
            Self::Other => "other",
        }
    }
}

/// Ring-buffer cap for per-consumer duration samples. 256 is enough
/// for stable p50/p95 estimates without unbounded memory growth.
const DURATIONS_RING_CAP: usize = 256;
const MIN_REGRESSION_SAMPLES: usize = 16;
const REGRESSION_BUDGET_PERCENT: u8 = 25;

#[derive(Default)]
struct ConsumerCounters {
    in_flight: usize,
    total_calls: u64,
    total_failures: u64,
    last_durations_ms: VecDeque<u32>,
}

/// Clone-able handle. Stored in `AppState`; per-call sites wrap their
/// async LLM call with `.observe(consumer, fut)` to record stats.
#[derive(Clone)]
pub struct InferenceMetrics {
    inner: Arc<Mutex<HashMap<InferenceConsumer, ConsumerCounters>>>,
    phases: Arc<Mutex<HashMap<(InferenceConsumer, InferencePhase), VecDeque<u32>>>>,
    contexts: Arc<Mutex<HashMap<(InferenceConsumer, InferencePhase), VecDeque<ContextSample>>>>,
}

#[derive(Debug, Clone, Copy)]
struct ContextSample {
    serialized_bytes: u64,
    estimated_tokens: u64,
}

impl Default for InferenceMetrics {
    fn default() -> Self {
        Self::new()
    }
}

impl InferenceMetrics {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            phases: Arc::new(Mutex::new(HashMap::new())),
            contexts: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Wrap an async LLM call. Increments `in_flight` before the
    /// call, records the duration + outcome after, and decrements
    /// `in_flight`. Generic over the call's `Result<T, E>` shape so
    /// any inference consumer can pass its native result type.
    ///
    /// **Panic-safe.** If the wrapped future panics, the
    /// [`InflightGuard`] RAII handle decrements `in_flight` during
    /// unwind so the counter doesn't leak. The other counters
    /// (total_calls, total_failures, durations) only update on
    /// normal completion — a panicked call isn't "completed" in
    /// any meaningful sense, so it's correctly absent from the
    /// totals.
    pub async fn observe<T, E, F>(&self, consumer: InferenceConsumer, fut: F) -> Result<T, E>
    where
        F: Future<Output = Result<T, E>>,
    {
        let start = Instant::now();
        {
            let mut g = self.inner.lock().unwrap();
            g.entry(consumer).or_default().in_flight += 1;
        }
        // RAII: ensures `in_flight` decrements on any exit path,
        // including a panic propagating through `.await`.
        let _guard = InflightGuard {
            metrics: self,
            consumer,
        };
        let result = fut.await;
        let ms = start.elapsed().as_millis().min(u32::MAX as u128) as u32;
        // Normal-completion bookkeeping. The guard's drop handles
        // in_flight; we only do the "this call ended cleanly" stats
        // here, so a panic short-circuits these without leaving
        // partial state.
        {
            let mut g = self.inner.lock().unwrap();
            let c = g.entry(consumer).or_default();
            c.total_calls += 1;
            if result.is_err() {
                c.total_failures += 1;
            }
            if c.last_durations_ms.len() >= DURATIONS_RING_CAP {
                c.last_durations_ms.pop_front();
            }
            c.last_durations_ms.push_back(ms);
        }
        result
    }

    /// Record one phase duration without retaining request or prompt content.
    pub fn record_phase(
        &self,
        consumer: InferenceConsumer,
        phase: InferencePhase,
        duration: std::time::Duration,
    ) {
        let mut phases = self.phases.lock().unwrap();
        let samples = phases.entry((consumer, phase)).or_default();
        if samples.len() >= DURATIONS_RING_CAP {
            samples.pop_front();
        }
        samples.push_back(duration.as_millis().min(u32::MAX as u128) as u32);
    }

    /// Record numeric context-size estimates for a serialized inference request.
    /// Prompt bytes and text are never retained. Token estimates use the same
    /// conservative four-bytes-per-token heuristic as history budgeting.
    pub fn record_context(
        &self,
        consumer: InferenceConsumer,
        phase: InferencePhase,
        serialized_bytes: usize,
    ) {
        let bytes = serialized_bytes as u64;
        let sample = ContextSample {
            serialized_bytes: bytes,
            estimated_tokens: bytes.div_ceil(4),
        };
        let mut contexts = self.contexts.lock().unwrap();
        let samples = contexts.entry((consumer, phase)).or_default();
        if samples.len() >= DURATIONS_RING_CAP {
            samples.pop_front();
        }
        samples.push_back(sample);
    }

    /// Capture a JSON-friendly view of the current state. Sorts
    /// consumers by their wire-name for deterministic rendering.
    pub fn snapshot(&self) -> MetricsSnapshot {
        let g = self.inner.lock().unwrap();
        let mut consumers: Vec<ConsumerSnapshot> = g
            .iter()
            .map(|(consumer, c)| {
                let mut sorted: Vec<u32> = c.last_durations_ms.iter().copied().collect();
                sorted.sort_unstable();
                ConsumerSnapshot {
                    consumer: *consumer,
                    in_flight: c.in_flight,
                    total_calls: c.total_calls,
                    total_failures: c.total_failures,
                    sample_count: sorted.len(),
                    p50_ms: percentile(&sorted, 50),
                    p95_ms: percentile(&sorted, 95),
                }
            })
            .collect();
        consumers.sort_by_key(|c| c.consumer.as_str());
        let phases = self.phases.lock().unwrap();
        let mut phase_snapshots = phases
            .iter()
            .map(|((consumer, phase), samples)| {
                let mut sorted: Vec<u32> = samples.iter().copied().collect();
                sorted.sort_unstable();
                PhaseSnapshot {
                    consumer: *consumer,
                    phase: *phase,
                    sample_count: sorted.len(),
                    p50_ms: percentile(&sorted, 50),
                    p95_ms: percentile(&sorted, 95),
                    baseline_p95_ms: rolling_baseline(&samples.iter().copied().collect::<Vec<_>>()),
                    regression_budget_percent: REGRESSION_BUDGET_PERCENT,
                    regression_detected: regression_detected(
                        &samples.iter().copied().collect::<Vec<_>>(),
                    ),
                }
            })
            .collect::<Vec<_>>();
        phase_snapshots.sort_by_key(|entry| (entry.consumer.as_str(), entry.phase.as_str()));
        let contexts = self.contexts.lock().unwrap();
        let mut context_snapshots = contexts
            .iter()
            .map(|((consumer, phase), samples)| {
                let bytes = sorted_context_values(samples, |sample| sample.serialized_bytes);
                let tokens = sorted_context_values(samples, |sample| sample.estimated_tokens);
                ContextSnapshot {
                    consumer: *consumer,
                    phase: *phase,
                    sample_count: samples.len(),
                    p50_serialized_bytes: percentile_u64(&bytes, 50),
                    p95_serialized_bytes: percentile_u64(&bytes, 95),
                    p50_estimated_tokens: percentile_u64(&tokens, 50),
                    p95_estimated_tokens: percentile_u64(&tokens, 95),
                    baseline_p95_estimated_tokens: rolling_baseline_u64(
                        &samples
                            .iter()
                            .map(|sample| sample.estimated_tokens)
                            .collect::<Vec<_>>(),
                    ),
                    regression_budget_percent: REGRESSION_BUDGET_PERCENT,
                    regression_detected: regression_detected_u64(
                        &samples
                            .iter()
                            .map(|sample| sample.estimated_tokens)
                            .collect::<Vec<_>>(),
                    ),
                }
            })
            .collect::<Vec<_>>();
        context_snapshots.sort_by_key(|entry| (entry.consumer.as_str(), entry.phase.as_str()));
        MetricsSnapshot {
            consumers,
            phases: phase_snapshots,
            contexts: context_snapshots,
        }
    }
}

/// RAII guard that decrements `in_flight` on drop. Panic-safe: if
/// the awaited future unwinds through `observe`, the guard's drop
/// still runs, leaving the in-flight counter consistent.
struct InflightGuard<'a> {
    metrics: &'a InferenceMetrics,
    consumer: InferenceConsumer,
}

impl Drop for InflightGuard<'_> {
    fn drop(&mut self) {
        // `lock().unwrap()` panicking inside drop is the only failure
        // mode and it'd be a poison error — which only happens if
        // another panic already poisoned the mutex. Letting drop
        // double-panic in that case aborts the process, which is
        // the desired behavior (the metrics state is suspect anyway).
        let mut g = self.metrics.inner.lock().unwrap();
        let c = g.entry(self.consumer).or_default();
        c.in_flight = c.in_flight.saturating_sub(1);
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct MetricsSnapshot {
    pub consumers: Vec<ConsumerSnapshot>,
    pub phases: Vec<PhaseSnapshot>,
    pub contexts: Vec<ContextSnapshot>,
}

/// Bounded prompt/context size distribution, estimated from serialized request
/// bytes; only numeric aggregates are retained.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ContextSnapshot {
    pub consumer: InferenceConsumer,
    pub phase: InferencePhase,
    pub sample_count: usize,
    pub p50_serialized_bytes: Option<u64>,
    pub p95_serialized_bytes: Option<u64>,
    pub p50_estimated_tokens: Option<u64>,
    pub p95_estimated_tokens: Option<u64>,
    pub baseline_p95_estimated_tokens: Option<u64>,
    pub regression_budget_percent: u8,
    pub regression_detected: Option<bool>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PhaseSnapshot {
    pub consumer: InferenceConsumer,
    pub phase: InferencePhase,
    pub sample_count: usize,
    pub p50_ms: Option<u32>,
    pub p95_ms: Option<u32>,
    pub baseline_p95_ms: Option<u32>,
    pub regression_budget_percent: u8,
    pub regression_detected: Option<bool>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ConsumerSnapshot {
    pub consumer: InferenceConsumer,
    pub in_flight: usize,
    pub total_calls: u64,
    pub total_failures: u64,
    pub sample_count: usize,
    /// `None` when there are no samples yet.
    pub p50_ms: Option<u32>,
    pub p95_ms: Option<u32>,
}

fn percentile(sorted: &[u32], pct: u8) -> Option<u32> {
    if sorted.is_empty() {
        return None;
    }
    let rank = (pct as f64 / 100.0 * (sorted.len() as f64 - 1.0)).round() as usize;
    sorted.get(rank.min(sorted.len() - 1)).copied()
}

fn sorted_context_values(
    samples: &VecDeque<ContextSample>,
    field: impl Fn(&ContextSample) -> u64,
) -> Vec<u64> {
    let mut values = samples.iter().map(field).collect::<Vec<_>>();
    values.sort_unstable();
    values
}

fn percentile_u64(sorted: &[u64], pct: u8) -> Option<u64> {
    if sorted.is_empty() {
        return None;
    }
    let rank = (pct as f64 / 100.0 * (sorted.len() as f64 - 1.0)).round() as usize;
    sorted.get(rank.min(sorted.len() - 1)).copied()
}

fn rolling_baseline(samples: &[u32]) -> Option<u32> {
    if samples.len() < MIN_REGRESSION_SAMPLES {
        return None;
    }
    let midpoint = samples.len() / 2;
    let mut older = samples[..midpoint].to_vec();
    older.sort_unstable();
    percentile(&older, 95)
}

fn regression_detected(samples: &[u32]) -> Option<bool> {
    let baseline = rolling_baseline(samples)?;
    let midpoint = samples.len() / 2;
    let mut recent = samples[midpoint..].to_vec();
    recent.sort_unstable();
    let current = percentile(&recent, 95)?;
    Some(current as u64 * 100 > baseline as u64 * (100 + REGRESSION_BUDGET_PERCENT as u64))
}

fn rolling_baseline_u64(samples: &[u64]) -> Option<u64> {
    if samples.len() < MIN_REGRESSION_SAMPLES {
        return None;
    }
    let midpoint = samples.len() / 2;
    let mut older = samples[..midpoint].to_vec();
    older.sort_unstable();
    percentile_u64(&older, 95)
}

fn regression_detected_u64(samples: &[u64]) -> Option<bool> {
    let baseline = rolling_baseline_u64(samples)?;
    let midpoint = samples.len() / 2;
    let mut recent = samples[midpoint..].to_vec();
    recent.sort_unstable();
    let current = percentile_u64(&recent, 95)?;
    Some(
        current.saturating_mul(100)
            > baseline.saturating_mul(100 + REGRESSION_BUDGET_PERCENT as u64),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn ok_call(ms: u64) -> Result<&'static str, &'static str> {
        tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
        Ok("ok")
    }

    async fn err_call(ms: u64) -> Result<&'static str, &'static str> {
        tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
        Err("boom")
    }

    #[tokio::test]
    async fn observe_records_a_successful_call() {
        let m = InferenceMetrics::new();
        let result = m.observe(InferenceConsumer::Automations, ok_call(5)).await;
        assert!(result.is_ok());
        let snap = m.snapshot();
        let auto = snap
            .consumers
            .iter()
            .find(|c| c.consumer == InferenceConsumer::Automations)
            .unwrap();
        assert_eq!(auto.in_flight, 0);
        assert_eq!(auto.total_calls, 1);
        assert_eq!(auto.total_failures, 0);
        assert_eq!(auto.sample_count, 1);
        assert!(auto.p50_ms.is_some());
    }

    #[test]
    fn phase_metrics_keep_bounded_aggregate_samples_without_input_content() {
        let metrics = InferenceMetrics::new();
        metrics.record_phase(
            InferenceConsumer::Chat,
            InferencePhase::PromptAssembly,
            std::time::Duration::from_millis(5),
        );
        metrics.record_phase(
            InferenceConsumer::Chat,
            InferencePhase::PromptAssembly,
            std::time::Duration::from_millis(9),
        );
        let snapshot = metrics.snapshot();
        let prompt = snapshot
            .phases
            .iter()
            .find(|entry| entry.phase == InferencePhase::PromptAssembly)
            .unwrap();
        assert_eq!(prompt.sample_count, 2);
        assert_eq!(prompt.p50_ms, Some(9));
        assert_eq!(prompt.p95_ms, Some(9));
    }

    #[test]
    fn context_metrics_keep_only_bounded_numeric_estimates() {
        let metrics = InferenceMetrics::new();
        metrics.record_context(InferenceConsumer::Chat, InferencePhase::PromptAssembly, 40);
        metrics.record_context(InferenceConsumer::Chat, InferencePhase::PromptAssembly, 100);
        let snapshot = metrics.snapshot();
        let context = snapshot
            .contexts
            .iter()
            .find(|entry| {
                entry.consumer == InferenceConsumer::Chat
                    && entry.phase == InferencePhase::PromptAssembly
            })
            .unwrap();
        assert_eq!(context.sample_count, 2);
        assert_eq!(context.p50_serialized_bytes, Some(100));
        assert_eq!(context.p95_serialized_bytes, Some(100));
        assert_eq!(context.p50_estimated_tokens, Some(25));
        assert_eq!(context.p95_estimated_tokens, Some(25));
    }

    #[test]
    fn context_metrics_ring_buffer_is_bounded() {
        let metrics = InferenceMetrics::new();
        for bytes in 0..(DURATIONS_RING_CAP + 10) {
            metrics.record_context(
                InferenceConsumer::Chat,
                InferencePhase::PromptAssembly,
                bytes,
            );
        }
        let context = metrics.snapshot().contexts.remove(0);
        assert_eq!(context.sample_count, DURATIONS_RING_CAP);
        assert_eq!(context.p50_serialized_bytes, Some(138));
    }

    #[test]
    fn phase_and_context_budgets_flag_a_twenty_five_percent_p95_increase() {
        let metrics = InferenceMetrics::new();
        for _ in 0..16 {
            metrics.record_phase(
                InferenceConsumer::Chat,
                InferencePhase::PromptAssembly,
                std::time::Duration::from_millis(100),
            );
            metrics.record_context(InferenceConsumer::Chat, InferencePhase::PromptAssembly, 400);
        }
        for _ in 0..16 {
            metrics.record_phase(
                InferenceConsumer::Chat,
                InferencePhase::PromptAssembly,
                std::time::Duration::from_millis(126),
            );
            metrics.record_context(InferenceConsumer::Chat, InferencePhase::PromptAssembly, 504);
        }
        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.phases[0].baseline_p95_ms, Some(100));
        assert_eq!(snapshot.phases[0].regression_budget_percent, 25);
        assert_eq!(snapshot.phases[0].regression_detected, Some(true));
        assert_eq!(
            snapshot.contexts[0].baseline_p95_estimated_tokens,
            Some(100)
        );
        assert_eq!(snapshot.contexts[0].regression_detected, Some(true));
    }

    #[tokio::test]
    async fn observe_records_failures_separately() {
        let m = InferenceMetrics::new();
        let _ = m.observe(InferenceConsumer::Chat, err_call(1)).await;
        let _ = m.observe(InferenceConsumer::Chat, ok_call(1)).await;
        let snap = m.snapshot();
        let c = snap
            .consumers
            .iter()
            .find(|c| c.consumer == InferenceConsumer::Chat)
            .unwrap();
        assert_eq!(c.total_calls, 2);
        assert_eq!(c.total_failures, 1);
    }

    #[tokio::test]
    async fn in_flight_decrements_when_call_ends_even_on_error() {
        let m = InferenceMetrics::new();
        let _ = m.observe(InferenceConsumer::Research, err_call(1)).await;
        let snap = m.snapshot();
        let r = snap
            .consumers
            .iter()
            .find(|c| c.consumer == InferenceConsumer::Research)
            .unwrap();
        assert_eq!(r.in_flight, 0);
    }

    #[tokio::test]
    async fn snapshot_groups_by_consumer() {
        let m = InferenceMetrics::new();
        m.observe(InferenceConsumer::Chat, ok_call(1))
            .await
            .unwrap();
        m.observe(InferenceConsumer::Automations, ok_call(1))
            .await
            .unwrap();
        m.observe(InferenceConsumer::Automations, ok_call(1))
            .await
            .unwrap();
        let snap = m.snapshot();
        let auto = snap
            .consumers
            .iter()
            .find(|c| c.consumer == InferenceConsumer::Automations)
            .unwrap();
        let chat = snap
            .consumers
            .iter()
            .find(|c| c.consumer == InferenceConsumer::Chat)
            .unwrap();
        assert_eq!(auto.total_calls, 2);
        assert_eq!(chat.total_calls, 1);
    }

    #[tokio::test]
    async fn percentiles_are_stable_with_one_sample() {
        let m = InferenceMetrics::new();
        m.observe(InferenceConsumer::Other, ok_call(10))
            .await
            .unwrap();
        let snap = m.snapshot();
        let s = snap
            .consumers
            .iter()
            .find(|c| c.consumer == InferenceConsumer::Other)
            .unwrap();
        let p50 = s.p50_ms.unwrap();
        let p95 = s.p95_ms.unwrap();
        // With one sample, both percentiles are that sample.
        assert_eq!(p50, p95);
        assert!(p50 >= 10);
    }

    #[tokio::test]
    async fn observe_decrements_in_flight_when_wrapped_future_panics() {
        // Panic-safety: even if the LLM call panics mid-flight, the
        // RAII guard must decrement in_flight so the counter
        // converges to 0 after the panic propagates.
        use futures::FutureExt;
        let m = InferenceMetrics::new();
        let m_clone = m.clone();
        let res = std::panic::AssertUnwindSafe(async move {
            m_clone
                .observe::<&str, &str, _>(InferenceConsumer::Automations, async {
                    panic!("simulated LLM crash");
                })
                .await
        })
        .catch_unwind()
        .await;
        assert!(res.is_err(), "panic must propagate through observe");
        // total_calls is NOT incremented on panic (the call didn't
        // complete), but in_flight is back to 0.
        let snap = m.snapshot();
        let auto = snap
            .consumers
            .iter()
            .find(|c| c.consumer == InferenceConsumer::Automations)
            .unwrap();
        assert_eq!(auto.in_flight, 0, "guard must decrement on panic");
        assert_eq!(
            auto.total_calls, 0,
            "panicked call must not count toward totals"
        );
    }

    #[tokio::test]
    async fn ring_buffer_caps_at_256_samples() {
        let m = InferenceMetrics::new();
        for _ in 0..(DURATIONS_RING_CAP + 50) {
            m.observe(InferenceConsumer::Routines, ok_call(0))
                .await
                .unwrap();
        }
        let snap = m.snapshot();
        let r = snap
            .consumers
            .iter()
            .find(|c| c.consumer == InferenceConsumer::Routines)
            .unwrap();
        assert_eq!(r.total_calls, (DURATIONS_RING_CAP + 50) as u64);
        assert_eq!(r.sample_count, DURATIONS_RING_CAP);
    }

    #[test]
    fn percentile_helper_handles_empty_and_singleton() {
        assert_eq!(percentile(&[], 50), None);
        assert_eq!(percentile(&[7], 50), Some(7));
        assert_eq!(percentile(&[7], 95), Some(7));
        // 0..=99 → p50 ≈ middle, p95 ≈ tail.
        let v: Vec<u32> = (0..100).collect();
        let p50 = percentile(&v, 50).unwrap();
        let p95 = percentile(&v, 95).unwrap();
        assert!(p50 >= 49 && p50 <= 50);
        assert!(p95 >= 94 && p95 <= 95);
    }
}
