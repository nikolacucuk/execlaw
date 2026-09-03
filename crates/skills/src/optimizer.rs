//! Offline skill optimizer — Phase D (§11).
//!
//! After a skill has been used `REUSE_THRESHOLD` times with "success"
//! outcomes, this module collects a sample of recent successful
//! trajectories and asks the Small backend to produce a candidate
//! improvement proposal via the improvement-prompt path already used
//! by [`crate::reuse_update::ReuseUpdateWorker`].
//!
//! # Design
//!
//! The optimizer is intentionally conservative:
//!
//! - It only fires at exact multiples of `REUSE_THRESHOLD` (5, 10,
//!   15, …) so it doesn't re-trigger on every additional use.
//! - It uses `SkillStore::submit_proposal` with `ProposalKind::VersionFork`
//!   — the operator always reviews before anything is promoted.
//! - `OptimizerWorker::maybe_optimize` is cheap to call: it does one
//!   `COUNT(*)` query and exits immediately when the threshold isn't
//!   crossed.
//!
//! # Integration
//!
//! Wire `OptimizerWorker::maybe_optimize` into the turn-end path of
//! `chats.rs` after `close_open_invocations`. Pass each returned
//! `(invocation_id, skill_id)` pair to `maybe_optimize`. It checks
//! the count; if the threshold is crossed it collects trajectories
//! and submits the proposal, returning the new `ProposalId`.
//!
//! Because the optimizer calls the inference backend it **must** run
//! on an async executor. Call it from a `tokio::spawn` in the turn-
//! end path so it never adds latency to the user-visible response.

use crate::model::{NewProposal, ProposalId, ProposalKind, SkillId};
use crate::sanitizer::{SanitizationReport, sanitize_step};
use crate::store::SkillStore;
use crate::summarizer::{
    SkillSummarizer, SummarizerOutput, SummarizerPrompt, build_improvement_prompt,
};
use execlaw_core::events::{EventKind, EventLog, ToolResultPayload, ToolUsePayload};
use execlaw_core::ids::{ConversationId, EventSeq};
use std::sync::Arc;
use tracing::{debug, info, warn};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Number of successful invocations that must accumulate before the
/// optimizer proposes a new version. Triggering at each exact
/// multiple (5, 10, 15, …) prevents runaway re-firing while still
/// capturing improvement signal as the skill sees more use.
pub const REUSE_THRESHOLD: u32 = 5;

/// Maximum number of recent successful invocations to sample when
/// building the improvement prompt. Larger windows give richer
/// context; smaller windows keep the prompt within token budgets.
pub const MAX_SAMPLE: usize = 3;

// ---------------------------------------------------------------------------
// OptimizerWorker
// ---------------------------------------------------------------------------

/// Holds the resources needed to decide whether to fire and to
/// submit the proposal when it does.
pub struct OptimizerWorker {
    pub store: Arc<SkillStore>,
    /// Direct database handle so we can build `EventLog` on-demand
    /// without a lifetime parameter on the struct.
    pub db: execlaw_core::Database,
    pub summarizer: Arc<dyn SkillSummarizer>,
}

impl OptimizerWorker {
    /// Called at turn-end for each `(invocation_id, skill_id)` pair
    /// returned by `SkillStore::close_open_invocations`.
    ///
    /// Returns `Some(ProposalId)` if a proposal was submitted, `None`
    /// if the threshold wasn't crossed or no improvement was found.
    ///
    /// Errors from the inference backend or the store are returned so
    /// the caller can log them; they are non-fatal for the turn.
    pub async fn maybe_optimize(&self, skill_id: SkillId) -> Result<Option<ProposalId>, String> {
        // 1. Check whether we've crossed a multiple of the threshold.
        let count = self
            .store
            .count_successful_invocations(skill_id)
            .map_err(|e| format!("optimizer: count query failed: {e}"))?;

        if count == 0 || count % REUSE_THRESHOLD != 0 {
            return Ok(None);
        }

        debug!(
            skill_id = skill_id.0,
            count, "optimizer: threshold crossed, collecting trajectories"
        );

        // 2. Load the skill's current body so we can pass it to
        //    the improvement prompt.
        let skill = self
            .store
            .get_by_id(skill_id)
            .map_err(|e| format!("optimizer: get skill failed: {e}"))?;
        let skill = match skill {
            Some(s) => s,
            None => {
                warn!(skill_id = skill_id.0, "optimizer: skill not found");
                return Ok(None);
            }
        };

        // 3. Collect recent successful conversation IDs for this skill.
        let conversations = self
            .store
            .recent_successful_conversations(skill_id, MAX_SAMPLE as u32)
            .map_err(|e| format!("optimizer: conversation query failed: {e}"))?;

        if conversations.is_empty() {
            return Ok(None);
        }

        // 4. Build a combined trajectory from the sampled conversations.
        let mut all_steps: Vec<crate::sanitizer::SanitizedStep> = Vec::new();
        let mut ordinal: u32 = 1;
        let mut report = SanitizationReport::default();

        for conversation_id in &conversations {
            let cid = ConversationId::from(conversation_id.as_str());
            let log = EventLog::new(&self.db);
            let events = match log.replay_since(&cid, EventSeq(0)) {
                Ok(evs) => evs,
                Err(e) => {
                    warn!(
                        conversation_id = %conversation_id,
                        "optimizer: replay failed: {e}"
                    );
                    continue;
                }
            };

            for ev in &events {
                if ev.kind == EventKind::ToolUse {
                    let tool_use: ToolUsePayload = match ev.decode_payload() {
                        Ok(p) => p,
                        Err(_) => continue,
                    };
                    // Find the matching tool_result.
                    let result: Result<serde_json::Value, String> = events
                        .iter()
                        .find(|e| {
                            e.kind == EventKind::ToolResult
                                && e.decode_payload::<ToolResultPayload>()
                                    .ok()
                                    .map(|r| r.ordinal == tool_use.ordinal)
                                    .unwrap_or(false)
                        })
                        .and_then(|e| e.decode_payload::<ToolResultPayload>().ok())
                        .map(|r| r.outcome)
                        .unwrap_or(Err("(no result found)".to_owned()));

                    let step = sanitize_step(
                        ordinal,
                        &tool_use.tool_name,
                        &tool_use.args_json,
                        &result,
                        &mut report,
                    );
                    all_steps.push(step);
                    ordinal += 1;
                }
            }
        }

        if all_steps.is_empty() {
            debug!(
                skill_id = skill_id.0,
                "optimizer: no sanitizable steps found"
            );
            return Ok(None);
        }

        // 5. Build the improvement prompt and call the summarizer.
        let summarizer_prompt = SummarizerPrompt {
            steps: all_steps.clone(),
            user_intent: None,
            max_tokens: 1024,
        };

        let (system, user) = build_improvement_prompt(
            &skill.name,
            &skill.current_version.body_md,
            &summarizer_prompt,
        );

        let prompt_for_summarizer = SummarizerPrompt {
            steps: all_steps,
            user_intent: Some(format!("__SYSTEM__\n{system}\n__USER__\n{user}")),
            max_tokens: 1024,
        };

        let output = self
            .summarizer
            .summarize(prompt_for_summarizer)
            .await
            .map_err(|e| format!("optimizer: summarizer failed: {e}"))?;

        match output {
            SummarizerOutput::Skip { reason } => {
                debug!(
                    skill_id = skill_id.0,
                    reason = %reason,
                    "optimizer: model says SKIP"
                );
                Ok(None)
            }
            SummarizerOutput::Draft(draft) => {
                // 6. Submit the improvement as a VersionFork proposal.
                let proposal = NewProposal {
                    kind: ProposalKind::VersionFork,
                    target_skill_id: Some(skill_id),
                    proposed_name: skill.name.clone(),
                    description: draft.description,
                    body_md: draft.body_md,
                    frontmatter_json: skill.current_version.frontmatter_json.clone(),
                    source_run_id: format!("optimizer:count={count}"),
                    trajectory_summary: Some(format!(
                        "Optimizer proposal triggered at {count} successful uses"
                    )),
                    tool_calls_observed: ordinal - 1,
                };

                let now_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0);

                let pid = self
                    .store
                    .submit_proposal(proposal, now_ms)
                    .map_err(|e| format!("optimizer: submit_proposal failed: {e}"))?;

                info!(
                    skill_id = skill_id.0,
                    skill_name = %skill.name,
                    proposal_id = %pid.0,
                    "optimizer: improvement proposal submitted"
                );

                Ok(Some(pid))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{NewSkill, NewSkillVersion};
    use crate::scanner::Strictness;
    use crate::store::SkillStore;
    use crate::summarizer::{SummarizerOutput, SummarizerPrompt};
    use execlaw_core::Database;
    use execlaw_core::db::DbConfig;
    use execlaw_core::migrations::MigrationRunner;
    use std::sync::Arc;

    fn fresh_db() -> Database {
        let db = Database::open(&DbConfig::in_memory_unencrypted()).unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        db
    }

    fn sample_skill(name: &str) -> NewSkill {
        NewSkill {
            name: name.to_string(),
            source: "admin:test".into(),
            registration_kind: crate::model::RegistrationKind::Authored,
            owning_plugin_id: None,
            initial_version: NewSkillVersion {
                description: "optimizer test skill".into(),
                body_md: "## Steps\n- Do the thing".into(),
                frontmatter_json: r#"{"name":"test","tags":[]}"#.into(),
                authored_by: "admin:test".into(),
                promotion_notes: None,
            },
            resources: vec![],
        }
    }

    fn create_skill(store: &SkillStore, name: &str) {
        store
            .create(sample_skill(name), Strictness::Warn, 1000)
            .unwrap();
    }

    fn record_n_successes(store: &SkillStore, skill_name: &str, n: u32, prefix: &str) {
        for i in 0..n {
            let cid = format!("{prefix}-conv-{i}");
            store.record_invocation(skill_name, &cid, 1000).unwrap();
            store
                .close_open_invocations(&cid, "success", 1, 2000)
                .unwrap();
        }
    }

    /// A summarizer stub that always returns Skip.
    struct SkipSummarizer;

    #[async_trait::async_trait]
    impl SkillSummarizer for SkipSummarizer {
        async fn summarize(&self, _prompt: SummarizerPrompt) -> Result<SummarizerOutput, String> {
            Ok(SummarizerOutput::Skip {
                reason: "test skip".into(),
            })
        }
    }

    #[tokio::test]
    async fn below_threshold_returns_none() {
        let db = fresh_db();
        let store = Arc::new(SkillStore::new(db.clone()));
        let worker = OptimizerWorker {
            store: store.clone(),
            db: db.clone(),
            summarizer: Arc::new(SkipSummarizer),
        };

        create_skill(&store, "test-ns/optimizer-skill");
        // 3 successes — below threshold of 5.
        record_n_successes(&store, "test-ns/optimizer-skill", 3, "below");

        let skill_id = store.get("test-ns/optimizer-skill").unwrap().unwrap().id;
        let result = worker.maybe_optimize(skill_id).await.unwrap();
        assert!(result.is_none(), "below threshold should return None");
    }

    #[tokio::test]
    async fn at_threshold_skip_returns_none() {
        let db = fresh_db();
        let store = Arc::new(SkillStore::new(db.clone()));
        let worker = OptimizerWorker {
            store: store.clone(),
            db: db.clone(),
            summarizer: Arc::new(SkipSummarizer),
        };

        create_skill(&store, "test-ns/skip-skill");
        record_n_successes(&store, "test-ns/skip-skill", REUSE_THRESHOLD, "skip");

        let skill_id = store.get("test-ns/skip-skill").unwrap().unwrap().id;
        let result = worker.maybe_optimize(skill_id).await.unwrap();
        assert!(result.is_none(), "skip result should be None");
    }

    #[tokio::test]
    async fn count_successful_invocations_only_counts_success() {
        let db = fresh_db();
        let store = Arc::new(SkillStore::new(db.clone()));

        create_skill(&store, "test-ns/count-skill");

        // 2 successes + 1 failure.
        record_n_successes(&store, "test-ns/count-skill", 2, "cs");
        store
            .record_invocation("test-ns/count-skill", "conv-fail", 1000)
            .unwrap();
        store
            .close_open_invocations("conv-fail", "failure", 1, 2000)
            .unwrap();

        let skill_id = store.get("test-ns/count-skill").unwrap().unwrap().id;
        let count = store.count_successful_invocations(skill_id).unwrap();
        assert_eq!(count, 2, "only successful invocations should be counted");
    }

    #[tokio::test]
    async fn at_non_threshold_count_returns_none() {
        let db = fresh_db();
        let store = Arc::new(SkillStore::new(db.clone()));
        let worker = OptimizerWorker {
            store: store.clone(),
            db: db.clone(),
            summarizer: Arc::new(SkipSummarizer),
        };

        create_skill(&store, "test-ns/off-threshold");
        // 7 successes — not a multiple of 5.
        record_n_successes(&store, "test-ns/off-threshold", 7, "off");

        let skill_id = store.get("test-ns/off-threshold").unwrap().unwrap().id;
        let result = worker.maybe_optimize(skill_id).await.unwrap();
        assert!(
            result.is_none(),
            "7 uses (not a multiple of 5) should not fire"
        );
    }
}
