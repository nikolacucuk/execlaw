//! Host-maintained resource versions for preview/execute preconditions.

use crate::db::{Database, DbError};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// The current version token observed for one named resource.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceVersion {
    pub resource_key: String,
    pub version_token: String,
    pub conditional_updates: bool,
    pub observed_at: i64,
}

/// Immutable version captured when a proposal is prepared.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceVersionPrecondition {
    pub resource_key: String,
    pub expected_version: Option<String>,
    pub conditional_updates: bool,
}

/// Current state compared with a prepared precondition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceVersionCheck {
    pub resource_key: String,
    pub expected_version: Option<String>,
    pub current_version: Option<String>,
    pub conditional_updates: bool,
    pub matches: bool,
}

/// Errors returned by the resource-version store.
#[derive(Debug, Error)]
pub enum ResourceVersionError {
    #[error(transparent)]
    Db(#[from] DbError),
    #[error("invalid resource version: {0}")]
    Invalid(String),
}

/// SQLite-backed version observations used to reject stale proposals.
pub struct ResourceVersionStore<'db> {
    db: &'db Database,
}

impl<'db> ResourceVersionStore<'db> {
    /// Create a store bound to the host database.
    pub fn new(db: &'db Database) -> Self {
        Self { db }
    }

    /// Record a provider-observed version token for a resource.
    ///
    /// `conditional_updates` must be true only when the provider can enforce
    /// the token at its own write boundary (for example with an ETag/If-Match).
    pub fn observe(
        &self,
        resource_key: &str,
        version_token: &str,
        conditional_updates: bool,
        observed_at: i64,
    ) -> Result<ResourceVersion, ResourceVersionError> {
        validate_resource_key(resource_key)?;
        if version_token.is_empty() || version_token.len() > 512 {
            return Err(ResourceVersionError::Invalid(
                "version token must contain 1 to 512 bytes".into(),
            ));
        }
        self.db.with_conn(|connection| {
            connection.execute(
                "INSERT INTO state_resource_versions(resource_key,version_token,conditional_updates,observed_at) \
                 VALUES (?1,?2,?3,?4) ON CONFLICT(resource_key) DO UPDATE SET \
                 version_token=excluded.version_token,conditional_updates=excluded.conditional_updates,observed_at=excluded.observed_at",
                params![resource_key, version_token, conditional_updates, observed_at],
            )?;
            Ok(())
        })?;
        Ok(ResourceVersion {
            resource_key: resource_key.to_owned(),
            version_token: version_token.to_owned(),
            conditional_updates,
            observed_at,
        })
    }

    /// Load the latest provider observation for one resource.
    pub fn get(&self, resource_key: &str) -> Result<Option<ResourceVersion>, ResourceVersionError> {
        validate_resource_key(resource_key)?;
        self.db
            .with_conn(|connection| {
                connection
                    .query_row(
                        "SELECT resource_key,version_token,conditional_updates,observed_at \
                         FROM state_resource_versions WHERE resource_key=?1",
                        [resource_key],
                        |row| {
                            Ok(ResourceVersion {
                                resource_key: row.get(0)?,
                                version_token: row.get(1)?,
                                conditional_updates: row.get(2)?,
                                observed_at: row.get(3)?,
                            })
                        },
                    )
                    .optional()
                    .map_err(DbError::from)
            })
            .map_err(Into::into)
    }

    /// Capture the currently known version for each requested resource.
    /// Missing observations are retained as unresolved preconditions and fail
    /// closed when checked later.
    pub fn capture(
        &self,
        resource_keys: &[String],
    ) -> Result<Vec<ResourceVersionPrecondition>, ResourceVersionError> {
        if resource_keys.len() > 64 {
            return Err(ResourceVersionError::Invalid(
                "a proposal may bind at most 64 resources".into(),
            ));
        }
        let mut unique = std::collections::BTreeSet::new();
        let mut preconditions = Vec::with_capacity(resource_keys.len());
        for key in resource_keys {
            validate_resource_key(key)?;
            if !unique.insert(key) {
                return Err(ResourceVersionError::Invalid(format!(
                    "duplicate resource key '{key}'"
                )));
            }
            let current = self.get(key)?;
            preconditions.push(ResourceVersionPrecondition {
                resource_key: key.clone(),
                expected_version: current
                    .as_ref()
                    .map(|version| version.version_token.clone()),
                conditional_updates: current.is_some_and(|version| version.conditional_updates),
            });
        }
        Ok(preconditions)
    }

    /// Compare prepared resource versions with current provider observations.
    pub fn check(
        &self,
        preconditions: &[ResourceVersionPrecondition],
    ) -> Result<Vec<ResourceVersionCheck>, ResourceVersionError> {
        if preconditions.len() > 64 {
            return Err(ResourceVersionError::Invalid(
                "a proposal may bind at most 64 resources".into(),
            ));
        }
        let mut unique = std::collections::BTreeSet::new();
        preconditions
            .iter()
            .map(|precondition| {
                validate_resource_key(&precondition.resource_key)?;
                if !unique.insert(&precondition.resource_key) {
                    return Err(ResourceVersionError::Invalid(format!(
                        "duplicate resource key '{}'",
                        precondition.resource_key
                    )));
                }
                let current = self.get(&precondition.resource_key)?;
                let current_version = current
                    .as_ref()
                    .map(|version| version.version_token.clone());
                let conditional_updates = current
                    .as_ref()
                    .is_some_and(|version| version.conditional_updates);
                let matches = precondition.expected_version.is_some()
                    && precondition.expected_version == current_version;
                Ok(ResourceVersionCheck {
                    resource_key: precondition.resource_key.clone(),
                    expected_version: precondition.expected_version.clone(),
                    current_version,
                    conditional_updates,
                    matches,
                })
            })
            .collect()
    }
}

fn validate_resource_key(key: &str) -> Result<(), ResourceVersionError> {
    if key.trim().is_empty() || key.len() > 256 || key.trim() != key {
        return Err(ResourceVersionError::Invalid(
            "resource key must contain 1 to 256 trimmed bytes".into(),
        ));
    }
    Ok(())
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
    fn changed_or_unobserved_resource_versions_fail_closed() {
        let db = fresh_db();
        let store = ResourceVersionStore::new(&db);
        store.observe("record/7", "rev-1", true, 10).unwrap();
        let captured = store.capture(&["record/7".into()]).unwrap();
        assert!(store.check(&captured).unwrap()[0].matches);

        store.observe("record/7", "rev-2", true, 11).unwrap();
        let changed = store.check(&captured).unwrap();
        assert!(!changed[0].matches);
        assert_eq!(changed[0].current_version.as_deref(), Some("rev-2"));

        let missing = store.capture(&["recipient/unknown".into()]).unwrap();
        assert!(!store.check(&missing).unwrap()[0].matches);
        assert!(!missing[0].conditional_updates);
    }

    #[test]
    fn resource_observation_reports_conditional_update_capability() {
        let db = fresh_db();
        let store = ResourceVersionStore::new(&db);
        store
            .observe("calendar/event/4", "etag-abc", false, 12)
            .unwrap();
        let captured = store.capture(&["calendar/event/4".into()]).unwrap();
        assert_eq!(captured[0].expected_version.as_deref(), Some("etag-abc"));
        assert!(!captured[0].conditional_updates);
    }
}
