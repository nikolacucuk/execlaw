//! Governed memory assets, agent loadouts, and local knowledge indexes.
//!
//! The asset row is metadata only. Existing event-sourced memory, skills, and
//! derived knowledge stores remain authoritative for their content. This
//! module supplies the common Tencent-inspired registry and bounded retrieval
//! surface without introducing a second service or trust model.

use crate::db::{Database, DbError};
use rusqlite::{OptionalExtension, params, params_from_iter};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const MEMORY_RERANKER_VERSION: &str = "local-hybrid-rrf-v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetType {
    Memory,
    Skill,
    Wiki,
    CodeGraph,
    Research,
}

impl AssetType {
    fn as_sql(self) -> &'static str {
        match self {
            Self::Memory => "memory",
            Self::Skill => "skill",
            Self::Wiki => "wiki",
            Self::CodeGraph => "code_graph",
            Self::Research => "research",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "memory" => Self::Memory,
            "skill" => Self::Skill,
            "wiki" => Self::Wiki,
            "code_graph" => Self::CodeGraph,
            "research" => Self::Research,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetVisibility {
    Private,
    Team,
    Restricted,
    Agent,
}

impl AssetVisibility {
    fn as_sql(self) -> &'static str {
        match self {
            Self::Private => "private",
            Self::Team => "team",
            Self::Restricted => "restricted",
            Self::Agent => "agent",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InjectionMode {
    Hot,
    Discoverable,
    ToolOnly,
}

impl InjectionMode {
    fn as_sql(self) -> &'static str {
        match self {
            Self::Hot => "hot",
            Self::Discoverable => "discoverable",
            Self::ToolOnly => "tool_only",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryAsset {
    pub asset_id: String,
    pub asset_type: AssetType,
    pub name: String,
    pub description: String,
    pub owner_scope: String,
    pub visibility: AssetVisibility,
    pub trust_floor: String,
    pub status: String,
    pub version: i64,
    pub source_ref: Option<String>,
    pub content_ref: Option<String>,
    pub source_hash: Option<String>,
    pub expires_at: Option<i64>,
    pub last_used_at: Option<i64>,
    pub usage_count: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone)]
pub struct NewMemoryAsset<'a> {
    pub asset_id: &'a str,
    pub asset_type: AssetType,
    pub name: &'a str,
    pub description: &'a str,
    pub owner_scope: &'a str,
    pub visibility: AssetVisibility,
    pub trust_floor: &'a str,
    pub source_ref: Option<&'a str>,
    pub content_ref: Option<&'a str>,
    pub source_hash: Option<&'a str>,
    pub now_unix: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetBinding {
    pub asset_id: String,
    pub agent_scope: String,
    pub injection_mode: InjectionMode,
    pub priority: i64,
    pub max_chars: i64,
    pub created_at: i64,
}

/// An asset admitted to a turn's HOT loadout after trust, lifecycle, and
/// byte-budget policy have been applied.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedLoadoutAsset {
    pub asset: MemoryAsset,
    pub binding: AssetBinding,
    pub max_chars: i64,
}

/// Metadata-only proof of why a governed asset was injected into one turn.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TurnAssetLoadoutReceipt {
    pub agent_scope: String,
    pub conversation_trust_class: String,
    pub readable_trust_classes: Vec<String>,
    pub readable_owner_scopes: Vec<String>,
    pub resolved_at: i64,
    pub retrieval_query_sha256: Option<String>,
    pub assets: Vec<TurnAssetLoadoutEntry>,
    pub retrieved_assets: Vec<TurnAssetRetrievalEntry>,
}

/// One asset that passed every loadout policy check and was actually injected.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TurnAssetLoadoutEntry {
    pub asset_id: String,
    pub name: String,
    pub asset_type: AssetType,
    pub version: i64,
    pub source_hash: Option<String>,
    pub owner_scope: String,
    pub visibility: AssetVisibility,
    pub trust_floor: String,
    pub status: String,
    pub expires_at: Option<i64>,
    pub binding_agent_scope: String,
    pub binding_mode: InjectionMode,
    pub binding_priority: i64,
    pub binding_max_chars: i64,
    pub injected_chars: usize,
    pub admission_reasons: Vec<String>,
}

/// One trust-eligible retrieval result injected into the model context.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TurnAssetRetrievalEntry {
    pub asset_id: String,
    pub name: String,
    pub asset_type: AssetType,
    pub version: i64,
    pub source_hash: Option<String>,
    pub owner_scope: String,
    pub visibility: AssetVisibility,
    pub trust_floor: String,
    pub expires_at: Option<i64>,
    pub score_micros: i64,
    pub lexical_rank: i64,
    pub vector_rank: Option<i64>,
    pub reranker_version: String,
    pub injected_chars: usize,
    pub admission_reasons: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetHit {
    pub asset: MemoryAsset,
    pub score: f64,
    pub lexical_rank: i64,
    pub vector_rank: Option<i64>,
}

/// Operator-selected model identities used by trust-first local retrieval.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryRetrievalConfig {
    pub embedding_model_id: String,
    pub reranker_version: String,
    pub updated_at: i64,
}

/// Active, source-versioned content that is missing from a selected embedding index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbeddingRebuildCandidate {
    pub asset_id: String,
    pub source_hash: String,
    pub input_text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WikiPage {
    pub wiki_id: String,
    pub page_ref: String,
    pub title: String,
    pub body: String,
    pub source_path: String,
    pub source_hash: String,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeNode {
    pub graph_id: String,
    pub symbol: String,
    pub kind: String,
    pub file_path: String,
    pub start_line: i64,
    pub end_line: i64,
    pub source: Option<String>,
}

#[derive(Debug, Error)]
pub enum MemoryAssetError {
    #[error(transparent)]
    Db(#[from] DbError),
    #[error("asset {0} not found")]
    NotFound(String),
    #[error("invalid asset type or visibility in database")]
    InvalidEnum,
    #[error("embedding dimensions do not match")]
    InvalidEmbedding,
    #[error("unsupported or invalid memory retrieval configuration")]
    InvalidRetrievalConfig,
    #[error("eligible memory candidate set exceeds the {limit}-asset search bound")]
    CandidateSetTooLarge { limit: usize },
}

pub struct MemoryAssetStore<'db> {
    db: &'db Database,
}

impl<'db> MemoryAssetStore<'db> {
    pub fn new(db: &'db Database) -> Self {
        Self { db }
    }

    /// Read the configured local embedding model and reranker algorithm version.
    pub fn retrieval_config(&self) -> Result<Option<MemoryRetrievalConfig>, MemoryAssetError> {
        self.db
            .with_conn(|connection| {
                connection
                    .query_row(
                        "SELECT embedding_model_id, reranker_version, updated_at \
                         FROM config_memory_retrieval WHERE singleton = 1",
                        [],
                        |row| {
                            Ok(MemoryRetrievalConfig {
                                embedding_model_id: row.get(0)?,
                                reranker_version: row.get(1)?,
                                updated_at: row.get(2)?,
                            })
                        },
                    )
                    .optional()
                    .map_err(DbError::from)
            })
            .map_err(Into::into)
    }

    /// Save the embedding model and supported local reranker version. Changing the
    /// embedding model leaves old derived vectors until callers rebuild its index.
    pub fn set_retrieval_config(
        &self,
        embedding_model_id: &str,
        reranker_version: &str,
        now_unix: i64,
    ) -> Result<(), MemoryAssetError> {
        if embedding_model_id.trim().is_empty()
            || embedding_model_id.len() > 256
            || reranker_version != MEMORY_RERANKER_VERSION
        {
            return Err(MemoryAssetError::InvalidRetrievalConfig);
        }
        self.db.with_conn(|connection| {
            connection.execute(
                "INSERT INTO config_memory_retrieval(singleton, embedding_model_id, reranker_version, updated_at) \
                 VALUES (1, ?1, ?2, ?3) ON CONFLICT(singleton) DO UPDATE SET \
                 embedding_model_id=excluded.embedding_model_id, \
                 reranker_version=excluded.reranker_version, updated_at=excluded.updated_at",
                params![embedding_model_id, reranker_version, now_unix],
            )?;
            Ok(())
        })?;
        Ok(())
    }

    /// Return bounded assets that lack a current embedding for `model_id`.
    pub fn embedding_rebuild_candidates(
        &self,
        model_id: &str,
        limit: u32,
    ) -> Result<Vec<EmbeddingRebuildCandidate>, MemoryAssetError> {
        if model_id.trim().is_empty() || model_id.len() > 256 {
            return Err(MemoryAssetError::InvalidEmbedding);
        }
        self.db
            .with_conn(|connection| {
                let mut statement = connection.prepare(
                    "SELECT a.asset_id, a.source_hash, a.name, a.description, a.content_ref \
                 FROM memory_assets a LEFT JOIN memory_asset_embeddings e \
                   ON e.asset_id = a.asset_id AND e.model_id = ?1 \
                 WHERE a.status = 'active' AND a.source_hash IS NOT NULL \
                   AND a.content_ref IS NOT NULL \
                   AND (e.asset_id IS NULL OR e.source_hash <> a.source_hash) \
                 ORDER BY a.created_at, a.asset_id LIMIT ?2",
                )?;
                let rows = statement
                    .query_map(params![model_id, limit.clamp(1, 128) as i64], |row| {
                        let asset_id: String = row.get(0)?;
                        let source_hash: String = row.get(1)?;
                        let name: String = row.get(2)?;
                        let description: String = row.get(3)?;
                        let content: String = row.get(4)?;
                        Ok(EmbeddingRebuildCandidate {
                            asset_id,
                            source_hash,
                            input_text: format!(
                                "{name}\n{description}\n{}",
                                content.chars().take(32_768).collect::<String>()
                            ),
                        })
                    })?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(rows)
            })
            .map_err(Into::into)
    }

    pub fn create(&self, asset: NewMemoryAsset<'_>) -> Result<(), MemoryAssetError> {
        self.db.transaction(|tx| {
            let tombstoned: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM state_memory_asset_deletion_tombstones \
                 WHERE asset_id = ?1)",
                params![asset.asset_id],
                |row| row.get(0),
            )?;
            if tombstoned {
                return Err(DbError::Invariant(
                    "memory asset ID is protected by a privacy tombstone".into(),
                ));
            }
            tx.execute(
                "INSERT INTO memory_assets(
                    asset_id, asset_type, name, description, owner_scope,
                    visibility, trust_floor, source_ref, content_ref, source_hash,
                    created_at, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11)",
                params![
                    asset.asset_id,
                    asset.asset_type.as_sql(),
                    asset.name,
                    asset.description,
                    asset.owner_scope,
                    asset.visibility.as_sql(),
                    asset.trust_floor,
                    asset.source_ref,
                    asset.content_ref,
                    asset.source_hash,
                    asset.now_unix,
                ],
            )?;
            Ok(())
        })?;
        Ok(())
    }

    /// Delete a governed asset, its bindings, embeddings, and FTS projection.
    /// The tombstone remains after deletion to fence delayed writers and replay.
    pub fn delete(
        &self,
        asset_id: &str,
        requested_by: &str,
        now_unix: i64,
    ) -> Result<bool, MemoryAssetError> {
        if requested_by.trim().is_empty() || requested_by.len() > 128 {
            return Err(MemoryAssetError::Db(DbError::Invariant(
                "memory asset deletion actor is invalid".into(),
            )));
        }
        let asset_id = asset_id.to_owned();
        let requested_by = requested_by.to_owned();
        self.db
            .transaction(|tx| {
                let tombstoned: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM state_memory_asset_deletion_tombstones \
                     WHERE asset_id = ?1)",
                    params![asset_id],
                    |row| row.get(0),
                )?;
                if tombstoned {
                    return Ok(false);
                }
                let exists: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM memory_assets WHERE asset_id = ?1)",
                    params![asset_id],
                    |row| row.get(0),
                )?;
                tx.execute(
                    "INSERT INTO state_memory_asset_deletion_tombstones \
                     (asset_id, requested_by, requested_at, completed_at) \
                     VALUES (?1, ?2, ?3, ?3)",
                    params![asset_id, requested_by, now_unix],
                )?;
                tx.execute(
                    "DELETE FROM memory_asset_search WHERE asset_id = ?1",
                    params![asset_id],
                )?;
                // FTS5 has no foreign-key cascade, so remove this derived
                // wiki projection before the relational wiki rows cascade.
                tx.execute(
                    "DELETE FROM knowledge_wiki_search WHERE wiki_id IN (\
                       SELECT wiki_id FROM knowledge_wikis WHERE asset_id = ?1\
                     )",
                    params![asset_id],
                )?;
                if !exists {
                    return Ok(false);
                }
                tx.execute(
                    "DELETE FROM memory_assets WHERE asset_id = ?1",
                    params![asset_id],
                )?;
                Ok(true)
            })
            .map_err(MemoryAssetError::from)
    }

    pub fn get(&self, asset_id: &str) -> Result<Option<MemoryAsset>, MemoryAssetError> {
        Ok(self.db.with_conn(|c| {
            Ok(c.query_row(
                "SELECT asset_id, asset_type, name, description, owner_scope,
                        visibility, trust_floor, status, version, source_ref,
                        content_ref, source_hash, expires_at, last_used_at,
                        usage_count, created_at, updated_at
                 FROM memory_assets WHERE asset_id = ?1",
                params![asset_id],
                row_to_asset,
            )
            .optional()?)
        })?)
    }

    pub fn list(&self, limit: u32) -> Result<Vec<MemoryAsset>, MemoryAssetError> {
        Ok(self.db.with_conn(|connection| {
            let mut statement = connection.prepare(
                "SELECT asset_id, asset_type, name, description, owner_scope,
                        visibility, trust_floor, status, version, source_ref,
                        content_ref, source_hash, expires_at, last_used_at,
                        usage_count, created_at, updated_at
                 FROM memory_assets ORDER BY updated_at DESC, asset_id LIMIT ?1",
            )?;
            Ok(statement
                .query_map(params![limit.min(200) as i64], row_to_asset)?
                .collect::<Result<Vec<_>, _>>()?)
        })?)
    }

    pub fn bind(
        &self,
        asset_id: &str,
        agent_scope: &str,
        injection_mode: InjectionMode,
        priority: i64,
        max_chars: i64,
        now_unix: i64,
    ) -> Result<(), MemoryAssetError> {
        if self.get(asset_id)?.is_none() {
            return Err(MemoryAssetError::NotFound(asset_id.to_owned()));
        }
        self.db.with_conn(|c| {
            c.execute(
                "INSERT INTO memory_asset_bindings
                    (asset_id, agent_scope, injection_mode, priority, max_chars, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(asset_id, agent_scope) DO UPDATE SET
                    injection_mode = excluded.injection_mode,
                    priority = excluded.priority,
                    max_chars = excluded.max_chars",
                params![
                    asset_id,
                    agent_scope,
                    injection_mode.as_sql(),
                    priority,
                    max_chars,
                    now_unix
                ],
            )?;
            Ok(())
        })?;
        Ok(())
    }

    pub fn list_loadout(
        &self,
        agent_scope: &str,
        limit: u32,
    ) -> Result<Vec<AssetBinding>, MemoryAssetError> {
        Ok(self.db.with_conn(|c| {
            let mut stmt = c.prepare(
                "SELECT asset_id, agent_scope, injection_mode, priority, max_chars, created_at
                 FROM memory_asset_bindings
                 WHERE agent_scope = ?1
                 ORDER BY priority DESC, created_at ASC LIMIT ?2",
            )?;
            Ok(stmt
                .query_map(params![agent_scope, limit as i64], row_to_binding)?
                .collect::<Result<Vec<_>, _>>()?)
        })?)
    }

    pub fn unbind(&self, asset_id: &str, agent_scope: &str) -> Result<bool, MemoryAssetError> {
        Ok(self.db.with_conn(|connection| {
            Ok(connection.execute(
                "DELETE FROM memory_asset_bindings WHERE asset_id = ?1 AND agent_scope = ?2",
                params![asset_id, agent_scope],
            )? > 0)
        })?)
    }

    /// Resolve HOT assets for a turn without exposing unauthorized metadata to
    /// ranking or prompt assembly. `readable_trust_classes` is derived by the
    /// caller from the conversation's read-down trust policy.
    pub fn resolve_hot_loadout(
        &self,
        agent_scope: &str,
        readable_trust_classes: &[&str],
        readable_owner_scopes: &[&str],
        now_unix: i64,
        char_budget: usize,
        limit: u32,
    ) -> Result<Vec<ResolvedLoadoutAsset>, MemoryAssetError> {
        let bindings = self.list_loadout(agent_scope, limit)?;
        let mut used = 0_usize;
        let mut resolved = Vec::new();
        for binding in bindings {
            if binding.injection_mode != InjectionMode::Hot {
                continue;
            }
            let Some(asset) = self.get(&binding.asset_id)? else {
                continue;
            };
            let visibility_allowed = match asset.visibility {
                AssetVisibility::Private | AssetVisibility::Team => true,
                AssetVisibility::Restricted => readable_trust_classes.contains(&"Controller"),
                AssetVisibility::Agent => binding.agent_scope == agent_scope,
            };
            if asset.status != "active"
                || asset.expires_at.is_some_and(|expiry| expiry <= now_unix)
                || !readable_trust_classes
                    .iter()
                    .any(|trust| *trust == asset.trust_floor)
                || !readable_owner_scopes
                    .iter()
                    .any(|scope| *scope == asset.owner_scope)
                || !visibility_allowed
            {
                continue;
            }
            let requested = usize::try_from(binding.max_chars).unwrap_or(usize::MAX);
            let available = char_budget.saturating_sub(used);
            if requested == 0 || requested > available {
                continue;
            }
            used = used.saturating_add(requested);
            let max_chars = binding.max_chars;
            resolved.push(ResolvedLoadoutAsset {
                asset,
                binding,
                max_chars,
            });
        }
        Ok(resolved)
    }

    /// Persist the metadata-only selection receipt for one user turn.
    pub fn record_turn_loadout(
        &self,
        conversation_id: &str,
        input_event_seq: i64,
        receipt: &TurnAssetLoadoutReceipt,
    ) -> Result<(), MemoryAssetError> {
        if input_event_seq <= 0 || receipt.assets.len() > 16 {
            return Err(DbError::Invariant("turn asset loadout receipt is invalid".into()).into());
        }
        let json = serde_json::to_string(receipt)
            .map_err(|error| DbError::Serde(format!("asset loadout receipt: {error}")))?;
        if json.len() > 32 * 1024 {
            return Err(
                DbError::Invariant("turn asset loadout receipt exceeds its bound".into()).into(),
            );
        }
        self.db.transaction(|tx| {
            tx.execute(
                "INSERT OR IGNORE INTO state_turn_asset_loadouts \
                 (conversation_id, input_event_seq, receipt_json, resolved_at) \
                 VALUES (?1, ?2, ?3, ?4)",
                params![conversation_id, input_event_seq, json, receipt.resolved_at],
            )?;
            let saved: String = tx.query_row(
                "SELECT receipt_json FROM state_turn_asset_loadouts \
                 WHERE conversation_id = ?1 AND input_event_seq = ?2",
                params![conversation_id, input_event_seq],
                |row| row.get(0),
            )?;
            let saved: TurnAssetLoadoutReceipt = serde_json::from_str(&saved)
                .map_err(|error| DbError::Serde(format!("stored asset loadout receipt: {error}")))?;
            let mut current = receipt.clone();
            // Resolution time is diagnostic metadata. Reopening an unchanged
            // run must keep its original timestamp and receipt bytes.
            current.resolved_at = saved.resolved_at;
            if saved != current {
                return Err(DbError::Invariant(format!(
                    "turn {conversation_id}:{input_event_seq} was reopened with a different memory loadout"
                )));
            }
            Ok(())
        })?;
        Ok(())
    }

    /// Read the selection receipt for a durable user event without reading asset content.
    pub fn turn_loadout(
        &self,
        conversation_id: &str,
        input_event_seq: i64,
    ) -> Result<Option<TurnAssetLoadoutReceipt>, MemoryAssetError> {
        self.db
            .with_conn(|connection| {
                connection
                    .query_row(
                        "SELECT receipt_json FROM state_turn_asset_loadouts \
                         WHERE conversation_id = ?1 AND input_event_seq = ?2",
                        params![conversation_id, input_event_seq],
                        |row| row.get::<_, String>(0),
                    )
                    .optional()
                    .map_err(DbError::from)
            })?
            .map(|json| {
                serde_json::from_str(&json)
                    .map_err(|error| DbError::Serde(format!("asset loadout receipt: {error}")))
            })
            .transpose()
            .map_err(Into::into)
    }

    pub fn touch(&self, asset_id: &str, now_unix: i64) -> Result<(), MemoryAssetError> {
        self.db.with_conn(|c| {
            c.execute(
                "UPDATE memory_assets SET usage_count = usage_count + 1,
                    last_used_at = ?2 WHERE asset_id = ?1",
                params![asset_id, now_unix],
            )?;
            Ok(())
        })?;
        Ok(())
    }

    /// Lexical BM25 retrieval with optional vector candidates merged by
    /// reciprocal-rank fusion. Authorization must be applied by the caller
    /// before passing the returned assets to a model.
    pub fn search(
        &self,
        query: &str,
        vector: Option<&[f32]>,
        model_id: Option<&str>,
        limit: u32,
    ) -> Result<Vec<AssetHit>, MemoryAssetError> {
        if vector.is_some_and(|vector| !valid_embedding_vector(vector)) {
            return Err(MemoryAssetError::InvalidEmbedding);
        }
        let lexical = self.db.with_conn(|c| {
            let mut stmt = c.prepare(
                "SELECT a.asset_id, bm25(memory_asset_search)
                 FROM memory_asset_search
                 JOIN memory_assets a ON a.asset_id = memory_asset_search.asset_id
                 WHERE memory_asset_search MATCH ?1 AND a.status <> 'archived'
                 ORDER BY bm25(memory_asset_search) LIMIT ?2",
            )?;
            Ok(stmt
                .query_map(params![sanitize_fts_query(query), limit as i64], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, f64>(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()?)
        })?;
        let mut vector_ids = Vec::new();
        if let (Some(vector), Some(model_id)) = (vector, model_id) {
            vector_ids = self.vector_candidates(vector, model_id, limit)?;
        }
        let mut ids = lexical.iter().map(|(id, _)| id.clone()).collect::<Vec<_>>();
        for (id, _) in &vector_ids {
            if !ids.iter().any(|existing| existing == id) {
                ids.push(id.clone());
            }
        }
        let mut hits = Vec::new();
        for id in ids {
            let Some(asset) = self.get(&id)? else {
                continue;
            };
            let lexical_rank = lexical
                .iter()
                .position(|(candidate, _)| candidate == &id)
                .map(|i| i as i64 + 1);
            let vector_rank = vector_ids
                .iter()
                .position(|(candidate, _)| candidate == &id)
                .map(|i| i as i64 + 1);
            let score = 1.0 / (60.0 + lexical_rank.unwrap_or(10_000) as f64)
                + vector_rank
                    .map(|rank| 1.0 / (60.0 + rank as f64))
                    .unwrap_or(0.0);
            hits.push(AssetHit {
                asset,
                score,
                lexical_rank: lexical_rank.unwrap_or(0),
                vector_rank,
            });
        }
        hits.sort_by(|a, b| b.score.total_cmp(&a.score));
        hits.truncate(limit as usize);
        Ok(hits)
    }

    /// Search an agent's eligible assets, applying scope, trust, lifecycle,
    /// visibility, and time filters before lexical/vector rank fusion.
    ///
    /// The search fails rather than silently truncating if more than 900 assets
    /// are eligible. The cap keeps the parameterized FTS/vector joins under
    /// SQLite's conservative variable limit while preserving candidate recall.
    pub fn search_eligible(
        &self,
        query: &str,
        vector: Option<&[f32]>,
        embedding_model_id: Option<&str>,
        agent_scope: &str,
        readable_trust_classes: &[&str],
        readable_owner_scopes: &[&str],
        eligible_injection_modes: &[InjectionMode],
        as_of_unix: i64,
        limit: u32,
    ) -> Result<Vec<AssetHit>, MemoryAssetError> {
        const MAX_ELIGIBLE_ASSETS: usize = 900;
        if vector.is_some_and(|vector| !valid_embedding_vector(vector)) {
            return Err(MemoryAssetError::InvalidEmbedding);
        }
        if agent_scope.trim().is_empty()
            || readable_trust_classes.is_empty()
            || readable_owner_scopes.is_empty()
            || eligible_injection_modes.is_empty()
        {
            return Ok(Vec::new());
        }
        let limit = limit.clamp(1, 200);
        let trust_placeholders = (0..readable_trust_classes.len())
            .map(|index| format!("?{}", index + 3))
            .collect::<Vec<_>>()
            .join(",");
        let owner_start = 3 + readable_trust_classes.len();
        let owner_placeholders = (0..readable_owner_scopes.len())
            .map(|index| format!("?{}", owner_start + index))
            .collect::<Vec<_>>()
            .join(",");
        let mode_start = owner_start + readable_owner_scopes.len();
        let mode_placeholders = (0..eligible_injection_modes.len())
            .map(|index| format!("?{}", mode_start + index))
            .collect::<Vec<_>>()
            .join(",");
        let eligibility_sql = format!(
            "SELECT DISTINCT a.asset_id FROM memory_assets a \
             JOIN memory_asset_bindings b ON b.asset_id = a.asset_id \
             WHERE b.agent_scope = ?1 AND a.created_at <= ?2 \
               AND b.injection_mode IN ({mode_placeholders}) \
               AND a.status = 'active' AND (a.expires_at IS NULL OR a.expires_at > ?2) \
               AND a.trust_floor IN ({trust_placeholders}) \
               AND a.owner_scope IN ({owner_placeholders}) \
               AND (a.visibility <> 'restricted' OR 'Controller' IN ({trust_placeholders})) \
             ORDER BY a.asset_id LIMIT ?{}",
            mode_start + eligible_injection_modes.len()
        );
        let eligible_ids = self.db.with_conn(|connection| {
            let mut values: Vec<Box<dyn rusqlite::ToSql>> =
                vec![Box::new(agent_scope.to_owned()), Box::new(as_of_unix)];
            for trust_class in readable_trust_classes {
                values.push(Box::new((*trust_class).to_owned()));
            }
            for owner_scope in readable_owner_scopes {
                values.push(Box::new((*owner_scope).to_owned()));
            }
            for injection_mode in eligible_injection_modes {
                values.push(Box::new(injection_mode.as_sql().to_owned()));
            }
            values.push(Box::new((MAX_ELIGIBLE_ASSETS + 1) as i64));
            let mut statement = connection.prepare(&eligibility_sql)?;
            statement
                .query_map(params_from_iter(values.iter()), |row| {
                    row.get::<_, String>(0)
                })?
                .collect::<Result<std::collections::HashSet<_>, _>>()
                .map_err(DbError::from)
        })?;
        if eligible_ids.len() > MAX_ELIGIBLE_ASSETS {
            return Err(MemoryAssetError::CandidateSetTooLarge {
                limit: MAX_ELIGIBLE_ASSETS,
            });
        }
        if eligible_ids.is_empty() {
            return Ok(Vec::new());
        }

        let eligible_placeholders = (0..eligible_ids.len())
            .map(|index| format!("?{}", index + 2))
            .collect::<Vec<_>>()
            .join(",");
        let lexical_sql = format!(
            "SELECT a.asset_id, bm25(memory_asset_search) \
             FROM memory_asset_search \
             JOIN memory_assets a ON a.asset_id = memory_asset_search.asset_id \
             WHERE memory_asset_search MATCH ?1 AND a.asset_id IN ({eligible_placeholders}) \
             ORDER BY bm25(memory_asset_search) LIMIT ?{}",
            eligible_ids.len() + 2
        );
        let mut lexical = self.db.with_conn(|connection| {
            let mut values: Vec<Box<dyn rusqlite::ToSql>> =
                vec![Box::new(sanitize_fts_query(query))];
            let mut ids = eligible_ids.iter().cloned().collect::<Vec<_>>();
            ids.sort();
            for id in ids {
                values.push(Box::new(id));
            }
            values.push(Box::new(limit as i64));
            let mut statement = connection.prepare(&lexical_sql)?;
            statement
                .query_map(params_from_iter(values.iter()), |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()
                .map_err(DbError::from)
        })?;
        let vector_ids = match (vector, embedding_model_id) {
            (Some(vector), Some(model_id)) => {
                self.vector_candidates_for_allowed(vector, model_id, &eligible_ids, limit)?
            }
            _ => Vec::new(),
        };
        let mut candidate_ids = std::collections::BTreeSet::new();
        candidate_ids.extend(lexical.iter().map(|(asset_id, _)| asset_id.clone()));
        candidate_ids.extend(vector_ids.iter().map(|(asset_id, _)| asset_id.clone()));
        let mut hits = Vec::with_capacity(candidate_ids.len());
        for asset_id in candidate_ids {
            let Some(asset) = self.get(&asset_id)? else {
                continue;
            };
            let lexical_rank = lexical
                .iter()
                .position(|(candidate, _)| candidate == &asset_id)
                .map(|index| index as i64 + 1);
            let vector_rank = vector_ids
                .iter()
                .position(|(candidate, _)| candidate == &asset_id)
                .map(|index| index as i64 + 1);
            let score = 1.0 / (60.0 + lexical_rank.unwrap_or(10_000) as f64)
                + vector_rank
                    .map(|rank| 1.0 / (60.0 + rank as f64))
                    .unwrap_or(0.0);
            hits.push(AssetHit {
                asset,
                score,
                lexical_rank: lexical_rank.unwrap_or(0),
                vector_rank,
            });
        }
        hits.sort_by(|left, right| right.score.total_cmp(&left.score));
        hits.truncate(limit as usize);
        lexical.clear();
        Ok(hits)
    }

    pub fn upsert_embedding(
        &self,
        asset_id: &str,
        model_id: &str,
        vector: &[f32],
        source_hash: &str,
        now_unix: i64,
    ) -> Result<(), MemoryAssetError> {
        let Some(asset) = self.get(asset_id)? else {
            return Err(MemoryAssetError::NotFound(asset_id.to_owned()));
        };
        if model_id.trim().is_empty()
            || model_id.len() > 256
            || source_hash.trim().is_empty()
            || source_hash.len() > 256
            || asset.source_hash.as_deref() != Some(source_hash)
            || !valid_embedding_vector(vector)
        {
            return Err(MemoryAssetError::InvalidEmbedding);
        }
        let json = serde_json::to_string(vector).map_err(|_| MemoryAssetError::InvalidEmbedding)?;
        self.db.with_conn(|c| {
            c.execute(
                "INSERT INTO memory_asset_embeddings
                    (asset_id, model_id, dimensions, vector_json, source_hash, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(asset_id, model_id) DO UPDATE SET
                    dimensions = excluded.dimensions, vector_json = excluded.vector_json,
                    source_hash = excluded.source_hash, created_at = excluded.created_at",
                params![
                    asset_id,
                    model_id,
                    vector.len() as i64,
                    json,
                    source_hash,
                    now_unix
                ],
            )?;
            Ok(())
        })?;
        Ok(())
    }

    pub fn upsert_wiki_page(&self, page: &WikiPage) -> Result<(), MemoryAssetError> {
        self.db.with_conn(|c| {
            c.execute(
                "INSERT INTO knowledge_wiki_pages
                    (wiki_id, page_ref, title, body, source_path, source_hash, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(wiki_id, page_ref) DO UPDATE SET
                    title = excluded.title, body = excluded.body,
                    source_path = excluded.source_path, source_hash = excluded.source_hash,
                    updated_at = excluded.updated_at",
                params![
                    page.wiki_id,
                    page.page_ref,
                    page.title,
                    page.body,
                    page.source_path,
                    page.source_hash,
                    page.updated_at
                ],
            )?;
            c.execute(
                "DELETE FROM knowledge_wiki_search WHERE wiki_id = ?1 AND page_ref = ?2",
                params![page.wiki_id, page.page_ref],
            )?;
            c.execute(
                "INSERT INTO knowledge_wiki_search(wiki_id, page_ref, title, body)
                 VALUES (?1, ?2, ?3, ?4)",
                params![page.wiki_id, page.page_ref, page.title, page.body],
            )?;
            Ok(())
        })?;
        Ok(())
    }

    pub fn search_wiki(
        &self,
        wiki_id: &str,
        query: &str,
        limit: u32,
    ) -> Result<Vec<WikiPage>, MemoryAssetError> {
        Ok(self.db.with_conn(|c| {
            let mut stmt = c.prepare(
                "SELECT p.wiki_id, p.page_ref, p.title, p.body, p.source_path, p.source_hash, p.updated_at
                 FROM knowledge_wiki_search s
                 JOIN knowledge_wiki_pages p ON p.wiki_id = s.wiki_id AND p.page_ref = s.page_ref
                 WHERE s.wiki_id = ?1 AND knowledge_wiki_search MATCH ?2
                 ORDER BY bm25(knowledge_wiki_search) LIMIT ?3",
            )?;
            Ok(stmt.query_map(params![wiki_id, sanitize_fts_query(query), limit as i64], |r| {
                Ok(WikiPage { wiki_id: r.get(0)?, page_ref: r.get(1)?, title: r.get(2)?, body: r.get(3)?, source_path: r.get(4)?, source_hash: r.get(5)?, updated_at: r.get(6)? })
            })?.collect::<Result<Vec<_>, _>>()?)
        })?)
    }

    pub fn upsert_code_node(&self, node: &CodeNode) -> Result<(), MemoryAssetError> {
        self.db.with_conn(|c| {
            c.execute(
                "INSERT INTO knowledge_code_nodes
                    (graph_id, symbol, kind, file_path, start_line, end_line, source)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(graph_id, symbol, file_path, start_line) DO UPDATE SET
                    kind = excluded.kind, end_line = excluded.end_line, source = excluded.source",
                params![
                    node.graph_id,
                    node.symbol,
                    node.kind,
                    node.file_path,
                    node.start_line,
                    node.end_line,
                    node.source
                ],
            )?;
            Ok(())
        })?;
        Ok(())
    }

    pub fn add_code_edge(
        &self,
        graph_id: &str,
        caller: &str,
        callee: &str,
        kind: &str,
    ) -> Result<(), MemoryAssetError> {
        self.db.with_conn(|c| {
            c.execute(
                "INSERT OR IGNORE INTO knowledge_code_edges(graph_id, caller, callee, kind)
                 VALUES (?1, ?2, ?3, ?4)",
                params![graph_id, caller, callee, kind],
            )?;
            Ok(())
        })?;
        Ok(())
    }

    pub fn search_code(
        &self,
        graph_id: &str,
        symbol: &str,
        limit: u32,
    ) -> Result<Vec<CodeNode>, MemoryAssetError> {
        Ok(self.db.with_conn(|c| {
            let mut stmt = c.prepare(
                "SELECT graph_id, symbol, kind, file_path, start_line, end_line, source
                 FROM knowledge_code_nodes WHERE graph_id = ?1 AND symbol LIKE ?2
                 ORDER BY symbol LIMIT ?3",
            )?;
            Ok(stmt
                .query_map(
                    params![
                        graph_id,
                        format!("%{}%", symbol.replace('%', "")),
                        limit as i64
                    ],
                    |r| {
                        Ok(CodeNode {
                            graph_id: r.get(0)?,
                            symbol: r.get(1)?,
                            kind: r.get(2)?,
                            file_path: r.get(3)?,
                            start_line: r.get(4)?,
                            end_line: r.get(5)?,
                            source: r.get(6)?,
                        })
                    },
                )?
                .collect::<Result<Vec<_>, _>>()?)
        })?)
    }

    pub fn callers(
        &self,
        graph_id: &str,
        symbol: &str,
        limit: u32,
    ) -> Result<Vec<String>, MemoryAssetError> {
        self.edge_names(graph_id, symbol, "callee", limit)
    }

    pub fn callees(
        &self,
        graph_id: &str,
        symbol: &str,
        limit: u32,
    ) -> Result<Vec<String>, MemoryAssetError> {
        self.edge_names(graph_id, symbol, "caller", limit)
    }

    /// Return a bounded breadth-first impact set following callers and
    /// callees. The graph is derived and revision-scoped; callers should
    /// reject stale graphs before exposing this result to a model.
    pub fn impact(
        &self,
        graph_id: &str,
        symbol: &str,
        depth: u32,
        limit: u32,
    ) -> Result<Vec<String>, MemoryAssetError> {
        let mut frontier = vec![symbol.to_owned()];
        let mut seen = std::collections::BTreeSet::new();
        for _ in 0..depth.max(1) {
            let mut next = Vec::new();
            for current in frontier {
                if !seen.insert(current.clone()) {
                    continue;
                }
                next.extend(self.callers(graph_id, &current, limit)?);
                next.extend(self.callees(graph_id, &current, limit)?);
                if seen.len() >= limit as usize {
                    break;
                }
            }
            frontier = next;
            if frontier.is_empty() || seen.len() >= limit as usize {
                break;
            }
        }
        seen.remove(symbol);
        Ok(seen.into_iter().take(limit as usize).collect())
    }

    fn edge_names(
        &self,
        graph_id: &str,
        symbol: &str,
        side: &str,
        limit: u32,
    ) -> Result<Vec<String>, MemoryAssetError> {
        let (returned_column, filtered_column) = match side {
            "callee" => ("caller", "callee"),
            "caller" => ("callee", "caller"),
            _ => return Ok(Vec::new()),
        };
        Ok(self.db.with_conn(|c| {
            let sql = format!("SELECT {returned_column} FROM knowledge_code_edges WHERE graph_id = ?1 AND {filtered_column} = ?2 ORDER BY {returned_column} LIMIT ?3");
            let mut stmt = c.prepare(&sql)?;
            Ok(stmt.query_map(params![graph_id, symbol, limit as i64], |r| r.get(0))?.collect::<Result<Vec<_>, _>>()?)
        })?)
    }

    fn vector_candidates(
        &self,
        query: &[f32],
        model_id: &str,
        limit: u32,
    ) -> Result<Vec<(String, f32)>, MemoryAssetError> {
        let rows = self.db.with_conn(|c| {
            let mut stmt = c.prepare(
                "SELECT e.asset_id, e.dimensions, e.vector_json
                 FROM memory_asset_embeddings e
                 JOIN memory_assets a ON a.asset_id = e.asset_id
                 WHERE e.model_id = ?1 AND e.source_hash = a.source_hash
                   AND a.status = 'active'",
            )?;
            Ok(stmt
                .query_map(params![model_id], |r| {
                    let id: String = r.get(0)?;
                    let dimensions: usize = r.get::<_, i64>(1)? as usize;
                    let json: String = r.get(2)?;
                    Ok((id, dimensions, json))
                })?
                .collect::<Result<Vec<_>, _>>()?)
        })?;
        let mut scored = Vec::new();
        for (id, dimensions, json) in rows {
            if dimensions != query.len() {
                continue;
            }
            let Ok(values) = serde_json::from_str::<Vec<f32>>(&json) else {
                continue;
            };
            let score = cosine_similarity(query, &values);
            scored.push((id, score));
        }
        scored.sort_by(|a, b| b.1.total_cmp(&a.1));
        scored.truncate(limit as usize);
        Ok(scored)
    }

    fn vector_candidates_for_allowed(
        &self,
        query: &[f32],
        model_id: &str,
        eligible_ids: &std::collections::HashSet<String>,
        limit: u32,
    ) -> Result<Vec<(String, f32)>, MemoryAssetError> {
        if eligible_ids.is_empty() {
            return Ok(Vec::new());
        }
        let placeholders = (0..eligible_ids.len())
            .map(|index| format!("?{}", index + 2))
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT e.asset_id, e.dimensions, e.vector_json FROM memory_asset_embeddings e \
             JOIN memory_assets a ON a.asset_id = e.asset_id \
             WHERE e.model_id = ?1 AND e.asset_id IN ({placeholders}) \
               AND e.source_hash = a.source_hash AND a.status = 'active'"
        );
        let rows = self.db.with_conn(|connection| {
            let mut values: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(model_id.to_owned())];
            let mut ids = eligible_ids.iter().cloned().collect::<Vec<_>>();
            ids.sort();
            for id in ids {
                values.push(Box::new(id));
            }
            let mut statement = connection.prepare(&sql)?;
            statement
                .query_map(params_from_iter(values.iter()), |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)? as usize,
                        row.get::<_, String>(2)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()
                .map_err(DbError::from)
        })?;
        let mut scored = Vec::new();
        for (asset_id, dimensions, vector_json) in rows {
            if dimensions != query.len() {
                continue;
            }
            let Ok(candidate) = serde_json::from_str::<Vec<f32>>(&vector_json) else {
                continue;
            };
            scored.push((asset_id, cosine_similarity(query, &candidate)));
        }
        scored.sort_by(|left, right| right.1.total_cmp(&left.1));
        scored.truncate(limit as usize);
        Ok(scored)
    }
}

fn sanitize_fts_query(query: &str) -> String {
    query
        .split_whitespace()
        .filter_map(|word| {
            let clean: String = word.chars().filter(|ch| ch.is_alphanumeric()).collect();
            (!clean.is_empty()).then_some(clean)
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn cosine_similarity(left: &[f32], right: &[f32]) -> f32 {
    let (mut dot, mut left_norm, mut right_norm) = (0.0, 0.0, 0.0);
    for (a, b) in left.iter().zip(right) {
        dot += a * b;
        left_norm += a * a;
        right_norm += b * b;
    }
    if left_norm == 0.0 || right_norm == 0.0 {
        0.0
    } else {
        dot / (left_norm.sqrt() * right_norm.sqrt())
    }
}

fn valid_embedding_vector(vector: &[f32]) -> bool {
    !vector.is_empty()
        && vector.len() <= 8192
        && vector.iter().all(|value| value.is_finite())
        && vector.iter().any(|value| *value != 0.0)
}

fn row_to_asset(r: &rusqlite::Row<'_>) -> rusqlite::Result<MemoryAsset> {
    Ok(MemoryAsset {
        asset_id: r.get(0)?,
        asset_type: AssetType::parse(&r.get::<_, String>(1)?)
            .ok_or_else(|| rusqlite::Error::InvalidQuery)?,
        name: r.get(2)?,
        description: r.get(3)?,
        owner_scope: r.get(4)?,
        visibility: match r.get::<_, String>(5)?.as_str() {
            "private" => AssetVisibility::Private,
            "team" => AssetVisibility::Team,
            "restricted" => AssetVisibility::Restricted,
            "agent" => AssetVisibility::Agent,
            _ => return Err(rusqlite::Error::InvalidQuery),
        },
        trust_floor: r.get(6)?,
        status: r.get(7)?,
        version: r.get(8)?,
        source_ref: r.get(9)?,
        content_ref: r.get(10)?,
        source_hash: r.get(11)?,
        expires_at: r.get(12)?,
        last_used_at: r.get(13)?,
        usage_count: r.get(14)?,
        created_at: r.get(15)?,
        updated_at: r.get(16)?,
    })
}

fn row_to_binding(r: &rusqlite::Row<'_>) -> rusqlite::Result<AssetBinding> {
    Ok(AssetBinding {
        asset_id: r.get(0)?,
        agent_scope: r.get(1)?,
        injection_mode: match r.get::<_, String>(2)?.as_str() {
            "hot" => InjectionMode::Hot,
            "discoverable" => InjectionMode::Discoverable,
            "tool_only" => InjectionMode::ToolOnly,
            _ => return Err(rusqlite::Error::InvalidQuery),
        },
        priority: r.get(3)?,
        max_chars: r.get(4)?,
        created_at: r.get(5)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        db::{Database, DbConfig},
        migrations::MigrationRunner,
    };

    fn db() -> Database {
        let db = Database::open(&DbConfig::in_memory_unencrypted()).unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        db
    }

    #[test]
    fn loadout_and_hybrid_search_are_bounded_and_versioned() {
        let db = db();
        let store = MemoryAssetStore::new(&db);
        store
            .create(NewMemoryAsset {
                asset_id: "skill-1",
                asset_type: AssetType::Skill,
                name: "Release checklist",
                description: "release validation",
                owner_scope: "controller",
                visibility: AssetVisibility::Private,
                trust_floor: "Controller",
                source_ref: Some("release.md"),
                content_ref: None,
                source_hash: Some("a"),
                now_unix: 1,
            })
            .unwrap();
        store
            .bind(
                "skill-1",
                "builder",
                InjectionMode::Discoverable,
                10,
                4000,
                1,
            )
            .unwrap();
        store
            .upsert_embedding("skill-1", "local-test", &[1.0, 0.0], "a", 1)
            .unwrap();
        assert_eq!(store.list_loadout("builder", 10).unwrap().len(), 1);
        let hits = store
            .search("release", Some(&[1.0, 0.0]), Some("local-test"), 5)
            .unwrap();
        assert_eq!(hits[0].asset.asset_id, "skill-1");
        assert!(hits[0].score > 0.0);
    }

    #[test]
    fn retrieval_config_and_embedding_rebuild_candidates_track_source_versions() {
        let db = db();
        let store = MemoryAssetStore::new(&db);
        store
            .create(NewMemoryAsset {
                asset_id: "embed-asset",
                asset_type: AssetType::Memory,
                name: "Local embedding test",
                description: "versioned source",
                owner_scope: "global",
                visibility: AssetVisibility::Private,
                trust_floor: "Controller",
                source_ref: Some("fixture"),
                content_ref: Some("source text"),
                source_hash: Some("source-v1"),
                now_unix: 1,
            })
            .unwrap();
        store
            .set_retrieval_config("embedding-model-v1", MEMORY_RERANKER_VERSION, 2)
            .unwrap();
        assert_eq!(
            store.retrieval_config().unwrap(),
            Some(MemoryRetrievalConfig {
                embedding_model_id: "embedding-model-v1".into(),
                reranker_version: MEMORY_RERANKER_VERSION.into(),
                updated_at: 2,
            })
        );
        let candidates = store
            .embedding_rebuild_candidates("embedding-model-v1", 10)
            .unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].source_hash, "source-v1");
        assert!(candidates[0].input_text.contains("source text"));

        store
            .upsert_embedding(
                "embed-asset",
                "embedding-model-v1",
                &[1.0, 0.0],
                "source-v1",
                3,
            )
            .unwrap();
        assert!(
            store
                .embedding_rebuild_candidates("embedding-model-v1", 10)
                .unwrap()
                .is_empty()
        );
        db.with_conn(|connection| {
            connection.execute(
                "UPDATE memory_assets SET source_hash = 'source-v2', content_ref = 'updated source' WHERE asset_id = 'embed-asset'",
                [],
            )?;
            Ok(())
        })
        .unwrap();
        let candidates = store
            .embedding_rebuild_candidates("embedding-model-v1", 10)
            .unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].source_hash, "source-v2");
    }

    #[test]
    fn eligible_hybrid_search_filters_trust_before_vector_ranking() {
        let db = db();
        let store = MemoryAssetStore::new(&db);
        for (asset_id, trust_floor, hash) in [
            ("eligible", "KnownTrusted", "eligible-source"),
            ("controller-only", "Controller", "restricted-source"),
        ] {
            store
                .create(NewMemoryAsset {
                    asset_id,
                    asset_type: AssetType::Memory,
                    name: asset_id,
                    description: "synthetic retrieval fixture",
                    owner_scope: "global",
                    visibility: AssetVisibility::Private,
                    trust_floor,
                    source_ref: Some("fixture"),
                    content_ref: Some("synthetic content"),
                    source_hash: Some(hash),
                    now_unix: 1,
                })
                .unwrap();
            store
                .bind(asset_id, "default", InjectionMode::Discoverable, 1, 512, 1)
                .unwrap();
        }
        // The forbidden candidate is a perfect cosine match. Eligibility must
        // remove it before vector rank assignment and reciprocal-rank fusion.
        store
            .upsert_embedding(
                "eligible",
                "fixture-index-v1",
                &[0.0, 1.0],
                "eligible-source",
                2,
            )
            .unwrap();
        store
            .upsert_embedding(
                "controller-only",
                "fixture-index-v1",
                &[1.0, 0.0],
                "restricted-source",
                2,
            )
            .unwrap();
        let hits = store
            .search_eligible(
                "no lexical match",
                Some(&[1.0, 0.0]),
                Some("fixture-index-v1"),
                "default",
                &["KnownTrusted", "KnownLimited"],
                &["global"],
                &[InjectionMode::Discoverable],
                3,
                10,
            )
            .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].asset.asset_id, "eligible");
        assert_eq!(hits[0].vector_rank, Some(1));
    }

    #[test]
    fn hot_loadout_filters_trust_and_expiry_before_budgeting() {
        let db = db();
        let store = MemoryAssetStore::new(&db);
        for (id, floor, expiry, priority, owner_scope, visibility) in [
            (
                "controller-only",
                "Controller",
                None,
                100,
                "principal:test",
                AssetVisibility::Private,
            ),
            (
                "readable",
                "KnownLimited",
                None,
                90,
                "principal:test",
                AssetVisibility::Private,
            ),
            (
                "expired",
                "KnownLimited",
                Some(50),
                80,
                "principal:test",
                AssetVisibility::Private,
            ),
            (
                "other-owner",
                "KnownLimited",
                None,
                70,
                "principal:other",
                AssetVisibility::Private,
            ),
            (
                "restricted",
                "KnownLimited",
                None,
                60,
                "principal:test",
                AssetVisibility::Restricted,
            ),
        ] {
            store
                .create(NewMemoryAsset {
                    asset_id: id,
                    asset_type: AssetType::Memory,
                    name: id,
                    description: "fixture",
                    owner_scope,
                    visibility,
                    trust_floor: floor,
                    source_ref: None,
                    content_ref: Some("safe contents"),
                    source_hash: Some("fixture-hash"),
                    now_unix: 1,
                })
                .unwrap();
            store
                .bind(id, "default", InjectionMode::Hot, priority, 12, 1)
                .unwrap();
            if let Some(expiry) = expiry {
                db.with_conn(|connection| {
                    connection.execute(
                        "UPDATE memory_assets SET expires_at = ?1 WHERE asset_id = ?2",
                        params![expiry, id],
                    )?;
                    Ok(())
                })
                .unwrap();
            }
        }

        let assets = store
            .resolve_hot_loadout(
                "default",
                &["KnownLimited", "UnknownPending"],
                &["global", "principal:test"],
                100,
                12,
                10,
            )
            .unwrap();
        assert_eq!(assets.len(), 1);
        assert_eq!(assets[0].asset.asset_id, "readable");
        assert_eq!(assets[0].max_chars, 12);
        assert_eq!(assets[0].binding.agent_scope, "default");
        assert_eq!(assets[0].binding.priority, 90);
        assert_eq!(assets[0].binding.injection_mode, InjectionMode::Hot);
    }

    #[test]
    fn turn_loadout_receipt_is_immutable_and_metadata_only() {
        let db = db();
        db.with_conn(|connection| {
            connection.execute(
                "INSERT INTO state_conversations \
                 (conversation_id, kind, phase, trust_class, modality) \
                 VALUES ('loadout-conversation', 'ControllerDM', 'idle', 'Controller', 'Text')",
                [],
            )?;
            Ok(())
        })
        .unwrap();
        let receipt = TurnAssetLoadoutReceipt {
            agent_scope: "default".into(),
            conversation_trust_class: "Controller".into(),
            readable_trust_classes: vec!["Controller".into()],
            readable_owner_scopes: vec!["global".into(), "controller".into()],
            resolved_at: 100,
            retrieval_query_sha256: None,
            assets: vec![TurnAssetLoadoutEntry {
                asset_id: "asset-1".into(),
                name: "release notes".into(),
                asset_type: AssetType::Memory,
                version: 3,
                source_hash: Some("sha256:source".into()),
                owner_scope: "controller".into(),
                visibility: AssetVisibility::Private,
                trust_floor: "Controller".into(),
                status: "active".into(),
                expires_at: None,
                binding_agent_scope: "default".into(),
                binding_mode: InjectionMode::Hot,
                binding_priority: 80,
                binding_max_chars: 240,
                injected_chars: 129,
                admission_reasons: vec![
                    "active".into(),
                    "trust_readable".into(),
                    "owner_scope_readable".into(),
                    "visibility_allowed".into(),
                    "within_loadout_budget".into(),
                ],
            }],
            retrieved_assets: Vec::new(),
        };
        let store = MemoryAssetStore::new(&db);
        store
            .record_turn_loadout("loadout-conversation", 7, &receipt)
            .unwrap();
        assert_eq!(
            store.turn_loadout("loadout-conversation", 7).unwrap(),
            Some(receipt.clone())
        );
        let mut repeated_resolution = receipt.clone();
        repeated_resolution.resolved_at += 60;
        store
            .record_turn_loadout("loadout-conversation", 7, &repeated_resolution)
            .unwrap();
        assert_eq!(
            store.turn_loadout("loadout-conversation", 7).unwrap(),
            Some(receipt.clone()),
            "retry preserves the original resolution timestamp"
        );
        let mut changed = receipt;
        changed.assets[0].source_hash = Some("sha256:changed".into());
        assert!(
            store
                .record_turn_loadout("loadout-conversation", 7, &changed)
                .is_err()
        );
    }

    #[test]
    fn wiki_and_code_graph_queries_are_revision_scoped() {
        let db = db();
        let store = MemoryAssetStore::new(&db);
        store
            .create(NewMemoryAsset {
                asset_id: "wiki-asset",
                asset_type: AssetType::Wiki,
                name: "Wiki fixture",
                description: "",
                owner_scope: "global",
                visibility: AssetVisibility::Private,
                trust_floor: "Controller",
                source_ref: None,
                content_ref: None,
                source_hash: Some("wiki-hash"),
                now_unix: 1,
            })
            .unwrap();
        store
            .create(NewMemoryAsset {
                asset_id: "graph-asset",
                asset_type: AssetType::CodeGraph,
                name: "Graph fixture",
                description: "",
                owner_scope: "global",
                visibility: AssetVisibility::Private,
                trust_floor: "Controller",
                source_ref: None,
                content_ref: None,
                source_hash: Some("graph-hash"),
                now_unix: 1,
            })
            .unwrap();
        db.with_conn(|connection| {
            connection.execute(
                "INSERT INTO knowledge_wikis(wiki_id,asset_id,root_path,revision,created_at,updated_at) \
                 VALUES ('wiki-1','wiki-asset','docs','rev-1',1,1)",
                [],
            )?;
            connection.execute(
                "INSERT INTO knowledge_code_graphs(graph_id,asset_id,root_path,revision,created_at,updated_at) \
                 VALUES ('graph-1','graph-asset','src','rev-1',1,1)",
                [],
            )?;
            Ok(())
        })
        .unwrap();
        store
            .upsert_wiki_page(&WikiPage {
                wiki_id: "wiki-1".into(),
                page_ref: "release".into(),
                title: "Release plan".into(),
                body: "Ship after review".into(),
                source_path: "docs/release.md".into(),
                source_hash: "h1".into(),
                updated_at: 1,
            })
            .unwrap();
        assert_eq!(store.search_wiki("wiki-1", "release", 5).unwrap().len(), 1);
        store
            .upsert_code_node(&CodeNode {
                graph_id: "graph-1".into(),
                symbol: "build_prompt".into(),
                kind: "function".into(),
                file_path: "src/prompt.rs".into(),
                start_line: 1,
                end_line: 4,
                source: None,
            })
            .unwrap();
        store
            .upsert_code_node(&CodeNode {
                graph_id: "graph-1".into(),
                symbol: "run_turn".into(),
                kind: "function".into(),
                file_path: "src/turn.rs".into(),
                start_line: 5,
                end_line: 8,
                source: None,
            })
            .unwrap();
        store
            .add_code_edge("graph-1", "run_turn", "build_prompt", "calls")
            .unwrap();
        assert_eq!(
            store.callers("graph-1", "build_prompt", 5).unwrap(),
            vec!["run_turn"]
        );
        assert_eq!(
            store.search_code("other-graph", "build", 5).unwrap().len(),
            0
        );
    }
}
