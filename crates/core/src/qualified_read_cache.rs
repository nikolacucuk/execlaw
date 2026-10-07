//! Conversation- and authority-scoped cache for qualified, read-only tool results.

use crate::db::{Database, DbError};
use rusqlite::params;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CachedRead {
    pub result: serde_json::Value,
    pub provenance: serde_json::Value,
    pub source_revision: String,
    pub expires_at: i64,
}

#[derive(Debug, Clone)]
pub struct ReadCacheKey<'a> {
    pub conversation_id: &'a str,
    pub authority_scope: &'a str,
    pub tool_name: &'a str,
    pub tool_version: &'a str,
    pub canonical_arguments: &'a serde_json::Value,
    pub source_revision: &'a str,
}

pub struct QualifiedReadCache<'db> {
    db: &'db Database,
}

impl<'db> QualifiedReadCache<'db> {
    pub fn new(db: &'db Database) -> Self {
        Self { db }
    }

    fn key(key: &ReadCacheKey<'_>) -> Result<(String, String), DbError> {
        let args = serde_json::to_string(key.canonical_arguments)
            .map_err(|e| DbError::Invariant(format!("cannot canonicalize read arguments: {e}")))?;
        let args_hash = hex::encode(Sha256::digest(args.as_bytes()));
        let material = format!(
            "{}\0{}\0{}\0{}\0{}\0{}",
            key.conversation_id,
            key.authority_scope,
            key.tool_name,
            key.tool_version,
            args_hash,
            key.source_revision
        );
        Ok((hex::encode(Sha256::digest(material.as_bytes())), args_hash))
    }

    /// Return a fresh cached result only after the caller reauthorizes this hit.
    /// Callers must pass a read-only tool operation; effect results are rejected.
    pub fn get_authorized(
        &self,
        key: &ReadCacheKey<'_>,
        is_read_only: bool,
        now: i64,
        reauthorize: impl FnOnce(&str, &str, &str) -> bool,
    ) -> Result<Option<CachedRead>, DbError> {
        if !is_read_only {
            return Ok(None);
        }
        let (cache_key, _) = Self::key(key)?;
        let record = self.db.with_conn(|conn| {
            Ok(conn
                .query_row(
                    "SELECT result_json,provenance_json,source_revision,expires_at \
                FROM qualified_read_cache WHERE cache_key=?1",
                    [&cache_key],
                    |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, String>(2)?,
                            r.get::<_, i64>(3)?,
                        ))
                    },
                )
                .optional()?)
        })?;
        let Some((result, provenance, revision, expires_at)) = record else {
            return Ok(None);
        };
        if expires_at <= now || revision != key.source_revision {
            return Ok(None);
        }
        if !reauthorize(key.conversation_id, key.authority_scope, key.tool_name) {
            return Ok(None);
        }
        let result = serde_json::from_str(&result)
            .map_err(|e| DbError::Invariant(format!("invalid cached result: {e}")))?;
        let provenance = serde_json::from_str(&provenance)
            .map_err(|e| DbError::Invariant(format!("invalid cached provenance: {e}")))?;
        Ok(Some(CachedRead {
            result,
            provenance,
            source_revision: revision,
            expires_at,
        }))
    }

    /// Store a read result with its source provenance. Effectful operations cannot be cached.
    pub fn put(
        &self,
        key: &ReadCacheKey<'_>,
        is_read_only: bool,
        result: &serde_json::Value,
        provenance: &serde_json::Value,
        expires_at: i64,
        now: i64,
    ) -> Result<bool, DbError> {
        if !is_read_only || expires_at <= now {
            return Ok(false);
        }
        let (cache_key, args_hash) = Self::key(key)?;
        let result =
            serde_json::to_string(result).map_err(|e| DbError::Invariant(e.to_string()))?;
        let provenance =
            serde_json::to_string(provenance).map_err(|e| DbError::Invariant(e.to_string()))?;
        self.db.with_conn(|conn| {
            conn.execute("INSERT INTO qualified_read_cache(cache_key,conversation_id,authority_scope,tool_name,tool_version,canonical_args_hash,source_revision,result_json,provenance_json,expires_at,created_at) \
                VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11) ON CONFLICT(cache_key) DO UPDATE SET result_json=excluded.result_json,provenance_json=excluded.provenance_json,expires_at=excluded.expires_at,created_at=excluded.created_at",
                params![cache_key,key.conversation_id,key.authority_scope,key.tool_name,key.tool_version,args_hash,key.source_revision,result,provenance,expires_at,now])?;
            Ok(())
        })?;
        Ok(true)
    }

    /// Invalidate every cached result tied to an authority scope after a grant change.
    pub fn invalidate_scope(
        &self,
        conversation_id: &str,
        authority_scope: &str,
    ) -> Result<usize, DbError> {
        self.db.with_conn(|conn| {
            Ok(conn.execute(
                "DELETE FROM qualified_read_cache WHERE conversation_id=?1 AND authority_scope=?2",
                params![conversation_id, authority_scope],
            )?)
        })
    }

    /// Invalidate cached results for a resource after its source revision changes or it is deleted.
    pub fn invalidate_source_revision(&self, source_revision: &str) -> Result<usize, DbError> {
        self.db.with_conn(|conn| {
            Ok(conn.execute(
                "DELETE FROM qualified_read_cache WHERE source_revision=?1",
                [source_revision],
            )?)
        })
    }
}

use rusqlite::OptionalExtension;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{db::DbConfig, migrations::MigrationRunner};
    fn fresh() -> Database {
        let db = Database::open(&DbConfig::in_memory_unencrypted()).unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        db
    }
    #[test]
    fn scoped_versioned_cache_reauthorizes_and_rejects_effects() {
        let db = fresh();
        let cache = QualifiedReadCache::new(&db);
        let key = ReadCacheKey {
            conversation_id: "c1",
            authority_scope: "calendar:read",
            tool_name: "events.list",
            tool_version: "2",
            canonical_arguments: &serde_json::json!({"from":"today"}),
            source_revision: "rev-1",
        };
        assert!(
            cache
                .put(
                    &key,
                    true,
                    &serde_json::json!([1]),
                    &serde_json::json!({"source":"calendar"}),
                    100,
                    1
                )
                .unwrap()
        );
        assert!(
            cache
                .get_authorized(&key, true, 2, |_, _, _| true)
                .unwrap()
                .is_some()
        );
        assert!(
            cache
                .get_authorized(&key, true, 2, |_, _, _| false)
                .unwrap()
                .is_none()
        );
        assert!(
            cache
                .get_authorized(&key, false, 2, |_, _, _| true)
                .unwrap()
                .is_none()
        );
        let changed = ReadCacheKey {
            source_revision: "rev-2",
            ..key.clone()
        };
        assert!(
            cache
                .get_authorized(&changed, true, 2, |_, _, _| true)
                .unwrap()
                .is_none()
        );
        assert_eq!(cache.invalidate_scope("c1", "calendar:read").unwrap(), 1);
        assert!(
            cache
                .get_authorized(&key, true, 2, |_, _, _| true)
                .unwrap()
                .is_none()
        );
    }
}
