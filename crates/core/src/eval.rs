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
