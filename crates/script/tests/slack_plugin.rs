use async_trait::async_trait;
use execlaw_script::{
    AttachmentBytes, CreatedArtifact, HostCapError, HostCapabilities, InboundMessage, RouteOutcome,
    ScriptEngine, ScriptPlugin, WsFrameHandler, WsSubscriptionHandle,
};
use rhai::Dynamic;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;

struct TestCaps;

#[async_trait]
impl HostCapabilities for TestCaps {
    async fn sidecar_url(&self, _: &str) -> Option<String> {
        None
    }
    async fn ws_subscribe_with_init(
        &self,
        _: String,
        _: Vec<(String, String)>,
        _: Vec<String>,
        _: WsFrameHandler,
    ) -> Result<WsSubscriptionHandle, HostCapError> {
        unreachable!("decoder fixture has no websocket")
    }
    async fn route_inbound(&self, _: InboundMessage) -> Result<RouteOutcome, HostCapError> {
        unreachable!("decoder fixture does not route")
    }
    async fn get_attachment_bytes_b64(&self, _: &str) -> Result<AttachmentBytes, HostCapError> {
        unreachable!("decoder fixture has no attachments")
    }
    async fn vault_get(&self, _: &str, _: &str) -> Result<Option<String>, HostCapError> {
        Ok(None)
    }
    async fn vault_put(&self, _: &str, _: &str, _: &str) -> Result<(), HostCapError> {
        Ok(())
    }
    async fn vault_delete(&self, _: &str, _: &str) -> Result<bool, HostCapError> {
        Ok(false)
    }
    async fn create_artifact_attachment(
        &self,
        _: &str,
        _: &str,
        _: &str,
        _: Vec<u8>,
        _: Option<i64>,
    ) -> Result<CreatedArtifact, HostCapError> {
        unreachable!("decoder fixture creates no attachments")
    }
}

fn slack_plugin() -> ScriptPlugin {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.pop();
    path.pop();
    path.push("plugins/slack/main.rhai");
    let src = std::fs::read_to_string(&path).unwrap();
    let engine = ScriptEngine::new();
    let _ = engine.set_host_capabilities(Arc::new(TestCaps));
    ScriptPlugin::from_source("slack", &src, &engine).unwrap()
}

fn json_to_dynamic(value: Value) -> Dynamic {
    match value {
        Value::Null => Dynamic::UNIT,
        Value::Bool(value) => Dynamic::from(value),
        Value::Number(value) => {
            if let Some(integer) = value.as_i64() {
                Dynamic::from(integer)
            } else if let Some(number) = value.as_f64() {
                Dynamic::from(number)
            } else {
                Dynamic::UNIT
            }
        }
        Value::String(value) => Dynamic::from(value),
        Value::Array(values) => Dynamic::from(
            values
                .into_iter()
                .map(json_to_dynamic)
                .collect::<rhai::Array>(),
        ),
        Value::Object(values) => {
            let mut map = rhai::Map::new();
            for (key, value) in values {
                map.insert(key.into(), json_to_dynamic(value));
            }
            Dynamic::from(map)
        }
    }
}

#[test]
fn slack_main_rhai_parses() {
    slack_plugin();
}

#[tokio::test]
async fn slack_timestamp_parser_converts_provider_seconds_and_microseconds() {
    let plugin = slack_plugin();
    let parsed = plugin
        .invoke_async(
            "parse_slack_ts_ms",
            vec![Dynamic::from("1700000001.000003")],
        )
        .await
        .unwrap();
    let seconds = plugin
        .invoke_async("parse_int", vec![Dynamic::from("1700000001")])
        .await
        .unwrap();
    assert_eq!(seconds.as_i64(), Some(1_700_000_001));
    assert_eq!(parsed.as_i64(), Some(1_700_000_001_000));
}

#[tokio::test]
async fn create_and_edit_preserve_provider_message_and_thread_ids() {
    let plugin = slack_plugin();
    let payload = json_to_dynamic(json!({"event_id":"Ev-create"}));
    let create = plugin.invoke_async("decode_event", vec![
        Dynamic::from("T1"),
        json_to_dynamic(json!({"type":"message","channel":"D123","channel_type":"im","user":"U1","ts":"1700000000.000001","thread_ts":"1699999999.000001","text":"hello"})),
        payload,
    ]).await.unwrap();
    assert_eq!(create["message_operation"], "create");
    assert_eq!(create["target_message_id"], "1700000000.000001");
    assert_eq!(create["reply_to_message_id"], "1699999999.000001");
    assert_eq!(create["source_event_id"], "Ev-create");

    let edit = plugin.invoke_async("decode_event", vec![
        Dynamic::from("T1"),
        json_to_dynamic(json!({"type":"message","subtype":"message_changed","channel":"D123","channel_type":"im","user":"U1","ts":"1700000000.000001","thread_ts":"1699999999.000001","edited":{"ts":"1700000001.000003"},"text":"edited"})),
        json_to_dynamic(json!({"event_id":"Ev-edit"})),
    ]).await.unwrap();
    assert_eq!(edit["message_operation"], "edit");
    assert_eq!(edit["target_message_id"], "1700000000.000001");
    assert!(edit["source_revision"].is_number(), "{edit}");
    assert!(
        edit["source_revision"].as_i64().unwrap() > create["source_revision"].as_i64().unwrap()
    );
}

#[tokio::test]
async fn deletes_and_reactions_use_normalized_non_approval_operations() {
    let plugin = slack_plugin();
    let deleted = plugin.invoke_async("decode_deleted_event", vec![
        Dynamic::from("T1"),
        json_to_dynamic(json!({"channel":"D123","ts":"1700000002.000000","deleted_ts":"1700000000.000001","previous_message":{"user":"U1"}})),
        json_to_dynamic(json!({"event_id":"Ev-delete"})),
    ]).await.unwrap();
    assert_eq!(deleted["message_operation"], "delete");
    assert_eq!(deleted["target_message_id"], "1700000000.000001");

    let reaction = plugin.invoke_async("decode_reaction_event", vec![
        Dynamic::from("T1"),
        json_to_dynamic(json!({"type":"reaction_added","user":"U2","reaction":"thumbsup","event_ts":"1700000003.000000","item":{"type":"message","channel":"D123","ts":"1700000000.000001"}})),
        json_to_dynamic(json!({"event_id":"Ev-reaction"})),
    ]).await.unwrap();
    assert_eq!(reaction["message_operation"], "reaction_add");
    assert_eq!(reaction["target_message_id"], "1700000000.000001");
    assert_eq!(reaction["reaction"], "thumbsup");
}
