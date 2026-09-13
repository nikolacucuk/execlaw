//! Governed memory assets, agent loadouts, and local knowledge indexes.
//!
//! The asset row is metadata only. Existing event-sourced memory, skills, and
//! derived knowledge stores remain authoritative for their content. This
//! module supplies the common Tencent-inspired registry and bounded retrieval
//! surface without introducing a second service or trust model.

use crate::db::{Database, DbError};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use thiserror::Error;

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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetHit {
    pub asset: MemoryAsset,
    pub score: f64,
    pub lexical_rank: i64,
    pub vector_rank: Option<i64>,
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
}

pub struct MemoryAssetStore<'db> {
    db: &'db Database,
}

impl<'db> MemoryAssetStore<'db> {
    pub fn new(db: &'db Database) -> Self {
        Self { db }
    }

    pub fn create(&self, asset: NewMemoryAsset<'_>) -> Result<(), MemoryAssetError> {
        self.db.with_conn(|c| {
            c.execute(
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
                params![asset_id, agent_scope, injection_mode.as_sql(), priority, max_chars, now_unix],
            )?;
            Ok(())
        })?;
        Ok(())
    }

    pub fn list_loadout(&self, agent_scope: &str, limit: u32) -> Result<Vec<AssetBinding>, MemoryAssetError> {
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
            let Some(asset) = self.get(&id)? else { continue };
            let lexical_rank = lexical.iter().position(|(candidate, _)| candidate == &id).map(|i| i as i64 + 1);
            let vector_rank = vector_ids.iter().position(|(candidate, _)| candidate == &id).map(|i| i as i64 + 1);
            let score = 1.0 / (60.0 + lexical_rank.unwrap_or(10_000) as f64)
                + vector_rank.map(|rank| 1.0 / (60.0 + rank as f64)).unwrap_or(0.0);
            hits.push(AssetHit { asset, score, lexical_rank: lexical_rank.unwrap_or(0), vector_rank });
        }
        hits.sort_by(|a, b| b.score.total_cmp(&a.score));
        hits.truncate(limit as usize);
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
        if self.get(asset_id)?.is_none() {
            return Err(MemoryAssetError::NotFound(asset_id.to_owned()));
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
                params![asset_id, model_id, vector.len() as i64, json, source_hash, now_unix],
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
                params![page.wiki_id, page.page_ref, page.title, page.body, page.source_path, page.source_hash, page.updated_at],
            )?;
            c.execute("DELETE FROM knowledge_wiki_search WHERE wiki_id = ?1 AND page_ref = ?2", params![page.wiki_id, page.page_ref])?;
            c.execute(
                "INSERT INTO knowledge_wiki_search(wiki_id, page_ref, title, body)
                 VALUES (?1, ?2, ?3, ?4)",
                params![page.wiki_id, page.page_ref, page.title, page.body],
            )?;
            Ok(())
        })?;
        Ok(())
    }

    pub fn search_wiki(&self, wiki_id: &str, query: &str, limit: u32) -> Result<Vec<WikiPage>, MemoryAssetError> {
        Ok(self.db.with_conn(|c| {
            let mut stmt = c.prepare(
                "SELECT p.wiki_id, p.page_ref, p.title, p.body, p.source_path, p.source_hash, p.updated_at
                 FROM knowledge_wiki_search s
                 JOIN knowledge_wiki_pages p ON p.wiki_id = s.wiki_id AND p.page_ref = s.page_ref
                 WHERE s.wiki_id = ?1 AND s MATCH ?2
                 ORDER BY bm25(s) LIMIT ?3",
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
                params![node.graph_id, node.symbol, node.kind, node.file_path, node.start_line, node.end_line, node.source],
            )?;
            Ok(())
        })?;
        Ok(())
    }

    pub fn add_code_edge(&self, graph_id: &str, caller: &str, callee: &str, kind: &str) -> Result<(), MemoryAssetError> {
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

    pub fn search_code(&self, graph_id: &str, symbol: &str, limit: u32) -> Result<Vec<CodeNode>, MemoryAssetError> {
        Ok(self.db.with_conn(|c| {
            let mut stmt = c.prepare(
                "SELECT graph_id, symbol, kind, file_path, start_line, end_line, source
                 FROM knowledge_code_nodes WHERE graph_id = ?1 AND symbol LIKE ?2
                 ORDER BY symbol LIMIT ?3",
            )?;
            Ok(stmt.query_map(params![graph_id, format!("%{}%", symbol.replace('%', "")), limit as i64], |r| {
                Ok(CodeNode { graph_id: r.get(0)?, symbol: r.get(1)?, kind: r.get(2)?, file_path: r.get(3)?, start_line: r.get(4)?, end_line: r.get(5)?, source: r.get(6)? })
            })?.collect::<Result<Vec<_>, _>>()?)
        })?)
    }

    pub fn callers(&self, graph_id: &str, symbol: &str, limit: u32) -> Result<Vec<String>, MemoryAssetError> {
        self.edge_names(graph_id, symbol, "callee", limit)
    }

    pub fn callees(&self, graph_id: &str, symbol: &str, limit: u32) -> Result<Vec<String>, MemoryAssetError> {
        self.edge_names(graph_id, symbol, "caller", limit)
    }

    /// Return a bounded breadth-first impact set following callers and
    /// callees. The graph is derived and revision-scoped; callers should
    /// reject stale graphs before exposing this result to a model.
    pub fn impact(&self, graph_id: &str, symbol: &str, depth: u32, limit: u32) -> Result<Vec<String>, MemoryAssetError> {
        let mut frontier = vec![symbol.to_owned()];
        let mut seen = std::collections::BTreeSet::new();
        for _ in 0..depth.max(1) {
            let mut next = Vec::new();
            for current in frontier {
                if !seen.insert(current.clone()) { continue; }
                next.extend(self.callers(graph_id, &current, limit)?);
                next.extend(self.callees(graph_id, &current, limit)?);
                if seen.len() >= limit as usize { break; }
            }
            frontier = next;
            if frontier.is_empty() || seen.len() >= limit as usize { break; }
        }
        seen.remove(symbol);
        Ok(seen.into_iter().take(limit as usize).collect())
    }

    fn edge_names(&self, graph_id: &str, symbol: &str, side: &str, limit: u32) -> Result<Vec<String>, MemoryAssetError> {
        let column = match side { "callee" => "callee", "caller" => "caller", _ => return Ok(Vec::new()) };
        Ok(self.db.with_conn(|c| {
            let sql = format!("SELECT {} FROM knowledge_code_edges WHERE graph_id = ?1 AND {} = ?2 ORDER BY {} LIMIT ?3", column, side, column);
            let mut stmt = c.prepare(&sql)?;
            Ok(stmt.query_map(params![graph_id, symbol, limit as i64], |r| r.get(0))?.collect::<Result<Vec<_>, _>>()?)
        })?)
    }

    fn vector_candidates(&self, query: &[f32], model_id: &str, limit: u32) -> Result<Vec<(String, f32)>, MemoryAssetError> {
        let rows = self.db.with_conn(|c| {
            let mut stmt = c.prepare(
                "SELECT asset_id, dimensions, vector_json FROM memory_asset_embeddings
                 WHERE model_id = ?1",
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
            if dimensions != query.len() { continue; }
            let Ok(values) = serde_json::from_str::<Vec<f32>>(&json) else { continue; };
            let score = cosine_similarity(query, &values);
            scored.push((id, score));
        }
        scored.sort_by(|a, b| b.1.total_cmp(&a.1));
        scored.truncate(limit as usize);
        Ok(scored)
    }
}

fn sanitize_fts_query(query: &str) -> String {
    query.split_whitespace().filter_map(|word| {
        let clean: String = word.chars().filter(|ch| ch.is_alphanumeric()).collect();
        (!clean.is_empty()).then_some(clean)
    }).collect::<Vec<_>>().join(" ")
}

fn cosine_similarity(left: &[f32], right: &[f32]) -> f32 {
    let (mut dot, mut left_norm, mut right_norm) = (0.0, 0.0, 0.0);
    for (a, b) in left.iter().zip(right) { dot += a * b; left_norm += a * a; right_norm += b * b; }
    if left_norm == 0.0 || right_norm == 0.0 { 0.0 } else { dot / (left_norm.sqrt() * right_norm.sqrt()) }
}

fn row_to_asset(r: &rusqlite::Row<'_>) -> rusqlite::Result<MemoryAsset> {
    Ok(MemoryAsset {
        asset_id: r.get(0)?,
        asset_type: AssetType::parse(&r.get::<_, String>(1)?).ok_or_else(|| rusqlite::Error::InvalidQuery)?,
        name: r.get(2)?, description: r.get(3)?, owner_scope: r.get(4)?,
        visibility: match r.get::<_, String>(5)?.as_str() { "private" => AssetVisibility::Private, "team" => AssetVisibility::Team, "restricted" => AssetVisibility::Restricted, "agent" => AssetVisibility::Agent, _ => return Err(rusqlite::Error::InvalidQuery) },
        trust_floor: r.get(6)?, status: r.get(7)?, version: r.get(8)?, source_ref: r.get(9)?, content_ref: r.get(10)?, source_hash: r.get(11)?, expires_at: r.get(12)?, last_used_at: r.get(13)?, usage_count: r.get(14)?, created_at: r.get(15)?, updated_at: r.get(16)?,
    })
}

fn row_to_binding(r: &rusqlite::Row<'_>) -> rusqlite::Result<AssetBinding> {
    Ok(AssetBinding { asset_id: r.get(0)?, agent_scope: r.get(1)?, injection_mode: match r.get::<_, String>(2)?.as_str() { "hot" => InjectionMode::Hot, "discoverable" => InjectionMode::Discoverable, "tool_only" => InjectionMode::ToolOnly, _ => return Err(rusqlite::Error::InvalidQuery) }, priority: r.get(3)?, max_chars: r.get(4)?, created_at: r.get(5)? })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{db::{Database, DbConfig}, migrations::MigrationRunner};

    fn db() -> Database {
        let db = Database::open(&DbConfig::in_memory_unencrypted()).unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        db
    }

    #[test]
    fn loadout_and_hybrid_search_are_bounded_and_versioned() {
        let db = db();
        let store = MemoryAssetStore::new(&db);
        store.create(NewMemoryAsset { asset_id: "skill-1", asset_type: AssetType::Skill, name: "Release checklist", description: "release validation", owner_scope: "controller", visibility: AssetVisibility::Private, trust_floor: "Controller", source_ref: Some("release.md"), content_ref: None, source_hash: Some("a"), now_unix: 1 }).unwrap();
        store.bind("skill-1", "builder", InjectionMode::Discoverable, 10, 4000, 1).unwrap();
        store.upsert_embedding("skill-1", "local-test", &[1.0, 0.0], "a", 1).unwrap();
        assert_eq!(store.list_loadout("builder", 10).unwrap().len(), 1);
        let hits = store.search("release", Some(&[1.0, 0.0]), Some("local-test"), 5).unwrap();
        assert_eq!(hits[0].asset.asset_id, "skill-1");
        assert!(hits[0].score > 0.0);
    }

    #[test]
    fn wiki_and_code_graph_queries_are_revision_scoped() {
        let db = db();
        let store = MemoryAssetStore::new(&db);
        store.upsert_wiki_page(&WikiPage { wiki_id: "wiki-1".into(), page_ref: "release".into(), title: "Release plan".into(), body: "Ship after review".into(), source_path: "docs/release.md".into(), source_hash: "h1".into(), updated_at: 1 }).unwrap();
        assert_eq!(store.search_wiki("wiki-1", "release", 5).unwrap().len(), 1);
        store.upsert_code_node(&CodeNode { graph_id: "graph-1".into(), symbol: "build_prompt".into(), kind: "function".into(), file_path: "src/prompt.rs".into(), start_line: 1, end_line: 4, source: None }).unwrap();
        store.upsert_code_node(&CodeNode { graph_id: "graph-1".into(), symbol: "run_turn".into(), kind: "function".into(), file_path: "src/turn.rs".into(), start_line: 5, end_line: 8, source: None }).unwrap();
        store.add_code_edge("graph-1", "run_turn", "build_prompt", "calls").unwrap();
        assert_eq!(store.callers("graph-1", "build_prompt", 5).unwrap(), vec!["run_turn"]);
        assert_eq!(store.search_code("other-graph", "build", 5).unwrap().len(), 0);
    }
}
