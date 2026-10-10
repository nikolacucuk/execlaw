//! Durable idempotency state for caller-submitted chat turns.

use crate::db::{Database, DbError};
use rusqlite::{OptionalExtension, params};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::io::{self, Write};

// An older pending reservation may have crossed an external-effect boundary
// before process loss, so retries report uncertainty instead of automatically
// dispatching the same request a second time.
const PENDING_REQUEST_STALE_AFTER_SECS: i64 = 300;

/// State returned when reserving a client request identifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatRequestState {
    /// The caller owns a new reservation and may start the run.
    Reserved { run_id: String },
    /// A matching request is still active; do not dispatch it again.
    InProgress { run_id: String },
    /// Replay this durable response instead of creating another run.
    Replay {
        run_id: String,
        status_code: u16,
        response_json: String,
    },
    /// The previous attempt stopped with an outcome that needs reconciliation.
    Unknown {
        run_id: String,
        detail: Option<String>,
    },
}

/// Metadata-only view of a request that did not reach a saved HTTP response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnresolvedChatRequest {
    pub principal_id: String,
    pub conversation_id: String,
    pub client_request_id: String,
    pub run_id: String,
    pub pending: bool,
}

/// SQLite-backed caller request reservations and cached responses.
pub struct ChatRequestStore<'db> {
    db: &'db Database,
}

impl<'db> ChatRequestStore<'db> {
    /// Bind to an initialized core database.
    pub fn new(db: &'db Database) -> Self {
        Self { db }
    }

    /// Hash a request body using its deterministic serde JSON encoding.
    pub fn body_hash<T: Serialize>(body: &T) -> Result<String, DbError> {
        struct HashWriter(Sha256);
        impl Write for HashWriter {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.0.update(bytes);
                Ok(bytes.len())
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let mut writer = HashWriter(Sha256::new());
        serde_json::to_writer(&mut writer, body)
            .map_err(|error| DbError::Invariant(format!("serialize request body: {error}")))?;
        Ok(hex::encode(writer.0.finalize()))
    }

    /// Reserve a request id scoped to one principal and conversation, or
    /// return its persisted state. Reusing the id with a different body fails.
    pub fn reserve(
        &self,
        principal_id: &str,
        conversation_id: &str,
        client_request_id: &str,
        body_hash: &str,
        run_id: &str,
        now: i64,
    ) -> Result<ChatRequestState, DbError> {
        validate_scope(
            principal_id,
            conversation_id,
            client_request_id,
            body_hash,
            run_id,
        )?;
        self.db.transaction(|tx| {
            let inserted = tx.execute(
                "INSERT OR IGNORE INTO state_chat_request_keys
                 (principal_id, conversation_id, client_request_id, body_hash,
                  run_id, status, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, 'pending', ?6, ?6)",
                params![
                    principal_id,
                    conversation_id,
                    client_request_id,
                    body_hash,
                    run_id,
                    now
                ],
            )?;
            let stored: (
                String,
                String,
                String,
                Option<i64>,
                Option<String>,
                Option<String>,
                i64,
            ) = tx.query_row(
                "SELECT body_hash, run_id, status, response_status, response_json, outcome_detail,
                        updated_at
                     FROM state_chat_request_keys
                     WHERE principal_id = ?1 AND conversation_id = ?2 AND client_request_id = ?3",
                params![principal_id, conversation_id, client_request_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                    ))
                },
            )?;
            if stored.0 != body_hash {
                return Err(DbError::Invariant(
                    "client request id was reused with a different body".into(),
                ));
            }
            let run_id = stored.1;
            match stored.2.as_str() {
                "pending" if inserted == 1 => Ok(ChatRequestState::Reserved { run_id }),
                "pending" if now.saturating_sub(stored.6) >= PENDING_REQUEST_STALE_AFTER_SECS => {
                    let detail = "pending request lease expired; outcome requires reconciliation";
                    tx.execute(
                        "UPDATE state_chat_request_keys
                         SET status = 'unknown', outcome_detail = ?4, updated_at = ?5
                         WHERE principal_id = ?1 AND conversation_id = ?2
                           AND client_request_id = ?3 AND status = 'pending'",
                        params![
                            principal_id,
                            conversation_id,
                            client_request_id,
                            detail,
                            now
                        ],
                    )?;
                    Ok(ChatRequestState::Unknown {
                        run_id,
                        detail: Some(detail.to_owned()),
                    })
                }
                "pending" => Ok(ChatRequestState::InProgress { run_id }),
                "completed" => Ok(ChatRequestState::Replay {
                    run_id,
                    status_code: u16::try_from(stored.3.unwrap_or_default()).map_err(|_| {
                        DbError::Invariant("saved HTTP status is out of range".into())
                    })?,
                    response_json: stored.4.ok_or_else(|| {
                        DbError::Invariant("completed request has no saved response".into())
                    })?,
                }),
                "unknown" => Ok(ChatRequestState::Unknown {
                    run_id,
                    detail: stored.5,
                }),
                status => Err(DbError::Invariant(format!(
                    "unknown chat request state '{status}'"
                ))),
            }
        })
    }

    /// Find a recent pending request with the same authenticated body.
    /// This lets a duplicate send join the live turn without claiming its
    /// model step or minting another user event.
    ///
    /// ```ignore
    /// let run_id = ChatRequestStore::new(&db)
    ///     .in_progress_run_for_body("controller", "conversation", &body_hash, now - 300)?;
    /// ```
    pub fn in_progress_run_for_body(
        &self,
        principal_id: &str,
        conversation_id: &str,
        body_hash: &str,
        updated_since: i64,
    ) -> Result<Option<String>, DbError> {
        self.db.with_conn(|connection| {
            connection
                .query_row(
                    "SELECT run_id FROM state_chat_request_keys
                     WHERE principal_id = ?1 AND conversation_id = ?2 AND body_hash = ?3
                       AND status = 'pending' AND updated_at >= ?4
                     ORDER BY updated_at DESC LIMIT 1",
                    params![principal_id, conversation_id, body_hash, updated_since],
                    |row| row.get(0),
                )
                .optional()
                .map_err(DbError::from)
        })
    }

    /// Save the exact HTTP response for retries. Only pending reservations
    /// can transition to completed.
    pub fn complete(
        &self,
        principal_id: &str,
        conversation_id: &str,
        client_request_id: &str,
        run_id: &str,
        status_code: u16,
        response_json: &str,
        now: i64,
    ) -> Result<(), DbError> {
        if response_json.len() > 2 * 1024 * 1024 || !(200..=599).contains(&status_code) {
            return Err(DbError::Invariant("invalid saved chat response".into()));
        }
        let changed = self.db.with_conn(|connection| {
            Ok(connection.execute(
                "UPDATE state_chat_request_keys
                 SET run_id = ?4, status = 'completed', response_status = ?5,
                     response_json = ?6, outcome_detail = NULL, updated_at = ?7
                 WHERE principal_id = ?1 AND conversation_id = ?2 AND client_request_id = ?3
                   AND status = 'pending'",
                params![
                    principal_id,
                    conversation_id,
                    client_request_id,
                    run_id,
                    status_code,
                    response_json,
                    now
                ],
            )?)
        })?;
        if changed != 1 {
            return Err(DbError::Invariant("chat request is not pending".into()));
        }
        Ok(())
    }

    /// Refresh the active reservation lease while its handler is executing.
    pub fn heartbeat(
        &self,
        principal_id: &str,
        conversation_id: &str,
        client_request_id: &str,
        now: i64,
    ) -> Result<bool, DbError> {
        self.db.with_conn(|connection| {
            let changed = connection.execute(
                "UPDATE state_chat_request_keys SET updated_at = ?4
                     WHERE principal_id = ?1 AND conversation_id = ?2
                       AND client_request_id = ?3 AND status = 'pending'",
                params![principal_id, conversation_id, client_request_id, now],
            )?;
            Ok(changed == 1)
        })
    }

    /// Bind a pending client reservation to its durable conversation run as
    /// soon as the input sequence is known. A process restart can then
    /// distinguish a resumable runner turn from an unstarted reservation.
    pub fn bind_execution_run(
        &self,
        principal_id: &str,
        conversation_id: &str,
        client_request_id: &str,
        reserved_run_id: &str,
        execution_run_id: &str,
        now: i64,
    ) -> Result<(), DbError> {
        let changed = self.db.with_conn(|connection| {
            connection
                .execute(
                    "UPDATE state_chat_request_keys SET run_id = ?5, updated_at = ?6 \
                 WHERE principal_id = ?1 AND conversation_id = ?2 AND client_request_id = ?3 \
                   AND status = 'pending' AND run_id = ?4",
                    params![
                        principal_id,
                        conversation_id,
                        client_request_id,
                        reserved_run_id,
                        execution_run_id,
                        now
                    ],
                )
                .map_err(DbError::from)
        })?;
        if changed == 1 {
            return Ok(());
        }
        let stored_run = self.db.with_conn(|connection| {
            connection
                .query_row(
                    "SELECT run_id FROM state_chat_request_keys \
                 WHERE principal_id = ?1 AND conversation_id = ?2 AND client_request_id = ?3 \
                   AND status = 'pending'",
                    params![principal_id, conversation_id, client_request_id],
                    |row| row.get::<_, String>(0),
                )
                .map_err(DbError::from)
        })?;
        if stored_run == execution_run_id {
            Ok(())
        } else {
            Err(DbError::Invariant(
                "chat request reservation could not bind execution run".into(),
            ))
        }
    }

    /// Reopen an unknown request only after the caller proves the referenced
    /// runner input is a safe durable checkpoint to resume.
    pub fn reopen_unknown_for_recovery(
        &self,
        principal_id: &str,
        conversation_id: &str,
        client_request_id: &str,
        previous_run_id: &str,
        execution_run_id: &str,
        now: i64,
    ) -> Result<(), DbError> {
        let changed = self.db.with_conn(|connection| {
            connection
                .execute(
                    "UPDATE state_chat_request_keys SET run_id = ?5, status = 'pending', \
                    outcome_detail = NULL, updated_at = ?6 \
                 WHERE principal_id = ?1 AND conversation_id = ?2 AND client_request_id = ?3 \
                   AND run_id = ?4 AND status = 'unknown'",
                    params![
                        principal_id,
                        conversation_id,
                        client_request_id,
                        previous_run_id,
                        execution_run_id,
                        now
                    ],
                )
                .map_err(DbError::from)
        })?;
        if changed == 1 {
            Ok(())
        } else {
            Err(DbError::Invariant(
                "chat request is not an unknown recovery candidate".into(),
            ))
        }
    }

    /// Mark an interrupted or uncertain attempt; retries remain visible
    /// instead of automatically issuing a second turn/effect.
    pub fn mark_unknown(
        &self,
        principal_id: &str,
        conversation_id: &str,
        client_request_id: &str,
        detail: Option<&str>,
        now: i64,
    ) -> Result<(), DbError> {
        if detail.is_some_and(|value| value.len() > 512) {
            return Err(DbError::Invariant(
                "outcome detail exceeds 512 bytes".into(),
            ));
        }
        let changed = self.db.with_conn(|connection| {
            Ok(connection.execute(
                "UPDATE state_chat_request_keys
                 SET status = 'unknown', outcome_detail = ?4, updated_at = ?5
                 WHERE principal_id = ?1 AND conversation_id = ?2 AND client_request_id = ?3
                   AND status = 'pending'",
                params![
                    principal_id,
                    conversation_id,
                    client_request_id,
                    detail,
                    now
                ],
            )?)
        })?;
        if changed != 1 {
            return Err(DbError::Invariant("chat request is not pending".into()));
        }
        Ok(())
    }

    /// List requests whose caller response was not durably recorded.
    pub fn list_unresolved(&self, limit: usize) -> Result<Vec<UnresolvedChatRequest>, DbError> {
        self.db.with_conn(|connection| {
            let mut statement = connection.prepare_cached(
                "SELECT principal_id, conversation_id, client_request_id, run_id, status
                 FROM state_chat_request_keys WHERE status IN ('pending', 'unknown')
                 ORDER BY created_at, principal_id, conversation_id, client_request_id
                 LIMIT ?1",
            )?;
            statement
                .query_map([limit.clamp(1, 1_000)], |row| {
                    let status: String = row.get(4)?;
                    Ok(UnresolvedChatRequest {
                        principal_id: row.get(0)?,
                        conversation_id: row.get(1)?,
                        client_request_id: row.get(2)?,
                        run_id: row.get(3)?,
                        pending: status == "pending",
                    })
                })?
                .collect::<Result<Vec<_>, _>>()
                .map_err(DbError::from)
        })
    }

    /// Cache a response reconstructed from a completed durable run after restart.
    pub fn complete_reconciled(
        &self,
        request: &UnresolvedChatRequest,
        status_code: u16,
        response_json: &str,
        now: i64,
    ) -> Result<(), DbError> {
        if response_json.len() > 2 * 1024 * 1024 || !(200..=599).contains(&status_code) {
            return Err(DbError::Invariant(
                "invalid reconciled chat response".into(),
            ));
        }
        let changed = self.db.with_conn(|connection| {
            connection
                .execute(
                    "UPDATE state_chat_request_keys
                     SET status = 'completed', response_status = ?5, response_json = ?6,
                         outcome_detail = NULL, updated_at = ?7
                     WHERE principal_id = ?1 AND conversation_id = ?2
                       AND client_request_id = ?3 AND run_id = ?4
                       AND status IN ('pending', 'unknown')",
                    params![
                        request.principal_id,
                        request.conversation_id,
                        request.client_request_id,
                        request.run_id,
                        status_code,
                        response_json,
                        now
                    ],
                )
                .map_err(DbError::from)
        })?;
        if changed == 1 {
            return Ok(());
        }
        let stored: Option<(String, Option<i64>, Option<String>)> =
            self.db.with_conn(|connection| {
                connection
                    .query_row(
                        "SELECT run_id, response_status, response_json
                     FROM state_chat_request_keys
                     WHERE principal_id = ?1 AND conversation_id = ?2 AND client_request_id = ?3
                       AND status = 'completed'",
                        params![
                            request.principal_id,
                            request.conversation_id,
                            request.client_request_id
                        ],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                    )
                    .optional()
                    .map_err(DbError::from)
            })?;
        match stored {
            Some((run_id, Some(saved_status), Some(saved_json)))
                if run_id == request.run_id
                    && saved_status == i64::from(status_code)
                    && saved_json == response_json =>
            {
                Ok(())
            }
            _ => Err(DbError::Invariant(
                "chat request changed while its completed run was reconciled".into(),
            )),
        }
    }
}

fn validate_scope(
    principal_id: &str,
    conversation_id: &str,
    request_id: &str,
    body_hash: &str,
    run_id: &str,
) -> Result<(), DbError> {
    if principal_id.trim().is_empty()
        || principal_id.len() > 128
        || conversation_id.trim().is_empty()
        || conversation_id.len() > 128
        || request_id.trim().is_empty()
        || request_id.len() > 128
        || !request_id.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
        || run_id.trim().is_empty()
        || run_id.len() > 160
        || body_hash.len() != 64
        || !body_hash
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(DbError::Invariant(
            "invalid chat request idempotency scope".into(),
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
    fn retries_replay_exact_response_and_reject_changed_body() {
        let db = fresh_db();
        let store = ChatRequestStore::new(&db);
        let hash = ChatRequestStore::body_hash(&serde_json::json!({"text":"hello"})).unwrap();
        assert_eq!(
            store
                .reserve("controller", "thread-a", "req-1", &hash, "run-a", 10)
                .unwrap(),
            ChatRequestState::Reserved {
                run_id: "run-a".into()
            }
        );
        assert_eq!(
            store
                .reserve("controller", "thread-a", "req-1", &hash, "run-other", 11)
                .unwrap(),
            ChatRequestState::InProgress {
                run_id: "run-a".into()
            }
        );
        store
            .complete(
                "controller",
                "thread-a",
                "req-1",
                "run-a",
                200,
                r#"{"assistant_seq":2}"#,
                12,
            )
            .unwrap();
        assert_eq!(
            store
                .reserve("controller", "thread-a", "req-1", &hash, "run-new", 13)
                .unwrap(),
            ChatRequestState::Replay {
                run_id: "run-a".into(),
                status_code: 200,
                response_json: r#"{"assistant_seq":2}"#.into()
            }
        );
        let changed =
            ChatRequestStore::body_hash(&serde_json::json!({"text":"different"})).unwrap();
        assert!(
            store
                .reserve("controller", "thread-a", "req-1", &changed, "run-new", 14)
                .is_err()
        );
    }

    #[test]
    fn active_duplicate_lookup_requires_same_scope_body_and_live_reservation() {
        let db = fresh_db();
        let store = ChatRequestStore::new(&db);
        let hash = ChatRequestStore::body_hash(&serde_json::json!({"text":"slow reply"})).unwrap();
        store
            .reserve(
                "controller",
                "thread-a",
                "first",
                &hash,
                "turn:thread-a:1",
                100,
            )
            .unwrap();
        assert_eq!(
            store
                .in_progress_run_for_body("controller", "thread-a", &hash, 99)
                .unwrap(),
            Some("turn:thread-a:1".into())
        );
        assert!(
            store
                .in_progress_run_for_body("other", "thread-a", &hash, 99)
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .in_progress_run_for_body("controller", "thread-a", &hash, 101)
                .unwrap()
                .is_none()
        );
        let different =
            ChatRequestStore::body_hash(&serde_json::json!({"text":"new work"})).unwrap();
        assert!(
            store
                .in_progress_run_for_body("controller", "thread-a", &different, 99)
                .unwrap()
                .is_none()
        );
        store
            .complete(
                "controller",
                "thread-a",
                "first",
                "turn:thread-a:1",
                200,
                "{}",
                102,
            )
            .unwrap();
        assert!(
            store
                .in_progress_run_for_body("controller", "thread-a", &hash, 99)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn uncertain_result_is_returned_for_operator_reconciliation() {
        let db = fresh_db();
        let store = ChatRequestStore::new(&db);
        let hash = "a".repeat(64);
        store
            .reserve("controller", "thread-a", "req-2", &hash, "run-b", 10)
            .unwrap();
        store
            .mark_unknown(
                "controller",
                "thread-a",
                "req-2",
                Some("delivery uncertain"),
                12,
            )
            .unwrap();
        assert_eq!(
            store
                .reserve("controller", "thread-a", "req-2", &hash, "run-other", 13)
                .unwrap(),
            ChatRequestState::Unknown {
                run_id: "run-b".into(),
                detail: Some("delivery uncertain".into())
            }
        );
    }

    #[test]
    fn safe_runner_recovery_reopens_unknown_reservation_and_caches_replay() {
        let db = fresh_db();
        let store = ChatRequestStore::new(&db);
        let hash = ChatRequestStore::body_hash(&serde_json::json!({"text":"resume"})).unwrap();
        assert!(matches!(
            store
                .reserve("controller", "thread-r", "req-r", &hash, "request-run", 10)
                .unwrap(),
            ChatRequestState::Reserved { .. }
        ));
        store
            .bind_execution_run(
                "controller",
                "thread-r",
                "req-r",
                "request-run",
                "turn:thread-r:7",
                11,
            )
            .unwrap();
        store
            .mark_unknown(
                "controller",
                "thread-r",
                "req-r",
                Some("safe checkpoint available"),
                12,
            )
            .unwrap();
        assert!(matches!(
            store
                .reserve("controller", "thread-r", "req-r", &hash, "request-run", 13)
                .unwrap(),
            ChatRequestState::Unknown { ref run_id, .. } if run_id == "turn:thread-r:7"
        ));
        store
            .reopen_unknown_for_recovery(
                "controller",
                "thread-r",
                "req-r",
                "turn:thread-r:7",
                "turn:thread-r:7",
                14,
            )
            .unwrap();
        store
            .complete(
                "controller",
                "thread-r",
                "req-r",
                "turn:thread-r:7",
                200,
                r#"{"assistant_seq":8}"#,
                15,
            )
            .unwrap();
        assert!(matches!(
            store
                .reserve("controller", "thread-r", "req-r", &hash, "request-run", 16)
                .unwrap(),
            ChatRequestState::Replay { run_id, .. } if run_id == "turn:thread-r:7"
        ));
    }

    #[test]
    fn startup_reconciliation_caches_a_response_for_a_completed_run() {
        let db = fresh_db();
        let store = ChatRequestStore::new(&db);
        let hash = ChatRequestStore::body_hash(&serde_json::json!({"text":"done"})).unwrap();
        store
            .reserve(
                "controller",
                "thread-r",
                "req-startup",
                &hash,
                "request-r",
                10,
            )
            .unwrap();
        store
            .bind_execution_run(
                "controller",
                "thread-r",
                "req-startup",
                "request-r",
                "turn:thread-r:7",
                11,
            )
            .unwrap();
        store
            .mark_unknown(
                "controller",
                "thread-r",
                "req-startup",
                Some("process restarted"),
                12,
            )
            .unwrap();

        let unresolved = store.list_unresolved(10).unwrap();
        assert_eq!(unresolved.len(), 1);
        assert!(!unresolved[0].pending);
        store
            .complete_reconciled(
                &unresolved[0],
                200,
                r#"{"conversation_id":"thread-r","assistant_text":"done"}"#,
                13,
            )
            .unwrap();
        assert!(matches!(
            store
                .reserve("controller", "thread-r", "req-startup", &hash, "new-run", 14)
                .unwrap(),
            ChatRequestState::Replay { run_id, .. } if run_id == "turn:thread-r:7"
        ));
    }

    #[test]
    fn abandoned_pending_request_becomes_unknown_instead_of_replaying_effects() {
        let db = fresh_db();
        let store = ChatRequestStore::new(&db);
        let hash = "c".repeat(64);
        store
            .reserve("controller", "thread-a", "req-expired", &hash, "run-c", 10)
            .unwrap();
        assert_eq!(
            store
                .reserve(
                    "controller",
                    "thread-a",
                    "req-expired",
                    &hash,
                    "run-other",
                    311,
                )
                .unwrap(),
            ChatRequestState::Unknown {
                run_id: "run-c".into(),
                detail: Some(
                    "pending request lease expired; outcome requires reconciliation".into()
                ),
            }
        );
    }

    #[test]
    fn active_request_heartbeat_prevents_stale_outcome_classification() {
        let db = fresh_db();
        let store = ChatRequestStore::new(&db);
        let hash = "e".repeat(64);
        store
            .reserve("controller", "thread-a", "req-live", &hash, "run-live", 10)
            .unwrap();
        assert!(
            store
                .heartbeat("controller", "thread-a", "req-live", 309)
                .unwrap()
        );
        assert_eq!(
            store
                .reserve(
                    "controller",
                    "thread-a",
                    "req-live",
                    &hash,
                    "run-other",
                    608
                )
                .unwrap(),
            ChatRequestState::InProgress {
                run_id: "run-live".into()
            }
        );
    }

    #[test]
    fn pending_request_reservation_survives_database_reopen() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("chat-requests.db");
        let db = Database::open(&DbConfig {
            path: path.clone(),
            key: None,
        })
        .unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        let hash = "b".repeat(64);
        ChatRequestStore::new(&db)
            .reserve(
                "controller",
                "thread-a",
                "req-restart",
                &hash,
                "run-stable",
                10,
            )
            .unwrap();
        let completed_hash = "d".repeat(64);
        let store = ChatRequestStore::new(&db);
        store
            .reserve(
                "controller",
                "thread-a",
                "req-complete",
                &completed_hash,
                "run-complete",
                11,
            )
            .unwrap();
        store
            .complete(
                "controller",
                "thread-a",
                "req-complete",
                "turn:thread-a:4",
                200,
                r#"{"assistant_seq":5}"#,
                12,
            )
            .unwrap();
        drop(db);

        let reopened = Database::open(&DbConfig { path, key: None }).unwrap();
        MigrationRunner::new(&reopened).apply_all().unwrap();
        assert_eq!(
            ChatRequestStore::new(&reopened)
                .reserve(
                    "controller",
                    "thread-a",
                    "req-restart",
                    &hash,
                    "run-other",
                    20
                )
                .unwrap(),
            ChatRequestState::InProgress {
                run_id: "run-stable".into()
            }
        );
        assert_eq!(
            ChatRequestStore::new(&reopened)
                .reserve(
                    "controller",
                    "thread-a",
                    "req-complete",
                    &completed_hash,
                    "run-new",
                    21,
                )
                .unwrap(),
            ChatRequestState::Replay {
                run_id: "turn:thread-a:4".into(),
                status_code: 200,
                response_json: r#"{"assistant_seq":5}"#.into(),
            }
        );
    }
}
