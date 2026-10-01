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
    pub executor_replayed_turns: usize,
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
        if EventKind::parse(&event.kind) == EventKind::Other && event.kind != "other" {
            anyhow::bail!("event {} has an unsupported event kind", event.seq);
        }
    }

    let db = Database::open(&DbConfig::in_memory_unencrypted())?;
    MigrationRunner::new(&db).apply_all()?;
    let conversation_id = ConversationId::from(format!("fixture-replay-{}", validation.fixture_id));
    let key = Sha256::digest(b"execlaw-regression-fixture-replay-key-v1").to_vec();
    let log = EventLog::new(&db).with_hmac_key(key);
    for (index, source) in fixture.events.iter().enumerate() {
        let kind = EventKind::parse(&source.kind);
        let mut event = EventRecord::new(
            conversation_id.clone(),
            EventSeq(index as i64 + 1),
            kind,
            &source.payload,
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
            || actual.kind.as_str() != expected.kind
            || actual.actor != expected.actor
            || actual.committed_at != expected.committed_at
        {
            anyhow::bail!("event-log replay diverged at source event {}", expected.seq);
        }
        let payload: serde_json::Value = actual.decode_payload()?;
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
        digest.update(&actual.payload);
        digest.update(actual.committed_at.to_le_bytes());
    }
    if mocks_checked != validation.tool_calls_checked {
        anyhow::bail!(
            "replayed {mocks_checked} mock responses, expected {}",
            validation.tool_calls_checked
        );
    }
    Ok(ReplayExecution {
        fixture_id: validation.fixture_id,
        events_replayed: replayed.len(),
        mock_tool_responses_replayed: mocks_checked,
        executor_replayed_turns: replay_with_production_executor(fixture).await?,
        effects_enabled: false,
        hmac_verified: true,
        replay_sha256: hex::encode(digest.finalize()),
    })
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
) -> anyhow::Result<usize> {
    use execlaw_core::events::{
        EventKind, EventLog, EventRecord, ToolResultPayload, ToolUsePayload,
    };

    let turns = fixture_turns(fixture)?;
    let mut responses = Vec::new();
    for turn in &turns {
        let model_turn = turn.model_turn.as_ref().ok_or_else(|| {
            anyhow::anyhow!("executor replay requires a terminal model_turn for every user_msg")
        })?;
        let model_payload = &model_turn.payload;
        if !turn.tool_uses.is_empty() {
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
            responses.push(serde_json::json!({
                "id":"fixture-tool-round",
                "model":model_payload.get("model").and_then(serde_json::Value::as_str).unwrap_or("fixture-model"),
                "choices":[{"index":0,"message":{"role":"assistant","content":null,"tool_calls":calls},"finish_reason":"tool_calls"}],
                "usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}
            }));
        }
        responses.push(serde_json::json!({
            "id":"fixture-final-round",
            "model":model_payload.get("model").and_then(serde_json::Value::as_str).unwrap_or("fixture-model"),
            "choices":[{"index":0,"message":{"role":"assistant","content":model_payload.get("text").and_then(serde_json::Value::as_str).unwrap_or_default(),"tool_calls":[]},"finish_reason":model_payload.get("finish_reason").cloned().unwrap_or(serde_json::Value::String("stop".into()))}],
            "usage":{"prompt_tokens":model_payload.get("prompt_tokens").and_then(serde_json::Value::as_u64).unwrap_or(0),"completion_tokens":model_payload.get("completion_tokens").and_then(serde_json::Value::as_u64).unwrap_or(0),"total_tokens":model_payload.get("prompt_tokens").and_then(serde_json::Value::as_u64).unwrap_or(0)+model_payload.get("completion_tokens").and_then(serde_json::Value::as_u64).unwrap_or(0)}
        }));
    }
    let inference_url = serve_recorded_completions(responses).await?;
    let db = Database::open(&DbConfig::in_memory_unencrypted())?;
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
    for turn in &turns {
        let user = turn
            .user
            .as_ref()
            .expect("validated fixture turn has a user message");
        let payload = &user.payload;
        if payload
            .get("attachment_ids")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|items| !items.is_empty())
        {
            anyhow::bail!(
                "executor fixture replay requires attached media to be exported as local fixtures"
            );
        }
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
        let tool_names = expected_mocks
            .values()
            .map(|value| value.0.clone())
            .collect::<std::collections::BTreeSet<_>>();
        let tools = tool_names
            .iter()
            .map(|name| {
                execlaw_inference_api::ToolDeclaration::function(
                    name,
                    "Offline fixture replay tool; all effects are disabled.",
                    serde_json::json!({"type":"object","additionalProperties":true}),
                )
            })
            .collect::<Vec<_>>();
        let dispatch = Arc::new(FixtureToolDispatch {
            next_ordinal: AtomicU32::new(0),
            expected: expected_mocks,
        });
        let executor = TurnExecutor::new(
            execlaw_inference_api::InferenceClient::new(inference_url.clone()),
            dispatch,
        );
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
            discoverable_tools: tools,
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
        executor
            .resume_turn_from_event(&db, &conversation_id, user_seq, &cfg, Vec::new())
            .await
            .map_err(|error| {
                anyhow::anyhow!(
                    "production turn executor replay failed at source turn {}: {error}",
                    user.seq
                )
            })?;
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
    Ok(turns.len())
}

async fn serve_recorded_completions(responses: Vec<serde_json::Value>) -> anyhow::Result<String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    tokio::spawn(async move {
        for response in responses {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            let mut buffer = [0u8; 32_768];
            let _ =
                tokio::time::timeout(std::time::Duration::from_secs(2), socket.read(&mut buffer))
                    .await;
            let Ok(body) = serde_json::to_vec(&response) else {
                continue;
            };
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
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
        assert!(!report.effects_enabled);
        assert!(report.hmac_verified);
        assert_eq!(report.replay_sha256.len(), 64);
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
        assert_eq!(report.executor_replayed_turns, 1);
        assert!(!report.effects_enabled);
        assert!(report.hmac_verified);
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
}
