//! Attachment + artifact row models (§2.9.1, §2.9.2).
//!
//! Blobs themselves live on disk under `~/.execlaw/blobs/...`; these tables
//! only carry metadata.

use crate::db::{Database, DbError};
use crate::ids::{AttachmentId, ConversationId, ResearchJobId};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, RwLock, Weak};

static ARTIFACT_PATH_LOCKS: OnceLock<Mutex<std::collections::HashMap<PathBuf, Weak<RwLock<()>>>>> =
    OnceLock::new();

fn artifact_path_lock(path: &Path) -> Arc<RwLock<()>> {
    let locks = ARTIFACT_PATH_LOCKS.get_or_init(|| Mutex::new(Default::default()));
    let Ok(mut locks) = locks.lock() else {
        return Arc::new(RwLock::new(()));
    };
    if let Some(lock) = locks.get(path).and_then(Weak::upgrade) {
        return lock;
    }
    let lock = Arc::new(RwLock::new(()));
    locks.insert(path.to_path_buf(), Arc::downgrade(&lock));
    lock
}

fn publish_content_addressed(
    path: &Path,
    root: &Path,
    bytes: &[u8],
    sha256: &str,
) -> Result<(), DbError> {
    std::fs::create_dir_all(root).map_err(|error| {
        DbError::Migration(format!(
            "create artifact directory {}: {error}",
            root.display()
        ))
    })?;
    if path.exists() {
        return verify_content_addressed(path, sha256, bytes.len() as u64);
    }
    let temporary = root.join(format!(".{sha256}.{}.tmp", uuid::Uuid::new_v4()));
    let temporary_lock = artifact_path_lock(&temporary);
    let _temporary_guard = temporary_lock
        .write()
        .map_err(|_| DbError::Config("artifact temp path lock poisoned".into()))?;
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        match std::fs::rename(&temporary, path) {
            Ok(()) => {}
            Err(error) if path.exists() => {
                verify_content_addressed(path, sha256, bytes.len() as u64)?;
                let _ = std::fs::remove_file(&temporary);
                let _ = error;
            }
            Err(error) => return Err(error.into()),
        }
        if let Ok(directory) = std::fs::File::open(root) {
            let _ = directory.sync_all();
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

fn verify_content_addressed(
    path: &Path,
    expected_sha256: &str,
    expected_bytes: u64,
) -> Result<(), DbError> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() || metadata.len() != expected_bytes {
        return Err(DbError::Invariant(
            "content-addressed artifact path is not a regular file of the referenced size".into(),
        ));
    }
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut total = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        total = total.saturating_add(count as u64);
        hasher.update(&buffer[..count]);
    }
    if total != expected_bytes || hex::encode(hasher.finalize()) != expected_sha256 {
        return Err(DbError::Invariant(
            "existing content-addressed artifact does not match its digest or size".into(),
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttachmentRow {
    pub id: AttachmentId,
    pub conversation_id: ConversationId,
    pub mime_type: String,
    pub path: String,
    pub sha256: String,
    pub received_at: i64,
    /// Operator-facing filename — what the file is called in the
    /// SPA's download chip, in transport messages, and in the
    /// python_sandbox plugin's `/work/<convo>/uploads/` mount.
    /// `None` for legacy rows minted before migration 0006 and for
    /// ingest paths that haven't been wired to populate it yet;
    /// hydration falls back to a derived default when missing.
    #[serde(default)]
    pub filename: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactRow {
    pub id: String,
    pub research_job_id: Option<ResearchJobId>,
    pub kind: String, // "research_pdf" | "image" | "plugin_artifact" | "other"
    pub mime_type: String,
    pub path: String,
    pub sha256: String,
    pub bytes: Option<i64>,
    pub created_at: i64,
    /// Plugin that minted this artifact, when applicable. `None` for
    /// research-pipeline artifacts and any pre-0002-migration rows.
    pub plugin_id: Option<String>,
    /// Operator-facing filename (browser save-as, outbound transport
    /// attachment name). Kept separate from `path` because `path` is the
    /// sha256-named blob on disk and would be opaque to a human.
    pub filename: Option<String>,
    /// Unix-seconds wall-clock TTL; `None` means no TTL. The ephemeral
    /// sweeper culls expired rows + their on-disk bytes.
    pub expires_at: Option<i64>,
}

/// Outcome of [`AttachmentStore::insert_plugin_artifact`]. Carries the new
/// row's identifiers so the caller (a Rhai binding) can hand them back to
/// the plugin script.
#[derive(Debug, Clone)]
pub struct PluginArtifactCreated {
    pub attachment_id: String,
    pub sha256: String,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResultArtifactChunk {
    pub artifact_id: String,
    pub sha256: String,
    pub mime_type: String,
    pub offset: u64,
    pub next_offset: u64,
    pub total_bytes: u64,
    pub content: String,
}

/// Compute the standard on-disk path for a plugin artifact, given the
/// artifacts root directory and the bytes' sha256. Content-addressed —
/// two artifacts with identical bytes share one on-disk file.
pub fn plugin_artifact_path(root: &Path, sha256: &str) -> PathBuf {
    root.join(sha256)
}

pub struct AttachmentStore<'db> {
    db: &'db Database,
}

impl<'db> AttachmentStore<'db> {
    pub fn new(db: &'db Database) -> Self {
        Self { db }
    }

    pub fn insert(&self, row: &AttachmentRow) -> Result<(), DbError> {
        self.db.with_conn(|c| {
            c.execute(
                "INSERT INTO state_attachments(id, conversation_id, mime_type, path, sha256, received_at, filename) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    row.id.as_str(),
                    row.conversation_id.as_str(),
                    row.mime_type,
                    row.path,
                    row.sha256,
                    row.received_at,
                    row.filename,
                ],
            )?;
            Ok(())
        })?;
        self.ensure_information_label(
            crate::information_store::InformationSubject {
                kind: "attachment".into(),
                id: row.id.as_str().into(),
                sha256: row.sha256.clone(),
            },
            Some(row.conversation_id.as_str()),
            "attachment_upload",
            row.received_at,
        )
    }

    /// List every attachment for one conversation. Used by the
    /// python-sandbox plugin's hydration step to materialize the
    /// blobs into `/work/<convo>/uploads/` on first execute.
    ///
    /// Ordered by `received_at` ascending so the agent's mental
    /// model (uploads listed in arrival order) matches the chat
    /// scroll.
    pub fn list_for_conversation(
        &self,
        conversation_id: &ConversationId,
    ) -> Result<Vec<AttachmentRow>, DbError> {
        let cid = conversation_id.as_str().to_owned();
        self.db.with_conn(|c| {
            let mut stmt = c.prepare(
                "SELECT id, conversation_id, mime_type, path, sha256, received_at, filename \
                 FROM state_attachments \
                 WHERE conversation_id = ?1 \
                 ORDER BY received_at ASC",
            )?;
            let rows = stmt.query_map(params![cid], |r| {
                Ok(AttachmentRow {
                    id: AttachmentId::from(r.get::<_, String>(0)?),
                    conversation_id: ConversationId::from(r.get::<_, String>(1)?),
                    mime_type: r.get(2)?,
                    path: r.get(3)?,
                    sha256: r.get(4)?,
                    received_at: r.get(5)?,
                    filename: r.get::<_, Option<String>>(6)?,
                })
            })?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    /// Look up an attachment by id. Returns `None` if the row is
    /// missing — caller distinguishes "no such id" from "DB error"
    /// via Result + Option.
    pub fn get(&self, id: &AttachmentId) -> Result<Option<AttachmentRow>, DbError> {
        let id_owned = id.as_str().to_owned();
        self.db.with_conn(|c| {
            let row = c
                .query_row(
                    "SELECT id, conversation_id, mime_type, path, sha256, received_at, filename \
                     FROM state_attachments WHERE id = ?1",
                    params![id_owned],
                    |r| {
                        Ok(AttachmentRow {
                            id: AttachmentId::from(r.get::<_, String>(0)?),
                            conversation_id: ConversationId::from(r.get::<_, String>(1)?),
                            mime_type: r.get(2)?,
                            path: r.get(3)?,
                            sha256: r.get(4)?,
                            received_at: r.get(5)?,
                            filename: r.get::<_, Option<String>>(6)?,
                        })
                    },
                )
                .ok();
            Ok(row)
        })
    }

    /// Read an attachment while holding a reference lock against concurrent
    /// garbage collection. Returns `None` when its row was concurrently purged.
    pub fn read_attachment_bytes(&self, id: &AttachmentId) -> Result<Option<Vec<u8>>, DbError> {
        self.read_attachment_bytes_limited(id, usize::MAX)
    }

    /// Read an attachment only when its on-disk size is within the supplied bound.
    pub fn read_attachment_bytes_limited(
        &self,
        id: &AttachmentId,
        maximum_bytes: usize,
    ) -> Result<Option<Vec<u8>>, DbError> {
        let Some(row) = self.get(id)? else {
            return Ok(None);
        };
        let path = PathBuf::from(&row.path);
        let path_lock = artifact_path_lock(&path);
        let _path_guard = path_lock
            .read()
            .map_err(|_| DbError::Config("attachment path lock poisoned".into()))?;
        let Some(current) = self.get(id)? else {
            return Ok(None);
        };
        if current.path != row.path || current.sha256 != row.sha256 {
            return Ok(None);
        }
        if std::fs::metadata(&path)?.len() > maximum_bytes as u64 {
            return Err(DbError::Invariant(format!(
                "attachment exceeds the {maximum_bytes} byte read limit"
            )));
        }
        let bytes = std::fs::read(&path)?;
        if hex::encode(Sha256::digest(&bytes)) != row.sha256 {
            return Err(DbError::Invariant("attachment blob digest mismatch".into()));
        }
        Ok(Some(bytes))
    }

    /// Open an attachment under its reference lock. Once the OS file handle
    /// is open, POSIX keeps the inode readable after unlink and Windows keeps
    /// the delete blocked until the reader closes it.
    pub fn open_attachment_file(
        &self,
        id: &AttachmentId,
    ) -> Result<Option<std::fs::File>, DbError> {
        let Some(row) = self.get(id)? else {
            return Ok(None);
        };
        let path = PathBuf::from(&row.path);
        let path_lock = artifact_path_lock(&path);
        let _path_guard = path_lock
            .read()
            .map_err(|_| DbError::Config("attachment path lock poisoned".into()))?;
        let Some(current) = self.get(id)? else {
            return Ok(None);
        };
        if current.path != row.path || current.sha256 != row.sha256 {
            return Ok(None);
        }
        match std::fs::File::open(path) {
            Ok(file) => Ok(Some(file)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    /// Remove one attachment row and delete its file only when no other
    /// attachment or artifact row still references the same path.
    pub fn purge_attachment(&self, id: &AttachmentId) -> Result<bool, DbError> {
        let attachment_id = id.as_str().to_owned();
        let Some(before) = self.get(id)? else {
            return Ok(false);
        };
        let path_lock = artifact_path_lock(Path::new(&before.path));
        let _path_guard = path_lock
            .write()
            .map_err(|_| DbError::Config("attachment path lock poisoned".into()))?;
        let removed = self.db.transaction(|tx| {
            let path: Option<String> = tx
                .query_row(
                    "SELECT path FROM state_attachments WHERE id = ?1",
                    params![attachment_id],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(path) = path else {
                return Ok(None);
            };
            tx.execute(
                "DELETE FROM state_attachments WHERE id = ?1",
                params![attachment_id],
            )?;
            let references: i64 = tx.query_row(
                "SELECT (SELECT COUNT(*) FROM state_attachments WHERE path = ?1) + \
                        (SELECT COUNT(*) FROM state_artifacts WHERE path = ?1)",
                params![path],
                |row| row.get(0),
            )?;
            Ok(Some((path, references == 0)))
        })?;
        if let Some((path, no_references)) = removed {
            if no_references {
                // Missing files are already purged; other I/O failures are
                // returned so a durable caller can retry the cleanup.
                match std::fs::remove_file(&path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => {
                        return Err(DbError::Migration(format!(
                            "purge attachment file {}: {error}",
                            path
                        )));
                    }
                }
            }
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub fn insert_artifact(&self, row: &ArtifactRow) -> Result<(), DbError> {
        let path_lock = artifact_path_lock(Path::new(&row.path));
        let _path_guard = path_lock
            .read()
            .map_err(|_| DbError::Config("artifact path lock poisoned".into()))?;
        self.insert_artifact_metadata(row)?;
        self.ensure_artifact_label(row)
    }

    fn insert_artifact_metadata(&self, row: &ArtifactRow) -> Result<(), DbError> {
        self.db.with_conn(|c| {
            c.execute(
                "INSERT INTO state_artifacts(id, research_job_id, kind, mime_type, path, sha256, bytes, created_at, plugin_id, filename, expires_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    row.id,
                    row.research_job_id.as_ref().map(|r| r.as_str().to_owned()),
                    row.kind,
                    row.mime_type,
                    row.path,
                    row.sha256,
                    row.bytes,
                    row.created_at,
                    row.plugin_id,
                    row.filename,
                    row.expires_at,
                ],
            )?;
            Ok(())
        })
    }

    fn ensure_artifact_label(&self, row: &ArtifactRow) -> Result<(), DbError> {
        self.ensure_information_label(
            crate::information_store::InformationSubject {
                kind: "artifact".into(),
                id: row.id.clone(),
                sha256: row.sha256.clone(),
            },
            row.plugin_id.as_deref(),
            "artifact_output",
            row.created_at,
        )
    }

    /// Persist a large JSON tool result under a conversation/run-scoped ID.
    pub fn insert_tool_result_artifact(
        &self,
        artifacts_root: &Path,
        conversation_id: &ConversationId,
        run_id: &str,
        bytes: &[u8],
        now: i64,
    ) -> Result<PluginArtifactCreated, DbError> {
        self.insert_tool_result_artifact_with_id(
            artifacts_root,
            conversation_id,
            run_id,
            &uuid::Uuid::new_v4().to_string(),
            bytes,
            now,
        )
    }

    /// Persist a run-scoped result under a stable id. Retrying after a kill
    /// returns the same artifact only when its owner and bytes still match.
    ///
    /// ```ignore
    /// store.insert_tool_result_artifact_with_id(root, &conversation, run_id, "child-result", bytes, now)?;
    /// ```
    pub fn insert_tool_result_artifact_with_id(
        &self,
        artifacts_root: &Path,
        conversation_id: &ConversationId,
        run_id: &str,
        artifact_id: &str,
        bytes: &[u8],
        now: i64,
    ) -> Result<PluginArtifactCreated, DbError> {
        if bytes.len() > 50 * 1024 * 1024 {
            return Err(DbError::Invariant(
                "tool result exceeds the 50 MiB artifact limit".into(),
            ));
        }
        if artifact_id.is_empty()
            || artifact_id.len() > 128
            || !artifact_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        {
            return Err(DbError::Invariant(
                "tool result artifact id is invalid".into(),
            ));
        }
        let sha256 = hex::encode(Sha256::digest(bytes));
        let path = plugin_artifact_path(artifacts_root, &sha256);
        let path_lock = artifact_path_lock(&path);
        let _path_guard = path_lock
            .write()
            .map_err(|_| DbError::Config("artifact path lock poisoned".into()))?;
        publish_content_addressed(&path, artifacts_root, bytes, &sha256)?;
        let expires_at = now.saturating_add(30 * 24 * 60 * 60);
        self.db.transaction(|tx| {
            let inserted = tx.execute(
                "INSERT OR IGNORE INTO state_artifacts(id,research_job_id,kind,mime_type,path,sha256,bytes,created_at,plugin_id,filename,expires_at) \
                 VALUES (?1,NULL,'tool_result','application/json; charset=utf-8',?2,?3,?4,?5,NULL,'tool-result.json',?6)",
                params![artifact_id, path.to_string_lossy(), sha256, bytes.len() as i64, now, expires_at],
            )?;
            if inserted == 0 {
                let existing: (String, String, i64, String) = tx.query_row(
                    "SELECT path,sha256,bytes,kind FROM state_artifacts WHERE id=?1",
                    [artifact_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )?;
                if existing != (path.to_string_lossy().into_owned(), sha256.clone(), bytes.len() as i64, "tool_result".into()) {
                    return Err(DbError::Invariant("artifact id was reused with different bytes or kind".into()));
                }
            }
            let linked = tx.execute(
                "INSERT OR IGNORE INTO state_tool_result_artifacts(artifact_id,conversation_id,run_id,sha256,byte_length,mime_type,created_at,expires_at) \
                 VALUES (?1,?2,?3,?4,?5,'application/json; charset=utf-8',?6,?7)",
                params![artifact_id, conversation_id.as_str(), run_id, sha256, bytes.len() as i64, now, expires_at],
            )?;
            if linked == 0 {
                let existing: (String, String, String, i64) = tx.query_row(
                    "SELECT conversation_id,run_id,sha256,byte_length FROM state_tool_result_artifacts WHERE artifact_id=?1",
                    [artifact_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )?;
                if existing != (conversation_id.as_str().to_owned(), run_id.to_owned(), sha256.clone(), bytes.len() as i64) {
                    return Err(DbError::Invariant("artifact id belongs to another run or digest".into()));
                }
            }
            Ok(())
        })?;
        let subject = crate::information_store::InformationSubject {
            kind: "artifact".into(),
            id: artifact_id.to_owned(),
            sha256: sha256.clone(),
        };
        let labels = crate::information_store::InformationLabelStore::new(self.db);
        if labels.get(&subject)?.is_none() {
            if let Some((run_subject, _)) = labels.latest_for_identity("run", run_id)? {
                labels.transform(
                    &subject,
                    &[run_subject],
                    "child_run_result",
                    "host:child-run",
                    now,
                )?;
            } else {
                self.ensure_information_label(
                    subject.clone(),
                    Some(conversation_id.as_str()),
                    "child_run_result",
                    now,
                )?;
            }
        }
        Ok(PluginArtifactCreated {
            attachment_id: artifact_id.to_owned(),
            sha256,
            size_bytes: bytes.len() as u64,
        })
    }

    fn ensure_information_label(
        &self,
        subject: crate::information_store::InformationSubject,
        owner_id: Option<&str>,
        source_kind: &str,
        at: i64,
    ) -> Result<(), DbError> {
        let labels = crate::information_store::InformationLabelStore::new(self.db);
        if labels.get(&subject)?.is_some() {
            return Ok(());
        }
        let label = crate::information::InformationLabel::observed(
            crate::information::Sensitivity::Sensitive,
            owner_id.map(str::to_owned),
            "UnknownPending",
            source_kind,
            &subject.id,
            std::iter::empty(),
        );
        labels.observe(&subject, &label, "host:attachment-store", at)
    }

    /// Read at most 8 KiB from an unexpired artifact belonging to this run.
    pub fn read_tool_result_artifact(
        &self,
        artifacts_root: &Path,
        artifact_id: &str,
        conversation_id: &ConversationId,
        run_id: &str,
        offset: u64,
        limit: u32,
        now: i64,
    ) -> Result<Option<ToolResultArtifactChunk>, DbError> {
        let metadata = self.db.with_conn(|conn| {
            conn.query_row(
                "SELECT sha256,byte_length,mime_type FROM state_tool_result_artifacts \
             WHERE artifact_id=?1 AND conversation_id=?2 AND run_id=?3 AND expires_at>?4",
                params![artifact_id, conversation_id.as_str(), run_id, now],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, u64>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(DbError::from)
        })?;
        let Some((sha256, total_bytes, mime_type)) = metadata else {
            return Ok(None);
        };
        let path = plugin_artifact_path(artifacts_root, &sha256);
        let path_lock = artifact_path_lock(&path);
        let _path_guard = path_lock
            .read()
            .map_err(|_| DbError::Config("artifact path lock poisoned".into()))?;
        let still_authorized = self.db.with_conn(|conn| {
            Ok(conn.query_row(
                "SELECT COUNT(*) FROM state_tool_result_artifacts \
                 WHERE artifact_id=?1 AND conversation_id=?2 AND run_id=?3 AND sha256=?4 \
                   AND byte_length=?5 AND expires_at>?6",
                params![
                    artifact_id,
                    conversation_id.as_str(),
                    run_id,
                    sha256,
                    total_bytes,
                    now
                ],
                |row| row.get::<_, i64>(0),
            )? > 0)
        })?;
        if !still_authorized {
            return Ok(None);
        }
        let bytes = std::fs::read(&path).map_err(|error| {
            DbError::Migration(format!(
                "tool result artifact read {}: {error}",
                path.display()
            ))
        })?;
        if hex::encode(Sha256::digest(&bytes)) != sha256 || bytes.len() as u64 != total_bytes {
            return Err(DbError::Invariant(
                "tool result artifact integrity check failed".into(),
            ));
        }
        let text = std::str::from_utf8(&bytes).map_err(|error| {
            DbError::Invariant(format!("tool result artifact is not UTF-8 JSON: {error}"))
        })?;
        let mut start = usize::try_from(offset)
            .unwrap_or(usize::MAX)
            .min(text.len());
        while start < text.len() && !text.is_char_boundary(start) {
            start += 1;
        }
        let max_end = start
            .saturating_add(limit.clamp(1, 8192) as usize)
            .min(text.len());
        let mut end = max_end;
        while end > start && !text.is_char_boundary(end) {
            end -= 1;
        }
        Ok(Some(ToolResultArtifactChunk {
            artifact_id: artifact_id.to_owned(),
            sha256,
            mime_type,
            offset: start as u64,
            next_offset: end as u64,
            total_bytes,
            content: text[start..end].to_owned(),
        }))
    }

    /// Look up an artifact row by id. Returns `None` for missing rows;
    /// `Err` only for DB failures. Used by [`get_attachment_bytes_b64`]'s
    /// fallback path so plugin-generated artifacts share the read surface
    /// with inbound `state_attachments`.
    pub fn get_artifact(&self, id: &str) -> Result<Option<ArtifactRow>, DbError> {
        let id_owned = id.to_owned();
        self.db.with_conn(|c| {
            let row = c
                .query_row(
                    "SELECT id, research_job_id, kind, mime_type, path, sha256, bytes, created_at, \
                            plugin_id, filename, expires_at \
                       FROM state_artifacts WHERE id = ?1",
                    params![id_owned],
                    |r| {
                        Ok(ArtifactRow {
                            id: r.get::<_, String>(0)?,
                            research_job_id: r
                                .get::<_, Option<String>>(1)?
                                .map(ResearchJobId::from),
                            kind: r.get(2)?,
                            mime_type: r.get(3)?,
                            path: r.get(4)?,
                            sha256: r.get(5)?,
                            bytes: r.get(6)?,
                            created_at: r.get(7)?,
                            plugin_id: r.get(8)?,
                            filename: r.get(9)?,
                            expires_at: r.get(10)?,
                        })
                    },
                )
                .ok();
            Ok(row)
        })
    }

    /// Read an artifact while holding a reference lock against concurrent
    /// garbage collection. Callers must perform their normal owner/scope
    /// authorization before exposing the returned bytes.
    pub fn read_artifact_bytes(&self, id: &str) -> Result<Option<Vec<u8>>, DbError> {
        self.read_artifact_bytes_limited(id, usize::MAX)
    }

    /// Read an artifact only when its on-disk size is within the supplied bound.
    pub fn read_artifact_bytes_limited(
        &self,
        id: &str,
        maximum_bytes: usize,
    ) -> Result<Option<Vec<u8>>, DbError> {
        let Some(row) = self.get_artifact(id)? else {
            return Ok(None);
        };
        let path = PathBuf::from(&row.path);
        let path_lock = artifact_path_lock(&path);
        let _path_guard = path_lock
            .read()
            .map_err(|_| DbError::Config("artifact path lock poisoned".into()))?;
        let Some(current) = self.get_artifact(id)? else {
            return Ok(None);
        };
        if current.path != row.path || current.sha256 != row.sha256 {
            return Ok(None);
        }
        if std::fs::metadata(&path)?.len() > maximum_bytes as u64 {
            return Err(DbError::Invariant(format!(
                "artifact exceeds the {maximum_bytes} byte read limit"
            )));
        }
        let bytes = std::fs::read(&path)?;
        if hex::encode(Sha256::digest(&bytes)) != row.sha256 {
            return Err(DbError::Invariant("artifact blob digest mismatch".into()));
        }
        if current
            .bytes
            .is_some_and(|expected| expected >= 0 && expected as usize != bytes.len())
        {
            return Err(DbError::Invariant("artifact blob size mismatch".into()));
        }
        Ok(Some(bytes))
    }

    /// Open an artifact under its reference lock; see
    /// [`AttachmentStore::open_attachment_file`] for the lifetime contract.
    pub fn open_artifact_file(&self, id: &str) -> Result<Option<std::fs::File>, DbError> {
        let Some(row) = self.get_artifact(id)? else {
            return Ok(None);
        };
        let path = PathBuf::from(&row.path);
        let path_lock = artifact_path_lock(&path);
        let _path_guard = path_lock
            .read()
            .map_err(|_| DbError::Config("artifact path lock poisoned".into()))?;
        let Some(current) = self.get_artifact(id)? else {
            return Ok(None);
        };
        if current.path != row.path || current.sha256 != row.sha256 {
            return Ok(None);
        }
        match std::fs::File::open(path) {
            Ok(file) => Ok(Some(file)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    /// Write a plugin-rendered artifact: hash the bytes, store them on
    /// disk under `artifacts_root/<sha256>` (content-addressed; idempotent
    /// for identical bytes), and insert a `state_artifacts` row with
    /// `kind = "plugin_artifact"`. Returns the new attachment id + size +
    /// sha so the caller can echo it back to the agent.
    ///
    /// The artifact id is a fresh UUID. Two artifacts with identical
    /// bytes get distinct ids but share one on-disk file — counted by the
    /// row's `path` field rather than `state_blobs.refcount` (artifacts
    /// don't participate in the blob refcount system today).
    pub fn insert_plugin_artifact(
        &self,
        artifacts_root: &Path,
        plugin_id: &str,
        filename: &str,
        mime_type: &str,
        bytes: &[u8],
        ttl_seconds: Option<i64>,
        now: i64,
    ) -> Result<PluginArtifactCreated, DbError> {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        let sha = format!("{:x}", hasher.finalize());
        let path = plugin_artifact_path(artifacts_root, &sha);
        let path_lock = artifact_path_lock(&path);
        let _path_guard = path_lock
            .write()
            .map_err(|_| DbError::Config("artifact path lock poisoned".into()))?;
        publish_content_addressed(&path, artifacts_root, bytes, &sha)?;

        let attachment_id = uuid::Uuid::new_v4().to_string();
        let expires_at = ttl_seconds.map(|t| now + t);
        let row = ArtifactRow {
            id: attachment_id.clone(),
            research_job_id: None,
            kind: "plugin_artifact".into(),
            mime_type: mime_type.to_owned(),
            path: path.to_string_lossy().into_owned(),
            sha256: sha.clone(),
            bytes: Some(bytes.len() as i64),
            created_at: now,
            plugin_id: Some(plugin_id.to_owned()),
            filename: Some(filename.to_owned()),
            expires_at,
        };
        self.insert_artifact_metadata(&row)?;
        self.ensure_artifact_label(&row)?;
        Ok(PluginArtifactCreated {
            attachment_id,
            sha256: sha,
            size_bytes: bytes.len() as u64,
        })
    }

    /// Streaming variant of [`AttachmentStore::insert_plugin_artifact`].
    ///
    /// Reads `source` in 64 KB chunks, hashing incrementally and writing
    /// straight to a temp file under `artifacts_root`. On EOF the temp
    /// is renamed to `<artifacts_root>/<sha256>` atomically; if a blob
    /// with that hash already exists (content-addressed dedup hit), the
    /// temp is deleted instead.
    ///
    /// Why a separate method instead of slurping the file and calling
    /// the bytes-based variant: a 100 MB Parquet artifact would otherwise
    /// allocate 100 MB on the heap and double-buffer it through Vec<u8>
    /// before the SHA-256 even starts. The streaming path keeps memory
    /// bounded to ~64 KB regardless of file size — important for the
    /// python_sandbox output watcher (Phase 4) which publishes whatever
    /// the agent's cell writes, with our 50 MB output ceiling.
    ///
    /// The hash + on-disk bytes are byte-identical to what
    /// `insert_plugin_artifact(file_contents)` would produce — verified
    /// by the test `streaming_and_bytes_paths_agree_on_sha`.
    pub fn insert_plugin_artifact_from_path(
        &self,
        artifacts_root: &Path,
        plugin_id: &str,
        filename: &str,
        mime_type: &str,
        source: &Path,
        ttl_seconds: Option<i64>,
        now: i64,
    ) -> Result<PluginArtifactCreated, DbError> {
        // Create the destination root before we open the source so we
        // fail fast if the operator's data dir is misconfigured.
        std::fs::create_dir_all(artifacts_root).map_err(|e| {
            DbError::Migration(format!(
                "plugin artifact from path: create_dir_all {}: {e}",
                artifacts_root.display()
            ))
        })?;

        let mut src = std::fs::File::open(source).map_err(|e| {
            DbError::Migration(format!(
                "plugin artifact from path: open source {}: {e}",
                source.display()
            ))
        })?;

        // Temp filename includes a UUID so two concurrent publishes
        // of the same conversation's outputs don't race on the same
        // temp. RAII-cleaned on every error path via the explicit
        // remove_file calls below.
        let tmp_path = artifacts_root.join(format!(".tmp-{}", uuid::Uuid::new_v4()));
        let tmp_lock = artifact_path_lock(&tmp_path);
        let _tmp_guard = tmp_lock
            .write()
            .map_err(|_| DbError::Config("artifact temp path lock poisoned".into()))?;
        let mut tmp = std::fs::File::create(&tmp_path).map_err(|e| {
            DbError::Migration(format!(
                "plugin artifact from path: create temp {}: {e}",
                tmp_path.display()
            ))
        })?;

        let mut hasher = Sha256::new();
        let mut total: u64 = 0;
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = match src.read(&mut buf) {
                Ok(n) => n,
                Err(e) => {
                    let _ = std::fs::remove_file(&tmp_path);
                    return Err(DbError::Migration(format!(
                        "plugin artifact from path: read {}: {e}",
                        source.display()
                    )));
                }
            };
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            if let Err(e) = tmp.write_all(&buf[..n]) {
                let _ = std::fs::remove_file(&tmp_path);
                return Err(DbError::Migration(format!(
                    "plugin artifact from path: write temp {}: {e}",
                    tmp_path.display()
                )));
            }
            total += n as u64;
        }
        if let Err(e) = tmp.flush() {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(DbError::Migration(format!(
                "plugin artifact from path: flush temp {}: {e}",
                tmp_path.display()
            )));
        }
        if let Err(e) = tmp.sync_all() {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(DbError::Migration(format!(
                "plugin artifact from path: sync temp {}: {e}",
                tmp_path.display()
            )));
        }
        drop(tmp); // close before rename — required on Windows

        let sha = format!("{:x}", hasher.finalize());
        let final_path = plugin_artifact_path(artifacts_root, &sha);
        let path_lock = artifact_path_lock(&final_path);
        let _path_guard = path_lock
            .write()
            .map_err(|_| DbError::Config("artifact path lock poisoned".into()))?;
        if final_path.exists() {
            // Content-addressed dedup: an artifact with these exact
            // bytes is already on disk. Drop the temp; the existing
            // file is what we want.
            verify_content_addressed(&final_path, &sha, total)?;
            let _ = std::fs::remove_file(&tmp_path);
        } else {
            std::fs::rename(&tmp_path, &final_path).map_err(|e| {
                // Cross-filesystem rename can fail with EXDEV; fall
                // back to copy+remove. Rare in practice — the
                // artifacts_root is on the same FS as the temp dir.
                let _ = std::fs::remove_file(&tmp_path);
                DbError::Migration(format!(
                    "plugin artifact from path: rename {} -> {}: {e}",
                    tmp_path.display(),
                    final_path.display()
                ))
            })?;
            if let Ok(directory) = std::fs::File::open(artifacts_root) {
                let _ = directory.sync_all();
            }
        }

        let attachment_id = uuid::Uuid::new_v4().to_string();
        let expires_at = ttl_seconds.map(|t| now + t);
        let row = ArtifactRow {
            id: attachment_id.clone(),
            research_job_id: None,
            kind: "plugin_artifact".into(),
            mime_type: mime_type.to_owned(),
            path: final_path.to_string_lossy().into_owned(),
            sha256: sha.clone(),
            bytes: Some(total as i64),
            created_at: now,
            plugin_id: Some(plugin_id.to_owned()),
            filename: Some(filename.to_owned()),
            expires_at,
        };
        self.insert_artifact_metadata(&row)?;
        self.ensure_artifact_label(&row)?;
        Ok(PluginArtifactCreated {
            attachment_id,
            sha256: sha,
            size_bytes: total,
        })
    }

    /// Purge every artifact owned by `plugin_id` — both the DB row and,
    /// when no other row still references the same on-disk path
    /// (content-addressed dedupe), the file itself.
    ///
    /// Used by the plugin lifecycle's `purge` path (SPA uninstall +
    /// factory reset) to make "remove this plugin" a true clean-slate
    /// operation. Mirrors `sweep_expired_plugin_artifacts` but scoped by
    /// `plugin_id` instead of `expires_at`; the dedupe-aware
    /// "only delete the blob when refcount hits zero" logic is the same.
    ///
    /// Returns the number of `state_artifacts` rows removed. A plugin
    /// that never minted any artifacts is a no-op `Ok(0)`. Idempotent —
    /// calling twice for the same `plugin_id` returns 0 on the second
    /// call.
    pub fn purge_artifacts_for_plugin(&self, plugin_id: &str) -> Result<usize, DbError> {
        let rows: Vec<(String, String)> = self.db.with_conn(|c| {
            let mut stmt =
                c.prepare("SELECT id, path FROM state_artifacts WHERE plugin_id = ?1")?;
            let iter = stmt.query_map(params![plugin_id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?;
            let mut out = Vec::new();
            for row in iter {
                out.push(row?);
            }
            Ok(out)
        })?;
        let mut removed = 0usize;
        for (id, path) in rows {
            let path_lock = artifact_path_lock(Path::new(&path));
            let _path_guard = path_lock
                .write()
                .map_err(|_| DbError::Config("artifact path lock poisoned".into()))?;
            let id_for_delete = id.clone();
            self.db.with_conn(|c| {
                c.execute(
                    "DELETE FROM state_artifacts WHERE id = ?1",
                    params![id_for_delete],
                )?;
                Ok(())
            })?;
            // Refcount-aware blob delete: only unlink the on-disk file
            // when no surviving `state_artifacts` row points at the
            // same `path`. Two plugins emitting identical chart bytes
            // share one blob; uninstalling one must not break the
            // other.
            let still_used: i64 = self.db.with_conn(|c| {
                Ok(c.query_row(
                    "SELECT (SELECT COUNT(*) FROM state_artifacts WHERE path = ?1) + \
                            (SELECT COUNT(*) FROM state_attachments WHERE path = ?1)",
                    params![path.clone()],
                    |r| r.get(0),
                )?)
            })?;
            if still_used == 0 {
                // Best-effort delete — missing file is fine (already
                // GC'd, never written due to dedupe race, etc).
                match std::fs::remove_file(&path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
            }
            removed += 1;
        }
        Ok(removed)
    }

    /// Sweep expired plugin artifacts. Removes rows whose `expires_at`
    /// is in the past (relative to `now`) AND deletes the on-disk file
    /// for each — but only when no OTHER row still references the same
    /// sha (since artifacts are content-addressed and could be shared
    /// across plugins or identical re-renders).
    ///
    /// Returns the number of rows removed. Idempotent: a second call
    /// with the same `now` is a no-op.
    pub fn sweep_expired_plugin_artifacts(&self, now: i64) -> Result<usize, DbError> {
        // Two-step so we can release on-disk bytes safely.
        let expired: Vec<(String, String)> = self.db.with_conn(|c| {
            let mut stmt = c.prepare(
                "SELECT id, path FROM state_artifacts \
                 WHERE expires_at IS NOT NULL AND expires_at <= ?1",
            )?;
            let rows = stmt.query_map(params![now], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })?;
        let mut removed = 0usize;
        for (id, path) in expired {
            let path_lock = artifact_path_lock(Path::new(&path));
            let _path_guard = path_lock
                .write()
                .map_err(|_| DbError::Config("artifact path lock poisoned".into()))?;
            let id_for_delete = id.clone();
            self.db.with_conn(|c| {
                c.execute(
                    "DELETE FROM state_artifacts WHERE id = ?1",
                    params![id_for_delete],
                )?;
                Ok(())
            })?;
            // Only delete on-disk bytes when no other row points at the
            // same path. This is the content-addressed dedupe — two
            // identical chart renders share one file.
            let still_used: i64 = self.db.with_conn(|c| {
                Ok(c.query_row(
                    "SELECT (SELECT COUNT(*) FROM state_artifacts WHERE path = ?1) + \
                            (SELECT COUNT(*) FROM state_attachments WHERE path = ?1)",
                    params![path.clone()],
                    |r| r.get(0),
                )?)
            })?;
            if still_used == 0 {
                // Best-effort delete — a missing file is fine (manually
                // cleaned up, never written due to dedupe race, etc).
                match std::fs::remove_file(&path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
            }
            removed += 1;
        }
        Ok(removed)
    }

    /// Remove aged orphan blobs and interrupted-write temp files.
    ///
    /// Only regular files whose names match the content-addressed SHA-256
    /// format or this module's temporary naming scheme are considered. A
    /// live DB reference in either attachment or artifact metadata protects
    /// the blob. Active readers, publishers, and removers coordinate through
    /// a per-path lock; the grace period also recovers files left by process
    /// termination between atomic rename and SQLite commit.
    pub fn sweep_orphan_artifact_blobs(
        &self,
        artifacts_root: &Path,
        now: i64,
        minimum_age_seconds: i64,
    ) -> Result<usize, DbError> {
        let entries = match std::fs::read_dir(artifacts_root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(error) => return Err(error.into()),
        };
        let mut removed = 0usize;
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            let metadata = std::fs::symlink_metadata(&path)?;
            if !metadata.file_type().is_file()
                || !(is_sha256_filename(&name)
                    || (name.starts_with('.')
                        && (name.ends_with(".tmp") || name.starts_with(".tmp-"))))
            {
                continue;
            }
            let Some(modified_at) = metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|duration| duration.as_secs().min(i64::MAX as u64) as i64)
            else {
                continue;
            };
            if now.saturating_sub(modified_at) < minimum_age_seconds.max(0) {
                continue;
            }

            let path_lock = artifact_path_lock(&path);
            let _path_guard = path_lock
                .write()
                .map_err(|_| DbError::Config("artifact path lock poisoned".into()))?;
            if is_sha256_filename(&name) {
                let referenced: i64 = self.db.with_conn(|connection| {
                    Ok(connection.query_row(
                        "SELECT (SELECT COUNT(*) FROM state_artifacts WHERE path = ?1) + \
                                (SELECT COUNT(*) FROM state_attachments WHERE path = ?1)",
                        [path.to_string_lossy().as_ref()],
                        |row| row.get(0),
                    )?)
                })?;
                if referenced != 0 {
                    continue;
                }
            }
            match std::fs::remove_file(&path) {
                Ok(()) => removed += 1,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(removed)
    }
}

fn is_sha256_filename(name: &str) -> bool {
    name.len() == 64 && name.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{Database, DbConfig};
    use crate::ids::EventSeq;
    use crate::migrations::MigrationRunner;
    use crate::runs::{NewRun, RunStore};

    #[test]
    fn orphan_collection_preserves_references_and_recovers_crash_files() {
        let db = Database::open(&DbConfig::in_memory_unencrypted()).unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("artifacts");
        std::fs::create_dir_all(&root).unwrap();
        let store = AttachmentStore::new(&db);
        let live_bytes = b"referenced artifact";
        let live_sha = hex::encode(Sha256::digest(live_bytes));
        let live_path = plugin_artifact_path(&root, &live_sha);
        publish_content_addressed(&live_path, &root, live_bytes, &live_sha).unwrap();
        store
            .insert_artifact(&ArtifactRow {
                id: "live-artifact".into(),
                research_job_id: None,
                kind: "other".into(),
                mime_type: "application/octet-stream".into(),
                path: live_path.to_string_lossy().into_owned(),
                sha256: live_sha,
                bytes: Some(live_bytes.len() as i64),
                created_at: 1,
                plugin_id: None,
                filename: None,
                expires_at: None,
            })
            .unwrap();

        let orphan_bytes = b"renamed before crash";
        let orphan_sha = hex::encode(Sha256::digest(orphan_bytes));
        let orphan_path = plugin_artifact_path(&root, &orphan_sha);
        std::fs::write(&orphan_path, orphan_bytes).unwrap();
        let interrupted_temp = root.join(format!(".{orphan_sha}.interrupted.tmp"));
        std::fs::write(&interrupted_temp, b"partial write").unwrap();

        let now = chrono::Utc::now().timestamp().saturating_add(1);
        assert_eq!(store.sweep_orphan_artifact_blobs(&root, now, 0).unwrap(), 2);
        assert!(
            live_path.is_file(),
            "a referenced artifact must survive collection"
        );
        assert!(
            !orphan_path.exists(),
            "a rename-before-commit orphan must be collected"
        );
        assert!(
            !interrupted_temp.exists(),
            "an interrupted temp write must be collected"
        );
        assert_eq!(
            store
                .read_artifact_bytes("live-artifact")
                .unwrap()
                .unwrap()
                .as_slice(),
            live_bytes
        );
    }

    #[test]
    fn artifact_read_lock_keeps_a_concurrent_collector_out_until_file_open() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("reader-lock-probe");
        std::fs::write(&path, b"readable").unwrap();
        let lock = artifact_path_lock(&path);
        let reader = lock.read().unwrap();
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let collector_lock = Arc::clone(&lock);
        let collector = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            let _guard = collector_lock.write().unwrap();
            done_tx.send(()).unwrap();
        });
        started_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .unwrap();
        assert!(
            done_rx
                .recv_timeout(std::time::Duration::from_millis(20))
                .is_err()
        );
        drop(reader);
        done_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .unwrap();
        collector.join().unwrap();
    }

    #[test]
    fn stable_tool_result_artifact_reuses_only_same_run_and_digest() {
        let db = Database::open(&DbConfig::in_memory_unencrypted()).unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        db.with_conn(|connection| {
            connection.execute(
                "INSERT INTO state_conversations(conversation_id,kind,phase,trust_class,modality) \
                 VALUES('artifact-scope','ControllerDM','idle','Controller','Text')",
                [],
            )?;
            connection.execute(
                "INSERT INTO state_events(conversation_id,seq,kind,payload,committed_at,actor) \
                 VALUES('artifact-scope',1,'user_msg',X'00',1,'controller')",
                [],
            )?;
            Ok(())
        })
        .unwrap();
        let conversation = ConversationId::from("artifact-scope");
        let runs = RunStore::new(&db);
        let first = runs
            .create_run(&NewRun {
                conversation_id: conversation.clone(),
                parent_run_id: None,
                input_event_seq: EventSeq(1),
                started_at: 1,
                deadline_at: None,
            })
            .unwrap();
        let second = runs
            .create_run(&NewRun {
                conversation_id: conversation.clone(),
                parent_run_id: None,
                input_event_seq: EventSeq(1),
                started_at: 2,
                deadline_at: None,
            })
            .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let store = AttachmentStore::new(&db);
        let a = store
            .insert_tool_result_artifact_with_id(
                directory.path(),
                &conversation,
                &first,
                "child-result-1",
                b"{}",
                10,
            )
            .unwrap();
        let b = store
            .insert_tool_result_artifact_with_id(
                directory.path(),
                &conversation,
                &first,
                "child-result-1",
                b"{}",
                11,
            )
            .unwrap();
        assert_eq!(a.attachment_id, b.attachment_id);
        assert!(
            store
                .read_tool_result_artifact(
                    directory.path(),
                    &a.attachment_id,
                    &ConversationId::from("another-conversation"),
                    &first,
                    0,
                    100,
                    12,
                )
                .unwrap()
                .is_none()
        );
        let label = crate::information_store::InformationLabelStore::new(&db)
            .get(&crate::information_store::InformationSubject {
                kind: "artifact".into(),
                id: a.attachment_id.clone(),
                sha256: a.sha256.clone(),
            })
            .unwrap()
            .unwrap();
        assert_eq!(
            label.sensitivity,
            crate::information::Sensitivity::Sensitive
        );
        assert_eq!(label.provenance[0].source_kind, "run_input");
        assert_eq!(
            label.provenance[0].transformation.as_deref(),
            Some("child_run_result")
        );
        assert!(
            store
                .insert_tool_result_artifact_with_id(
                    directory.path(),
                    &conversation,
                    &second,
                    "child-result-1",
                    b"{}",
                    12,
                )
                .is_err()
        );
        assert!(
            store
                .insert_tool_result_artifact_with_id(
                    directory.path(),
                    &conversation,
                    &first,
                    "child-result-1",
                    b"{\"changed\":true}",
                    12,
                )
                .is_err()
        );
        let count: i64 = db.with_conn(|connection| {
            Ok(connection.query_row(
                "SELECT COUNT(*) FROM state_tool_result_artifacts WHERE artifact_id='child-result-1'",
                [], |row| row.get(0),
            )?)
        }).unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn attachment_and_artifact_insert() {
        let db = Database::open(&DbConfig::in_memory_unencrypted()).unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        let store = AttachmentStore::new(&db);

        store
            .insert(&AttachmentRow {
                id: AttachmentId::new(),
                conversation_id: ConversationId::from("c"),
                mime_type: "image/jpeg".into(),
                path: "/tmp/x.jpg".into(),
                sha256: "a".repeat(64),
                received_at: 1,
                filename: None,
            })
            .unwrap();

        store
            .insert_artifact(&ArtifactRow {
                id: "art1".into(),
                research_job_id: Some(ResearchJobId::from("rj-1")),
                kind: "research_pdf".into(),
                mime_type: "application/pdf".into(),
                path: "/tmp/r.pdf".into(),
                sha256: "b".repeat(64),
                bytes: Some(12345),
                created_at: 2,
                plugin_id: None,
                filename: None,
                expires_at: None,
            })
            .unwrap();
    }

    /// Plugin-artifact round-trip: insert bytes, look up the row, verify
    /// the file on disk matches. Covers A1's happy path.
    #[test]
    fn plugin_artifact_round_trip_writes_disk_and_reads_back() {
        let db = Database::open(&DbConfig::in_memory_unencrypted()).unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        let store = AttachmentStore::new(&db);
        let tmp = tempfile::tempdir().unwrap();

        let bytes = b"\x89PNG\r\n\x1a\nfake-chart-bytes";
        let created = store
            .insert_plugin_artifact(
                tmp.path(),
                "open-meteo",
                "forecast.png",
                "image/png",
                bytes,
                Some(3600),
                1_700_000_000,
            )
            .unwrap();
        assert_eq!(created.size_bytes, bytes.len() as u64);
        assert_eq!(created.sha256.len(), 64);

        let row = store.get_artifact(&created.attachment_id).unwrap().unwrap();
        assert_eq!(row.kind, "plugin_artifact");
        assert_eq!(row.plugin_id.as_deref(), Some("open-meteo"));
        assert_eq!(row.filename.as_deref(), Some("forecast.png"));
        assert_eq!(row.mime_type, "image/png");
        assert_eq!(row.expires_at, Some(1_700_000_000 + 3600));

        let on_disk = std::fs::read(&row.path).unwrap();
        assert_eq!(on_disk, bytes);
    }

    #[test]
    fn list_for_conversation_returns_only_that_convo_in_received_order() {
        let db = Database::open(&DbConfig::in_memory_unencrypted()).unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        let store = AttachmentStore::new(&db);

        // Two convos, three rows each. Insert in mixed order, assert
        // we get only one convo's rows back AND in received_at order.
        let a = ConversationId::from("convo-a");
        let b = ConversationId::from("convo-b");

        for (cid, name, received_at) in [
            (&a, "a1.csv", 100),
            (&b, "b1.csv", 200),
            (&a, "a2.csv", 50),
            (&b, "b2.csv", 250),
            (&a, "a3.csv", 150),
        ] {
            store
                .insert(&AttachmentRow {
                    id: AttachmentId::new(),
                    conversation_id: cid.clone(),
                    mime_type: "text/csv".into(),
                    path: format!("/blobs/{name}"),
                    sha256: hex::encode(Sha256::digest(name.as_bytes())),
                    received_at,
                    filename: Some(name.into()),
                })
                .unwrap();
        }

        let a_rows = store.list_for_conversation(&a).unwrap();
        assert_eq!(a_rows.len(), 3);
        // Ordered ascending by received_at: 50, 100, 150 → a2, a1, a3
        assert_eq!(a_rows[0].filename.as_deref(), Some("a2.csv"));
        assert_eq!(a_rows[1].filename.as_deref(), Some("a1.csv"));
        assert_eq!(a_rows[2].filename.as_deref(), Some("a3.csv"));

        let b_rows = store.list_for_conversation(&b).unwrap();
        assert_eq!(b_rows.len(), 2);
        assert_eq!(b_rows[0].filename.as_deref(), Some("b1.csv"));
        assert_eq!(b_rows[1].filename.as_deref(), Some("b2.csv"));

        // Cross-isolation: convo-c has no rows.
        let c_rows = store
            .list_for_conversation(&ConversationId::from("convo-c"))
            .unwrap();
        assert!(c_rows.is_empty());
    }

    /// Phase 6 — `state_attachments.filename` round-trips through
    /// insert + get. None → None on the way out; Some("foo.csv") →
    /// Some("foo.csv"). The column is the hydration layer's source
    /// of truth for the operator-facing filename on inbound files.
    #[test]
    fn attachment_row_round_trips_filename() {
        let db = Database::open(&DbConfig::in_memory_unencrypted()).unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        let store = AttachmentStore::new(&db);

        // With filename
        let id_with = AttachmentId::new();
        store
            .insert(&AttachmentRow {
                id: id_with.clone(),
                conversation_id: ConversationId::from("c1"),
                mime_type: "text/csv".into(),
                path: "/tmp/sales.csv".into(),
                sha256: "deadbeef".repeat(8),
                received_at: 100,
                filename: Some("sales.csv".into()),
            })
            .unwrap();
        let got = store.get(&id_with).unwrap().unwrap();
        assert_eq!(got.filename.as_deref(), Some("sales.csv"));

        // Without filename (legacy / not-yet-wired ingest paths)
        let id_without = AttachmentId::new();
        store
            .insert(&AttachmentRow {
                id: id_without.clone(),
                conversation_id: ConversationId::from("c1"),
                mime_type: "text/csv".into(),
                path: "/tmp/other.csv".into(),
                sha256: "cafebabe".repeat(8),
                received_at: 200,
                filename: None,
            })
            .unwrap();
        let got = store.get(&id_without).unwrap().unwrap();
        assert_eq!(got.filename, None);
    }

    /// Streaming variant produces a row whose on-disk file matches
    /// the source bytes — the same end state as the bytes-based
    /// variant, just without the in-memory copy.
    #[test]
    fn plugin_artifact_from_path_writes_disk_and_reads_back() {
        let db = Database::open(&DbConfig::in_memory_unencrypted()).unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        let store = AttachmentStore::new(&db);
        let tmp = tempfile::tempdir().unwrap();

        let source = tmp.path().join("region_summary.csv");
        let content = b"region,total\nWest,4200000\nEast,3100000\n";
        std::fs::write(&source, content).unwrap();

        let artifacts_root = tmp.path().join("artifacts");
        let created = store
            .insert_plugin_artifact_from_path(
                &artifacts_root,
                "python-sandbox",
                "region_summary.csv",
                "text/csv",
                &source,
                None,
                1_700_000_000,
            )
            .unwrap();
        assert_eq!(created.size_bytes as usize, content.len());
        assert_eq!(created.sha256.len(), 64);

        let row = store.get_artifact(&created.attachment_id).unwrap().unwrap();
        assert_eq!(row.plugin_id.as_deref(), Some("python-sandbox"));
        assert_eq!(row.filename.as_deref(), Some("region_summary.csv"));
        assert_eq!(row.mime_type, "text/csv");
        assert_eq!(row.bytes, Some(content.len() as i64));
        assert_eq!(row.expires_at, None);
        assert_eq!(std::fs::read(&row.path).unwrap(), content);
    }

    /// Streaming and bytes variants MUST produce the same on-disk
    /// state for identical source content. This is the dedup
    /// invariant that lets the Phase 4 watcher safely publish
    /// already-on-disk files without worrying about a parallel
    /// bytes-based caller producing a different blob.
    #[test]
    fn streaming_and_bytes_paths_agree_on_sha() {
        let db = Database::open(&DbConfig::in_memory_unencrypted()).unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        let store = AttachmentStore::new(&db);
        let tmp = tempfile::tempdir().unwrap();

        // 256 KB of well-mixed bytes — covers several 64 KB read
        // iterations in the streaming path. If anything in the
        // chunking math was off, the hashes would diverge.
        let bytes: Vec<u8> = (0..256u32 * 1024)
            .map(|i| (i as u8).wrapping_mul(31))
            .collect();
        let source = tmp.path().join("data.bin");
        std::fs::write(&source, &bytes).unwrap();

        let artifacts_root = tmp.path().join("artifacts");

        let a = store
            .insert_plugin_artifact(
                &artifacts_root,
                "python-sandbox",
                "data.bin",
                "application/octet-stream",
                &bytes,
                None,
                1,
            )
            .unwrap();
        let b = store
            .insert_plugin_artifact_from_path(
                &artifacts_root,
                "python-sandbox",
                "data.bin",
                "application/octet-stream",
                &source,
                None,
                2,
            )
            .unwrap();
        assert_eq!(a.sha256, b.sha256, "sha must match across paths");
        assert_eq!(a.size_bytes, b.size_bytes);
        // Distinct row ids but pointing at the same dedup'd blob.
        assert_ne!(a.attachment_id, b.attachment_id);
        let row_a = store.get_artifact(&a.attachment_id).unwrap().unwrap();
        let row_b = store.get_artifact(&b.attachment_id).unwrap().unwrap();
        assert_eq!(row_a.path, row_b.path, "both rows point at one blob");
    }

    /// Streaming a source that's already a content-addressed blob
    /// in the same artifacts_root must NOT clobber the existing
    /// file. Critical for the python_sandbox flow where the
    /// kernel writes to /work/outputs/, hydration may have stored
    /// the same bytes earlier, and our publish dedups.
    #[test]
    fn plugin_artifact_from_path_dedups_when_blob_exists() {
        let db = Database::open(&DbConfig::in_memory_unencrypted()).unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        let store = AttachmentStore::new(&db);
        let tmp = tempfile::tempdir().unwrap();
        let artifacts_root = tmp.path().join("artifacts");

        let bytes = b"identical-content-published-twice";

        // First publish via streaming path.
        let source1 = tmp.path().join("v1.csv");
        std::fs::write(&source1, bytes).unwrap();
        let a = store
            .insert_plugin_artifact_from_path(
                &artifacts_root,
                "python-sandbox",
                "v1.csv",
                "text/csv",
                &source1,
                None,
                1,
            )
            .unwrap();
        let blob_path =
            std::path::PathBuf::from(&store.get_artifact(&a.attachment_id).unwrap().unwrap().path);
        let blob_mtime_before = std::fs::metadata(&blob_path).unwrap().modified().unwrap();

        // Sleep enough for mtime to change if a clobber happened.
        std::thread::sleep(std::time::Duration::from_millis(50));

        // Second publish — identical content, different source path.
        let source2 = tmp.path().join("v2.csv");
        std::fs::write(&source2, bytes).unwrap();
        let b = store
            .insert_plugin_artifact_from_path(
                &artifacts_root,
                "python-sandbox",
                "v2.csv",
                "text/csv",
                &source2,
                None,
                2,
            )
            .unwrap();
        assert_eq!(a.sha256, b.sha256);

        let blob_mtime_after = std::fs::metadata(&blob_path).unwrap().modified().unwrap();
        assert_eq!(
            blob_mtime_before, blob_mtime_after,
            "dedup hit MUST NOT rewrite the existing blob on disk"
        );

        // No stray temp files left in artifacts_root.
        let strays: Vec<_> = std::fs::read_dir(&artifacts_root)
            .unwrap()
            .filter_map(|e| {
                let name = e.unwrap().file_name().to_string_lossy().into_owned();
                if name.starts_with(".tmp-") {
                    Some(name)
                } else {
                    None
                }
            })
            .collect();
        assert!(
            strays.is_empty(),
            "temp files must be cleaned up on dedup; found {strays:?}"
        );
    }

    /// Source file missing → returns an error rather than panicking
    /// or inserting a zero-byte artifact.
    #[test]
    fn plugin_artifact_from_path_errors_on_missing_source() {
        let db = Database::open(&DbConfig::in_memory_unencrypted()).unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        let store = AttachmentStore::new(&db);
        let tmp = tempfile::tempdir().unwrap();

        let err = store
            .insert_plugin_artifact_from_path(
                &tmp.path().join("artifacts"),
                "python-sandbox",
                "nope.csv",
                "text/csv",
                &tmp.path().join("does-not-exist.csv"),
                None,
                1,
            )
            .expect_err("must error on missing source");
        let msg = format!("{err}");
        assert!(
            msg.contains("does-not-exist.csv") || msg.contains("open source"),
            "error should mention the missing source: {msg}"
        );
    }

    /// Two artifacts with identical bytes share the on-disk file
    /// (content-addressed) but get distinct row ids.
    #[test]
    fn plugin_artifact_dedupes_bytes_across_rows() {
        let db = Database::open(&DbConfig::in_memory_unencrypted()).unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        let store = AttachmentStore::new(&db);
        let tmp = tempfile::tempdir().unwrap();

        let bytes = b"identical-bytes-content";
        let a = store
            .insert_plugin_artifact(
                tmp.path(),
                "open-meteo",
                "chart.png",
                "image/png",
                bytes,
                None,
                1,
            )
            .unwrap();
        let b = store
            .insert_plugin_artifact(
                tmp.path(),
                "open-meteo",
                "chart-2.png",
                "image/png",
                bytes,
                None,
                2,
            )
            .unwrap();
        assert_ne!(a.attachment_id, b.attachment_id, "ids must be distinct");
        assert_eq!(a.sha256, b.sha256, "sha must match for identical bytes");
        let row_a = store.get_artifact(&a.attachment_id).unwrap().unwrap();
        let row_b = store.get_artifact(&b.attachment_id).unwrap().unwrap();
        assert_eq!(row_a.path, row_b.path, "both rows point at the same blob");
    }

    /// TTL sweeper removes expired rows and frees the on-disk bytes
    /// when no other row references them. Non-expired rows survive.
    #[test]
    fn sweep_expired_plugin_artifacts_removes_only_past_ttl_rows() {
        let db = Database::open(&DbConfig::in_memory_unencrypted()).unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        let store = AttachmentStore::new(&db);
        let tmp = tempfile::tempdir().unwrap();

        let expired = store
            .insert_plugin_artifact(
                tmp.path(),
                "open-meteo",
                "old.png",
                "image/png",
                b"old-bytes",
                Some(10),
                1_700_000_000,
            )
            .unwrap();
        let fresh = store
            .insert_plugin_artifact(
                tmp.path(),
                "open-meteo",
                "new.png",
                "image/png",
                b"new-bytes",
                Some(3600),
                1_700_000_000,
            )
            .unwrap();
        let no_ttl = store
            .insert_plugin_artifact(
                tmp.path(),
                "open-meteo",
                "forever.png",
                "image/png",
                b"forever-bytes",
                None,
                1_700_000_000,
            )
            .unwrap();

        // Sweep at expired.expires_at + 1.
        let removed = store
            .sweep_expired_plugin_artifacts(1_700_000_000 + 11)
            .unwrap();
        assert_eq!(removed, 1);

        assert!(
            store
                .get_artifact(&expired.attachment_id)
                .unwrap()
                .is_none()
        );
        assert!(store.get_artifact(&fresh.attachment_id).unwrap().is_some());
        assert!(store.get_artifact(&no_ttl.attachment_id).unwrap().is_some());

        // Idempotent — second sweep removes nothing.
        let again = store
            .sweep_expired_plugin_artifacts(1_700_000_000 + 11)
            .unwrap();
        assert_eq!(again, 0);
    }

    /// The plugin-lifecycle `purge` path calls `purge_artifacts_for_plugin`
    /// to wipe one plugin's artifacts. Other plugins' rows + blobs must
    /// survive. The refcount-aware blob delete must NOT unlink a file
    /// still pointed at by another row.
    #[test]
    fn purge_artifacts_for_plugin_wipes_only_that_plugin_and_respects_dedupe() {
        let db = Database::open(&DbConfig::in_memory_unencrypted()).unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        let store = AttachmentStore::new(&db);
        let tmp = tempfile::tempdir().unwrap();

        // open-meteo writes two distinct artifacts (different bytes).
        // weather-station writes one artifact with IDENTICAL bytes to
        // open-meteo's chart-a — dedupe shares the blob across plugins.
        let chart_a = store
            .insert_plugin_artifact(
                tmp.path(),
                "open-meteo",
                "chart-a.png",
                "image/png",
                b"shared-chart-bytes",
                None,
                1,
            )
            .unwrap();
        let chart_b = store
            .insert_plugin_artifact(
                tmp.path(),
                "open-meteo",
                "chart-b.png",
                "image/png",
                b"open-meteo-only-bytes",
                None,
                2,
            )
            .unwrap();
        let other_plugin_share = store
            .insert_plugin_artifact(
                tmp.path(),
                "weather-station",
                "ws-chart.png",
                "image/png",
                b"shared-chart-bytes",
                None,
                3,
            )
            .unwrap();
        // Confirm starting dedupe state.
        let row_a = store.get_artifact(&chart_a.attachment_id).unwrap().unwrap();
        let row_share = store
            .get_artifact(&other_plugin_share.attachment_id)
            .unwrap()
            .unwrap();
        assert_eq!(
            row_a.path, row_share.path,
            "shared bytes must point at the same blob",
        );
        let shared_blob_path = row_a.path.clone();
        let unique_blob_path = store
            .get_artifact(&chart_b.attachment_id)
            .unwrap()
            .unwrap()
            .path;
        assert!(std::path::Path::new(&shared_blob_path).exists());
        assert!(std::path::Path::new(&unique_blob_path).exists());

        // Purge open-meteo. Expected:
        //   * chart_a row gone, chart_b row gone, ws-chart row survives.
        //   * unique_blob_path file gone (no surviving row).
        //   * shared_blob_path file STAYS (ws-chart still references it).
        let removed = store.purge_artifacts_for_plugin("open-meteo").unwrap();
        assert_eq!(removed, 2);
        assert!(
            store
                .get_artifact(&chart_a.attachment_id)
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .get_artifact(&chart_b.attachment_id)
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .get_artifact(&other_plugin_share.attachment_id)
                .unwrap()
                .is_some(),
            "other plugins' rows must survive",
        );
        assert!(
            !std::path::Path::new(&unique_blob_path).exists(),
            "unique blob must be unlinked once refcount hits zero",
        );
        assert!(
            std::path::Path::new(&shared_blob_path).exists(),
            "shared blob must survive — weather-station still references it",
        );

        // Idempotent — second call returns 0.
        assert_eq!(store.purge_artifacts_for_plugin("open-meteo").unwrap(), 0);
    }
}
