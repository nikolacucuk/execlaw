//! Reapply deletion fences from the database being replaced by a restore.

use execlaw_core::Database;
use execlaw_core::memory_assets::MemoryAssetStore;
use execlaw_skills::SkillStore;
use rusqlite::params;

struct ResearchTombstone {
    deletion_id: String,
    resource_id: String,
    requested_by: String,
    request_source: String,
    payload_json: String,
    status: String,
    attempt_count: i64,
    last_error: Option<String>,
    requested_at: i64,
    updated_at: i64,
    completed_at: Option<i64>,
}

struct AssetTombstone {
    asset_id: String,
    requested_by: String,
    requested_at: i64,
}

struct MemoryTombstone {
    target_kind: String,
    target_id: String,
    assertion_id: Option<String>,
    source_conversation_id: String,
    source_event_seq: i64,
    request_id: String,
    requested_by: String,
    requested_at: i64,
}

struct SkillTombstone {
    name_sha256: String,
    requested_by: String,
    requested_at: i64,
    name: Option<String>,
}

/// Reapplies privacy tombstones from the live target into a restored snapshot.
///
/// `execlaw restore --force` can otherwise replace a newer deletion fence with
/// an older backup. Only deletion metadata is copied; the current database's
/// conversation or model data is never merged into the restored snapshot.
pub(crate) fn reapply_from(current: &Database, restored: &Database) -> anyhow::Result<usize> {
    let (research, assets, memory, skills) = current
        .with_conn(|connection| {
            let research = {
                let mut statement = connection.prepare(
                    "SELECT deletion_id, resource_id, requested_by, request_source, payload_json, \
                     status, attempt_count, last_error, requested_at, updated_at, completed_at \
                     FROM state_privacy_deletion_jobs",
                )?;
                statement
                    .query_map([], |row| {
                        Ok(ResearchTombstone {
                            deletion_id: row.get(0)?,
                            resource_id: row.get(1)?,
                            requested_by: row.get(2)?,
                            request_source: row.get(3)?,
                            payload_json: row.get(4)?,
                            status: row.get(5)?,
                            attempt_count: row.get(6)?,
                            last_error: row.get(7)?,
                            requested_at: row.get(8)?,
                            updated_at: row.get(9)?,
                            completed_at: row.get(10)?,
                        })
                    })?
                    .collect::<Result<Vec<_>, _>>()?
            };
            let assets = {
                let mut statement = connection.prepare(
                    "SELECT asset_id, requested_by, requested_at \
                     FROM state_memory_asset_deletion_tombstones",
                )?;
                statement
                    .query_map([], |row| {
                        Ok(AssetTombstone {
                            asset_id: row.get(0)?,
                            requested_by: row.get(1)?,
                            requested_at: row.get(2)?,
                        })
                    })?
                    .collect::<Result<Vec<_>, _>>()?
            };
            let memory = {
                let mut statement = connection.prepare(
                    "SELECT target_kind, target_id, assertion_id, source_conversation_id, \
                     source_event_seq, request_id, requested_by, requested_at \
                     FROM state_memory_privacy_tombstones",
                )?;
                statement
                    .query_map([], |row| {
                        Ok(MemoryTombstone {
                            target_kind: row.get(0)?,
                            target_id: row.get(1)?,
                            assertion_id: row.get(2)?,
                            source_conversation_id: row.get(3)?,
                            source_event_seq: row.get(4)?,
                            request_id: row.get(5)?,
                            requested_by: row.get(6)?,
                            requested_at: row.get(7)?,
                        })
                    })?
                    .collect::<Result<Vec<_>, _>>()?
            };
            let skills = {
                let mut statement = connection.prepare(
                    "SELECT t.name_sha256, t.requested_by, t.requested_at, s.name \
                     FROM state_skill_privacy_tombstones t \
                     LEFT JOIN state_skills s ON s.id = t.skill_id",
                )?;
                statement
                    .query_map([], |row| {
                        Ok(SkillTombstone {
                            name_sha256: row.get(0)?,
                            requested_by: row.get(1)?,
                            requested_at: row.get(2)?,
                            name: row.get(3)?,
                        })
                    })?
                    .collect::<Result<Vec<_>, _>>()?
            };
            Ok((research, assets, memory, skills))
        })
        .map_err(|error| anyhow::anyhow!("read existing privacy tombstones: {error}"))?;

    restored
        .transaction(|tx| {
            for tombstone in &research {
                tx.execute(
                    "INSERT OR IGNORE INTO state_privacy_deletion_jobs \
                     (deletion_id, resource_kind, resource_id, requested_by, request_source, \
                      payload_json, status, attempt_count, last_error, requested_at, updated_at, completed_at) \
                     VALUES (?1, 'research_job', ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                    params![
                        tombstone.deletion_id,
                        tombstone.resource_id,
                        tombstone.requested_by,
                        tombstone.request_source,
                        tombstone.payload_json,
                        tombstone.status,
                        tombstone.attempt_count,
                        tombstone.last_error,
                        tombstone.requested_at,
                        tombstone.updated_at,
                        tombstone.completed_at,
                    ],
                )?;
                if tombstone.status == "complete" {
                    tx.execute(
                        "DELETE FROM state_research_jobs WHERE id = ?1",
                        [&tombstone.resource_id],
                    )?;
                }
            }
            for tombstone in &memory {
                tx.execute(
                    "INSERT OR IGNORE INTO state_memory_privacy_tombstones \
                     (target_kind, target_id, assertion_id, source_conversation_id, source_event_seq, \
                      request_id, requested_by, requested_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    params![
                        tombstone.target_kind,
                        tombstone.target_id,
                        tombstone.assertion_id,
                        tombstone.source_conversation_id,
                        tombstone.source_event_seq,
                        tombstone.request_id,
                        tombstone.requested_by,
                        tombstone.requested_at,
                    ],
                )?;
                if tombstone.target_kind == "assertion" {
                    tx.execute(
                        "DELETE FROM memory_current_projection WHERE assertion_id = ?1",
                        [&tombstone.target_id],
                    )?;
                }
            }
            Ok(())
        })
        .map_err(|error| anyhow::anyhow!("reapply research and memory tombstones: {error}"))?;

    for tombstone in &assets {
        MemoryAssetStore::new(restored)
            .delete(
                &tombstone.asset_id,
                &tombstone.requested_by,
                tombstone.requested_at,
            )
            .map_err(|error| anyhow::anyhow!("reapply memory asset tombstone: {error}"))?;
    }

    for tombstone in &skills {
        if let Some(name) = &tombstone.name {
            let exists = restored
                .with_conn(|connection| {
                    Ok(connection.query_row(
                        "SELECT EXISTS(SELECT 1 FROM state_skills WHERE name = ?1)",
                        [name],
                        |row| row.get::<_, bool>(0),
                    )?)
                })
                .map_err(|error| anyhow::anyhow!("find restored forgotten skill: {error}"))?;
            if exists {
                SkillStore::new(restored.clone())
                    .forget(name, &tombstone.requested_by, tombstone.requested_at)
                    .map_err(|error| anyhow::anyhow!("reapply forgotten skill: {error}"))?;
                continue;
            }
        }

        restored
            .transaction(|tx| {
                let placeholder_name = format!("privacy-deleted-{}", tombstone.name_sha256);
                tx.execute(
                    "INSERT OR IGNORE INTO state_skills \
                     (name, current_version_id, state, source, registration_kind, owning_plugin_id, \
                      created_at, updated_at, archived_at) \
                     VALUES (?1, NULL, 'archived', 'privacy-deleted', 'authored', NULL, ?2, ?2, ?2)",
                    params![placeholder_name, tombstone.requested_at],
                )?;
                let skill_id: i64 = tx.query_row(
                    "SELECT id FROM state_skills WHERE name = ?1",
                    [&placeholder_name],
                    |row| row.get(0),
                )?;
                tx.execute(
                    "INSERT OR IGNORE INTO state_skill_privacy_tombstones \
                     (skill_id, name_sha256, requested_by, requested_at) VALUES (?1, ?2, ?3, ?4)",
                    params![
                        skill_id,
                        tombstone.name_sha256,
                        tombstone.requested_by,
                        tombstone.requested_at,
                    ],
                )?;
                Ok(())
            })
            .map_err(|error| anyhow::anyhow!("reapply forgotten skill name fence: {error}"))?;
    }

    Ok(research.len() + assets.len() + memory.len() + skills.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use execlaw_core::{DbConfig, migrations::MigrationRunner};

    fn fresh_db(path: &std::path::Path) -> Database {
        let db = Database::open(&DbConfig {
            path: path.to_path_buf(),
            key: None,
        })
        .unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        db
    }

    #[test]
    fn restore_reapplies_tombstones_for_research_memory_and_skills() {
        let dir = tempfile::tempdir().unwrap();
        let current = fresh_db(&dir.path().join("current.db"));
        let restored = fresh_db(&dir.path().join("restored.db"));
        current
            .with_conn(|connection| {
                connection.execute(
                    "INSERT INTO state_privacy_deletion_jobs \
                     (deletion_id, resource_kind, resource_id, requested_by, request_source, \
                      payload_json, status, requested_at, updated_at, completed_at) \
                     VALUES ('delete-1', 'research_job', 'job-1', 'controller-1', 'controller', \
                             '{}', 'complete', 10, 11, 11)",
                    [],
                )?;
                connection.execute(
                    "INSERT INTO state_memory_asset_deletion_tombstones \
                     (asset_id, requested_by, requested_at, completed_at) \
                     VALUES ('asset-1', 'controller-1', 12, 12)",
                    [],
                )?;
                connection.execute(
                    "INSERT INTO state_memory_privacy_tombstones \
                     (target_kind, target_id, assertion_id, source_conversation_id, source_event_seq, \
                      request_id, requested_by, requested_at) \
                     VALUES ('assertion', 'assertion-1', 'assertion-1', 'conversation-1', 1, \
                             'request-1', 'controller-1', 13)",
                    [],
                )?;
                connection.execute(
                    "INSERT INTO state_skills \
                     (name, current_version_id, state, source, registration_kind, owning_plugin_id, \
                      created_at, updated_at, archived_at) \
                     VALUES ('forgotten skill', NULL, 'archived', 'admin', 'authored', NULL, 14, 14, 14)",
                    [],
                )?;
                let skill_id = connection.last_insert_rowid();
                connection.execute(
                    "INSERT INTO state_skill_privacy_tombstones \
                     (skill_id, name_sha256, requested_by, requested_at) \
                     VALUES (?1, ?2, 'controller-1', 14)",
                    params![skill_id, "a".repeat(64)],
                )?;
                Ok(())
            })
            .unwrap();

        assert_eq!(reapply_from(&current, &restored).unwrap(), 4);
        let (research, asset, memory, skill) = restored
            .with_conn(|connection| {
                let research = connection.query_row(
                    "SELECT COUNT(*), MAX(status) FROM state_privacy_deletion_jobs \
                     WHERE resource_id = 'job-1'",
                    [],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
                )?;
                let asset = connection.query_row(
                    "SELECT COUNT(*) FROM state_memory_asset_deletion_tombstones \
                     WHERE asset_id = 'asset-1'",
                    [],
                    |row| row.get::<_, i64>(0),
                )?;
                let memory = connection.query_row(
                    "SELECT COUNT(*) FROM state_memory_privacy_tombstones \
                     WHERE target_id = 'assertion-1'",
                    [],
                    |row| row.get::<_, i64>(0),
                )?;
                let skill = connection.query_row(
                    "SELECT COUNT(*) FROM state_skill_privacy_tombstones \
                     WHERE name_sha256 = ?1",
                    ["a".repeat(64)],
                    |row| row.get::<_, i64>(0),
                )?;
                Ok((research, asset, memory, skill))
            })
            .unwrap();
        assert_eq!((research.0, research.1.as_str()), (1, "complete"));
        assert_eq!((asset, memory, skill), (1, 1, 1));
    }
}
