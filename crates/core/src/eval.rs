//! `eval_flagged` row + store (Phase 5 observability).
//!
//! Operators tag event ranges as regression targets via the
//! `execlaw eval flag` CLI. The LLM-judge harness reads matching
//! rows by label to decide which traces to replay against rubrics.

use crate::db::{Database, DbError};
use crate::ids::ConversationId;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EvalFlagRow {
    /// `None` until `insert` returns the auto-incremented row id.
    pub id: Option<i64>,
    pub conversation_id: ConversationId,
    /// Inclusive lower bound of the flagged event range.
    pub from_seq: i64,
    /// Inclusive upper bound.
    pub to_seq: i64,
    pub label: String,
    /// Free-form tags carried as JSON; the LLM-judge can pick rubrics
    /// based on these (e.g. `["trust-class", "rule-of-two"]`).
    pub tags: Vec<String>,
    /// Principal id of the operator who flagged this range.
    pub flagged_by: String,
    /// Unix-seconds timestamp.
    pub flagged_at: i64,
    pub notes: Option<String>,
}

/// Versioned, redacted export of an operator-consented flagged trajectory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegressionFixture {
    pub schema_version: u32,
    /// Must remain false: fixture replay never dispatches external effects.
    pub effects_enabled: bool,
    pub provenance: RegressionFixtureProvenance,
    pub redaction: RegressionFixtureRedaction,
    pub events: Vec<RegressionFixtureEvent>,
    pub expected_transitions: Vec<ExpectedStateTransition>,
    pub mock_tool_responses: Vec<MockToolResponse>,
    /// Optional local wire captures for failures in the incremental SSE frame decoder.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub raw_stream_fixtures: Vec<RawStreamFixture>,
    /// Synthetic image bytes used to replay multimodal turns without source media.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub synthetic_media: Vec<SyntheticMediaFixture>,
    /// Local inputs and expected outputs for the production pure turn-policy evaluator.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub policy_cases: Vec<PolicyEvaluationFixture>,
    /// Per-turn tool declarations captured from the durable run input manifest.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_catalogs: Vec<ToolCatalogFixture>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegressionFixtureProvenance {
    pub flagged_range_id: i64,
    pub source_conversation_sha256: String,
    pub source_fingerprint_sha256: String,
    pub label: String,
    pub tags: Vec<String>,
    pub from_seq: i64,
    pub to_seq: i64,
    pub flagged_at: i64,
    pub exported_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub incident_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_ref: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegressionFixtureRedaction {
    pub policy_version: String,
    pub redaction_map_sha256: String,
    pub replacements_applied: usize,
    pub synthetic_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegressionFixtureEvent {
    pub seq: i64,
    pub kind: String,
    pub actor: Option<String>,
    pub committed_at: i64,
    pub payload: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpectedStateTransition {
    pub seq: i64,
    pub transition: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MockToolResponse {
    pub turn_seq: i64,
    pub ordinal: u32,
    pub payload: Value,
}

/// A redacted raw byte stream split at the original network chunk boundaries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RawStreamFixture {
    pub fixture_id: String,
    /// Hex-encoded network chunks; keeping chunk boundaries reproduces framing bugs.
    pub chunks_hex: Vec<String>,
    /// Expected decoded `data:` payload for each SSE event.
    pub expected_data: Vec<String>,
}

/// Synthetic, non-source media content referenced by event `attachment_ids`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyntheticMediaFixture {
    pub attachment_id: String,
    pub mime_type: String,
    /// Hex-encoded synthetic image bytes; source attachment bytes must never be copied here.
    pub content_hex: String,
    pub synthetic: bool,
}

/// One pure policy-engine regression case with no external effects.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyEvaluationFixture {
    pub case_id: String,
    pub sender_trust: String,
    pub effective_trust: String,
    pub voice: bool,
    pub accesses_sensitive_data: bool,
    pub produces_external_effect: bool,
    pub expected: ExpectedPolicyDecision,
}

/// Expected response from `execlaw_policy::trust::evaluate_turn`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpectedPolicyDecision {
    pub drop_turn: bool,
    pub require_approval: bool,
    pub planner_executor: bool,
    pub spotlighting: bool,
    pub latency_band: String,
    pub capability_set: Vec<String>,
}

/// Exact policy-filtered and discoverable tool declarations shown for one user turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCatalogFixture {
    pub turn_seq: i64,
    pub source_catalog_hash: String,
    pub snapshot_sha256: String,
    pub tools: Vec<Value>,
    pub discoverable_tools: Vec<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegressionFixtureValidation {
    pub fixture_id: i64,
    pub events_checked: usize,
    pub tool_calls_checked: usize,
    pub transitions_checked: usize,
    pub effects_enabled: bool,
    pub valid: bool,
    pub incident_ref: Option<String>,
    pub release_ref: Option<String>,
}

/// Validate fixture structure and event/tool pairing without executing tools.
pub fn validate_regression_fixture(
    fixture: &RegressionFixture,
) -> Result<RegressionFixtureValidation, String> {
    if fixture.schema_version != 1 {
        return Err(format!(
            "unsupported regression fixture schema version {}",
            fixture.schema_version
        ));
    }
    if fixture.effects_enabled {
        return Err("regression fixture requests live effects".into());
    }
    let provenance = &fixture.provenance;
    if provenance.flagged_range_id <= 0 || provenance.from_seq > provenance.to_seq {
        return Err("regression fixture provenance range is invalid".into());
    }
    for (name, digest) in [
        (
            "conversation",
            provenance.source_conversation_sha256.as_str(),
        ),
        (
            "source range",
            provenance.source_fingerprint_sha256.as_str(),
        ),
        (
            "redaction map",
            fixture.redaction.redaction_map_sha256.as_str(),
        ),
    ] {
        if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(format!("regression fixture {name} hash is invalid"));
        }
    }
    if provenance.label.trim().is_empty()
        || provenance.label.len() > 256
        || provenance.tags.len() > 64
        || provenance
            .tags
            .iter()
            .any(|tag| tag.trim().is_empty() || tag.len() > 128)
    {
        return Err("regression fixture label or tags are invalid".into());
    }
    if fixture.redaction.policy_version.trim().is_empty()
        || fixture.redaction.policy_version.len() > 64
        || fixture.redaction.synthetic_ids.len() > 1024
    {
        return Err("regression fixture redaction metadata is invalid".into());
    }
    for (kind, reference) in [
        ("incident", provenance.incident_ref.as_deref()),
        ("release", provenance.release_ref.as_deref()),
    ] {
        if reference.is_some_and(|value| !valid_fixture_reference(value)) {
            return Err(format!("regression fixture {kind} reference is invalid"));
        }
    }
    if fixture.events.is_empty() || fixture.events.len() > 4096 {
        return Err("regression fixture must contain 1..=4096 source events".into());
    }
    if fixture.events.len() != fixture.expected_transitions.len() {
        return Err("expected transition count differs from source event count".into());
    }
    let fixture_bytes = serde_json::to_vec(fixture)
        .map_err(|error| format!("regression fixture cannot be serialized: {error}"))?;
    if fixture_bytes.len() > 8 * 1024 * 1024 {
        return Err("regression fixture exceeds the 8 MiB validation limit".into());
    }
    let mut previous_seq = None;
    let mut current_turn_seq = None;
    let mut tool_uses = std::collections::BTreeSet::new();
    let mut tool_results = std::collections::BTreeSet::new();
    for (event, expected) in fixture.events.iter().zip(&fixture.expected_transitions) {
        if event.seq < provenance.from_seq
            || event.seq > provenance.to_seq
            || previous_seq.is_some_and(|previous| event.seq <= previous)
            || expected.seq != event.seq
            || expected.transition != event.kind
        {
            return Err(format!(
                "fixture event {} has inconsistent sequence provenance",
                event.seq
            ));
        }
        previous_seq = Some(event.seq);
        if event.kind == "user_msg" {
            current_turn_seq = Some(event.seq);
        }
        if event.kind == "tool_use" || event.kind == "tool_result" {
            let turn_seq = current_turn_seq
                .ok_or_else(|| format!("tool event {} has no preceding user turn", event.seq))?;
            let ordinal = event
                .payload
                .get("ordinal")
                .and_then(Value::as_u64)
                .and_then(|ordinal| u32::try_from(ordinal).ok())
                .ok_or_else(|| format!("tool event {} has no valid ordinal", event.seq))?;
            let key = (turn_seq, ordinal);
            let inserted = if event.kind == "tool_use" {
                tool_uses.insert(key)
            } else {
                tool_results.insert(key)
            };
            if !inserted {
                return Err(format!(
                    "duplicate tool ordinal {ordinal} in turn {turn_seq}"
                ));
            }
        }
    }
    if tool_uses != tool_results {
        return Err("tool_use/tool_result pairs are incomplete in the flagged range".into());
    }
    let response_keys = fixture
        .mock_tool_responses
        .iter()
        .map(|response| (response.turn_seq, response.ordinal))
        .collect::<std::collections::BTreeSet<_>>();
    if response_keys.len() != fixture.mock_tool_responses.len() {
        return Err("fixture contains duplicate mock tool responses".into());
    }
    if response_keys != tool_results {
        return Err("mock tool responses do not match the recorded tool results".into());
    }
    if fixture
        .events
        .iter()
        .any(|event| event.payload.to_string().len() > 64 * 1024)
        || fixture
            .mock_tool_responses
            .iter()
            .any(|response| response.payload.to_string().len() > 64 * 1024)
    {
        return Err("fixture event or mock response exceeds the 64 KiB payload limit".into());
    }
    if fixture
        .redaction
        .synthetic_ids
        .iter()
        .any(|id| id.trim().is_empty() || id.contains('@'))
    {
        return Err("fixture contains an invalid synthetic identity".into());
    }
    if fixture.raw_stream_fixtures.len() > 128 {
        return Err("fixture contains more than 128 raw stream captures".into());
    }
    let mut raw_stream_ids = std::collections::HashSet::new();
    for raw in &fixture.raw_stream_fixtures {
        if raw.fixture_id.trim().is_empty()
            || raw.fixture_id.len() > 128
            || !raw_stream_ids.insert(raw.fixture_id.as_str())
            || raw.chunks_hex.is_empty()
            || raw.chunks_hex.len() > 4096
            || raw.expected_data.len() > 4096
            || raw
                .expected_data
                .iter()
                .any(|data| data.len() > 1024 * 1024)
        {
            return Err("raw stream fixture metadata is invalid".into());
        }
        let mut total_bytes = 0usize;
        for chunk in &raw.chunks_hex {
            if chunk.is_empty() || chunk.len() % 2 != 0 || chunk.len() > 2 * 1024 * 1024 {
                return Err("raw stream fixture contains an invalid hex chunk".into());
            }
            let bytes = hex::decode(chunk)
                .map_err(|_| "raw stream fixture contains invalid hexadecimal data")?;
            total_bytes = total_bytes.saturating_add(bytes.len());
            if total_bytes > 4 * 1024 * 1024 {
                return Err("raw stream fixture exceeds the 4 MiB wire limit".into());
            }
        }
    }
    if fixture.synthetic_media.len() > 32 {
        return Err("fixture contains more than 32 synthetic media items".into());
    }
    let mut media_ids = std::collections::HashSet::new();
    let mut media_bytes = 0usize;
    for media in &fixture.synthetic_media {
        if !media.synthetic
            || media.attachment_id.trim().is_empty()
            || media.attachment_id.len() > 128
            || media.attachment_id.contains('@')
            || !media_ids.insert(media.attachment_id.as_str())
            || !matches!(
                media.mime_type.as_str(),
                "image/png"
                    | "image/jpeg"
                    | "image/webp"
                    | "image/gif"
                    | "audio/wav"
                    | "audio/mpeg"
                    | "audio/ogg"
                    | "audio/mp4"
                    | "video/mp4"
                    | "video/webm"
                    | "application/pdf"
            )
            || media.content_hex.is_empty()
            || media.content_hex.len() % 2 != 0
            || media.content_hex.len() > 2 * 2 * 1024 * 1024
        {
            return Err("synthetic media fixture metadata is invalid".into());
        }
        let decoded = hex::decode(&media.content_hex)
            .map_err(|_| "synthetic media fixture contains invalid hexadecimal data")?;
        media_bytes = media_bytes.saturating_add(decoded.len());
        if media_bytes > 4 * 1024 * 1024 {
            return Err("synthetic media fixtures exceed the 4 MiB total limit".into());
        }
    }
    let mut referenced_media_ids = std::collections::HashSet::new();
    for event in fixture
        .events
        .iter()
        .filter(|event| event.kind == "user_msg")
    {
        if let Some(ids) = event
            .payload
            .get("attachment_ids")
            .and_then(Value::as_array)
        {
            for id in ids {
                let id = id
                    .as_str()
                    .ok_or_else(|| "fixture attachment ID is not a string".to_owned())?;
                if !media_ids.contains(id) {
                    return Err(format!(
                        "fixture attachment {id} is missing a synthetic media replacement"
                    ));
                }
                referenced_media_ids.insert(id);
            }
        }
    }
    if referenced_media_ids.len() != media_ids.len() {
        return Err("fixture contains an unreferenced synthetic media replacement".into());
    }
    if fixture.policy_cases.len() > 128 {
        return Err("fixture contains more than 128 policy evaluation cases".into());
    }
    let mut policy_case_ids = std::collections::HashSet::new();
    let known_trust_levels = [
        "Controller",
        "Delegated",
        "KnownTrusted",
        "KnownLimited",
        "UnknownPending",
        "Blocked",
    ];
    for case in &fixture.policy_cases {
        if case.case_id.trim().is_empty()
            || case.case_id.len() > 128
            || !policy_case_ids.insert(case.case_id.as_str())
            || !fixture
                .redaction
                .synthetic_ids
                .iter()
                .any(|id| id == &case.case_id)
            || !known_trust_levels.contains(&case.sender_trust.as_str())
            || !known_trust_levels.contains(&case.effective_trust.as_str())
            || !matches!(case.expected.latency_band.as_str(), "any" | "low_only")
            || case.expected.capability_set.len() > 32
            || case
                .expected
                .capability_set
                .iter()
                .any(|capability| capability.trim().is_empty() || capability.len() > 128)
        {
            return Err("policy evaluation fixture metadata is invalid".into());
        }
        let unique_capabilities: std::collections::HashSet<_> =
            case.expected.capability_set.iter().collect();
        if unique_capabilities.len() != case.expected.capability_set.len() {
            return Err("policy evaluation fixture has duplicate capabilities".into());
        }
    }
    if fixture.tool_catalogs.len() > 4096 {
        return Err("fixture contains more than 4096 tool catalog snapshots".into());
    }
    let mut tool_catalog_sequences = std::collections::HashSet::new();
    for catalog in &fixture.tool_catalogs {
        if catalog.turn_seq < provenance.from_seq
            || catalog.turn_seq > provenance.to_seq
            || !tool_catalog_sequences.insert(catalog.turn_seq)
            || [
                catalog.source_catalog_hash.as_str(),
                catalog.snapshot_sha256.as_str(),
            ]
            .iter()
            .any(|digest| {
                digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
            || catalog.tools.len() > 512
            || catalog.discoverable_tools.len() > 512
        {
            return Err("tool catalog snapshot metadata is invalid".into());
        }
        let snapshot = serde_json::json!({
            "tools":catalog.tools,
            "discoverable_tools":catalog.discoverable_tools,
        });
        let snapshot_hash = crate::harness::HarnessStore::fingerprint(&snapshot)
            .map_err(|error| format!("tool catalog snapshot cannot be hashed: {error}"))?;
        if snapshot_hash != catalog.snapshot_sha256 {
            return Err("tool catalog snapshot hash does not match its declarations".into());
        }
        if snapshot.to_string().len() > 1024 * 1024 {
            return Err("tool catalog snapshot exceeds the 1 MiB limit".into());
        }
        for declarations in [&catalog.tools, &catalog.discoverable_tools] {
            let mut names = std::collections::HashSet::new();
            for declaration in declarations {
                let function = declaration.get("function");
                let name = function
                    .and_then(|value| value.get("name"))
                    .and_then(Value::as_str);
                if declaration.get("type").and_then(Value::as_str) != Some("function")
                    || name.is_none_or(str::is_empty)
                    || !names.insert(name.unwrap_or_default())
                    || function
                        .and_then(|value| value.get("description"))
                        .and_then(Value::as_str)
                        .is_none()
                    || function
                        .and_then(|value| value.get("parameters"))
                        .and_then(Value::as_object)
                        .is_none()
                {
                    return Err(
                        "tool catalog snapshot contains an invalid or duplicate declaration".into(),
                    );
                }
            }
        }
    }
    let mut active_turn = None;
    for event in &fixture.events {
        if event.kind == "user_msg" {
            active_turn = Some(event.seq);
        } else if event.kind == "tool_use" {
            let turn_seq =
                active_turn.ok_or_else(|| "tool_use event has no user turn".to_owned())?;
            let tool_name = event
                .payload
                .get("tool_name")
                .and_then(Value::as_str)
                .ok_or_else(|| "tool_use event has no tool name".to_owned())?;
            let catalog = fixture
                .tool_catalogs
                .iter()
                .find(|catalog| catalog.turn_seq == turn_seq)
                .ok_or_else(|| format!("tool turn {turn_seq} has no captured catalog snapshot"))?;
            if !catalog
                .tools
                .iter()
                .chain(&catalog.discoverable_tools)
                .any(|entry| {
                    entry.pointer("/function/name").and_then(Value::as_str) == Some(tool_name)
                })
            {
                return Err(format!(
                    "tool '{tool_name}' is absent from turn {turn_seq}'s catalog"
                ));
            }
        }
    }
    if fixture.tool_catalogs.iter().any(|catalog| {
        !fixture
            .events
            .iter()
            .any(|event| event.kind == "user_msg" && event.seq == catalog.turn_seq)
    }) {
        return Err("tool catalog snapshot does not refer to a user event".into());
    }
    Ok(RegressionFixtureValidation {
        fixture_id: provenance.flagged_range_id,
        events_checked: fixture.events.len(),
        tool_calls_checked: tool_uses.len(),
        transitions_checked: fixture.expected_transitions.len(),
        effects_enabled: false,
        valid: true,
        incident_ref: provenance.incident_ref.clone(),
        release_ref: provenance.release_ref.clone(),
    })
}

fn valid_fixture_reference(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

pub struct EvalFlaggedStore<'db> {
    db: &'db Database,
}

impl<'db> EvalFlaggedStore<'db> {
    pub fn new(db: &'db Database) -> Self {
        Self { db }
    }

    /// Insert a flag. Returns the new row's auto-incremented id.
    pub fn insert(&self, row: &EvalFlagRow) -> Result<i64, DbError> {
        if row.from_seq > row.to_seq {
            return Err(DbError::Invariant(format!(
                "from_seq ({}) must be <= to_seq ({})",
                row.from_seq, row.to_seq
            )));
        }
        let tags_json =
            serde_json::to_vec(&row.tags).map_err(|e| DbError::Serde(format!("tags: {e}")))?;
        self.db.with_conn(|c| {
            c.execute(
                "INSERT INTO eval_flagged \
                 (conversation_id, from_seq, to_seq, label, tags_json, flagged_by, flagged_at, notes) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    row.conversation_id.as_str(),
                    row.from_seq,
                    row.to_seq,
                    row.label,
                    tags_json,
                    row.flagged_by,
                    row.flagged_at,
                    row.notes,
                ],
            )?;
            Ok(c.last_insert_rowid())
        })
    }

    /// List every flag, newest first.
    pub fn list_all(&self) -> Result<Vec<EvalFlagRow>, DbError> {
        self.query_all(None)
    }

    /// List every flag with the given label, newest first.
    pub fn list_by_label(&self, label: &str) -> Result<Vec<EvalFlagRow>, DbError> {
        self.query_all(Some(label))
    }

    /// Load one flagged range by id for an explicit fixture export.
    pub fn get(&self, id: i64) -> Result<Option<EvalFlagRow>, DbError> {
        self.db.with_conn(|connection| {
            let row = connection
                .query_row(
                    "SELECT id, conversation_id, from_seq, to_seq, label, tags_json, \
                            flagged_by, flagged_at, notes \
                     FROM eval_flagged WHERE id = ?1",
                    [id],
                    row_to_flag,
                )
                .optional()?;
            row.map(|(row, tags_json)| {
                let tags = serde_json::from_slice(&tags_json)
                    .map_err(|error| DbError::Serde(format!("tags: {error}")))?;
                Ok(EvalFlagRow { tags, ..row })
            })
            .transpose()
        })
    }

    fn query_all(&self, label: Option<&str>) -> Result<Vec<EvalFlagRow>, DbError> {
        self.db.with_conn(|c| {
            let (sql, label_param): (&str, Option<&str>) = match label {
                Some(_) => (
                    "SELECT id, conversation_id, from_seq, to_seq, label, tags_json, \
                            flagged_by, flagged_at, notes \
                     FROM eval_flagged WHERE label = ?1 ORDER BY flagged_at DESC",
                    label,
                ),
                None => (
                    "SELECT id, conversation_id, from_seq, to_seq, label, tags_json, \
                            flagged_by, flagged_at, notes \
                     FROM eval_flagged ORDER BY flagged_at DESC",
                    None,
                ),
            };
            let mut stmt = c.prepare_cached(sql)?;
            let rows = match label_param {
                Some(l) => stmt
                    .query_map(params![l], row_to_flag)?
                    .collect::<Result<Vec<_>, _>>()?,
                None => stmt
                    .query_map([], row_to_flag)?
                    .collect::<Result<Vec<_>, _>>()?,
            };
            let mut out = Vec::with_capacity(rows.len());
            for (row, tags_json) in rows {
                let tags: Vec<String> = serde_json::from_slice(&tags_json)
                    .map_err(|e| DbError::Serde(format!("tags: {e}")))?;
                out.push(EvalFlagRow { tags, ..row });
            }
            Ok(out)
        })
    }
}

#[allow(clippy::type_complexity)]
fn row_to_flag(r: &rusqlite::Row<'_>) -> rusqlite::Result<(EvalFlagRow, Vec<u8>)> {
    let id: i64 = r.get(0)?;
    let conv: String = r.get(1)?;
    let from_seq: i64 = r.get(2)?;
    let to_seq: i64 = r.get(3)?;
    let label: String = r.get(4)?;
    let tags_json: Vec<u8> = r
        .get::<_, Option<Vec<u8>>>(5)?
        .unwrap_or_else(|| b"[]".to_vec());
    let flagged_by: String = r.get(6)?;
    let flagged_at: i64 = r.get(7)?;
    let notes: Option<String> = r.get(8)?;
    Ok((
        EvalFlagRow {
            id: Some(id),
            conversation_id: ConversationId::from(conv),
            from_seq,
            to_seq,
            label,
            tags: vec![], // re-decoded by caller
            flagged_by,
            flagged_at,
            notes,
        },
        tags_json,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::DbConfig;
    use crate::migrations::MigrationRunner;

    fn fresh_db() -> Database {
        let db = Database::open(&DbConfig::in_memory_unencrypted()).unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        db
    }

    #[test]
    fn fixture_link_references_are_bounded_plain_identifiers() {
        assert!(valid_fixture_reference("INC-42"));
        assert!(valid_fixture_reference("v2026.09.29"));
        assert!(!valid_fixture_reference("https://example.test/INC-42"));
        assert!(!valid_fixture_reference(""));
        assert!(!valid_fixture_reference(&"x".repeat(129)));
    }

    #[test]
    fn offline_fixture_validation_preserves_incident_and_release_links() {
        let fixture = RegressionFixture {
            schema_version: 1,
            effects_enabled: false,
            provenance: RegressionFixtureProvenance {
                flagged_range_id: 7,
                source_conversation_sha256: "a".repeat(64),
                source_fingerprint_sha256: "b".repeat(64),
                label: "recovery-regression".into(),
                tags: vec!["recovery".into()],
                from_seq: 1,
                to_seq: 1,
                flagged_at: 10,
                exported_at: 11,
                incident_ref: Some("INC-42".into()),
                release_ref: Some("v2026.09.29".into()),
            },
            redaction: RegressionFixtureRedaction {
                policy_version: "redaction-v1".into(),
                redaction_map_sha256: "c".repeat(64),
                replacements_applied: 0,
                synthetic_ids: Vec::new(),
            },
            events: vec![RegressionFixtureEvent {
                seq: 1,
                kind: "user_msg".into(),
                actor: Some("controller".into()),
                committed_at: 10,
                payload: serde_json::json!({"text":"synthetic task"}),
            }],
            expected_transitions: vec![ExpectedStateTransition {
                seq: 1,
                transition: "user_msg".into(),
            }],
            mock_tool_responses: Vec::new(),
            raw_stream_fixtures: Vec::new(),
            synthetic_media: Vec::new(),
            policy_cases: Vec::new(),
            tool_catalogs: Vec::new(),
        };
        let validation = validate_regression_fixture(&fixture).unwrap();
        assert_eq!(validation.incident_ref.as_deref(), Some("INC-42"));
        assert_eq!(validation.release_ref.as_deref(), Some("v2026.09.29"));
        assert!(!validation.effects_enabled);
    }

    #[test]
    fn checked_in_synthetic_fixture_stays_valid_for_ci_replay() {
        let fixture: RegressionFixture = serde_json::from_str(include_str!(
            "../../../evals/fixtures/synthetic-offline-regression.json"
        ))
        .unwrap();
        let validation = validate_regression_fixture(&fixture).unwrap();
        assert_eq!(validation.fixture_id, 1);
        assert_eq!(validation.incident_ref.as_deref(), Some("SYNTHETIC-INC-1"));
        assert_eq!(validation.release_ref.as_deref(), Some("v0.0.0"));
        assert!(!validation.effects_enabled);
        assert_eq!(fixture.policy_cases.len(), 3);
        assert_eq!(fixture.raw_stream_fixtures.len(), 1);
        let first = hex::decode(&fixture.raw_stream_fixtures[0].chunks_hex[0]).unwrap();
        let second = hex::decode(&fixture.raw_stream_fixtures[0].chunks_hex[1]).unwrap();
        assert!(
            first.last() == Some(&0xC3) && second.first() == Some(&0xA9),
            "fixture chunk boundaries must split the UTF-8 code point"
        );
    }

    #[test]
    fn raw_stream_fixture_validation_rejects_malformed_hex() {
        let mut fixture: RegressionFixture = serde_json::from_str(include_str!(
            "../../../evals/fixtures/synthetic-offline-regression.json"
        ))
        .unwrap();
        fixture.raw_stream_fixtures[0].chunks_hex[0] = "xyz".into();
        assert!(validate_regression_fixture(&fixture).is_err());
    }

    #[test]
    fn attached_media_requires_a_synthetic_replacement_and_known_mime_type() {
        let fixture: RegressionFixture = serde_json::from_str(include_str!(
            "../../../evals/fixtures/synthetic-image-regression.json"
        ))
        .unwrap();
        assert!(validate_regression_fixture(&fixture).unwrap().valid);

        let mut missing_replacement = fixture.clone();
        missing_replacement.synthetic_media.clear();
        assert!(validate_regression_fixture(&missing_replacement).is_err());

        let mut source_bytes = fixture;
        source_bytes.synthetic_media[0].synthetic = false;
        assert!(validate_regression_fixture(&source_bytes).is_err());

        let mut audio = serde_json::from_str::<RegressionFixture>(include_str!(
            "../../../evals/fixtures/synthetic-image-regression.json"
        ))
        .unwrap();
        audio.synthetic_media[0].mime_type = "audio/wav".into();
        audio.synthetic_media[0].content_hex =
            "524946462400000057415645666d74201000000001000100401f0000401f0000010008006461746100000000".into();
        assert!(validate_regression_fixture(&audio).unwrap().valid);
        audio.synthetic_media[0].mime_type = "application/octet-stream".into();
        assert!(validate_regression_fixture(&audio).is_err());
    }

    #[test]
    fn policy_fixture_validation_rejects_duplicate_case_ids_and_unknown_trust() {
        let mut fixture: RegressionFixture = serde_json::from_str(include_str!(
            "../../../evals/fixtures/synthetic-offline-regression.json"
        ))
        .unwrap();
        fixture.policy_cases[1].case_id = fixture.policy_cases[0].case_id.clone();
        assert!(validate_regression_fixture(&fixture).is_err());

        let mut fixture: RegressionFixture = serde_json::from_str(include_str!(
            "../../../evals/fixtures/synthetic-offline-regression.json"
        ))
        .unwrap();
        fixture.policy_cases[0].sender_trust = "UntrustedTypo".into();
        assert!(validate_regression_fixture(&fixture).is_err());
    }

    fn mk_row(label: &str) -> EvalFlagRow {
        EvalFlagRow {
            id: None,
            conversation_id: ConversationId::from("c1"),
            from_seq: 1,
            to_seq: 10,
            label: label.into(),
            tags: vec!["trust-class".into()],
            flagged_by: "controller".into(),
            flagged_at: 100,
            notes: Some("the model leaked the api_key".into()),
        }
    }

    #[test]
    fn insert_and_list_round_trips_full_row() {
        let db = fresh_db();
        let store = EvalFlaggedStore::new(&db);
        let id = store.insert(&mk_row("regression-1")).unwrap();
        assert!(id > 0);
        let all = store.list_all().unwrap();
        assert_eq!(all.len(), 1);
        let r = &all[0];
        assert_eq!(r.id, Some(id));
        assert_eq!(r.label, "regression-1");
        assert_eq!(r.tags, vec!["trust-class".to_string()]);
        assert_eq!(r.notes.as_deref(), Some("the model leaked the api_key"));
    }

    #[test]
    fn list_by_label_filters_correctly() {
        let db = fresh_db();
        let store = EvalFlaggedStore::new(&db);
        store.insert(&mk_row("alpha")).unwrap();
        store.insert(&mk_row("beta")).unwrap();
        store.insert(&mk_row("alpha")).unwrap();

        assert_eq!(store.list_by_label("alpha").unwrap().len(), 2);
        assert_eq!(store.list_by_label("beta").unwrap().len(), 1);
        assert_eq!(store.list_by_label("nonexistent").unwrap().len(), 0);
    }

    #[test]
    fn list_orders_newest_first() {
        let db = fresh_db();
        let store = EvalFlaggedStore::new(&db);
        let mut early = mk_row("x");
        early.flagged_at = 100;
        let mut late = mk_row("x");
        late.flagged_at = 200;
        // Insert out of order.
        store.insert(&early).unwrap();
        store.insert(&late).unwrap();
        let rows = store.list_all().unwrap();
        assert_eq!(rows[0].flagged_at, 200);
        assert_eq!(rows[1].flagged_at, 100);
    }

    /// Adversarial: from_seq > to_seq is rejected as an Invariant
    /// error. Operators with typos shouldn't be able to write a
    /// nonsensical range.
    #[test]
    fn inverted_range_is_rejected() {
        let db = fresh_db();
        let store = EvalFlaggedStore::new(&db);
        let mut row = mk_row("bad");
        row.from_seq = 50;
        row.to_seq = 10;
        let err = store.insert(&row).unwrap_err();
        assert!(matches!(err, DbError::Invariant(_)));
    }

    #[test]
    fn empty_tags_round_trip() {
        let db = fresh_db();
        let store = EvalFlaggedStore::new(&db);
        let mut row = mk_row("notags");
        row.tags.clear();
        store.insert(&row).unwrap();
        let got = store.list_by_label("notags").unwrap();
        assert!(got[0].tags.is_empty());
    }
}
