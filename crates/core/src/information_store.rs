//! Durable host-owned labels for information that crosses run boundaries.

use crate::db::{Database, DbError};
use crate::information::{InformationLabel, Sensitivity};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

/// Stable reference to content whose label must survive transformations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InformationSubject {
    pub kind: String,
    pub id: String,
    pub sha256: String,
}

/// Append-only persistence for host-owned information-flow labels.
pub struct InformationLabelStore<'db> {
    db: &'db Database,
}

impl<'db> InformationLabelStore<'db> {
    /// Create a store over the shared state database.
    pub fn new(db: &'db Database) -> Self {
        Self { db }
    }

    /// Attach the initial label to an observed value. Existing subjects cannot
    /// be relabelled through this method; derived values use `transform`.
    pub fn observe(
        &self,
        subject: &InformationSubject,
        label: &InformationLabel,
        actor_id: &str,
        at: i64,
    ) -> Result<(), DbError> {
        validate_subject(subject)?;
        validate_actor(actor_id)?;
        if label.sensitivity < Sensitivity::Internal {
            return Err(DbError::Invariant(
                "observed runtime information cannot be labelled below Internal".into(),
            ));
        }
        self.db.transaction(|tx| {
            let exists: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM state_information_label_events \
                 WHERE subject_kind=?1 AND subject_id=?2 AND content_sha256=?3)",
                params![subject.kind, subject.id, subject.sha256],
                |row| row.get(0),
            )?;
            if exists {
                return Err(DbError::Invariant(
                    "information subject already has a label history".into(),
                ));
            }
            append_event(tx, subject, "observed", label, actor_id, None, at)
        })
    }

    /// Return the latest host-owned label for this exact content digest.
    pub fn get(&self, subject: &InformationSubject) -> Result<Option<InformationLabel>, DbError> {
        validate_subject(subject)?;
        self.db.with_conn(|connection| {
            let json: Option<String> = connection
                .query_row(
                    "SELECT information_label_json FROM state_information_label_events \
                     WHERE subject_kind=?1 AND subject_id=?2 AND content_sha256=?3 \
                     ORDER BY rowid DESC LIMIT 1",
                    params![subject.kind, subject.id, subject.sha256],
                    |row| row.get(0),
                )
                .optional()?;
            json.map(|value| {
                serde_json::from_str(&value)
                    .map_err(|error| DbError::Serde(format!("decode information label: {error}")))
            })
            .transpose()
        })
    }

    /// Return the latest subject reference and label for one stable identity.
    pub fn latest_for_identity(
        &self,
        kind: &str,
        id: &str,
    ) -> Result<Option<(InformationSubject, InformationLabel)>, DbError> {
        if kind.trim().is_empty() || kind.len() > 64 || id.trim().is_empty() || id.len() > 256 {
            return Err(DbError::Invariant("information identity is invalid".into()));
        }
        self.db.with_conn(|connection| {
            let row: Option<(String, String)> = connection
                .query_row(
                    "SELECT content_sha256,information_label_json FROM state_information_label_events \
                     WHERE subject_kind=?1 AND subject_id=?2 ORDER BY rowid DESC LIMIT 1",
                    params![kind, id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            row.map(|(sha256, json)| {
                let label = serde_json::from_str(&json).map_err(|error| {
                    DbError::Serde(format!("decode information label: {error}"))
                })?;
                Ok((
                    InformationSubject {
                        kind: kind.to_owned(),
                        id: id.to_owned(),
                        sha256,
                    },
                    label,
                ))
            })
            .transpose()
        })
    }

    /// Derive a label for transformed content by conservatively joining its
    /// source labels and recording the new content digest.
    pub fn transform(
        &self,
        output: &InformationSubject,
        sources: &[InformationSubject],
        operation: &str,
        actor_id: &str,
        at: i64,
    ) -> Result<InformationLabel, DbError> {
        validate_subject(output)?;
        validate_actor(actor_id)?;
        if operation.trim().is_empty() || operation.len() > 128 || sources.is_empty() {
            return Err(DbError::Invariant(
                "information transformation needs a bounded operation and source".into(),
            ));
        }
        let source_labels = sources
            .iter()
            .map(|source| self.get(source))
            .collect::<Result<Vec<_>, _>>()?;
        let labels = source_labels
            .into_iter()
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| {
                DbError::Invariant("information transformation source label is missing".into())
            })?;
        let label = InformationLabel::combine(&labels, operation).ok_or_else(|| {
            DbError::Invariant("information transformation has no sources".into())
        })?;
        self.db.transaction(|tx| {
            let exists: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM state_information_label_events \
                 WHERE subject_kind=?1 AND subject_id=?2 AND content_sha256=?3)",
                params![output.kind, output.id, output.sha256],
                |row| row.get(0),
            )?;
            if exists {
                return Err(DbError::Invariant(
                    "transformed information subject already has a label history".into(),
                ));
            }
            append_event(
                tx,
                output,
                "transformed",
                &label,
                actor_id,
                Some(operation),
                at,
            )
        })?;
        Ok(label)
    }

    /// Record an operator-authorized exception for one destination and scope.
    /// The caller must authenticate `actor_id` as a Controller before calling.
    pub fn declassify(
        &self,
        subject: &InformationSubject,
        actor_id: &str,
        destination: &str,
        scope: &str,
        at: i64,
    ) -> Result<InformationLabel, DbError> {
        validate_subject(subject)?;
        validate_actor(actor_id)?;
        let mut label = self
            .get(subject)?
            .ok_or_else(|| DbError::Invariant("information label does not exist".into()))?;
        label
            .declassify(actor_id, destination, scope, at)
            .map_err(|error| DbError::Invariant(error.into()))?;
        self.db.transaction(|tx| {
            append_event(
                tx,
                subject,
                "declassified",
                &label,
                actor_id,
                Some(scope),
                at,
            )
        })?;
        Ok(label)
    }
}

fn validate_subject(subject: &InformationSubject) -> Result<(), DbError> {
    if subject.kind.trim().is_empty()
        || subject.kind.len() > 64
        || subject.id.trim().is_empty()
        || subject.id.len() > 256
        || subject.sha256.len() != 64
        || !subject.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(DbError::Invariant("information subject is invalid".into()));
    }
    Ok(())
}

fn validate_actor(actor_id: &str) -> Result<(), DbError> {
    if actor_id.trim().is_empty() || actor_id.len() > 128 {
        return Err(DbError::Invariant(
            "information label actor is invalid".into(),
        ));
    }
    Ok(())
}

fn append_event(
    tx: &rusqlite::Transaction<'_>,
    subject: &InformationSubject,
    operation: &str,
    label: &InformationLabel,
    actor_id: &str,
    scope: Option<&str>,
    at: i64,
) -> Result<(), DbError> {
    let json = serde_json::to_string(label)
        .map_err(|error| DbError::Serde(format!("encode information label: {error}")))?;
    tx.execute(
        "INSERT INTO state_information_label_events \
         (event_id,subject_kind,subject_id,content_sha256,operation,information_label_json,actor_id,scope,created_at) \
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        params![
            uuid::Uuid::new_v4().to_string(),
            subject.kind,
            subject.id,
            subject.sha256,
            operation,
            json,
            actor_id,
            scope,
            at
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Database, DbConfig};
    use crate::migrations::MigrationRunner;

    fn subject(kind: &str, id: &str, digest_char: char) -> InformationSubject {
        InformationSubject {
            kind: kind.into(),
            id: id.into(),
            sha256: std::iter::repeat_n(digest_char, 64).collect(),
        }
    }

    fn label() -> InformationLabel {
        InformationLabel::observed(
            Sensitivity::Sensitive,
            Some("principal-1".into()),
            "KnownLimited",
            "message",
            "event-1",
            ["conversation:one".into()],
        )
    }

    #[test]
    fn labels_follow_summary_skill_artifact_and_child_transformations() {
        let db = Database::open(&DbConfig::in_memory_unencrypted()).unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        let store = InformationLabelStore::new(&db);
        let source = subject("observation", "event-1", 'a');
        store.observe(&source, &label(), "principal-1", 1).unwrap();
        let summary = subject("summary", "summary-1", 'b');
        store
            .transform(&summary, &[source], "summarize", "runner", 2)
            .unwrap();
        let skill = subject("skill", "skill-1", 'c');
        store
            .transform(&skill, &[summary], "capture_skill", "skills", 3)
            .unwrap();
        let artifact = subject("artifact", "artifact-1", 'd');
        store
            .transform(&artifact, &[skill], "publish_artifact", "runner", 4)
            .unwrap();
        let child = subject("child_result", "child-1", 'e');
        let propagated = store
            .transform(&child, &[artifact], "join_child_result", "runner", 5)
            .unwrap();

        assert_eq!(propagated.sensitivity, Sensitivity::Sensitive);
        assert_eq!(propagated.source_trust_class, "KnownLimited");
        assert!(!propagated.permits_destination("transport:external"));
    }

    #[test]
    fn declassification_is_durable_scoped_and_actor_attributed() {
        let db = Database::open(&DbConfig::in_memory_unencrypted()).unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        let store = InformationLabelStore::new(&db);
        let source = subject("artifact", "artifact-2", 'f');
        store.observe(&source, &label(), "principal-1", 1).unwrap();
        let declassified = store
            .declassify(&source, "controller-1", "transport:alice", "case-17", 2)
            .unwrap();
        assert!(declassified.permits_destination("transport:alice"));
        assert!(!declassified.permits_destination("transport:bob"));
        assert_eq!(store.get(&source).unwrap(), Some(declassified));
        let audit: (String, String, String) = db
            .with_conn(|connection| {
                Ok(connection.query_row(
                    "SELECT actor_id,scope,operation FROM state_information_label_events ORDER BY rowid DESC LIMIT 1",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )?)
            })
            .unwrap();
        assert_eq!(
            audit,
            (
                "controller-1".into(),
                "case-17".into(),
                "declassified".into()
            )
        );
    }
}
