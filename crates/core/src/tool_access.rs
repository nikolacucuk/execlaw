//! Per-tool access policy (Phase 8a).
//!
//! One row in `config_tool_access` per tool the runner might dispatch
//! to, regardless of source. Read on every tool call (the runner's
//! capability-check hot path) and written by:
//!   * Boot-time sync of built-in tools.
//!   * Plugin install / enable cycles.
//!   * MCP server tool-list reflection (Phase 8c).
//!   * Settings → Tools page mutations.
//!
//! `allowed_classes` is intentionally a JSON-array-of-strings rather
//! than a typed enum — `execlaw-core` doesn't depend on `execlaw-policy`,
//! so the trust-class vocabulary lives there. The server crate parses
//! the strings into `TrustLevel` values at dispatch time.
//!
//! Default-deny is the policy: a tool with `allowed_classes = []`
//! cannot be dispatched by anyone, even the controller. Operators
//! who intentionally want a tool unreachable can either flip
//! `enabled = false` or empty the list.

use crate::db::{Database, DbError};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

/// What kind of registration produced this tool. The Settings UI
/// uses this to badge rows; the access check itself doesn't care.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub enum ToolSource {
    Builtin,
    Plugin,
    Mcp,
}

impl ToolSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Builtin => "builtin",
            Self::Plugin => "plugin",
            Self::Mcp => "mcp",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "builtin" => Some(Self::Builtin),
            "plugin" => Some(Self::Plugin),
            "mcp" => Some(Self::Mcp),
            _ => None,
        }
    }
}

/// Full row shape; what the Settings page hydrates and the dispatch
/// gate consults. `allowed_classes` is a flat `Vec<String>` so the
/// store can stay agnostic of the policy crate's `TrustLevel` enum.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
pub struct ToolAccessRow {
    pub tool_name: String,
    pub source: ToolSource,
    pub source_id: Option<String>,
    pub enabled: bool,
    pub allowed_classes: Vec<String>,
    pub description: Option<String>,
    pub input_schema: Option<String>,
    pub first_seen_at: i64,
    pub last_seen_at: i64,
    pub removed_at: Option<i64>,
}

/// Immutable operator policy revision for one registered tool.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
pub struct ToolAccessPolicyRevision {
    pub revision_id: i64,
    pub tool_name: String,
    pub enabled: bool,
    pub allowed_classes: Vec<String>,
    pub revised_by: String,
    pub revised_at: i64,
    pub rollback_of: Option<i64>,
}

/// Subset used by registration paths — on first-sight we want to
/// upsert the tool's metadata + first-seen timestamp without
/// touching the operator's already-set `allowed_classes` /
/// `enabled` decision. See [`ToolAccessStore::upsert_seen`].
#[derive(Debug, Clone)]
pub struct ToolAccessSeed {
    pub tool_name: String,
    pub source: ToolSource,
    pub source_id: Option<String>,
    pub description: Option<String>,
    pub input_schema: Option<String>,
    /// The default allowlist applied ONLY when the row is being
    /// inserted for the first time. On subsequent re-syncs the
    /// existing operator policy is preserved.
    pub default_allowed_classes: Vec<String>,
}

pub struct ToolAccessStore<'db> {
    db: &'db Database,
}

impl<'db> ToolAccessStore<'db> {
    pub fn new(db: &'db Database) -> Self {
        Self { db }
    }

    /// Read the latest immutable operator-policy revision. A turn or queued
    /// effect bound to an older value must be reauthorized before execution.
    pub fn latest_policy_revision_id(&self) -> Result<i64, DbError> {
        self.db.with_conn(|connection| {
            let revision = connection.query_row(
                "SELECT COALESCE(MAX(revision_id), 0) FROM config_tool_access_policy_revisions",
                [],
                |row| row.get(0),
            )?;
            Ok(revision)
        })
    }

    /// Look up a single tool. Returns `None` if no row exists — the
    /// caller decides whether that means "default-deny" (production)
    /// or "default-allow" (during the in-flight migration before the
    /// boot-time sync has run).
    pub fn get(&self, tool_name: &str) -> Result<Option<ToolAccessRow>, DbError> {
        self.db.with_conn(|c| {
            let got = c
                .query_row(
                    "SELECT tool_name, source, source_id, enabled, allowed_classes, \
                            description, input_schema, first_seen_at, last_seen_at, \
                            removed_at \
                     FROM config_tool_access WHERE tool_name = ?1",
                    params![tool_name],
                    row_to_tool_access,
                )
                .ok();
            Ok(got)
        })
    }

    /// List every row. Sorted by `(source, tool_name)` so the
    /// Settings page renders the same order across reloads.
    pub fn list_all(&self) -> Result<Vec<ToolAccessRow>, DbError> {
        self.db.with_conn(|c| {
            let mut stmt = c.prepare(
                "SELECT tool_name, source, source_id, enabled, allowed_classes, \
                        description, input_schema, first_seen_at, last_seen_at, \
                        removed_at \
                 FROM config_tool_access \
                 ORDER BY source ASC, tool_name ASC",
            )?;
            let rows = stmt
                .query_map([], row_to_tool_access)?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(rows)
        })
    }

    /// Upsert from a registration sync. On INSERT: applies
    /// `default_allowed_classes` and stamps `first_seen_at = last_seen_at = now`.
    /// On UPDATE: refreshes metadata + bumps `last_seen_at`, but
    /// **never overwrites** the operator's `enabled` / `allowed_classes`
    /// choices. Removes any prior `removed_at` mark since the tool is
    /// once again live.
    pub fn upsert_seen(&self, seed: &ToolAccessSeed, now: i64) -> Result<(), DbError> {
        let allowed_json = serde_json::to_string(&seed.default_allowed_classes)
            .map_err(|e| DbError::Serde(format!("encoding allowed_classes: {e}")))?;
        self.db.with_conn(|c| {
            c.execute(
                "INSERT INTO config_tool_access \
                   (tool_name, source, source_id, enabled, allowed_classes, \
                    description, input_schema, first_seen_at, last_seen_at, \
                    removed_at) \
                 VALUES (?1, ?2, ?3, 1, ?4, ?5, ?6, ?7, ?7, NULL) \
                 ON CONFLICT(tool_name) DO UPDATE SET \
                    source = excluded.source, \
                    source_id = excluded.source_id, \
                    description = excluded.description, \
                    input_schema = excluded.input_schema, \
                    last_seen_at = excluded.last_seen_at, \
                    removed_at = NULL",
                params![
                    seed.tool_name,
                    seed.source.as_str(),
                    seed.source_id,
                    allowed_json,
                    seed.description,
                    seed.input_schema,
                    now,
                ],
            )?;
            Ok(())
        })
    }

    /// Mark a tool as no longer being listed by its source. We keep
    /// the row so the operator's policy survives a transient
    /// disappearance (an MCP server bouncing, a plugin briefly
    /// disabled). The dispatch gate treats `removed_at IS NOT NULL`
    /// as denied even before the trust-class check.
    pub fn mark_removed(&self, tool_name: &str, now: i64) -> Result<bool, DbError> {
        self.db.with_conn(|c| {
            let n = c.execute(
                "UPDATE config_tool_access SET removed_at = ?1 \
                 WHERE tool_name = ?2 AND removed_at IS NULL",
                params![now, tool_name],
            )?;
            Ok(n > 0)
        })
    }

    /// Operator-driven mutation from the Settings UI.
    pub fn set_policy(
        &self,
        tool_name: &str,
        enabled: bool,
        allowed_classes: &[String],
    ) -> Result<bool, DbError> {
        self.set_policy_with_actor(
            tool_name,
            enabled,
            allowed_classes,
            "system",
            chrono::Utc::now().timestamp(),
            None,
        )
    }

    /// Persist a new policy revision while preserving the operator and rollback chain.
    pub fn set_policy_with_actor(
        &self,
        tool_name: &str,
        enabled: bool,
        allowed_classes: &[String],
        actor: &str,
        now: i64,
        rollback_of: Option<i64>,
    ) -> Result<bool, DbError> {
        if actor.trim().is_empty() || actor.len() > 128 {
            return Err(DbError::Invariant(
                "policy revision actor is invalid".into(),
            ));
        }
        let allowed_json = serde_json::to_string(allowed_classes)
            .map_err(|e| DbError::Serde(format!("encoding allowed_classes: {e}")))?;
        self.db.transaction(|tx| {
            let changed = tx.execute(
                "UPDATE config_tool_access \
                 SET enabled = ?1, allowed_classes = ?2 \
                 WHERE tool_name = ?3",
                params![enabled as i64, allowed_json, tool_name],
            )?;
            if changed == 0 {
                return Ok(false);
            }
            let (current_enabled, current_allowed): (i64, String) = tx.query_row(
                "SELECT enabled, allowed_classes FROM config_tool_access WHERE tool_name = ?1",
                [tool_name],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            tx.execute(
                "INSERT INTO config_tool_access_policy_revisions \
                 (tool_name, enabled, allowed_classes_json, revised_by, revised_at, rollback_of) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    tool_name,
                    current_enabled,
                    current_allowed,
                    actor,
                    now,
                    rollback_of
                ],
            )?;
            Ok(true)
        })
    }

    /// Return recent revisions newest first.
    pub fn policy_revisions(
        &self,
        tool_name: &str,
        limit: usize,
    ) -> Result<Vec<ToolAccessPolicyRevision>, DbError> {
        let limit = limit.clamp(1, 100) as i64;
        self.db.with_conn(|connection| {
            let mut statement = connection.prepare(
                "SELECT revision_id, tool_name, enabled, allowed_classes_json, revised_by, revised_at, rollback_of \
                 FROM config_tool_access_policy_revisions WHERE tool_name = ?1 \
                 ORDER BY revision_id DESC LIMIT ?2",
            )?;
            let rows = statement.query_map(params![tool_name, limit], |row| {
                let classes: String = row.get(3)?;
                let allowed_classes = serde_json::from_str(&classes).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        3,
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                })?;
                Ok(ToolAccessPolicyRevision {
                    revision_id: row.get(0)?,
                    tool_name: row.get(1)?,
                    enabled: row.get::<_, i64>(2)? != 0,
                    allowed_classes,
                    revised_by: row.get(4)?,
                    revised_at: row.get(5)?,
                    rollback_of: row.get(6)?,
                })
            })?;
            rows.collect::<Result<Vec<_>, _>>().map_err(DbError::from)
        })
    }

    /// Restore a previous row state as a new revision; revision IDs never move backward.
    pub fn rollback_policy_revision(
        &self,
        tool_name: &str,
        revision_id: i64,
        actor: &str,
        now: i64,
    ) -> Result<bool, DbError> {
        let revision = self.db.with_conn(|connection| {
            connection
                .query_row(
                    "SELECT enabled, allowed_classes_json \
                     FROM config_tool_access_policy_revisions \
                     WHERE revision_id = ?1 AND tool_name = ?2",
                    params![revision_id, tool_name],
                    |row| {
                        let serialized: String = row.get(1)?;
                        let classes: Vec<String> =
                            serde_json::from_str(&serialized).map_err(|error| {
                                rusqlite::Error::FromSqlConversionFailure(
                                    1,
                                    rusqlite::types::Type::Text,
                                    Box::new(error),
                                )
                            })?;
                        Ok((row.get::<_, i64>(0)? != 0, classes))
                    },
                )
                .optional()
                .map_err(DbError::from)
        })?;
        let Some((enabled, allowed_classes)) = revision else {
            return Ok(false);
        };
        self.set_policy_with_actor(
            tool_name,
            enabled,
            &allowed_classes,
            actor,
            now,
            Some(revision_id),
        )
    }

    /// Bulk drop every row owned by a given source — used by the
    /// plugin uninstall path to atomically clear the operator's
    /// stored policy when the source itself is gone for good.
    pub fn delete_by_source(
        &self,
        source: ToolSource,
        source_id: Option<&str>,
    ) -> Result<usize, DbError> {
        self.db.with_conn(|c| {
            let n = match source_id {
                Some(id) => c.execute(
                    "DELETE FROM config_tool_access \
                     WHERE source = ?1 AND source_id = ?2",
                    params![source.as_str(), id],
                )?,
                None => c.execute(
                    "DELETE FROM config_tool_access WHERE source = ?1 AND source_id IS NULL",
                    params![source.as_str()],
                )?,
            };
            Ok(n)
        })
    }
}

fn row_to_tool_access(row: &rusqlite::Row<'_>) -> rusqlite::Result<ToolAccessRow> {
    let source_str: String = row.get(1)?;
    let source = ToolSource::parse(&source_str).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            1,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("unknown tool source: {source_str}"),
            )),
        )
    })?;
    let allowed_json: String = row.get(4)?;
    let allowed_classes: Vec<String> = serde_json::from_str(&allowed_json).unwrap_or_default();
    Ok(ToolAccessRow {
        tool_name: row.get(0)?,
        source,
        source_id: row.get(2)?,
        enabled: row.get::<_, i64>(3)? != 0,
        allowed_classes,
        description: row.get(5)?,
        input_schema: row.get(6)?,
        first_seen_at: row.get(7)?,
        last_seen_at: row.get(8)?,
        removed_at: row.get(9)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Database, DbConfig};
    use crate::migrations::MigrationRunner;

    fn fresh_db() -> Database {
        let db = Database::open(&DbConfig::in_memory_unencrypted()).unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        db
    }

    fn seed(tool: &str, source: ToolSource, id: Option<&str>, defaults: &[&str]) -> ToolAccessSeed {
        ToolAccessSeed {
            tool_name: tool.into(),
            source,
            source_id: id.map(str::to_string),
            description: Some(format!("desc-{tool}")),
            input_schema: Some(r#"{"type":"object"}"#.into()),
            default_allowed_classes: defaults.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn upsert_seen_creates_row_with_defaults() {
        let db = fresh_db();
        let store = ToolAccessStore::new(&db);
        store
            .upsert_seen(
                &seed(
                    "set_thread_name",
                    ToolSource::Builtin,
                    None,
                    &["Controller", "KnownTrusted"],
                ),
                100,
            )
            .unwrap();
        let row = store.get("set_thread_name").unwrap().unwrap();
        assert_eq!(row.source, ToolSource::Builtin);
        assert!(row.enabled);
        assert_eq!(row.allowed_classes, vec!["Controller", "KnownTrusted"]);
        assert_eq!(row.first_seen_at, 100);
        assert_eq!(row.last_seen_at, 100);
    }

    #[test]
    fn upsert_seen_preserves_operator_policy_on_resync() {
        let db = fresh_db();
        let store = ToolAccessStore::new(&db);
        store
            .upsert_seen(
                &seed(
                    "create_pr",
                    ToolSource::Mcp,
                    Some("github"),
                    &["Controller"],
                ),
                100,
            )
            .unwrap();
        // Operator widens the allowlist via the Settings page.
        store
            .set_policy(
                "create_pr",
                true,
                &["Controller".into(), "KnownTrusted".into()],
            )
            .unwrap();
        // Source resyncs (e.g. MCP server reconnect) — must NOT overwrite.
        store
            .upsert_seen(
                &seed(
                    "create_pr",
                    ToolSource::Mcp,
                    Some("github"),
                    &["Controller"],
                ),
                200,
            )
            .unwrap();
        let row = store.get("create_pr").unwrap().unwrap();
        assert_eq!(row.allowed_classes, vec!["Controller", "KnownTrusted"]);
        // last_seen_at advances even though the policy didn't.
        assert_eq!(row.last_seen_at, 200);
        assert_eq!(
            row.first_seen_at, 100,
            "first_seen must NOT shift on resync"
        );
    }

    #[test]
    fn upsert_seen_clears_removed_at_when_tool_returns() {
        let db = fresh_db();
        let store = ToolAccessStore::new(&db);
        store
            .upsert_seen(
                &seed("flaky_tool", ToolSource::Mcp, Some("svc"), &["Controller"]),
                100,
            )
            .unwrap();
        assert!(store.mark_removed("flaky_tool", 150).unwrap());
        assert!(
            store
                .get("flaky_tool")
                .unwrap()
                .unwrap()
                .removed_at
                .is_some()
        );
        // Source resync brings it back.
        store
            .upsert_seen(
                &seed("flaky_tool", ToolSource::Mcp, Some("svc"), &["Controller"]),
                200,
            )
            .unwrap();
        assert!(
            store
                .get("flaky_tool")
                .unwrap()
                .unwrap()
                .removed_at
                .is_none()
        );
    }

    #[test]
    fn list_all_orders_by_source_then_name() {
        let db = fresh_db();
        let store = ToolAccessStore::new(&db);
        store
            .upsert_seen(&seed("z_tool", ToolSource::Builtin, None, &[]), 1)
            .unwrap();
        store
            .upsert_seen(&seed("a_tool", ToolSource::Plugin, Some("p1"), &[]), 1)
            .unwrap();
        store
            .upsert_seen(&seed("b_tool", ToolSource::Builtin, None, &[]), 1)
            .unwrap();
        let names: Vec<String> = store
            .list_all()
            .unwrap()
            .into_iter()
            .map(|r| r.tool_name)
            .collect();
        // builtin < mcp < plugin alphabetically as strings.
        assert_eq!(names, vec!["b_tool", "z_tool", "a_tool"]);
    }

    #[test]
    fn set_policy_returns_false_for_unknown_tool() {
        let db = fresh_db();
        let store = ToolAccessStore::new(&db);
        let updated = store
            .set_policy("never_seen", false, &["Controller".into()])
            .unwrap();
        assert!(!updated);
    }

    #[test]
    fn rollback_restores_policy_as_a_new_append_only_revision() {
        let db = fresh_db();
        let store = ToolAccessStore::new(&db);
        store
            .upsert_seen(
                &seed(
                    "calendar.create",
                    ToolSource::Mcp,
                    Some("calendar"),
                    &["Controller"],
                ),
                1,
            )
            .unwrap();
        store
            .set_policy_with_actor(
                "calendar.create",
                true,
                &["Controller".into(), "KnownTrusted".into()],
                "controller-1",
                2,
                None,
            )
            .unwrap();
        let first = store.policy_revisions("calendar.create", 1).unwrap()[0].clone();
        store
            .set_policy_with_actor(
                "calendar.create",
                false,
                &["Controller".into()],
                "controller-1",
                3,
                None,
            )
            .unwrap();
        let second = store.policy_revisions("calendar.create", 1).unwrap()[0].clone();
        assert!(second.revision_id > first.revision_id);

        assert!(
            store
                .rollback_policy_revision("calendar.create", first.revision_id, "controller-1", 4,)
                .unwrap()
        );
        let restored = store.get("calendar.create").unwrap().unwrap();
        assert!(restored.enabled);
        assert_eq!(restored.allowed_classes, vec!["Controller", "KnownTrusted"]);
        let rollback = store.policy_revisions("calendar.create", 1).unwrap()[0].clone();
        assert!(rollback.revision_id > second.revision_id);
        assert_eq!(rollback.rollback_of, Some(first.revision_id));
        assert_eq!(rollback.revised_by, "controller-1");
    }

    #[test]
    fn delete_by_source_isolates_other_sources() {
        let db = fresh_db();
        let store = ToolAccessStore::new(&db);
        store
            .upsert_seen(&seed("t1", ToolSource::Plugin, Some("p1"), &[]), 1)
            .unwrap();
        store
            .upsert_seen(&seed("t2", ToolSource::Plugin, Some("p1"), &[]), 1)
            .unwrap();
        store
            .upsert_seen(&seed("t3", ToolSource::Plugin, Some("p2"), &[]), 1)
            .unwrap();
        store
            .upsert_seen(&seed("t4", ToolSource::Builtin, None, &[]), 1)
            .unwrap();
        let removed = store
            .delete_by_source(ToolSource::Plugin, Some("p1"))
            .unwrap();
        assert_eq!(removed, 2);
        // Survivors: p2 plugin tool + builtin.
        let names: Vec<_> = store
            .list_all()
            .unwrap()
            .into_iter()
            .map(|r| r.tool_name)
            .collect();
        assert_eq!(names, vec!["t4", "t3"]);
    }

    #[test]
    fn empty_allowed_classes_is_default_deny_invariant() {
        // The store stores what we tell it; the dispatch gate
        // implements the default-deny semantic. This test pins the
        // round-trip so an empty list survives to the gate.
        let db = fresh_db();
        let store = ToolAccessStore::new(&db);
        store
            .upsert_seen(&seed("locked", ToolSource::Mcp, Some("svc"), &[]), 1)
            .unwrap();
        let row = store.get("locked").unwrap().unwrap();
        assert!(row.allowed_classes.is_empty());
    }
}
