use execlaw_core::events::{EventKind, EventLog, EventRecord};
use execlaw_core::ids::{ConversationId, EventSeq};
use execlaw_core::tool::{ToolFailure, ToolFailureKind, ToolResultEnvelope};
use execlaw_core::{Database, DbConfig, MigrationRunner};
use execlaw_runner_local::turn::{ToolDispatch, TurnConfig, TurnExecutor};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

#[derive(Debug, Serialize)]
pub(super) struct ReplayExecution {
    pub fixture_id: i64,
    pub events_replayed: usize,
    pub mock_tool_responses_replayed: usize,
    pub raw_stream_fixtures_replayed: usize,
    pub synthetic_media_replayed: usize,
    pub policy_cases_replayed: usize,
    pub tool_catalog_snapshots_replayed: usize,
    pub executor_replayed_turns: usize,
    pub recovery_turns_replayed: usize,
    pub effects_enabled: bool,
    pub hmac_verified: bool,
    pub replay_sha256: String,
}

/// Rebuild a fixture in a fresh in-memory event log, verify the HMAC chain,
/// and compare every replayed envelope and mock result with its expectation.
pub(super) async fn replay(
    fixture: &execlaw_core::eval::RegressionFixture,
) -> anyhow::Result<ReplayExecution> {
    let validation = execlaw_core::eval::validate_regression_fixture(fixture)
        .map_err(|error| anyhow::anyhow!("fixture validation failed: {error}"))?;
    let raw_stream_fixtures_replayed = replay_raw_stream_fixtures(fixture)?;
    let policy_cases_replayed = replay_policy_cases(fixture)?;
    let mut mock_responses = BTreeMap::new();
    for response in &fixture.mock_tool_responses {
        if mock_responses
            .insert((response.turn_seq, response.ordinal), &response.payload)
            .is_some()
        {
            anyhow::bail!("fixture contains duplicate mock tool response keys");
        }
    }
    for (event, transition) in fixture.events.iter().zip(&fixture.expected_transitions) {
        if event.kind != transition.transition {
            anyhow::bail!("event {} does not match its expected transition", event.seq);
        }
    }

    let db = Database::open(&DbConfig::in_memory_unencrypted())?;
    MigrationRunner::new(&db).apply_all()?;
    let conversation_id = ConversationId::from(format!("fixture-replay-{}", validation.fixture_id));
    let key = Sha256::digest(b"execlaw-regression-fixture-replay-key-v1").to_vec();
    let log = EventLog::new(&db).with_hmac_key(key);
    for (index, source) in fixture.events.iter().enumerate() {
        let kind = EventKind::parse(&source.kind);
        let payload = if kind == EventKind::Other && source.kind != EventKind::Other.as_str() {
            serde_json::json!({"kind":source.kind,"payload":source.payload})
        } else {
            source.payload.clone()
        };
        let mut event = EventRecord::new(
            conversation_id.clone(),
            EventSeq(index as i64 + 1),
            kind,
            &payload,
            source.actor.clone(),
        )?;
        event.committed_at = source.committed_at;
        log.append(&event)?;
    }

    let replayed = log.replay_since(&conversation_id, EventSeq(0))?;
    if replayed.len() != fixture.events.len() {
        anyhow::bail!(
            "event log replay returned {} events, expected {}",
            replayed.len(),
            fixture.events.len()
        );
    }
    let mut digest = Sha256::new();
    let mut current_turn = fixture.provenance.from_seq;
    let mut mocks_checked = 0usize;
    for (index, (actual, expected)) in replayed.iter().zip(&fixture.events).enumerate() {
        let expected_seq = index as i64 + 1;
        if actual.seq.0 != expected_seq
            || actual.kind.as_str() != EventKind::parse(&expected.kind).as_str()
            || actual.actor != expected.actor
            || actual.committed_at != expected.committed_at
        {
            anyhow::bail!("event-log replay diverged at source event {}", expected.seq);
        }
        let payload: serde_json::Value = actual.decode_payload()?;
        let payload =
            if actual.kind == EventKind::Other && expected.kind != EventKind::Other.as_str() {
                if payload.get("kind").and_then(serde_json::Value::as_str) != Some(&expected.kind) {
                    anyhow::bail!(
                        "unknown event kind diverged at source event {}",
                        expected.seq
                    );
                }
                payload.get("payload").cloned().ok_or_else(|| {
                    anyhow::anyhow!(
                        "unknown event payload missing at source event {}",
                        expected.seq
                    )
                })?
            } else {
                payload
            };
        if payload != expected.payload {
            anyhow::bail!("event payload diverged at source event {}", expected.seq);
        }
        if expected.kind == "user_msg" {
            current_turn = expected.seq;
        }
        if expected.kind == "tool_result" {
            let ordinal = payload
                .get("ordinal")
                .and_then(serde_json::Value::as_u64)
                .and_then(|value| u32::try_from(value).ok())
                .ok_or_else(|| anyhow::anyhow!("tool result has an invalid ordinal"))?;
            let mock = mock_responses
                .get(&(current_turn, ordinal))
                .ok_or_else(|| anyhow::anyhow!("tool result has no matching mock response"))?;
            if *mock != &payload {
                anyhow::bail!("mock response diverged at source event {}", expected.seq);
            }
            mocks_checked += 1;
        }
        digest.update(actual.kind.as_str().as_bytes());
        digest.update([0]);
        digest.update(expected.kind.as_bytes());
        digest.update([0]);
        digest.update(&actual.payload);
        digest.update(actual.committed_at.to_le_bytes());
    }
    if mocks_checked != validation.tool_calls_checked {
        anyhow::bail!(
            "replayed {mocks_checked} mock responses, expected {}",
            validation.tool_calls_checked
        );
    }
    for raw in &fixture.raw_stream_fixtures {
        digest.update(raw.fixture_id.as_bytes());
        digest.update([0]);
        for chunk in &raw.chunks_hex {
            digest.update(hex::decode(chunk)?);
        }
        digest.update(serde_json::to_vec(&raw.expected_data)?);
    }
    for media in &fixture.synthetic_media {
        digest.update(media.attachment_id.as_bytes());
        digest.update([0]);
        digest.update(media.mime_type.as_bytes());
        digest.update([0]);
        digest.update(hex::decode(&media.content_hex)?);
    }
    for policy_case in &fixture.policy_cases {
        digest.update(serde_json::to_vec(policy_case)?);
    }
    for catalog in &fixture.tool_catalogs {
        digest.update(serde_json::to_vec(catalog)?);
    }
    let (executor_replayed_turns, recovery_turns_replayed) =
        replay_with_production_executor(fixture).await?;
    Ok(ReplayExecution {
        fixture_id: validation.fixture_id,
        events_replayed: replayed.len(),
        mock_tool_responses_replayed: mocks_checked,
        raw_stream_fixtures_replayed,
        synthetic_media_replayed: fixture.synthetic_media.len(),
        policy_cases_replayed,
        tool_catalog_snapshots_replayed: fixture.tool_catalogs.len(),
        executor_replayed_turns,
        recovery_turns_replayed,
        effects_enabled: false,
        hmac_verified: true,
        replay_sha256: hex::encode(digest.finalize()),
    })
}

fn replay_policy_cases(fixture: &execlaw_core::eval::RegressionFixture) -> anyhow::Result<usize> {
    use execlaw_policy::trust::{TrustLevel, TurnPolicyInput, evaluate_turn};

    for case in &fixture.policy_cases {
        let sender_trust = TrustLevel::parse(&case.sender_trust).ok_or_else(|| {
            anyhow::anyhow!(
                "policy fixture {} has an unknown sender trust",
                case.case_id
            )
        })?;
        let effective_trust = TrustLevel::parse(&case.effective_trust).ok_or_else(|| {
            anyhow::anyhow!(
                "policy fixture {} has an unknown effective trust",
                case.case_id
            )
        })?;
        let decision = evaluate_turn(TurnPolicyInput {
            effective_trust,
            sender_trust,
            voice: case.voice,
            accesses_sensitive_data: case.accesses_sensitive_data,
            produces_external_effect: case.produces_external_effect,
        });
        let actual = serde_json::json!({
            "drop_turn":decision.drop_turn,
            "require_approval":decision.require_approval,
            "planner_executor":decision.planner_executor,
            "spotlighting":decision.spotlighting,
            "latency_band":match decision.latency_band {
                execlaw_policy::trust::LatencyBand::Any => "any",
                execlaw_policy::trust::LatencyBand::LowOnly => "low_only",
            },
            "capability_set":decision.capability_set,
        });
        let expected = serde_json::to_value(&case.expected)?;
        if actual != expected {
            anyhow::bail!("policy fixture {} decision diverged", case.case_id);
        }
    }
    Ok(fixture.policy_cases.len())
}

fn replay_raw_stream_fixtures(
    fixture: &execlaw_core::eval::RegressionFixture,
) -> anyhow::Result<usize> {
    use execlaw_inference_api::SseDecoder;

    for raw in &fixture.raw_stream_fixtures {
        let mut decoder = SseDecoder::default();
        let mut actual_data = Vec::new();
        for chunk_hex in &raw.chunks_hex {
            let chunk = hex::decode(chunk_hex).map_err(|_| {
                anyhow::anyhow!("raw stream fixture {} has invalid hex", raw.fixture_id)
            })?;
            actual_data.extend(
                decoder
                    .push(&chunk, false)
                    .map_err(|error| {
                        anyhow::anyhow!(
                            "raw stream fixture {} decode failed: {error}",
                            raw.fixture_id
                        )
                    })?
                    .into_iter()
                    .map(|event| event.data),
            );
        }
        actual_data.extend(
            decoder
                .push(&[], true)
                .map_err(|error| {
                    anyhow::anyhow!(
                        "raw stream fixture {} EOF decode failed: {error}",
                        raw.fixture_id
                    )
                })?
                .into_iter()
                .map(|event| event.data),
        );
        if actual_data != raw.expected_data {
            anyhow::bail!(
                "raw stream fixture {} decoded frames diverged: got {}, expected {}",
                raw.fixture_id,
                actual_data.len(),
                raw.expected_data.len()
            );
        }
    }
    Ok(fixture.raw_stream_fixtures.len())
}

fn synthetic_image_urls_for_user(
    fixture: &execlaw_core::eval::RegressionFixture,
    user_payload: &serde_json::Value,
) -> anyhow::Result<Vec<String>> {
    use base64::Engine;

    let Some(ids) = user_payload
        .get("attachment_ids")
        .and_then(serde_json::Value::as_array)
    else {
        return Ok(Vec::new());
    };
    let mut urls = Vec::with_capacity(ids.len());
    for id in ids {
        let id = id
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("fixture attachment ID is not a string"))?;
        let media = fixture
            .synthetic_media
            .iter()
            .find(|media| media.attachment_id == id && media.synthetic)
            .ok_or_else(|| {
                anyhow::anyhow!("fixture attachment {id} has no synthetic replacement")
            })?;
        let bytes = hex::decode(&media.content_hex)?;
        let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
        urls.push(format!("data:{};base64,{encoded}", media.mime_type));
    }
    Ok(urls)
}

struct FixtureToolDispatch {
    next_ordinal: AtomicU32,
    expected: BTreeMap<u32, (String, serde_json::Value, ToolResultEnvelope)>,
}

#[async_trait::async_trait]
impl ToolDispatch for FixtureToolDispatch {
    async fn call(
        &self,
        tool_name: &str,
        args_json: &serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        match self.call_typed(tool_name, args_json).await {
            ToolResultEnvelope::Ok { value } => Ok(value),
            ToolResultEnvelope::Err { failure } => Err(failure.message),
        }
    }

    fn set_effect_ordinal(&self, ordinal: u32) {
        self.next_ordinal.store(ordinal, Ordering::SeqCst);
    }

    async fn call_typed(
        &self,
        tool_name: &str,
        args_json: &serde_json::Value,
    ) -> ToolResultEnvelope {
        let ordinal = self.next_ordinal.load(Ordering::SeqCst);
        let Some((expected_name, expected_args, result)) = self.expected.get(&ordinal) else {
            return ToolResultEnvelope::Err {
                failure: ToolFailure::new(
                    ToolFailureKind::Validation,
                    "fixture_mock_missing",
                    "fixture has no mock response for this tool ordinal",
                ),
            };
        };
        if expected_name != tool_name || expected_args != args_json {
            return ToolResultEnvelope::Err {
                failure: ToolFailure::new(
                    ToolFailureKind::Validation,
                    "fixture_tool_call_diverged",
                    "production executor tool call differed from the recorded fixture",
                ),
            };
        }
        result.clone()
    }
}

#[derive(Default)]
struct FixtureTurn {
    user: Option<execlaw_core::eval::RegressionFixtureEvent>,
    tool_uses: Vec<execlaw_core::eval::RegressionFixtureEvent>,
    tool_results: Vec<execlaw_core::eval::RegressionFixtureEvent>,
    model_turn: Option<execlaw_core::eval::RegressionFixtureEvent>,
}

async fn replay_with_production_executor(
    fixture: &execlaw_core::eval::RegressionFixture,
) -> anyhow::Result<(usize, usize)> {
    use execlaw_core::events::{
        EventKind, EventLog, EventRecord, ToolResultPayload, ToolUsePayload,
    };

    if fixture
        .synthetic_media
        .iter()
        .any(|media| !media.mime_type.starts_with("image/"))
    {
        // The current production turn API accepts image data URLs only. Keep
        // non-image replacements in the HMAC-verified offline fixture replay
        // until their local modality-specific executor path is implemented.
        return Ok((0, 0));
    }
    if !fixture
        .events
        .iter()
        .any(|event| event.kind == EventKind::UserMsg.as_str())
    {
        // Catalog and policy incidents can consist entirely of state events.
        // They still pass event-log replay, but have no user turn for the
        // production executor to run.
        return Ok((0, 0));
    }
    let turns = fixture_turns(fixture)?;
    let mut response_rounds = Vec::with_capacity(turns.len());
    for turn in &turns {
        let model_turn = turn.model_turn.as_ref().ok_or_else(|| {
            anyhow::anyhow!("executor replay requires a terminal model_turn for every user_msg")
        })?;
        let model_payload = &model_turn.payload;
        let tool_response = if !turn.tool_uses.is_empty() {
            let mut calls = Vec::with_capacity(turn.tool_uses.len());
            for event in &turn.tool_uses {
                let payload: ToolUsePayload = serde_json::from_value(event.payload.clone())?;
                calls.push(serde_json::json!({
                    "id": format!("fixture-call-{}", payload.ordinal),
                    "type": "function",
                    "function": {
                        "name": payload.tool_name,
                        "arguments": serde_json::to_string(&payload.args_json)?
                    }
                }));
            }
            Some(serde_json::json!({
                "id":"fixture-tool-round",
                "model":model_payload.get("model").and_then(serde_json::Value::as_str).unwrap_or("fixture-model"),
                "choices":[{"index":0,"message":{"role":"assistant","content":null,"tool_calls":calls},"finish_reason":"tool_calls"}],
                "usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}
            }))
        } else {
            None
        };
        let final_response = serde_json::json!({
            "id":"fixture-final-round",
            "model":model_payload.get("model").and_then(serde_json::Value::as_str).unwrap_or("fixture-model"),
            "choices":[{"index":0,"message":{"role":"assistant","content":model_payload.get("text").and_then(serde_json::Value::as_str).unwrap_or_default(),"tool_calls":[]},"finish_reason":model_payload.get("finish_reason").cloned().unwrap_or(serde_json::Value::String("stop".into()))}],
            "usage":{"prompt_tokens":model_payload.get("prompt_tokens").and_then(serde_json::Value::as_u64).unwrap_or(0),"completion_tokens":model_payload.get("completion_tokens").and_then(serde_json::Value::as_u64).unwrap_or(0),"total_tokens":model_payload.get("prompt_tokens").and_then(serde_json::Value::as_u64).unwrap_or(0)+model_payload.get("completion_tokens").and_then(serde_json::Value::as_u64).unwrap_or(0)}
        });
        response_rounds.push((tool_response, final_response));
    }
    let mut recovery_turns_replayed = 0usize;
    let fixture_db_dir = tempfile::tempdir()?;
    let fixture_db_config = DbConfig {
        path: fixture_db_dir.path().join("fixture-executor.db"),
        key: None,
    };
    let mut db = Database::open(&fixture_db_config)?;
    MigrationRunner::new(&db).apply_all()?;
    let conversation_id = ConversationId::from(format!(
        "fixture-executor-{}",
        fixture.provenance.flagged_range_id
    ));
    db.with_conn(|connection| {
        connection.execute(
            "INSERT INTO state_conversations (conversation_id,kind,phase,trust_class,modality) \
             VALUES (?1,'ControllerDM','idle','Controller','Text')",
            [conversation_id.as_str()],
        )?;
        Ok(())
    })?;
    let hmac_key = Sha256::digest(b"execlaw-fixture-executor-replay-key-v1").to_vec();
    for (turn, (tool_response, final_response)) in turns.iter().zip(response_rounds) {
        let user = turn
            .user
            .as_ref()
            .expect("validated fixture turn has a user message");
        let payload = &user.payload;
        let image_urls = synthetic_image_urls_for_user(fixture, payload)?;
        let mut expected_mocks = BTreeMap::new();
        for (call, result) in turn.tool_uses.iter().zip(&turn.tool_results) {
            let use_payload: ToolUsePayload = serde_json::from_value(call.payload.clone())?;
            let result_payload: ToolResultPayload = serde_json::from_value(result.payload.clone())?;
            let result = match result_payload.outcome {
                Ok(value) => ToolResultEnvelope::Ok { value },
                Err(message) => {
                    let failure: ToolFailure = serde_json::from_str(&message).map_err(|_| {
                        anyhow::anyhow!("fixture tool error is not a serialized ToolFailure")
                    })?;
                    ToolResultEnvelope::Err { failure }
                }
            };
            expected_mocks.insert(
                use_payload.ordinal,
                (use_payload.tool_name, use_payload.args_json, result),
            );
        }
        let catalog = fixture
            .tool_catalogs
            .iter()
            .find(|catalog| catalog.turn_seq == user.seq);
        let (tools, discoverable_tools) = if let Some(catalog) = catalog {
            (
                catalog
                    .tools
                    .iter()
                    .cloned()
                    .map(serde_json::from_value::<execlaw_inference_api::ToolDeclaration>)
                    .collect::<Result<Vec<_>, _>>()?,
                catalog
                    .discoverable_tools
                    .iter()
                    .cloned()
                    .map(serde_json::from_value::<execlaw_inference_api::ToolDeclaration>)
                    .collect::<Result<Vec<_>, _>>()?,
            )
        } else if turn.tool_uses.is_empty() {
            (Vec::new(), Vec::new())
        } else {
            anyhow::bail!("tool turn {} has no exact catalog snapshot", user.seq);
        };
        let event_log = EventLog::new(&db).with_hmac_key(hmac_key.clone());
        let user_seq = event_log.last_seq(&conversation_id)?.next();
        let mut user_event = EventRecord::new(
            conversation_id.clone(),
            user_seq,
            EventKind::UserMsg,
            payload,
            user.actor.clone(),
        )?;
        user_event.committed_at = user.committed_at;
        event_log.append(&user_event)?;
        let cfg = TurnConfig {
            model: execlaw_inference_api::ModelId(
                turn.model_turn
                    .as_ref()
                    .and_then(|event| event.payload.get("model"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("fixture-model")
                    .to_owned(),
            ),
            system_prompt: "Offline regression fixture replay. Tool effects are disabled.".into(),
            temperature: Some(0.0),
            max_tokens: Some(512),
            max_tool_rounds: 64,
            tools: tools.clone(),
            discoverable_tools,
            event_log_hmac_key: Some(hmac_key.clone()),
            phase_observer: None,
            reasoning_enabled: false,
            inbound_channel_origin: payload
                .get("channel_origin")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
            spotlight_delim: None,
            context_window_policy: "full_replay".into(),
            qualified_context_tokens: None,
            bytes_per_token_milli: 3_000,
            qualified_profile: None,
            tool_result_artifacts_root: None,
            summarizer_client: None,
            completion_contract: None,
            session: None,
        };
        if let Some(tool_response) = tool_response {
            // Interrupt after the tool checkpoint, close the database, then
            // resume the same durable run with a fresh executor process state.
            let first_url = serve_recorded_completions(
                vec![
                    tool_response,
                    serde_json::Value::String("synthetic interrupted response".into()),
                ],
                image_urls.clone(),
            )
            .await?;
            let first_executor = TurnExecutor::new(
                execlaw_inference_api::InferenceClient::new(first_url),
                Arc::new(FixtureToolDispatch {
                    next_ordinal: AtomicU32::new(0),
                    expected: expected_mocks.clone(),
                }),
            );
            if first_executor
                .resume_turn_from_event(&db, &conversation_id, user_seq, &cfg, image_urls.clone())
                .await
                .is_ok()
            {
                anyhow::bail!("fixture recovery probe unexpectedly completed before interruption");
            }
            drop(first_executor);
            let run_id = format!("turn:{}:{}", conversation_id.as_str(), user_seq.0);
            let completed_tool_steps: i64 = db.with_conn(|connection| {
                connection
                    .query_row(
                        "SELECT COUNT(*) FROM state_run_steps \
                         WHERE run_id = ?1 AND step_id LIKE 'tool:%' AND status = 'completed'",
                        [&run_id],
                        |row| row.get(0),
                    )
                    .map_err(Into::into)
            })?;
            if completed_tool_steps != turn.tool_uses.len() as i64 {
                anyhow::bail!(
                    "recovery fixture interruption occurred before all tool checkpoints were durable"
                );
            }
            drop(db);

            db = Database::open(&fixture_db_config)?;
            MigrationRunner::new(&db).apply_all()?;
            db.with_conn(|connection| {
                connection.execute(
                    "UPDATE state_run_steps SET lease_expires_at = 0 \
                     WHERE run_id = ?1 AND step_id = 'model:1'",
                    [&run_id],
                )?;
                Ok(())
            })?;
            let recovery_url =
                serve_recorded_completions(vec![final_response], image_urls.clone()).await?;
            let recovery_executor = TurnExecutor::new(
                execlaw_inference_api::InferenceClient::new(recovery_url),
                Arc::new(FixtureToolDispatch {
                    next_ordinal: AtomicU32::new(0),
                    expected: expected_mocks,
                }),
            );
            recovery_executor
                .resume_turn_from_event(&db, &conversation_id, user_seq, &cfg, image_urls)
                .await
                .map_err(|error| {
                    anyhow::anyhow!(
                        "production recovery replay failed at source turn {}: {error}",
                        user.seq
                    )
                })?;
            recovery_turns_replayed += 1;
        } else {
            let inference_url =
                serve_recorded_completions(vec![final_response], image_urls.clone()).await?;
            let executor = TurnExecutor::new(
                execlaw_inference_api::InferenceClient::new(inference_url),
                Arc::new(FixtureToolDispatch {
                    next_ordinal: AtomicU32::new(0),
                    expected: expected_mocks,
                }),
            );
            executor
                .resume_turn_from_event(&db, &conversation_id, user_seq, &cfg, image_urls)
                .await
                .map_err(|error| {
                    anyhow::anyhow!(
                        "production turn executor replay failed at source turn {}: {error}",
                        user.seq
                    )
                })?;
        }
        if user_seq.0
            != turns
                .iter()
                .take_while(|candidate| {
                    candidate
                        .user
                        .as_ref()
                        .is_some_and(|other| other.seq < user.seq)
                })
                .map(|candidate| candidate.tool_uses.len() + candidate.tool_results.len() + 2)
                .sum::<usize>() as i64
                + 1
        {
            anyhow::bail!(
                "fixture sequence mapping failed at source turn {}",
                user.seq
            );
        }
    }
    let replayed = EventLog::new(&db)
        .with_hmac_key(hmac_key)
        .replay_since(&conversation_id, EventSeq(0))?;
    if replayed.len() != fixture.events.len() {
        anyhow::bail!(
            "production executor emitted {} events, expected {}",
            replayed.len(),
            fixture.events.len()
        );
    }
    for (index, (actual, expected)) in replayed.iter().zip(&fixture.events).enumerate() {
        if actual.seq.0 != index as i64 + 1 || actual.kind.as_str() != expected.kind {
            anyhow::bail!(
                "production executor transition diverged at source event {}",
                expected.seq
            );
        }
        let actual_payload: serde_json::Value = actual.decode_payload()?;
        if expected.kind == "model_turn" {
            for key in [
                "model",
                "text",
                "finish_reason",
                "prompt_tokens",
                "completion_tokens",
                "channel_origin",
            ] {
                if actual_payload.get(key) != expected.payload.get(key) {
                    anyhow::bail!(
                        "production executor final response diverged at source event {} field {key}",
                        expected.seq
                    );
                }
            }
        } else if actual_payload != expected.payload {
            anyhow::bail!(
                "production executor payload diverged at source event {}",
                expected.seq
            );
        }
        if expected.kind != "model_turn" && actual.actor != expected.actor {
            anyhow::bail!(
                "production executor actor diverged at source event {}",
                expected.seq
            );
        }
    }
    Ok((turns.len(), recovery_turns_replayed))
}

async fn serve_recorded_completions(
    responses: Vec<serde_json::Value>,
    expected_image_urls: Vec<String>,
) -> anyhow::Result<String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    tokio::spawn(async move {
        for response in responses {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            let mut request = Vec::new();
            let mut chunk = [0u8; 8192];
            let body_range = loop {
                let read = match tokio::time::timeout(
                    std::time::Duration::from_secs(2),
                    socket.read(&mut chunk),
                )
                .await
                {
                    Ok(Ok(read)) if read > 0 => read,
                    _ => break None,
                };
                request.extend_from_slice(&chunk[..read]);
                if request.len() > 8 * 1024 * 1024 {
                    break None;
                }
                let Some(header_end) = request.windows(4).position(|window| window == b"\r\n\r\n")
                else {
                    continue;
                };
                let header = String::from_utf8_lossy(&request[..header_end]);
                let content_length = header.lines().find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                });
                let Some(content_length) =
                    content_length.filter(|length| *length <= 8 * 1024 * 1024)
                else {
                    break None;
                };
                let start = header_end + 4;
                if request.len() >= start.saturating_add(content_length) {
                    break Some(start..start + content_length);
                }
            };
            let Some(body_range) = body_range else {
                continue;
            };
            let request_body = &request[body_range];
            let images_present = expected_image_urls.iter().all(|url| {
                request_body
                    .windows(url.len())
                    .any(|window| window == url.as_bytes())
            });
            let (status, body) = if images_present {
                let Ok(body) = serde_json::to_vec(&response) else {
                    continue;
                };
                ("200 OK", body)
            } else {
                (
                    "422 Unprocessable Content",
                    br#"{"error":"synthetic image fixture was not sent to local inference"}"#
                        .to_vec(),
                )
            };
            let header = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = socket.write_all(header.as_bytes()).await;
            let _ = socket.write_all(&body).await;
        }
    });
    Ok(format!("http://{address}/v1"))
}

fn fixture_turns(
    fixture: &execlaw_core::eval::RegressionFixture,
) -> anyhow::Result<Vec<FixtureTurn>> {
    use execlaw_core::events::EventKind;

    let mut turns = Vec::<FixtureTurn>::new();
    for event in &fixture.events {
        match EventKind::parse(&event.kind) {
            EventKind::UserMsg => {
                turns.push(FixtureTurn {
                    user: Some(event.clone()),
                    ..FixtureTurn::default()
                });
            }
            EventKind::ToolUse => turns
                .last_mut()
                .ok_or_else(|| anyhow::anyhow!("tool_use has no fixture user turn"))?
                .tool_uses
                .push(event.clone()),
            EventKind::ToolResult => turns
                .last_mut()
                .ok_or_else(|| anyhow::anyhow!("tool_result has no fixture user turn"))?
                .tool_results
                .push(event.clone()),
            EventKind::ModelTurn => {
                let turn = turns
                    .last_mut()
                    .ok_or_else(|| anyhow::anyhow!("model_turn has no fixture user turn"))?;
                if turn.model_turn.replace(event.clone()).is_some() {
                    anyhow::bail!("fixture has more than one terminal model_turn per input turn");
                }
            }
            _ => anyhow::bail!(
                "production executor replay does not support fixture event kind '{}'",
                event.kind
            ),
        }
    }
    if turns.is_empty() {
        anyhow::bail!("executor replay fixture contains no user turns");
    }
    for turn in &turns {
        if turn.model_turn.is_none() || turn.tool_uses.len() != turn.tool_results.len() {
            anyhow::bail!(
                "executor replay requires paired tools and a terminal model_turn for every input turn"
            );
        }
    }
    Ok(turns)
}

#[cfg(test)]
mod tests {
    use super::replay;
    use execlaw_core::eval::RegressionFixture;

    #[tokio::test]
    async fn checked_in_synthetic_fixture_rebuilds_and_replays_its_event_log() {
        let fixture: RegressionFixture = serde_json::from_str(include_str!(
            "../../../evals/fixtures/synthetic-offline-regression.json"
        ))
        .unwrap();
        let report = replay(&fixture).await.unwrap();
        assert_eq!(report.events_replayed, 2);
        assert_eq!(report.mock_tool_responses_replayed, 0);
        assert_eq!(report.executor_replayed_turns, 1);
        assert_eq!(report.policy_cases_replayed, 3);
        assert_eq!(report.raw_stream_fixtures_replayed, 1);
        assert!(!report.effects_enabled);
        assert!(report.hmac_verified);
        assert_eq!(report.replay_sha256.len(), 64);
    }

    #[tokio::test]
    async fn unknown_event_kinds_round_trip_as_hmac_verified_other_events() {
        let mut fixture: RegressionFixture = serde_json::from_str(include_str!(
            "../../../evals/fixtures/synthetic-offline-regression.json"
        ))
        .unwrap();
        fixture.events.truncate(1);
        fixture.expected_transitions.truncate(1);
        fixture.events[0].kind = "future_checkpoint_kind".into();
        fixture.expected_transitions[0].transition = "future_checkpoint_kind".into();
        fixture.provenance.to_seq = fixture.provenance.from_seq;
        fixture.mock_tool_responses.clear();
        fixture.tool_catalogs.clear();
        fixture.synthetic_media.clear();

        let report = replay(&fixture).await.unwrap();
        assert_eq!(report.events_replayed, 1);
        assert_eq!(report.executor_replayed_turns, 0);
        assert!(report.hmac_verified);
    }

    #[tokio::test]
    async fn checked_in_mock_tool_fixture_replays_paired_results_without_effects() {
        let fixture: RegressionFixture = serde_json::from_str(include_str!(
            "../../../evals/fixtures/synthetic-offline-tool-regression.json"
        ))
        .unwrap();
        let report = replay(&fixture).await.unwrap();
        assert_eq!(report.events_replayed, 4);
        assert_eq!(report.mock_tool_responses_replayed, 1);
        assert_eq!(report.tool_catalog_snapshots_replayed, 1);
        assert_eq!(report.executor_replayed_turns, 1);
        assert_eq!(report.recovery_turns_replayed, 1);
        assert!(!report.effects_enabled);
        assert!(report.hmac_verified);
    }

    #[tokio::test]
    async fn checked_in_synthetic_image_fixture_replays_through_local_inference() {
        let fixture: RegressionFixture = serde_json::from_str(include_str!(
            "../../../evals/fixtures/synthetic-image-regression.json"
        ))
        .unwrap();
        let report = replay(&fixture).await.unwrap();
        assert_eq!(report.executor_replayed_turns, 1);
        assert_eq!(report.recovery_turns_replayed, 0);
        assert_eq!(report.synthetic_media_replayed, 1);
        assert!(!report.effects_enabled);
        assert!(report.hmac_verified);
    }

    #[tokio::test]
    async fn synthetic_audio_fixture_is_hmac_verified_without_entering_image_inference() {
        let mut fixture: RegressionFixture = serde_json::from_str(include_str!(
            "../../../evals/fixtures/synthetic-image-regression.json"
        ))
        .unwrap();
        fixture.synthetic_media[0].mime_type = "audio/wav".into();
        fixture.synthetic_media[0].content_hex =
            "524946462400000057415645666d74201000000001000100401f0000401f0000010008006461746100000000".into();
        let report = replay(&fixture).await.unwrap();
        assert_eq!(report.synthetic_media_replayed, 1);
        assert_eq!(report.executor_replayed_turns, 0);
        assert!(report.hmac_verified);
        assert!(!report.effects_enabled);
    }

    #[tokio::test]
    async fn replay_rejects_a_mock_tool_result_that_differs_from_the_recorded_event() {
        let mut fixture: RegressionFixture = serde_json::from_str(include_str!(
            "../../../evals/fixtures/synthetic-offline-tool-regression.json"
        ))
        .unwrap();
        fixture.mock_tool_responses[0].payload["outcome"]["Ok"]["value"] =
            serde_json::json!("different");
        assert!(replay(&fixture).await.is_err());
    }

    #[tokio::test]
    async fn replay_rejects_a_tool_call_missing_from_the_captured_catalog() {
        let mut fixture: RegressionFixture = serde_json::from_str(include_str!(
            "../../../evals/fixtures/synthetic-offline-tool-regression.json"
        ))
        .unwrap();
        let catalog = &mut fixture.tool_catalogs[0];
        catalog.tools.clear();
        catalog.discoverable_tools.clear();
        catalog.snapshot_sha256 = execlaw_core::harness::HarnessStore::fingerprint(
            &serde_json::json!({"tools": [], "discoverable_tools": []}),
        )
        .unwrap();
        let error = replay(&fixture).await.unwrap_err().to_string();
        assert!(error.contains("absent from turn 1's catalog"));
    }

    #[tokio::test]
    async fn replay_rejects_schema_drift_in_the_captured_tool_catalog() {
        let mut fixture: RegressionFixture = serde_json::from_str(include_str!(
            "../../../evals/fixtures/synthetic-offline-tool-regression.json"
        ))
        .unwrap();
        let catalog = &mut fixture.tool_catalogs[0];
        for declaration in catalog
            .tools
            .iter_mut()
            .chain(catalog.discoverable_tools.iter_mut())
        {
            declaration["function"]["parameters"]["properties"]["value"]["type"] =
                serde_json::json!("integer");
        }
        let snapshot = serde_json::json!({
            "tools":catalog.tools.clone(),
            "discoverable_tools":catalog.discoverable_tools.clone(),
        });
        catalog.snapshot_sha256 =
            execlaw_core::harness::HarnessStore::fingerprint(&snapshot).unwrap();
        assert!(replay(&fixture).await.is_err());
    }

    #[tokio::test]
    async fn replay_rejects_raw_stream_framing_regressions() {
        let mut fixture: RegressionFixture = serde_json::from_str(include_str!(
            "../../../evals/fixtures/synthetic-offline-regression.json"
        ))
        .unwrap();
        fixture.raw_stream_fixtures[0].expected_data[0] = "wrong-frame".into();
        let error = replay(&fixture).await.unwrap_err().to_string();
        assert!(error.contains("raw stream fixture"));
    }

    #[tokio::test]
    async fn replay_rejects_a_policy_decision_regression() {
        let mut fixture: RegressionFixture = serde_json::from_str(include_str!(
            "../../../evals/fixtures/synthetic-offline-regression.json"
        ))
        .unwrap();
        fixture.policy_cases[0].expected.require_approval = false;
        let error = replay(&fixture).await.unwrap_err().to_string();
        assert!(error.contains("policy fixture"));
    }

    #[tokio::test]
    async fn policy_only_fixture_replays_without_inventing_a_user_turn() {
        let mut fixture: RegressionFixture = serde_json::from_str(include_str!(
            "../../../evals/fixtures/synthetic-offline-regression.json"
        ))
        .unwrap();
        let seq = fixture.provenance.from_seq;
        fixture.provenance.to_seq = seq;
        fixture.events = vec![execlaw_core::eval::RegressionFixtureEvent {
            seq,
            kind: "trust_changed".into(),
            actor: Some("controller-fixture".into()),
            committed_at: 1,
            payload: serde_json::json!({"from":"UnknownPending","to":"KnownLimited"}),
        }];
        fixture.expected_transitions = vec![execlaw_core::eval::ExpectedStateTransition {
            seq,
            transition: "trust_changed".into(),
        }];
        fixture.mock_tool_responses.clear();

        let report = replay(&fixture).await.unwrap();
        assert_eq!(report.events_replayed, 1);
        assert_eq!(report.executor_replayed_turns, 0);
        assert!(!report.effects_enabled);
        assert!(report.hmac_verified);
    }
}
