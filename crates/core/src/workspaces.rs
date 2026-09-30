//! Content-addressed workspace roots and checkpoint manifests.

use crate::db::{Database, DbError};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceRootRecord {
    pub workspace_id: String,
    pub canonical_path: String,
    pub created_at: i64,
    pub created_by: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceFileEntry {
    pub path: String,
    pub sha256: String,
    pub byte_length: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceCheckpointRecord {
    pub checkpoint_id: String,
    pub workspace_id: String,
    pub run_id: String,
    pub parent_checkpoint_id: Option<String>,
    pub manifest: Vec<WorkspaceFileEntry>,
    pub total_bytes: u64,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunWorkspaceBinding {
    pub run_id: String,
    pub workspace_id: String,
    pub checkpoint_id: String,
    pub checkout_path: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceApplyFile {
    pub path: String,
    pub base_sha256: Option<String>,
    pub proposed_sha256: Option<String>,
    pub status: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceApplyReceipt {
    pub apply_id: String,
    pub run_id: String,
    pub workspace_id: String,
    pub checkpoint_id: String,
    pub preview_hash: String,
    pub status: String,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Error)]
pub enum WorkspaceStoreError {
    #[error(transparent)]
    Db(#[from] DbError),
    #[error("invalid workspace checkpoint: {0}")]
    Invalid(String),
}

/// Store for registered workspace roots and immutable checkpoint content.
pub struct WorkspaceStore<'db> {
    db: &'db Database,
}

impl<'db> WorkspaceStore<'db> {
    pub fn new(db: &'db Database) -> Self {
        Self { db }
    }

    /// Register an already-canonicalized Controller-selected workspace root.
    pub fn register_root(
        &self,
        canonical_path: &str,
        created_by: &str,
        now: i64,
    ) -> Result<WorkspaceRootRecord, WorkspaceStoreError> {
        if canonical_path.is_empty()
            || canonical_path.len() > 4096
            || created_by.is_empty()
            || created_by.len() > 128
        {
            return Err(WorkspaceStoreError::Invalid(
                "root path or creator exceeds its bounds".into(),
            ));
        }
        let workspace_id = uuid::Uuid::new_v4().to_string();
        self.db.with_conn(|connection| {
            connection.execute("INSERT INTO state_workspace_roots(workspace_id,canonical_path,created_at,created_by) VALUES (?1,?2,?3,?4)", params![workspace_id,canonical_path,now,created_by])?;
            Ok(())
        })?;
        self.get_root(&workspace_id)?.ok_or_else(|| {
            WorkspaceStoreError::Invalid("registered workspace root disappeared".into())
        })
    }

    /// Load a root by its opaque identifier.
    pub fn get_root(
        &self,
        workspace_id: &str,
    ) -> Result<Option<WorkspaceRootRecord>, WorkspaceStoreError> {
        self.db.with_conn(|connection| connection.query_row("SELECT workspace_id,canonical_path,created_at,created_by FROM state_workspace_roots WHERE workspace_id=?1", [workspace_id], |row| Ok(WorkspaceRootRecord { workspace_id:row.get(0)?,canonical_path:row.get(1)?,created_at:row.get(2)?,created_by:row.get(3)? })).optional().map_err(DbError::from)).map_err(WorkspaceStoreError::from)
    }

    /// List configured workspace roots in stable creation order.
    pub fn list_roots(&self) -> Result<Vec<WorkspaceRootRecord>, WorkspaceStoreError> {
        self.db.with_conn(|connection| {
            let mut statement = connection.prepare_cached("SELECT workspace_id,canonical_path,created_at,created_by FROM state_workspace_roots ORDER BY created_at,workspace_id")?;
            statement.query_map([], |row| Ok(WorkspaceRootRecord { workspace_id:row.get(0)?,canonical_path:row.get(1)?,created_at:row.get(2)?,created_by:row.get(3)? }))?.collect::<Result<Vec<_>,_>>().map_err(DbError::from)
        }).map_err(WorkspaceStoreError::from)
    }

    /// Bind an isolated run checkout to its immutable base checkpoint.
    pub fn bind_run_checkout(
        &self,
        run_id: &str,
        workspace_id: &str,
        checkpoint_id: &str,
        checkout_path: &str,
        now: i64,
    ) -> Result<(), WorkspaceStoreError> {
        if checkout_path.is_empty() || checkout_path.len() > 4096 {
            return Err(WorkspaceStoreError::Invalid(
                "checkout path exceeds its bound".into(),
            ));
        }
        self.db.transaction(|tx| {
            tx.execute("INSERT INTO state_run_workspace_bindings(run_id,workspace_id,checkpoint_id,checkout_path,created_at,updated_at) VALUES (?1,?2,?3,?4,?5,?5) ON CONFLICT(run_id) DO UPDATE SET workspace_id=excluded.workspace_id,checkpoint_id=excluded.checkpoint_id,checkout_path=excluded.checkout_path,updated_at=excluded.updated_at", params![run_id,workspace_id,checkpoint_id,checkout_path,now])?;
            Ok(())
        })?;
        Ok(())
    }

    /// Load the checkout bound to a durable run.
    pub fn binding_for_run(
        &self,
        run_id: &str,
    ) -> Result<Option<RunWorkspaceBinding>, WorkspaceStoreError> {
        self.db.with_conn(|connection| connection.query_row("SELECT run_id,workspace_id,checkpoint_id,checkout_path,created_at,updated_at FROM state_run_workspace_bindings WHERE run_id=?1", [run_id], |row| Ok(RunWorkspaceBinding { run_id:row.get(0)?,workspace_id:row.get(1)?,checkpoint_id:row.get(2)?,checkout_path:row.get(3)?,created_at:row.get(4)?,updated_at:row.get(5)? })).optional().map_err(DbError::from)).map_err(WorkspaceStoreError::from)
    }

    /// Create or load an idempotent, durable diff-application receipt before
    /// touching files in a registered workspace.
    pub fn begin_apply(
        &self,
        run_id: &str,
        workspace_id: &str,
        checkpoint_id: &str,
        preview_hash: &str,
        client_request_id: &str,
        changes: &[WorkspaceApplyFile],
        now: i64,
    ) -> Result<(WorkspaceApplyReceipt, bool), WorkspaceStoreError> {
        if preview_hash.len() != 64
            || client_request_id.is_empty()
            || client_request_id.len() > 128
            || !client_request_id
                .bytes()
                .all(|byte| (0x21..=0x7e).contains(&byte))
            || changes.len() > 10_000
        {
            return Err(WorkspaceStoreError::Invalid(
                "workspace apply request exceeds its bounds".into(),
            ));
        }
        let body = serde_json::to_vec(&serde_json::json!({"workspace_id":workspace_id,"checkpoint_id":checkpoint_id,"preview_hash":preview_hash,"changes":changes}))
            .map_err(|error| WorkspaceStoreError::Invalid(format!("encode workspace apply intent: {error}")))?;
        let request_hash = hex::encode(Sha256::digest(body));
        let candidate_id = uuid::Uuid::new_v4().to_string();
        let row = self.db.transaction(|tx| {
            let existing: Option<(String,String,String,String,String,String,i64,i64)> = tx.query_row("SELECT apply_id,run_id,workspace_id,checkpoint_id,preview_hash,request_hash,created_at,updated_at FROM state_workspace_apply_receipts WHERE run_id=?1 AND client_request_id=?2", params![run_id,client_request_id], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?,row.get(7)?))).optional()?;
            if let Some(existing)=existing {
                if existing.5 != request_hash { return Err(DbError::Invariant("workspace apply idempotency key was reused with a different diff".into())); }
                let status: String = tx.query_row("SELECT status FROM state_workspace_apply_receipts WHERE apply_id=?1", [&existing.0], |row| row.get(0))?;
                return Ok((existing.0,existing.1,existing.2,existing.3,existing.4,status,existing.6,existing.7,false));
            }
            tx.execute("INSERT INTO state_workspace_apply_receipts(apply_id,run_id,workspace_id,checkpoint_id,client_request_id,request_hash,preview_hash,status,created_at,updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,'applying',?8,?8)", params![candidate_id,run_id,workspace_id,checkpoint_id,client_request_id,request_hash,preview_hash,now])?;
            for change in changes {
                tx.execute("INSERT INTO state_workspace_apply_files(apply_id,path,base_sha256,proposed_sha256,status,updated_at) VALUES (?1,?2,?3,?4,'pending',?5)", params![candidate_id,change.path,change.base_sha256,change.proposed_sha256,now])?;
            }
            Ok((candidate_id.to_owned(),run_id.to_owned(),workspace_id.to_owned(),checkpoint_id.to_owned(),preview_hash.to_owned(),"applying".to_owned(),now,now,true))
        }).map_err(WorkspaceStoreError::from)?;
        Ok((
            WorkspaceApplyReceipt {
                apply_id: row.0,
                run_id: row.1,
                workspace_id: row.2,
                checkpoint_id: row.3,
                preview_hash: row.4,
                status: row.5,
                created_at: row.6,
                updated_at: row.7,
            },
            row.8,
        ))
    }

    /// Load an apply receipt and its per-file recovery state.
    pub fn list_apply_files(
        &self,
        apply_id: &str,
    ) -> Result<Vec<WorkspaceApplyFile>, WorkspaceStoreError> {
        self.db.with_conn(|connection| {
            let mut statement = connection.prepare_cached("SELECT path,base_sha256,proposed_sha256,status,error FROM state_workspace_apply_files WHERE apply_id=?1 ORDER BY path")?;
            statement.query_map([apply_id], |row| Ok(WorkspaceApplyFile {path:row.get(0)?,base_sha256:row.get(1)?,proposed_sha256:row.get(2)?,status:row.get(3)?,error:row.get(4)?}))?.collect::<Result<Vec<_>,_>>().map_err(DbError::from)
        }).map_err(WorkspaceStoreError::from)
    }

    /// Find an apply by its run-scoped client idempotency key.
    pub fn get_apply_by_request(
        &self,
        run_id: &str,
        client_request_id: &str,
    ) -> Result<Option<WorkspaceApplyReceipt>, WorkspaceStoreError> {
        self.db.with_conn(|connection| connection.query_row("SELECT apply_id,run_id,workspace_id,checkpoint_id,preview_hash,status,created_at,updated_at FROM state_workspace_apply_receipts WHERE run_id=?1 AND client_request_id=?2", params![run_id,client_request_id], |row| Ok(WorkspaceApplyReceipt {apply_id:row.get(0)?,run_id:row.get(1)?,workspace_id:row.get(2)?,checkpoint_id:row.get(3)?,preview_hash:row.get(4)?,status:row.get(5)?,created_at:row.get(6)?,updated_at:row.get(7)?})).optional().map_err(DbError::from)).map_err(WorkspaceStoreError::from)
    }

    /// Load the latest successfully applied run diff for owned-change restore.
    pub fn latest_applied_workspace_diff(
        &self,
        run_id: &str,
    ) -> Result<Option<WorkspaceApplyReceipt>, WorkspaceStoreError> {
        self.db.with_conn(|connection| connection.query_row("SELECT apply_id,run_id,workspace_id,checkpoint_id,preview_hash,status,created_at,updated_at FROM state_workspace_apply_receipts WHERE run_id=?1 AND status='applied' AND client_request_id NOT LIKE 'restore:%' ORDER BY updated_at DESC,apply_id DESC LIMIT 1", [run_id], |row| Ok(WorkspaceApplyReceipt {apply_id:row.get(0)?,run_id:row.get(1)?,workspace_id:row.get(2)?,checkpoint_id:row.get(3)?,preview_hash:row.get(4)?,status:row.get(5)?,created_at:row.get(6)?,updated_at:row.get(7)?})).optional().map_err(DbError::from)).map_err(WorkspaceStoreError::from)
    }

    /// Load the latest successful owned-change restore operation for a run.
    pub fn latest_workspace_restore(
        &self,
        run_id: &str,
    ) -> Result<Option<WorkspaceApplyReceipt>, WorkspaceStoreError> {
        self.db.with_conn(|connection| connection.query_row("SELECT apply_id,run_id,workspace_id,checkpoint_id,preview_hash,status,created_at,updated_at FROM state_workspace_apply_receipts WHERE run_id=?1 AND status='applied' AND client_request_id LIKE 'restore:%' ORDER BY updated_at DESC,apply_id DESC LIMIT 1", [run_id], |row| Ok(WorkspaceApplyReceipt {apply_id:row.get(0)?,run_id:row.get(1)?,workspace_id:row.get(2)?,checkpoint_id:row.get(3)?,preview_hash:row.get(4)?,status:row.get(5)?,created_at:row.get(6)?,updated_at:row.get(7)?})).optional().map_err(DbError::from)).map_err(WorkspaceStoreError::from)
    }

    /// Claim or reclaim a workspace apply lease. The owner fences stale
    /// requests after a process restart or concurrent apply attempt.
    pub fn claim_apply(
        &self,
        apply_id: &str,
        owner: &str,
        now: i64,
        lease_seconds: i64,
    ) -> Result<bool, WorkspaceStoreError> {
        self.db.transaction(|tx| {
            let workspace_id: Option<String> = tx.query_row("SELECT workspace_id FROM state_workspace_apply_receipts WHERE apply_id=?1 AND status IN ('applying','failed')",[apply_id],|row|row.get(0)).optional()?;
            let Some(workspace_id)=workspace_id else { return Ok(false); };
            let expires=now.saturating_add(lease_seconds.max(1));
            tx.execute("INSERT OR IGNORE INTO state_workspace_apply_locks(workspace_id,apply_id,lease_owner,lease_expires_at) VALUES (?1,?2,?3,?4)",params![workspace_id,apply_id,owner,expires])?;
            let lock_changed=tx.execute("UPDATE state_workspace_apply_locks SET apply_id=?2,lease_owner=?3,lease_expires_at=?4 WHERE workspace_id=?1 AND (lease_expires_at<=?5 OR lease_owner=?3)",params![workspace_id,apply_id,owner,expires,now])?;
            if lock_changed!=1 { return Ok(false); }
            let changed=tx.execute("UPDATE state_workspace_apply_receipts SET status='applying',lease_owner=?2,lease_expires_at=?3,updated_at=?4 WHERE apply_id=?1 AND status IN ('applying','failed') AND (lease_owner IS NULL OR lease_expires_at<=?4 OR lease_owner=?2)",params![apply_id,owner,expires,now])?;
            if changed!=1 { return Err(DbError::Invariant("workspace apply receipt lease changed while root lock was claimed".into())); }
            Ok(true)
        }).map_err(WorkspaceStoreError::from)
    }

    /// Finalize an apply after every file transition reached a terminal state.
    pub fn finish_apply(
        &self,
        apply_id: &str,
        owner: &str,
        status: &str,
        now: i64,
    ) -> Result<(), WorkspaceStoreError> {
        if !matches!(status, "applied" | "conflict" | "failed") {
            return Err(WorkspaceStoreError::Invalid(
                "invalid terminal workspace apply status".into(),
            ));
        }
        self.db.transaction(|tx| { let workspace_id: String=tx.query_row("SELECT workspace_id FROM state_workspace_apply_receipts WHERE apply_id=?1",[apply_id],|row|row.get(0))?; let changed=tx.execute("UPDATE state_workspace_apply_receipts SET status=?3,lease_owner=NULL,lease_expires_at=NULL,updated_at=?4 WHERE apply_id=?1 AND lease_owner=?2", params![apply_id,owner,status,now])?; if changed!=1 { return Err(DbError::Invariant("workspace apply lease was lost".into())); } tx.execute("DELETE FROM state_workspace_apply_locks WHERE workspace_id=?1 AND apply_id=?2 AND lease_owner=?3",params![workspace_id,apply_id,owner])?; Ok(()) }).map_err(WorkspaceStoreError::from)
    }

    /// Persist one file transition and its overall apply status.
    pub fn update_apply_file(
        &self,
        apply_id: &str,
        owner: &str,
        path: &str,
        status: &str,
        error: Option<&str>,
        receipt_status: &str,
        now: i64,
    ) -> Result<(), WorkspaceStoreError> {
        if !matches!(status, "pending" | "applied" | "conflict" | "failed")
            || !matches!(
                receipt_status,
                "applying" | "applied" | "conflict" | "failed"
            )
            || error.is_some_and(|value| value.len() > 2048)
        {
            return Err(WorkspaceStoreError::Invalid(
                "workspace apply status or error exceeds its bounds".into(),
            ));
        }
        self.db.transaction(|tx| {
            let lease_owner: Option<String> = tx.query_row("SELECT lease_owner FROM state_workspace_apply_receipts WHERE apply_id=?1", [apply_id], |row| row.get(0))?;
            if lease_owner.as_deref()!=Some(owner) { return Err(DbError::Invariant("workspace apply lease was lost".into())); }
            let workspace_id: String=tx.query_row("SELECT workspace_id FROM state_workspace_apply_receipts WHERE apply_id=?1",[apply_id],|row|row.get(0))?;
            tx.execute("UPDATE state_workspace_apply_files SET status=?3,error=?4,updated_at=?5 WHERE apply_id=?1 AND path=?2", params![apply_id,path,status,error,now])?;
            if tx.changes()!=1 { return Err(DbError::Invariant("workspace apply file receipt not found".into())); }
            tx.execute("UPDATE state_workspace_apply_receipts SET status=?3,lease_expires_at=?4,updated_at=?4 WHERE apply_id=?1 AND lease_owner=?2", params![apply_id,owner,receipt_status,now.saturating_add(60)])?;
            tx.execute("UPDATE state_workspace_apply_locks SET lease_expires_at=?3 WHERE workspace_id=?1 AND apply_id=?2 AND lease_owner=?4",params![workspace_id,apply_id,now.saturating_add(60),owner])?;
            Ok(())
        })?;
        Ok(())
    }

    /// Persist a sorted manifest and insert all file bytes by SHA-256.
    pub fn create_checkpoint(
        &self,
        workspace_id: &str,
        run_id: &str,
        parent_checkpoint_id: Option<&str>,
        files: &[(String, Vec<u8>)],
        now: i64,
    ) -> Result<WorkspaceCheckpointRecord, WorkspaceStoreError> {
        if files.len() > 10_000 {
            return Err(WorkspaceStoreError::Invalid(
                "workspace contains more than 10000 files".into(),
            ));
        }
        let mut ordered_files = files.iter().collect::<Vec<_>>();
        ordered_files.sort_by(|left, right| left.0.cmp(&right.0));
        let mut manifest = Vec::with_capacity(files.len());
        let mut total_bytes = 0u64;
        for (path, bytes) in &ordered_files {
            if path.is_empty()
                || path.len() > 1024
                || path.starts_with('/')
                || path
                    .split(['/', '\\'])
                    .any(|part| part == ".." || part.is_empty())
            {
                return Err(WorkspaceStoreError::Invalid(
                    "manifest contains an unsafe relative path".into(),
                ));
            }
            total_bytes = total_bytes.saturating_add(bytes.len() as u64);
            if total_bytes > 100 * 1024 * 1024 {
                return Err(WorkspaceStoreError::Invalid(
                    "workspace snapshot exceeds 100 MiB".into(),
                ));
            }
            manifest.push(WorkspaceFileEntry {
                path: path.clone(),
                sha256: hex::encode(Sha256::digest(bytes)),
                byte_length: bytes.len() as u64,
            });
        }
        let manifest_json = serde_json::to_string(&manifest)
            .map_err(|error| WorkspaceStoreError::Invalid(format!("encode manifest: {error}")))?;
        let checkpoint_id = uuid::Uuid::new_v4().to_string();
        self.db.transaction(|tx| {
            let root_exists: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM state_workspace_roots WHERE workspace_id=?1)", [workspace_id], |row| row.get(0))?;
            let run_exists: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM state_runs WHERE run_id=?1)", [run_id], |row| row.get(0))?;
            if !root_exists || !run_exists { return Err(DbError::Invariant("workspace root and durable run must exist before checkpointing".into())); }
            for ((_,bytes),entry) in ordered_files.iter().zip(manifest.iter()) {
                tx.execute("INSERT OR IGNORE INTO state_workspace_blobs(sha256,byte_length,contents,created_at) VALUES (?1,?2,?3,?4)", params![entry.sha256,bytes.len() as u64,bytes,now])?;
                let stored: (u64,Vec<u8>) = tx.query_row("SELECT byte_length,contents FROM state_workspace_blobs WHERE sha256=?1", [&entry.sha256], |row| Ok((row.get(0)?,row.get(1)?)))?;
                if stored.0 != bytes.len() as u64 || stored.1 != *bytes || hex::encode(Sha256::digest(&stored.1)) != entry.sha256 { return Err(DbError::Invariant("content-addressed workspace blob failed integrity validation".into())); }
            }
            tx.execute("INSERT INTO state_workspace_checkpoints(checkpoint_id,workspace_id,run_id,parent_checkpoint_id,manifest_json,total_bytes,file_count,created_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)", params![checkpoint_id,workspace_id,run_id,parent_checkpoint_id,manifest_json,total_bytes,manifest.len(),now])?;
            Ok(())
        })?;
        Ok(WorkspaceCheckpointRecord {
            checkpoint_id,
            workspace_id: workspace_id.to_owned(),
            run_id: run_id.to_owned(),
            parent_checkpoint_id: parent_checkpoint_id.map(str::to_owned),
            manifest,
            total_bytes,
            created_at: now,
        })
    }

    /// Read a checkpoint manifest by ID.
    pub fn get_checkpoint(
        &self,
        checkpoint_id: &str,
    ) -> Result<Option<WorkspaceCheckpointRecord>, WorkspaceStoreError> {
        self.db.with_conn(|connection| {
            connection.query_row("SELECT checkpoint_id,workspace_id,run_id,parent_checkpoint_id,manifest_json,total_bytes,created_at FROM state_workspace_checkpoints WHERE checkpoint_id=?1", [checkpoint_id], |row| {
                let json: String = row.get(4)?;
                Ok(WorkspaceCheckpointRecord { checkpoint_id:row.get(0)?,workspace_id:row.get(1)?,run_id:row.get(2)?,parent_checkpoint_id:row.get(3)?,manifest:serde_json::from_str(&json).map_err(|_| rusqlite::Error::InvalidQuery)?,total_bytes:row.get(5)?,created_at:row.get(6)? })
            }).optional().map_err(DbError::from)
        }).map_err(WorkspaceStoreError::from)
    }

    /// Verify and read a content-addressed snapshot blob.
    pub fn read_blob(&self, sha256: &str) -> Result<Option<Vec<u8>>, WorkspaceStoreError> {
        let bytes = self.db.with_conn(|connection| {
            connection
                .query_row(
                    "SELECT contents FROM state_workspace_blobs WHERE sha256=?1",
                    [sha256],
                    |row| row.get::<_, Vec<u8>>(0),
                )
                .optional()
                .map_err(DbError::from)
        })?;
        if let Some(bytes) = &bytes {
            if hex::encode(Sha256::digest(bytes)) != sha256 {
                return Err(WorkspaceStoreError::Invalid(
                    "workspace blob hash mismatch".into(),
                ));
            }
        }
        Ok(bytes)
    }
}
