//! Durable model qualifications, compaction provenance, and run trace cursors.

use crate::db::{Database, DbError};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum HarnessStoreError {
    #[error(transparent)]
    Database(#[from] DbError),
    #[error("serialization failed: {0}")]
    Serialization(#[from] serde_json::Error),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelIdentity {
    pub model_id: String,
    pub quantization: String,
    pub chat_template: String,
    pub backend_version: String,
    pub parser_version: String,
}

impl ModelIdentity {
    pub fn stable_hash(&self) -> Result<String, serde_json::Error> {
        let bytes = serde_json::to_vec(self)?;
        Ok(hex::encode(Sha256::digest(bytes)))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelCapabilityProfile {
    pub identity: ModelIdentity,
    pub context_tokens: u32,
    pub observed: serde_json::Value,
    pub qualified_at: i64,
    pub invalidated_at: Option<i64>,
}

/// Use native constrained decoding only when this exact identity passed the
/// strict JSON-schema probe; callers must still validate the returned value.
pub fn qualified_json_schema_format(
    profile: &ModelCapabilityProfile,
    name: &str,
    schema: serde_json::Value,
) -> Option<serde_json::Value> {
    let qualified = profile
        .observed
        .get("structured_json")
        .and_then(|check| check.get("passed"))
        .and_then(serde_json::Value::as_bool)
        == Some(true);
    let valid_schema = jsonschema::validator_for(&schema).is_ok();
    (qualified && valid_schema).then(|| {
        serde_json::json!({
            "type": "json_schema",
            "json_schema": { "name": name, "strict": true, "schema": schema }
        })
    })
}

/// Parse and host-validate structured model output even when native decoding
/// was enabled. Returns a typed JSON value only after schema validation.
pub fn validate_structured_output(
    schema: &serde_json::Value,
    output: &str,
) -> Result<serde_json::Value, String> {
    let value: serde_json::Value = serde_json::from_str(output)
        .map_err(|error| format!("structured output is not valid JSON: {error}"))?;
    let validator = jsonschema::validator_for(schema)
        .map_err(|error| format!("structured output schema is invalid: {error}"))?;
    if !validator.is_valid(&value) {
        return Err("structured output does not match the requested schema".into());
    }
    Ok(value)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CompactionReceipt {
    pub receipt_id: String,
    pub conversation_id: String,
    pub source_start_seq: i64,
    pub source_end_seq: i64,
    pub source_fingerprint: String,
    pub summary_version: u32,
    pub retained_constraints: Vec<String>,
    pub pending_work: Vec<String>,
    pub discarded_content: Vec<String>,
    pub trust_class: String,
    pub summary: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunTraceEvent {
    pub cursor: i64,
    pub run_id: String,
    pub event_kind: String,
    pub subject_id: Option<String>,
    pub status: String,
    pub metadata: serde_json::Value,
    pub created_at: i64,
}

pub struct HarnessStore<'db> {
    db: &'db Database,
}

impl<'db> HarnessStore<'db> {
    pub fn new(db: &'db Database) -> Self {
        Self { db }
    }

    pub fn fingerprint<T: Serialize>(value: &T) -> Result<String, HarnessStoreError> {
        Ok(hex::encode(Sha256::digest(serde_json::to_vec(value)?)))
    }

    pub fn save_profile(&self, profile: &ModelCapabilityProfile) -> Result<(), HarnessStoreError> {
        let identity_hash = profile.identity.stable_hash()?;
        let observed = serde_json::to_string(&profile.observed)?;
        self.db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO state_model_capability_profiles \
                 (identity_hash, model_id, quantization, chat_template, backend_version, parser_version, \
                  context_tokens, observed_json, qualified_at, invalidated_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10) \
                 ON CONFLICT(identity_hash) DO UPDATE SET context_tokens=excluded.context_tokens, \
                 observed_json=excluded.observed_json, qualified_at=excluded.qualified_at, \
                 invalidated_at=excluded.invalidated_at",
                params![identity_hash, profile.identity.model_id, profile.identity.quantization,
                    profile.identity.chat_template, profile.identity.backend_version, profile.identity.parser_version,
                    profile.context_tokens, observed, profile.qualified_at, profile.invalidated_at],
            )?;
            Ok(())
        })?;
        Ok(())
    }

    pub fn get_profile(
        &self,
        identity: &ModelIdentity,
    ) -> Result<Option<ModelCapabilityProfile>, HarnessStoreError> {
        let hash = identity.stable_hash()?;
        self.db.with_conn(|conn| {
            conn.query_row(
                "SELECT context_tokens, observed_json, qualified_at, invalidated_at \
                 FROM state_model_capability_profiles WHERE identity_hash=?1 AND invalidated_at IS NULL",
                [hash], |row| Ok((row.get::<_, u32>(0)?, row.get::<_, String>(1)?, row.get::<_, i64>(2)?, row.get::<_, Option<i64>>(3)?)),
            ).optional().map_err(DbError::from)
        })?.map(|(context_tokens, json, qualified_at, invalidated_at)| Ok(ModelCapabilityProfile {
            identity: identity.clone(), context_tokens, observed: serde_json::from_str(&json)?, qualified_at, invalidated_at,
        })).transpose()
    }

    pub fn invalidate_profile(
        &self,
        identity: &ModelIdentity,
        at: i64,
    ) -> Result<(), HarnessStoreError> {
        let hash = identity.stable_hash()?;
        self.db.with_conn(|conn| {
            conn.execute(
            "UPDATE state_model_capability_profiles SET invalidated_at=?2 WHERE identity_hash=?1",
            params![hash, at],
        )?;
            Ok(())
        })?;
        Ok(())
    }

    pub fn save_compaction_receipt(
        &self,
        receipt: &CompactionReceipt,
    ) -> Result<(), HarnessStoreError> {
        let retained_constraints = serde_json::to_string(&receipt.retained_constraints)?;
        let pending_work = serde_json::to_string(&receipt.pending_work)?;
        let discarded_content = serde_json::to_string(&receipt.discarded_content)?;
        self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE state_compaction_receipts SET invalidated_at=?3 \
                 WHERE conversation_id=?1 AND source_start_seq=?2 AND invalidated_at IS NULL \
                 AND (source_fingerprint<>?4 OR summary_version<>?5)",
                params![receipt.conversation_id, receipt.source_start_seq, receipt.created_at, receipt.source_fingerprint, receipt.summary_version],
            )?;
            conn.execute(
            "INSERT INTO state_compaction_receipts \
             (receipt_id, conversation_id, source_start_seq, source_end_seq, source_fingerprint, \
              summary_version, retained_constraints_json, pending_work_json, discarded_content_json, \
              trust_class, summary, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![receipt.receipt_id, receipt.conversation_id, receipt.source_start_seq,
                receipt.source_end_seq, receipt.source_fingerprint, receipt.summary_version,
                retained_constraints, pending_work, discarded_content, receipt.trust_class, receipt.summary, receipt.created_at],
        )?; Ok(()) })?;
        Ok(())
    }

    pub fn active_compaction_receipt(
        &self,
        conversation_id: &str,
        source_fingerprint: &str,
        summary_version: u32,
    ) -> Result<Option<CompactionReceipt>, HarnessStoreError> {
        Ok(self.db.with_conn(|conn| {
            conn.query_row(
                "SELECT receipt_id, conversation_id, source_start_seq, source_end_seq, source_fingerprint, \
                 summary_version, retained_constraints_json, pending_work_json, discarded_content_json, \
                 trust_class, summary, created_at FROM state_compaction_receipts \
                 WHERE conversation_id=?1 AND source_fingerprint=?2 AND summary_version=?3 AND invalidated_at IS NULL \
                 ORDER BY created_at DESC LIMIT 1",
                params![conversation_id, source_fingerprint, summary_version],
                row_to_compaction_receipt,
            ).optional().map_err(DbError::from)
        })?)
    }

    pub fn compaction_receipt(
        &self,
        receipt_id: &str,
    ) -> Result<Option<CompactionReceipt>, HarnessStoreError> {
        Ok(self.db.with_conn(|conn| {
            conn.query_row(
                "SELECT receipt_id, conversation_id, source_start_seq, source_end_seq, source_fingerprint, \
                 summary_version, retained_constraints_json, pending_work_json, discarded_content_json, \
                 trust_class, summary, created_at FROM state_compaction_receipts WHERE receipt_id=?1",
                [receipt_id], row_to_compaction_receipt,
            ).optional().map_err(DbError::from)
        })?)
    }

    /// Append a content-minimized event and return its resumable cursor.
    pub fn append_trace_event(
        &self,
        run_id: &str,
        event_kind: &str,
        subject_id: Option<&str>,
        status: &str,
        metadata: &serde_json::Value,
        created_at: i64,
    ) -> Result<i64, HarnessStoreError> {
        let metadata = serde_json::to_string(metadata)?;
        Ok(self.db.with_conn(|conn| {
            conn.execute("INSERT INTO state_run_trace_events (run_id,event_kind,subject_id,status,metadata_json,created_at) VALUES (?1,?2,?3,?4,?5,?6)",
                params![run_id,event_kind,subject_id,status,metadata,created_at])?;
            Ok(conn.last_insert_rowid())
        })?)
    }

    pub fn trace_events_after(
        &self,
        run_id: &str,
        after: i64,
        limit: usize,
    ) -> Result<Vec<RunTraceEvent>, HarnessStoreError> {
        Ok(self.db.with_conn(|conn| {
            let mut stmt = conn.prepare("SELECT cursor,event_kind,subject_id,status,metadata_json,created_at FROM state_run_trace_events WHERE run_id=?1 AND cursor>?2 ORDER BY cursor LIMIT ?3")?;
            let rows = stmt.query_map(params![run_id, after, limit.clamp(1, 500)], |row| Ok((row.get::<_, i64>(0)?,row.get::<_, String>(1)?,row.get::<_, Option<String>>(2)?,row.get::<_, String>(3)?,row.get::<_, String>(4)?,row.get::<_, i64>(5)?)))?;
            let mut result = Vec::new();
            for row in rows {
                let (cursor,event_kind,subject_id,status,json,created_at) = row?;
                result.push(RunTraceEvent { cursor, run_id: run_id.to_owned(), event_kind, subject_id, status, metadata: serde_json::from_str(&json).map_err(|e| DbError::Invariant(e.to_string()))?, created_at });
            }
            Ok(result)
        })?)
    }

    /// A cursor is valid only while its event remains in durable storage.
    /// Returning `true` tells clients to reload the authoritative run snapshot.
    pub fn trace_cursor_requires_reload(
        &self,
        run_id: &str,
        cursor: i64,
    ) -> Result<bool, HarnessStoreError> {
        if cursor <= 0 {
            return Ok(false);
        }
        Ok(self.db.with_conn(|conn| {
            let exists: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM state_run_trace_events WHERE run_id=?1 AND cursor=?2)",
                params![run_id, cursor],
                |row| row.get(0),
            )?;
            let latest: i64 = conn.query_row(
                "SELECT COALESCE(MAX(cursor),0) FROM state_run_trace_events WHERE run_id=?1",
                [run_id],
                |row| row.get(0),
            )?;
            Ok(!exists || cursor > latest)
        })?)
    }
}

fn row_to_compaction_receipt(row: &rusqlite::Row<'_>) -> rusqlite::Result<CompactionReceipt> {
    let decode_list = |index: usize| -> rusqlite::Result<Vec<String>> {
        let encoded: String = row.get(index)?;
        serde_json::from_str(&encoded).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                index,
                rusqlite::types::Type::Text,
                error.into(),
            )
        })
    };
    Ok(CompactionReceipt {
        receipt_id: row.get(0)?,
        conversation_id: row.get(1)?,
        source_start_seq: row.get(2)?,
        source_end_seq: row.get(3)?,
        source_fingerprint: row.get(4)?,
        summary_version: row.get(5)?,
        retained_constraints: decode_list(6)?,
        pending_work: decode_list(7)?,
        discarded_content: decode_list(8)?,
        trust_class: row.get(9)?,
        summary: row.get(10)?,
        created_at: row.get(11)?,
    })
}
