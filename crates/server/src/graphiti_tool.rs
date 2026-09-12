//! Built-in Graphiti temporal-memory tool.

use async_trait::async_trait;
use execlaw_core::Database;
use execlaw_core::config::{ConfigKv, ConfigTable};
use execlaw_core::graphiti::{GraphitiJob, GraphitiJobKind, GraphitiJobStore, NewGraphitiJob};
use execlaw_core::ids::EventSeq;
use execlaw_core::tool::{ToolCtx, ToolDescriptor, ToolImpl, ToolLatency, ToolOutcome, ToolSource};
use execlaw_core::vault_row::VaultRowStore;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::sync::Arc;

const TIMEOUT_SECS: u64 = 30;
pub(crate) const CONFIG_BASE_URL: &str = "graphiti.base_url";
pub(crate) const CONFIG_API_KEY_REF: &str = "graphiti.api_key_vault_ref";
const DEFAULT_MAX_ATTEMPTS: i64 = 5;

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum GraphitiAction {
    Status,
    #[serde(alias = "ingest_episode")]
    Ingest,
    Search,
    Retract,
    Reconcile,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GraphitiArgs {
    action: GraphitiAction,
    #[serde(default, alias = "episode_body")]
    episode: Option<Value>,
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    top_k: Option<usize>,
    #[serde(default)]
    evidence_id: Option<String>,
}

#[derive(Debug)]
struct GraphitiConfig {
    base_url: reqwest::Url,
    api_key: Option<String>,
}

pub struct GraphitiTool {
    descriptor: ToolDescriptor,
    db: Option<Database>,
}

impl GraphitiTool {
    pub fn new() -> Self {
        Self {
            descriptor: ToolDescriptor {
                name: "graphiti".into(),
                description: "Search or manage conversation-scoped temporal memory through the operator-configured Graphiti service.".into(),
                schema: json!({
                    "type": "object",
                    "properties": {
                        "action": {"type": "string", "enum": ["status", "ingest", "search", "retract", "reconcile"]},
                        "episode": {"type": "object"},
                        "query": {"type": "string"},
                        "top_k": {"type": "integer", "minimum": 1, "maximum": 100},
                        "evidence_id": {"type": "string"}
                    },
                    "required": ["action"],
                    "additionalProperties": false
                }),
                source: ToolSource::Builtin,
                latency: ToolLatency::High,
                capabilities: vec![],
                default_allowed_classes: vec!["Controller".into()],
                sensitive: true,
            },
            db: None,
        }
    }

    pub fn with_database(mut self, db: Database) -> Self {
        self.db = Some(db);
        self
    }
}

pub(crate) fn validate_base_url(base_url: &str) -> Result<reqwest::Url, String> {
    let parsed = reqwest::Url::parse(base_url).map_err(|error| error.to_string())?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.username() != ""
        || parsed.password().is_some()
        || parsed.path() != "/"
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(
            "Graphiti endpoint must be a root HTTP(S) URL without userinfo, query, or fragment"
                .to_owned(),
        );
    }
    Ok(parsed)
}

fn load_config(db: &Database) -> Result<GraphitiConfig, String> {
    let config = ConfigKv::new(db, ConfigTable::RuntimeSettings);
    let raw_base_url = config
        .get(CONFIG_BASE_URL)
        .map_err(|error| format!("read Graphiti endpoint config: {error}"))?
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| "Graphiti endpoint is not configured in SQLite".to_owned())?;
    let base_url = validate_base_url(&raw_base_url)?;
    let api_key = match config
        .get(CONFIG_API_KEY_REF)
        .map_err(|error| format!("read Graphiti credential reference: {error}"))?
        .filter(|value| !value.trim().is_empty())
    {
        None => None,
        Some(secret_ref) => {
            let bytes = VaultRowStore::new(db)
                .get(None, &secret_ref)
                .map_err(|error| format!("read Graphiti vault secret: {error}"))?
                .ok_or_else(|| format!("Graphiti vault secret '{secret_ref}' is missing"))?;
            let value = String::from_utf8(bytes)
                .map_err(|_| format!("Graphiti vault secret '{secret_ref}' is not UTF-8"))?;
            if value.is_empty() {
                return Err(format!("Graphiti vault secret '{secret_ref}' is empty"));
            }
            Some(value)
        }
    };
    Ok(GraphitiConfig { base_url, api_key })
}

fn validate_action_scope(args: &GraphitiArgs, scope: Option<&str>) -> Result<(), String> {
    if !matches!(args.action, GraphitiAction::Status) && scope.is_none() {
        return Err("Graphiti actions require a host-derived conversation scope".to_owned());
    }
    if args.top_k.is_some_and(|top_k| !(1..=100).contains(&top_k)) {
        return Err("top_k must be between 1 and 100".to_owned());
    }
    match args.action {
        GraphitiAction::Ingest if !matches!(args.episode, Some(Value::Object(_))) => {
            Err("action=ingest requires an `episode` object".to_owned())
        }
        GraphitiAction::Search
            if args
                .query
                .as_deref()
                .is_none_or(|value| value.trim().is_empty()) =>
        {
            Err("action=search requires non-empty `query`".to_owned())
        }
        GraphitiAction::Retract
            if args
                .evidence_id
                .as_deref()
                .is_none_or(|value| value.trim().is_empty()) =>
        {
            Err("action=retract requires non-empty `evidence_id`".to_owned())
        }
        _ => Ok(()),
    }
}

fn latest_source_event(db: &Database, conversation_id: &str) -> Result<EventSeq, String> {
    let seq = db
        .with_conn(|conn| {
            conn.query_row(
                "SELECT MAX(seq) FROM state_events WHERE conversation_id = ?1",
                [conversation_id],
                |row| row.get::<_, Option<i64>>(0),
            )
            .map_err(execlaw_core::DbError::from)
        })
        .map_err(|error| format!("read Graphiti source event: {error}"))?;
    seq.map(EventSeq)
        .ok_or_else(|| "Graphiti ingestion requires a persisted source event".to_owned())
}

fn enqueue_job(
    db: &Database,
    args: GraphitiArgs,
    conversation_id: &str,
    trust_class: &str,
    now: i64,
) -> ToolOutcome {
    let source_event_seq = match latest_source_event(db, conversation_id) {
        Ok(seq) => seq,
        Err(error) => return ToolOutcome::err("source_event_missing", error),
    };
    let kind = match args.action {
        GraphitiAction::Ingest => GraphitiJobKind::Ingest,
        GraphitiAction::Reconcile => GraphitiJobKind::Reconcile,
        _ => return ToolOutcome::err("invalid_argument", "action is not queueable"),
    };
    let evidence_id = match kind {
        GraphitiJobKind::Ingest => format!("execlaw:{conversation_id}:{}", source_event_seq.0),
        GraphitiJobKind::Reconcile => format!("reconcile:{conversation_id}:{}", source_event_seq.0),
    };
    let payload = args.episode.unwrap_or_else(|| json!({}));
    match GraphitiJobStore::new(db).enqueue(&NewGraphitiJob {
        kind,
        conversation_id: conversation_id.into(),
        trust_class: trust_class.to_owned(),
        source_event_seq,
        evidence_id: evidence_id.clone(),
        payload,
        max_attempts: DEFAULT_MAX_ATTEMPTS,
        now,
    }) {
        Ok(job_id) => ToolOutcome::ok(json!({
            "status": "queued",
            "job_id": job_id,
            "evidence_id": evidence_id,
            "source_event": {"conversation_id": conversation_id, "seq": source_event_seq.0}
        })),
        Err(error) => ToolOutcome::err("enqueue_failed", error.to_string()),
    }
}

#[async_trait]
impl ToolImpl for GraphitiTool {
    fn descriptor(&self) -> &ToolDescriptor {
        &self.descriptor
    }

    async fn invoke(&self, ctx: ToolCtx, args: Value) -> ToolOutcome {
        let Some(db) = self.db.as_ref() else {
            return ToolOutcome::err("not_configured", "Graphiti database is unavailable");
        };
        invoke_graphiti_scoped(
            args,
            Some(ctx.conversation_id.as_str()),
            Some(ctx.caller_trust.as_str()),
            db,
            ctx.clock.now_unix(),
        )
        .await
    }
}

pub async fn invoke_graphiti(args: Value, db: &Database) -> ToolOutcome {
    invoke_graphiti_scoped(args, None, None, db, chrono::Utc::now().timestamp()).await
}

async fn invoke_graphiti_scoped(
    args: Value,
    scope: Option<&str>,
    trust_class: Option<&str>,
    db: &Database,
    now: i64,
) -> ToolOutcome {
    let parsed: GraphitiArgs = match serde_json::from_value(args) {
        Ok(value) => value,
        Err(error) => return ToolOutcome::err("invalid_argument", error.to_string()),
    };
    if let Err(error) = validate_action_scope(&parsed, scope) {
        return ToolOutcome::err("invalid_argument", error);
    }
    if matches!(
        parsed.action,
        GraphitiAction::Ingest | GraphitiAction::Reconcile
    ) {
        return enqueue_job(
            db,
            parsed,
            scope.expect("validated scoped action"),
            trust_class.expect("model invocation supplies trust"),
            now,
        );
    }

    let config = match load_config(db) {
        Ok(config) => config,
        Err(error) => return ToolOutcome::err("not_configured", error),
    };
    let client = match crate::local_endpoint_policy::checked_client(
        db,
        "graphiti",
        config.base_url.as_str(),
        |builder| builder.timeout(std::time::Duration::from_secs(TIMEOUT_SECS)),
    ) {
        Ok((client, _)) => client,
        Err(error) => return ToolOutcome::err("endpoint_denied", error),
    };

    match parsed.action {
        GraphitiAction::Status => {
            send_request(&client, &config, reqwest::Method::GET, "health", None).await
        }
        GraphitiAction::Search => {
            let scope = scope.expect("validated scoped action");
            let outcome = send_request(
                &client,
                &config,
                reqwest::Method::POST,
                "search",
                Some(json!({
                    "group_id": scope,
                    "query": parsed.query.expect("validated query"),
                    "top_k": parsed.top_k.unwrap_or(8),
                })),
            )
            .await;
            validate_search_outcome(outcome, scope)
        }
        GraphitiAction::Retract => {
            send_request(
                &client,
                &config,
                reqwest::Method::POST,
                "retract",
                Some(json!({
                    "group_id": scope.expect("validated scoped action"),
                    "evidence_id": parsed.evidence_id.expect("validated evidence id"),
                })),
            )
            .await
        }
        GraphitiAction::Ingest | GraphitiAction::Reconcile => unreachable!(),
    }
}

async fn send_request(
    client: &reqwest::Client,
    config: &GraphitiConfig,
    method: reqwest::Method,
    path: &str,
    body: Option<Value>,
) -> ToolOutcome {
    let url = match config.base_url.join(path) {
        Ok(url) => url,
        Err(error) => return ToolOutcome::err("invalid_endpoint", error.to_string()),
    };
    let mut request = client.request(method, url);
    if let Some(api_key) = &config.api_key {
        request = request.bearer_auth(api_key);
    }
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = match request.send().await {
        Ok(response) => response,
        Err(error) => return ToolOutcome::err("request_failed", error.to_string()),
    };
    let status = response.status();
    if status.is_redirection() {
        return ToolOutcome::err("endpoint_denied", "Graphiti redirects are not allowed");
    }
    let text = match response.text().await {
        Ok(text) => text,
        Err(error) => return ToolOutcome::err("read_response_failed", error.to_string()),
    };
    if !status.is_success() {
        return ToolOutcome::err(
            "graphiti_failed",
            format!("status={} body={text}", status.as_u16()),
        );
    }
    let result = if text.trim().is_empty() {
        Value::Null
    } else {
        match serde_json::from_str(&text) {
            Ok(value) => value,
            Err(error) => return ToolOutcome::err("invalid_response", error.to_string()),
        }
    };
    ToolOutcome::ok(json!({"ok": true, "status": status.as_u16(), "result": result}))
}

fn validate_search_outcome(outcome: ToolOutcome, expected_scope: &str) -> ToolOutcome {
    let ToolOutcome::Ok(mut envelope) = outcome else {
        return outcome;
    };
    let Some(result) = envelope.get_mut("result") else {
        return ToolOutcome::err(
            "invalid_response",
            "Graphiti search response omitted result",
        );
    };
    let items = match result {
        Value::Array(items) => items,
        Value::Object(object) => match object.get_mut("results") {
            Some(Value::Array(items)) => items,
            _ => {
                return ToolOutcome::err(
                    "invalid_response",
                    "Graphiti search result must be an array or contain results[]",
                );
            }
        },
        _ => {
            return ToolOutcome::err(
                "invalid_response",
                "Graphiti search result must be an array or contain results[]",
            );
        }
    };
    for item in items {
        let Some(object) = item.as_object() else {
            return ToolOutcome::err("invalid_response", "Graphiti search item must be an object");
        };
        if object.get("group_id").and_then(Value::as_str) != Some(expected_scope) {
            return ToolOutcome::err(
                "scope_mismatch",
                "Graphiti returned an item outside the requested scope",
            );
        }
        let expected_evidence_prefix = format!("execlaw:{expected_scope}:");
        if object
            .get("evidence_id")
            .and_then(Value::as_str)
            .is_none_or(|value| !value.starts_with(&expected_evidence_prefix))
        {
            return ToolOutcome::err(
                "evidence_missing",
                "Graphiti returned an item without a scope-bound evidence_id",
            );
        }
    }
    ToolOutcome::Ok(envelope)
}

fn episode_payload(job: &GraphitiJob) -> Value {
    let mut payload = match job.payload.clone() {
        Value::Object(object) => object,
        _ => Map::new(),
    };
    payload.insert("group_id".into(), json!(job.conversation_id.as_str()));
    payload.insert("trust_scope".into(), json!(job.trust_class));
    payload.insert("evidence_id".into(), json!(job.evidence_id));
    payload.insert(
        "source_event".into(),
        json!({"conversation_id": job.conversation_id.as_str(), "seq": job.source_event_seq.0}),
    );
    payload.insert("source".into(), json!("execlaw"));
    Value::Object(payload)
}

pub(crate) async fn execute_job(db: &Database, job: &GraphitiJob) -> Result<(), String> {
    let config = load_config(db)?;
    let (client, _) = crate::local_endpoint_policy::checked_client(
        db,
        "graphiti",
        config.base_url.as_str(),
        |builder| builder.timeout(std::time::Duration::from_secs(TIMEOUT_SECS)),
    )?;
    let (path, payload) = match job.kind {
        GraphitiJobKind::Ingest => ("episodes", episode_payload(job)),
        GraphitiJobKind::Reconcile => (
            "reconcile",
            json!({
                "group_id": job.conversation_id.as_str(),
                "trust_scope": job.trust_class,
                "evidence_id": job.evidence_id,
                "source_event": {"conversation_id": job.conversation_id.as_str(), "seq": job.source_event_seq.0}
            }),
        ),
    };
    match send_request(&client, &config, reqwest::Method::POST, path, Some(payload)).await {
        ToolOutcome::Ok(_) => Ok(()),
        ToolOutcome::Err { code, message } => Err(format!("{code}: {message}")),
        ToolOutcome::Denied { reason } => Err(reason),
    }
}

pub fn graphiti_tools(db: Database) -> Vec<Arc<dyn ToolImpl>> {
    vec![Arc::new(GraphitiTool::new().with_database(db))]
}

#[cfg(test)]
mod tests {
    use super::*;
    use execlaw_core::db::DbConfig;
    use execlaw_core::events::{EventKind, EventLog, EventRecord};
    use execlaw_core::graphiti::GraphitiJobStore;
    use execlaw_core::ids::{ConversationId, EventSeq};
    use execlaw_core::migrations::MigrationRunner;
    use execlaw_core::tool::SystemClock;

    fn test_db() -> Database {
        let db = Database::open(&DbConfig::in_memory_unencrypted()).unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        db
    }

    #[test]
    fn model_schema_exposes_only_typed_actions_and_arguments() {
        let tool = GraphitiTool::new();
        let schema = &tool.descriptor().schema;
        let properties = schema["properties"].as_object().unwrap();
        for forbidden in ["base_url", "api_key", "method", "path", "raw_request"] {
            assert!(!properties.contains_key(forbidden));
        }
        assert_eq!(
            schema["properties"]["action"]["enum"],
            json!(["status", "ingest", "search", "retract", "reconcile"])
        );
        assert_eq!(tool.descriptor().default_allowed_classes, ["Controller"]);
        assert!(tool.descriptor().sensitive);
    }

    #[test]
    fn graphiti_args_reject_model_selected_endpoint_key_and_raw_request() {
        for forbidden in [
            json!({"action": "status", "base_url": "http://example.com"}),
            json!({"action": "status", "api_key": "secret"}),
            json!({"action": "status", "method": "GET"}),
            json!({"action": "raw_request"}),
        ] {
            assert!(serde_json::from_value::<GraphitiArgs>(forbidden).is_err());
        }
    }

    #[test]
    fn config_loads_endpoint_and_vault_backed_credential() {
        let db = test_db();
        let config = ConfigKv::new(&db, ConfigTable::RuntimeSettings);
        config
            .set(CONFIG_BASE_URL, "http://127.0.0.1:8000")
            .unwrap();
        config.set(CONFIG_API_KEY_REF, "graphiti-token").unwrap();
        VaultRowStore::new(&db)
            .put(None, "graphiti-token", b"secret", 1)
            .unwrap();
        let loaded = load_config(&db).unwrap();
        assert_eq!(loaded.base_url.as_str(), "http://127.0.0.1:8000/");
        assert_eq!(loaded.api_key.as_deref(), Some("secret"));
    }

    #[test]
    fn config_rejects_missing_vault_secret() {
        let db = test_db();
        let config = ConfigKv::new(&db, ConfigTable::RuntimeSettings);
        config
            .set(CONFIG_BASE_URL, "http://127.0.0.1:8000")
            .unwrap();
        config.set(CONFIG_API_KEY_REF, "missing-token").unwrap();
        assert!(load_config(&db).unwrap_err().contains("missing-token"));
    }

    #[test]
    fn search_results_require_matching_scope_and_evidence() {
        let valid = ToolOutcome::ok(
            json!({"result": [{"group_id": "conversation-1", "evidence_id": "execlaw:conversation-1:7"}]}),
        );
        assert!(matches!(
            validate_search_outcome(valid, "conversation-1"),
            ToolOutcome::Ok(_)
        ));
        let wrong_scope = ToolOutcome::ok(
            json!({"result": [{"group_id": "conversation-2", "evidence_id": "execlaw:conversation-2:7"}]}),
        );
        assert!(
            matches!(validate_search_outcome(wrong_scope, "conversation-1"), ToolOutcome::Err { code, .. } if code == "scope_mismatch")
        );
        let missing_evidence = ToolOutcome::ok(json!({"result": [{"group_id": "conversation-1"}]}));
        assert!(
            matches!(validate_search_outcome(missing_evidence, "conversation-1"), ToolOutcome::Err { code, .. } if code == "evidence_missing")
        );
    }

    #[tokio::test]
    async fn model_ingest_enqueues_without_calling_graphiti() {
        let db = test_db();
        let conversation_id = ConversationId::from("conversation-1");
        let source_event = EventRecord::new(
            conversation_id.clone(),
            EventSeq(1),
            EventKind::UserMsg,
            &json!({"text": "remember this"}),
            Some("controller".into()),
        )
        .unwrap();
        EventLog::new(&db).append(&source_event).unwrap();
        let ctx = ToolCtx::empty(conversation_id, "Controller", Arc::new(SystemClock));
        let outcome = GraphitiTool::new()
            .with_database(db.clone())
            .invoke(
                ctx,
                json!({"action": "ingest", "episode": {"body": "fact"}}),
            )
            .await;
        assert!(matches!(outcome, ToolOutcome::Ok(ref value) if value["status"] == "queued"));

        let claimed = GraphitiJobStore::new(&db)
            .claim_next(
                "test-worker",
                chrono::Utc::now().timestamp(),
                chrono::Utc::now().timestamp() + 60,
            )
            .unwrap()
            .expect("durable job");
        assert_eq!(claimed.conversation_id.as_str(), "conversation-1");
        assert_eq!(claimed.trust_class, "Controller");
        assert_eq!(claimed.source_event_seq, EventSeq(1));
        assert_eq!(claimed.evidence_id, "execlaw:conversation-1:1");
    }

    #[tokio::test]
    async fn configured_public_endpoint_is_denied_before_request() {
        let db = test_db();
        ConfigKv::new(&db, ConfigTable::RuntimeSettings)
            .set(CONFIG_BASE_URL, "http://8.8.8.8:8000")
            .unwrap();
        let outcome = invoke_graphiti(json!({"action": "status"}), &db).await;
        assert!(matches!(outcome, ToolOutcome::Err { code, .. } if code == "endpoint_denied"));
    }
}
