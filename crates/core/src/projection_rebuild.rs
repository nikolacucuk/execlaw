//! Durable generation switching for rebuildable derived projections.

use crate::db::{Database, DbError};
use rusqlite::{OptionalExtension, params};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionGeneration {
    pub projection_name: String,
    pub generation: i64,
    pub projection_version: String,
    pub status: String,
    pub cursor: Option<Vec<u8>>,
    pub source_watermark: i64,
    pub rows_written: i64,
}

pub struct ProjectionRebuildStore<'db> {
    db: &'db Database,
}

impl<'db> ProjectionRebuildStore<'db> {
    pub fn new(db: &'db Database) -> Self {
        Self { db }
    }

    /// Start a generation, or return its unfinished matching generation so a
    /// worker can resume after a process interruption.
    pub fn begin(
        &self,
        projection: &str,
        version: &str,
        now: i64,
    ) -> Result<ProjectionGeneration, DbError> {
        if projection.is_empty() || version.is_empty() {
            return Err(DbError::Config(
                "projection name and version are required".into(),
            ));
        }
        self.db.transaction(|tx| {
            if let Some(existing) = tx
                .query_row(
                    "SELECT projection_name,generation,projection_version,status,cursor,source_watermark,rows_written \
                     FROM state_projection_generations WHERE projection_name=?1 AND projection_version=?2 \
                       AND status IN ('building','validated') ORDER BY generation DESC LIMIT 1",
                    params![projection, version],
                    row_to_generation,
                )
                .optional()?
            {
                return Ok(existing);
            }
            let generation: i64 = tx.query_row(
                "SELECT COALESCE(MAX(generation),0)+1 FROM state_projection_generations WHERE projection_name=?1",
                [projection],
                |row| row.get(0),
            )?;
            tx.execute(
                "INSERT INTO state_projection_generations \
                 (projection_name,generation,projection_version,status,started_at,updated_at) \
                 VALUES (?1,?2,?3,'building',?4,?4)",
                params![projection, generation, version, now],
            )?;
            Ok(ProjectionGeneration {
                projection_name: projection.to_owned(),
                generation,
                projection_version: version.to_owned(),
                status: "building".into(),
                cursor: None,
                source_watermark: 0,
                rows_written: 0,
            })
        })
    }

    /// Persist one bounded batch checkpoint. The row count is cumulative, so
    /// retrying the same checkpoint after interruption is idempotent.
    pub fn checkpoint(
        &self,
        generation: &ProjectionGeneration,
        cursor: &[u8],
        watermark: i64,
        cumulative_rows_written: u64,
        now: i64,
    ) -> Result<(), DbError> {
        if watermark < generation.source_watermark || watermark < 0 {
            return Err(DbError::Invariant(
                "projection rebuild watermark moved backwards".into(),
            ));
        }
        let rows_written = i64::try_from(cumulative_rows_written)
            .map_err(|_| DbError::Invariant("projection batch row count overflow".into()))?;
        self.db.with_conn(|conn| {
            let changed = conn.execute(
                "UPDATE state_projection_generations SET cursor=?3,source_watermark=?4, \
                    rows_written=?5,updated_at=?6 \
                 WHERE projection_name=?1 AND generation=?2 AND status='building' \
                   AND source_watermark<=?4 AND rows_written<=?5",
                params![
                    generation.projection_name,
                    generation.generation,
                    cursor,
                    watermark,
                    rows_written,
                    now,
                ],
            )?;
            if changed != 1 {
                return Err(DbError::Invariant(
                    "projection generation is no longer writable".into(),
                ));
            }
            Ok(())
        })
    }

    /// Mark a fully checked generation ready for an atomic switch.
    pub fn validate(&self, projection: &str, generation: i64, now: i64) -> Result<(), DbError> {
        self.db.with_conn(|conn| {
            let changed = conn.execute(
                "UPDATE state_projection_generations SET status='validated',updated_at=?3 \
                 WHERE projection_name=?1 AND generation=?2 AND status='building'",
                params![projection, generation, now],
            )?;
            if changed != 1 {
                return Err(DbError::Invariant(
                    "only a building projection can be validated".into(),
                ));
            }
            Ok(())
        })
    }

    /// Atomically expose a validated generation. Readers should resolve the
    /// active generation through [`active_generation`] before querying data.
    pub fn activate(&self, projection: &str, generation: i64, now: i64) -> Result<(), DbError> {
        self.db.transaction(|tx| {
            let ready: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM state_projection_generations \
                 WHERE projection_name=?1 AND generation=?2 AND status='validated')",
                params![projection, generation],
                |row| row.get(0),
            )?;
            if !ready {
                return Err(DbError::Invariant(
                    "projection generation must validate before activation".into(),
                ));
            }
            tx.execute(
                "UPDATE state_projection_generations SET status='retired',updated_at=?2 \
                 WHERE projection_name=?1 AND status='active'",
                params![projection, now],
            )?;
            tx.execute(
                "UPDATE state_projection_generations SET status='active',updated_at=?3 \
                 WHERE projection_name=?1 AND generation=?2",
                params![projection, generation, now],
            )?;
            tx.execute(
                "INSERT INTO state_projection_activation(projection_name,generation,activated_at) \
                 VALUES (?1,?2,?3) ON CONFLICT(projection_name) DO UPDATE SET \
                   generation=excluded.generation,activated_at=excluded.activated_at",
                params![projection, generation, now],
            )?;
            Ok(())
        })
    }

    pub fn fail(
        &self,
        projection: &str,
        generation: i64,
        reason: &str,
        now: i64,
    ) -> Result<(), DbError> {
        self.db.with_conn(|conn| {
            let changed = conn.execute(
                "UPDATE state_projection_generations SET status='failed',error=?3,updated_at=?4 \
                 WHERE projection_name=?1 AND generation=?2 AND status IN ('building','validated')",
                params![projection, generation, reason, now],
            )?;
            if changed != 1 {
                return Err(DbError::Invariant(
                    "projection generation cannot be failed in its current state".into(),
                ));
            }
            Ok(())
        })
    }

    pub fn active_generation(
        &self,
        projection: &str,
    ) -> Result<Option<ProjectionGeneration>, DbError> {
        self.db.with_conn(|conn| {
            Ok(conn
                .query_row(
                    "SELECT g.projection_name,g.generation,g.projection_version,g.status,g.cursor, \
                        g.source_watermark,g.rows_written \
                 FROM state_projection_activation a JOIN state_projection_generations g \
                   ON g.projection_name=a.projection_name AND g.generation=a.generation \
                 WHERE a.projection_name=?1",
                    [projection],
                    row_to_generation,
                )
                .optional()?)
        })
    }
}

fn row_to_generation(row: &rusqlite::Row<'_>) -> rusqlite::Result<ProjectionGeneration> {
    Ok(ProjectionGeneration {
        projection_name: row.get(0)?,
        generation: row.get(1)?,
        projection_version: row.get(2)?,
        status: row.get(3)?,
        cursor: row.get(4)?,
        source_watermark: row.get(5)?,
        rows_written: row.get(6)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DbConfig, MigrationRunner};

    fn db() -> Database {
        let db = Database::open(&DbConfig::in_memory_unencrypted()).unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        db
    }

    #[test]
    fn interrupted_generation_resumes_without_becoming_visible() {
        let db = db();
        let store = ProjectionRebuildStore::new(&db);
        let first = store.begin("conversation_search", "v2", 10).unwrap();
        store.checkpoint(&first, b"event:32", 32, 32, 11).unwrap();
        let resumed = store.begin("conversation_search", "v2", 12).unwrap();
        assert_eq!(resumed.generation, first.generation);
        assert_eq!(resumed.status, "building");
        assert_eq!(resumed.source_watermark, 32);
        assert!(
            store
                .active_generation("conversation_search")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn readers_switch_only_after_validated_generation_activates() {
        let db = db();
        let store = ProjectionRebuildStore::new(&db);
        let old = store.begin("archive", "v1", 1).unwrap();
        store.validate("archive", old.generation, 2).unwrap();
        store.activate("archive", old.generation, 3).unwrap();
        let replacement = store.begin("archive", "v2", 4).unwrap();
        assert_eq!(
            store
                .active_generation("archive")
                .unwrap()
                .unwrap()
                .generation,
            old.generation
        );
        assert!(
            store
                .activate("archive", replacement.generation, 5)
                .is_err()
        );
        store
            .validate("archive", replacement.generation, 6)
            .unwrap();
        store
            .activate("archive", replacement.generation, 7)
            .unwrap();
        let active = store.active_generation("archive").unwrap().unwrap();
        assert_eq!(active.generation, replacement.generation);
        assert_eq!(active.status, "active");
    }

    #[test]
    fn rebuild_watermarks_cannot_move_backwards() {
        let db = db();
        let store = ProjectionRebuildStore::new(&db);
        let generation = store.begin("memory", "v3", 1).unwrap();
        store.checkpoint(&generation, b"a", 10, 5, 2).unwrap();
        assert!(store.checkpoint(&generation, b"b", 9, 2, 3).is_err());
    }

    #[test]
    fn failed_replacement_leaves_the_previous_generation_active() {
        let db = db();
        let store = ProjectionRebuildStore::new(&db);
        let active = store.begin("graph", "v1", 1).unwrap();
        store.validate("graph", active.generation, 2).unwrap();
        store.activate("graph", active.generation, 3).unwrap();
        let replacement = store.begin("graph", "v2", 4).unwrap();
        store
            .fail("graph", replacement.generation, "fixture failure", 5)
            .unwrap();
        assert_eq!(
            store
                .active_generation("graph")
                .unwrap()
                .unwrap()
                .generation,
            active.generation
        );
    }
}
