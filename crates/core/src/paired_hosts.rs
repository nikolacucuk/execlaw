//! Durable, explicitly paired operator-host delegation trial.
//!
//! This module stores authenticated task envelopes and typed artifact receipts.
//! It does not create network connections or invoke a remote host. An adapter
//! may exchange these signed envelopes only after the operator has paired a
//! local/VPN endpoint. Peer-requested authority and labels are intersected
//! with the ceilings stored at pairing time.

use crate::{Database, DbError};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::net::{IpAddr, Ipv4Addr};
use thiserror::Error;
use url::Url;

const MAX_SIGNED_ENVELOPE_BYTES: usize = 256 * 1024;
const MAX_ARTIFACT_SIZE_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum PairedHostError {
    #[error("database: {0}")]
    Db(#[from] DbError),
    #[error("invalid paired-host metadata: {0}")]
    Invalid(String),
    #[error("paired operator host '{0}' is not active or paired")]
    Unpaired(String),
    #[error("signed envelope was not produced by the paired host")]
    InvalidSignature,
    #[error("delegated task '{0}' does not exist")]
    TaskNotFound(String),
    #[error("delegated task transition is not allowed: {0}")]
    InvalidTransition(String),
    #[error("only a Controller can pair or revoke an operator host")]
    ControllerRequired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairedOperatorHost {
    pub peer_id: String,
    pub endpoint: String,
    pub inference_endpoint: String,
    pub key_fingerprint_sha256: String,
    pub allowed_capabilities: Vec<String>,
    pub allowed_data_labels: Vec<String>,
    pub paired_by: String,
    pub paired_at: i64,
    pub active: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DelegatedHostTaskStatus {
    Pending,
    Accepted,
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl DelegatedHostTaskStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Accepted => "accepted",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "pending" => Some(Self::Pending),
            "accepted" => Some(Self::Accepted),
            "running" => Some(Self::Running),
            "completed" => Some(Self::Completed),
            "failed" => Some(Self::Failed),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelegatedHostArtifact {
    pub artifact_id: String,
    pub kind: String,
    pub media_type: String,
    pub sha256: String,
    pub size_bytes: u64,
    pub data_labels: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DelegatedHostTask {
    pub task_id: String,
    pub peer_id: String,
    pub task: serde_json::Value,
    pub effective_capabilities: Vec<String>,
    pub effective_data_labels: Vec<String>,
    pub status: DelegatedHostTaskStatus,
    pub artifacts: Vec<DelegatedHostArtifact>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SignedTaskEnvelope {
    task_id: String,
    task: serde_json::Value,
    requested_capabilities: Vec<String>,
    requested_data_labels: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedDelegationEnvelope {
    pub peer_endpoint: String,
    pub payload: Vec<u8>,
    pub signature_hex: String,
}

/// Controller-approved configuration for one peer.
#[derive(Debug, Clone)]
pub struct PairOperatorHostRequest {
    pub peer_id: String,
    pub endpoint: String,
    pub inference_endpoint: String,
    pub public_key_hex: String,
    pub allowed_capabilities: Vec<String>,
    pub allowed_data_labels: Vec<String>,
    pub actor_trust: String,
    pub actor: String,
    pub now: i64,
}

/// One outbound task plus the local signing key supplied by its secure adapter.
pub struct IssueDelegatedHostTask<'a> {
    pub peer_id: &'a str,
    pub task_id: &'a str,
    pub task: serde_json::Value,
    pub requested_capabilities: &'a [String],
    pub requested_data_labels: &'a [String],
    pub local_identity: &'a SigningKey,
    pub now: i64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SignedTaskResult {
    task_id: String,
    outcome: String,
    artifacts: Vec<DelegatedHostArtifact>,
}

#[derive(Clone)]
pub struct PairedHostStore {
    db: Database,
}

impl PairedHostStore {
    pub fn new(db: Database) -> Self {
        Self { db }
    }

    /// Pair a Controller-approved host only when both its task and inference endpoints are local/VPN.
    pub fn pair(
        &self,
        request: &PairOperatorHostRequest,
    ) -> Result<PairedOperatorHost, PairedHostError> {
        if request.actor_trust != "Controller" {
            return Err(PairedHostError::ControllerRequired);
        }
        validate_id(&request.peer_id, "peer id", 128)?;
        validate_id(&request.actor, "actor", 128)?;
        validate_local_endpoint(&request.endpoint)?;
        validate_local_endpoint(&request.inference_endpoint)?;
        let public_key = decode_hex::<32>(&request.public_key_hex, "peer public key")?;
        let verifying_key = VerifyingKey::from_bytes(&public_key)
            .map_err(|_| PairedHostError::Invalid("peer public key is invalid".into()))?;
        let key_fingerprint = hex::encode(Sha256::digest(verifying_key.as_bytes()));
        let capabilities = normalize_allowlist(&request.allowed_capabilities, "capability")?;
        let labels = normalize_allowlist(&request.allowed_data_labels, "data label")?;
        let capabilities_json = serde_json::to_string(&capabilities)
            .map_err(|error| PairedHostError::Invalid(error.to_string()))?;
        let labels_json = serde_json::to_string(&labels)
            .map_err(|error| PairedHostError::Invalid(error.to_string()))?;
        self.db.transaction(|tx| {
            let existing_key: Option<String> = tx
                .query_row(
                    "SELECT public_key_hex FROM paired_operator_hosts WHERE peer_id=?1",
                    [&request.peer_id],
                    |row| row.get(0),
                )
                .optional()?;
            if existing_key
                .as_deref()
                .is_some_and(|existing| existing != request.public_key_hex.to_ascii_lowercase())
            {
                let active_tasks: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM delegated_host_tasks WHERE peer_id=?1
                     AND status IN ('pending','accepted','running')",
                    [&request.peer_id],
                    |row| row.get(0),
                )?;
                if active_tasks > 0 {
                    return Err(DbError::Invariant(
                        "peer key cannot change while delegated tasks are active".into(),
                    ));
                }
            }
            tx.execute(
                "INSERT INTO paired_operator_hosts
                 (peer_id,endpoint,inference_endpoint,public_key_hex,key_fingerprint_sha256,
                  allowed_capabilities_json,allowed_data_labels_json,paired_by,paired_at,active)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,1)
                 ON CONFLICT(peer_id) DO UPDATE SET endpoint=excluded.endpoint,
                  inference_endpoint=excluded.inference_endpoint,public_key_hex=excluded.public_key_hex,
                  key_fingerprint_sha256=excluded.key_fingerprint_sha256,
                  allowed_capabilities_json=excluded.allowed_capabilities_json,
                  allowed_data_labels_json=excluded.allowed_data_labels_json,
                  paired_by=excluded.paired_by,paired_at=excluded.paired_at,active=1",
                params![request.peer_id, request.endpoint, request.inference_endpoint, request.public_key_hex.to_ascii_lowercase(), key_fingerprint, capabilities_json, labels_json, request.actor, request.now],
            )?;
            Ok(())
        })?;
        Ok(PairedOperatorHost {
            peer_id: request.peer_id.clone(),
            endpoint: request.endpoint.clone(),
            inference_endpoint: request.inference_endpoint.clone(),
            key_fingerprint_sha256: key_fingerprint,
            allowed_capabilities: capabilities,
            allowed_data_labels: labels,
            paired_by: request.actor.clone(),
            paired_at: request.now,
            active: true,
        })
    }

    /// Revoke a peer without deleting its task history.
    pub fn revoke_peer(
        &self,
        peer_id: &str,
        actor_trust: &str,
        now: i64,
    ) -> Result<bool, PairedHostError> {
        if actor_trust != "Controller" {
            return Err(PairedHostError::ControllerRequired);
        }
        self.db
            .with_conn(|conn| {
                conn.execute(
                    "UPDATE paired_operator_hosts SET active=0,paired_at=?1 WHERE peer_id=?2 AND active=1",
                    params![now, peer_id],
                )
                .map(|changed| changed != 0)
                .map_err(DbError::from)
            })
            .map_err(Into::into)
    }

    /// Persist and sign an outbound task before returning it to a transport adapter.
    pub fn issue_signed_task(
        &self,
        request: IssueDelegatedHostTask<'_>,
    ) -> Result<SignedDelegationEnvelope, PairedHostError> {
        validate_id(request.task_id, "task id", 128)?;
        if request.task.is_null() {
            return Err(PairedHostError::Invalid(
                "task payload must not be null".into(),
            ));
        }
        let task_json = serde_json::to_string(&request.task)
            .map_err(|error| PairedHostError::Invalid(error.to_string()))?;
        if task_json.len() > MAX_SIGNED_ENVELOPE_BYTES {
            return Err(PairedHostError::Invalid(
                "outbound task exceeds 256 KiB".into(),
            ));
        }
        let (_, capability_ceiling, label_ceiling) = self.active_peer_policy(request.peer_id)?;
        let requested_capabilities =
            normalize_allowlist(request.requested_capabilities, "capability")?;
        let requested_data_labels =
            normalize_allowlist(request.requested_data_labels, "data label")?;
        let effective_capabilities = requested_capabilities
            .into_iter()
            .filter(|requested| capability_ceiling.contains(requested))
            .collect::<Vec<_>>();
        let effective_data_labels = requested_data_labels
            .into_iter()
            .filter(|requested| label_ceiling.contains(requested))
            .collect::<Vec<_>>();
        let envelope = SignedTaskEnvelope {
            task_id: request.task_id.to_owned(),
            task: request.task.clone(),
            requested_capabilities: effective_capabilities.clone(),
            requested_data_labels: effective_data_labels.clone(),
        };
        let payload = serde_json::to_vec(&envelope)
            .map_err(|error| PairedHostError::Invalid(error.to_string()))?;
        let signature_hex = hex::encode(request.local_identity.sign(&payload).to_bytes());
        let authority_json = serde_json::to_string(&effective_capabilities)
            .map_err(|error| PairedHostError::Invalid(error.to_string()))?;
        let labels_json = serde_json::to_string(&effective_data_labels)
            .map_err(|error| PairedHostError::Invalid(error.to_string()))?;
        self.db.transaction(|tx| {
            let existing: Option<(String, String, String, String, String)> = tx
                .query_row(
                    "SELECT peer_id,task_json,authority_json,data_labels_json,status
                     FROM delegated_host_tasks WHERE task_id=?1",
                    [request.task_id],
                    |row| {
                        Ok((
                            row.get(0)?,
                            row.get(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                        ))
                    },
                )
                .optional()?;
            if let Some((
                existing_peer,
                existing_task,
                existing_authority,
                existing_labels,
                status,
            )) = existing
            {
                if existing_peer != request.peer_id
                    || existing_task != task_json
                    || existing_authority != authority_json
                    || existing_labels != labels_json
                    || matches!(status.as_str(), "completed" | "failed" | "cancelled")
                {
                    return Err(DbError::Invariant(
                        "task id was already used for a different or terminal delegation".into(),
                    ));
                }
            } else {
                tx.execute(
                    "INSERT INTO delegated_host_tasks(task_id,peer_id,task_json,authority_json,
                     data_labels_json,status,artifacts_json,created_at,updated_at)
                     VALUES(?1,?2,?3,?4,?5,'pending','[]',?6,?6)",
                    params![
                        request.task_id,
                        request.peer_id,
                        task_json,
                        authority_json,
                        labels_json,
                        request.now
                    ],
                )?;
            }
            Ok(())
        })?;
        let peer = self
            .peer(request.peer_id)?
            .ok_or_else(|| PairedHostError::Unpaired(request.peer_id.to_owned()))?;
        if !peer.active {
            return Err(PairedHostError::Unpaired(request.peer_id.to_owned()));
        }
        Ok(SignedDelegationEnvelope {
            peer_endpoint: peer.endpoint,
            payload,
            signature_hex,
        })
    }

    /// Verify and persist an incoming signed task, narrowing authority and labels to the pair policy.
    pub fn receive_signed_task(
        &self,
        peer_id: &str,
        payload: &[u8],
        signature_hex: &str,
        now: i64,
    ) -> Result<DelegatedHostTask, PairedHostError> {
        if payload.len() > MAX_SIGNED_ENVELOPE_BYTES {
            return Err(PairedHostError::Invalid(
                "signed task exceeds 256 KiB".into(),
            ));
        }
        let (public_key, capabilities, allowed_labels) = self.active_peer_policy(peer_id)?;
        verify_signature(&public_key, payload, signature_hex)?;
        let envelope: SignedTaskEnvelope = serde_json::from_slice(payload)
            .map_err(|error| PairedHostError::Invalid(format!("signed task envelope: {error}")))?;
        validate_id(&envelope.task_id, "task id", 128)?;
        if envelope.task.is_null() {
            return Err(PairedHostError::Invalid(
                "task payload must not be null".into(),
            ));
        }
        let requested_capabilities =
            normalize_allowlist(&envelope.requested_capabilities, "capability")?;
        let requested_labels = normalize_allowlist(&envelope.requested_data_labels, "data label")?;
        let effective_capabilities = requested_capabilities
            .into_iter()
            .filter(|requested| capabilities.contains(requested))
            .collect::<Vec<_>>();
        let effective_data_labels = requested_labels
            .into_iter()
            .filter(|requested| allowed_labels.contains(requested))
            .collect::<Vec<_>>();
        let task_json = serde_json::to_string(&envelope.task)
            .map_err(|error| PairedHostError::Invalid(error.to_string()))?;
        let authority_json = serde_json::to_string(&effective_capabilities)
            .map_err(|error| PairedHostError::Invalid(error.to_string()))?;
        let labels_json = serde_json::to_string(&effective_data_labels)
            .map_err(|error| PairedHostError::Invalid(error.to_string()))?;
        self.db.transaction(|tx| {
            tx.execute(
                "INSERT INTO delegated_host_tasks(task_id,peer_id,task_json,authority_json,
                 data_labels_json,status,artifacts_json,created_at,updated_at)
                 VALUES(?1,?2,?3,?4,?5,'pending','[]',?6,?6)",
                params![
                    envelope.task_id,
                    peer_id,
                    task_json,
                    authority_json,
                    labels_json,
                    now
                ],
            )?;
            Ok(())
        })?;
        Ok(DelegatedHostTask {
            task_id: envelope.task_id,
            peer_id: peer_id.to_owned(),
            task: envelope.task,
            effective_capabilities,
            effective_data_labels,
            status: DelegatedHostTaskStatus::Pending,
            artifacts: Vec::new(),
            created_at: now,
            updated_at: now,
        })
    }

    /// Move a durable task through its local lifecycle; retries do not create a second task.
    pub fn transition_task(
        &self,
        task_id: &str,
        next: DelegatedHostTaskStatus,
        now: i64,
    ) -> Result<(), PairedHostError> {
        let current = self
            .task(task_id)?
            .ok_or_else(|| PairedHostError::TaskNotFound(task_id.into()))?;
        let allowed = matches!(
            (current.status, next),
            (
                DelegatedHostTaskStatus::Pending,
                DelegatedHostTaskStatus::Accepted
            ) | (
                DelegatedHostTaskStatus::Pending,
                DelegatedHostTaskStatus::Cancelled
            ) | (
                DelegatedHostTaskStatus::Accepted,
                DelegatedHostTaskStatus::Running
            ) | (
                DelegatedHostTaskStatus::Accepted,
                DelegatedHostTaskStatus::Cancelled
            ) | (
                DelegatedHostTaskStatus::Running,
                DelegatedHostTaskStatus::Cancelled
            )
        );
        if !allowed {
            return Err(PairedHostError::InvalidTransition(format!(
                "{} -> {}",
                current.status.as_str(),
                next.as_str()
            )));
        }
        self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE delegated_host_tasks SET status=?1,updated_at=?2 WHERE task_id=?3",
                params![next.as_str(), now, task_id],
            )?;
            Ok(())
        })?;
        Ok(())
    }

    /// Persist a peer-signed terminal result without accepting remote authority changes.
    pub fn record_signed_result(
        &self,
        peer_id: &str,
        payload: &[u8],
        signature_hex: &str,
        now: i64,
    ) -> Result<DelegatedHostTask, PairedHostError> {
        if payload.len() > MAX_SIGNED_ENVELOPE_BYTES {
            return Err(PairedHostError::Invalid(
                "signed task result exceeds 256 KiB".into(),
            ));
        }
        let (public_key, _, _) = self.active_peer_policy(peer_id)?;
        verify_signature(&public_key, payload, signature_hex)?;
        let result: SignedTaskResult = serde_json::from_slice(payload)
            .map_err(|error| PairedHostError::Invalid(format!("signed task result: {error}")))?;
        let task = self
            .task(&result.task_id)?
            .ok_or_else(|| PairedHostError::TaskNotFound(result.task_id.clone()))?;
        if task.peer_id != peer_id
            || !matches!(
                task.status,
                DelegatedHostTaskStatus::Pending
                    | DelegatedHostTaskStatus::Accepted
                    | DelegatedHostTaskStatus::Running
            )
        {
            return Err(PairedHostError::InvalidTransition(
                "result peer or task state mismatch".into(),
            ));
        }
        let next = match result.outcome.as_str() {
            "completed" => DelegatedHostTaskStatus::Completed,
            "failed" => DelegatedHostTaskStatus::Failed,
            _ => {
                return Err(PairedHostError::Invalid(
                    "remote outcome must be completed or failed".into(),
                ));
            }
        };
        let artifacts_json = serde_json::to_string(&result.artifacts)
            .map_err(|error| PairedHostError::Invalid(error.to_string()))?;
        for artifact in &result.artifacts {
            validate_artifact(artifact, &task.effective_data_labels)?;
        }
        self.db.transaction(|tx| {
            let changed = tx.execute(
                "UPDATE delegated_host_tasks SET status=?1,artifacts_json=?2,updated_at=?3
                 WHERE task_id=?4 AND peer_id=?5 AND status IN ('pending','accepted','running')",
                params![next.as_str(), artifacts_json, now, result.task_id, peer_id],
            )?;
            if changed != 1 {
                return Err(DbError::Invariant(
                    "task result raced with another state transition".into(),
                ));
            }
            Ok(())
        })?;
        self.task(&result.task_id)?
            .ok_or_else(|| PairedHostError::TaskNotFound(result.task_id))
    }

    /// Read durable task state after restart or transport reconnection.
    pub fn task(&self, task_id: &str) -> Result<Option<DelegatedHostTask>, PairedHostError> {
        let raw = self.db.with_conn(|conn| {
                conn.query_row(
                    "SELECT peer_id,task_json,authority_json,data_labels_json,status,artifacts_json,created_at,updated_at
                     FROM delegated_host_tasks WHERE task_id=?1",
                    [task_id],
                    |row| Ok((
                        row.get::<_, String>(0)?, row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?, row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?, row.get::<_, String>(5)?,
                        row.get::<_, i64>(6)?, row.get::<_, i64>(7)?,
                    )),
                )
                .optional()
                .map_err(DbError::from)
            }).map_err(PairedHostError::Db)?;
        let Some((
            peer_id,
            task_json,
            authority_json,
            labels_json,
            status,
            artifacts_json,
            created_at,
            updated_at,
        )) = raw
        else {
            return Ok(None);
        };
        let status = DelegatedHostTaskStatus::parse(&status)
            .ok_or_else(|| PairedHostError::Invalid("stored task status is corrupt".into()))?;
        Ok(Some(DelegatedHostTask {
            task_id: task_id.to_owned(),
            peer_id,
            task: serde_json::from_str(&task_json)
                .map_err(|error| PairedHostError::Invalid(error.to_string()))?,
            effective_capabilities: serde_json::from_str(&authority_json)
                .map_err(|error| PairedHostError::Invalid(error.to_string()))?,
            effective_data_labels: serde_json::from_str(&labels_json)
                .map_err(|error| PairedHostError::Invalid(error.to_string()))?,
            status,
            artifacts: serde_json::from_str(&artifacts_json)
                .map_err(|error| PairedHostError::Invalid(error.to_string()))?,
            created_at,
            updated_at,
        }))
    }

    pub fn peer(&self, peer_id: &str) -> Result<Option<PairedOperatorHost>, PairedHostError> {
        self.db.with_conn(|conn| {
            conn.query_row(
                "SELECT endpoint,inference_endpoint,key_fingerprint_sha256,allowed_capabilities_json,
                 allowed_data_labels_json,paired_by,paired_at,active FROM paired_operator_hosts WHERE peer_id=?1",
                [peer_id],
                |row| Ok(PairedOperatorHost {
                    peer_id: peer_id.to_owned(),
                    endpoint: row.get(0)?,
                    inference_endpoint: row.get(1)?,
                    key_fingerprint_sha256: row.get(2)?,
                    allowed_capabilities: serde_json::from_str(&row.get::<_, String>(3)?).unwrap_or_default(),
                    allowed_data_labels: serde_json::from_str(&row.get::<_, String>(4)?).unwrap_or_default(),
                    paired_by: row.get(5)?,
                    paired_at: row.get(6)?,
                    active: row.get::<_, i64>(7)? != 0,
                }),
            ).optional().map_err(DbError::from)
        }).map_err(Into::into)
    }

    fn active_peer_policy(
        &self,
        peer_id: &str,
    ) -> Result<(String, Vec<String>, Vec<String>), PairedHostError> {
        let policy = self
            .db
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT public_key_hex,allowed_capabilities_json,allowed_data_labels_json
                     FROM paired_operator_hosts WHERE peer_id=?1 AND active=1",
                    [peer_id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                        ))
                    },
                )
                .optional()
                .map_err(DbError::from)
            })
            .map_err(PairedHostError::Db)?;
        let Some((key, capabilities, labels)) = policy else {
            return Err(PairedHostError::Unpaired(peer_id.to_owned()));
        };
        let capabilities = serde_json::from_str(&capabilities).map_err(|error| {
            PairedHostError::Invalid(format!("stored capability ceiling: {error}"))
        })?;
        let labels = serde_json::from_str(&labels)
            .map_err(|error| PairedHostError::Invalid(format!("stored label ceiling: {error}")))?;
        Ok((key, capabilities, labels))
    }
}

fn verify_signature(
    public_key_hex: &str,
    payload: &[u8],
    signature_hex: &str,
) -> Result<(), PairedHostError> {
    let key_bytes = decode_hex::<32>(public_key_hex, "peer public key")?;
    let key = VerifyingKey::from_bytes(&key_bytes)
        .map_err(|_| PairedHostError::Invalid("stored peer key is invalid".into()))?;
    let signature_bytes = decode_hex::<64>(signature_hex, "peer signature")?;
    let signature = Signature::from_bytes(&signature_bytes);
    key.verify(payload, &signature)
        .map_err(|_| PairedHostError::InvalidSignature)
}

fn decode_hex<const N: usize>(value: &str, field: &str) -> Result<[u8; N], PairedHostError> {
    let bytes =
        hex::decode(value).map_err(|_| PairedHostError::Invalid(format!("{field} must be hex")))?;
    bytes
        .try_into()
        .map_err(|_| PairedHostError::Invalid(format!("{field} has the wrong length")))
}

fn validate_id(value: &str, field: &str, max: usize) -> Result<(), PairedHostError> {
    if value.trim().is_empty()
        || value.len() > max
        || value
            .bytes()
            .any(|byte| !(byte.is_ascii_alphanumeric() || b"-_.:".contains(&byte)))
    {
        return Err(PairedHostError::Invalid(format!("{field} is invalid")));
    }
    Ok(())
}

fn normalize_allowlist(values: &[String], field: &str) -> Result<Vec<String>, PairedHostError> {
    if values.len() > 128 {
        return Err(PairedHostError::Invalid(format!(
            "too many {field} entries"
        )));
    }
    let mut normalized = values
        .iter()
        .map(|value| value.trim().to_owned())
        .collect::<Vec<_>>();
    if normalized
        .iter()
        .any(|value| value.is_empty() || value.len() > 128)
    {
        return Err(PairedHostError::Invalid(format!(
            "{field} entry is invalid"
        )));
    }
    normalized.sort();
    normalized.dedup();
    Ok(normalized)
}

fn validate_local_endpoint(value: &str) -> Result<(), PairedHostError> {
    let url = Url::parse(value)
        .map_err(|error| PairedHostError::Invalid(format!("endpoint URL: {error}")))?;
    if url.scheme() != "https"
        || url.username() != ""
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(PairedHostError::Invalid(
            "paired host and inference URLs must use HTTPS without embedded credentials".into(),
        ));
    }
    let host = url
        .host_str()
        .ok_or_else(|| PairedHostError::Invalid("endpoint URL requires a host".into()))?
        .to_ascii_lowercase();
    let local = match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(ip)) => is_local_ipv4(ip),
        Ok(IpAddr::V6(ip)) => {
            ip.is_loopback() || ip.is_unique_local() || ip.is_unicast_link_local()
        }
        Err(_) => host == "localhost" || host.ends_with(".localhost"),
    };
    if !local {
        return Err(PairedHostError::Invalid(
            "paired host and inference endpoints must resolve only to operator local/VPN addresses"
                .into(),
        ));
    }
    Ok(())
}

fn is_local_ipv4(ip: Ipv4Addr) -> bool {
    let octets = ip.octets();
    ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || (octets[0] == 100 && (64..=127).contains(&octets[1]))
}

fn validate_artifact(
    artifact: &DelegatedHostArtifact,
    allowed_labels: &[String],
) -> Result<(), PairedHostError> {
    validate_id(&artifact.artifact_id, "artifact id", 128)?;
    if !matches!(artifact.kind.as_str(), "json" | "text" | "patch" | "opaque")
        || artifact.media_type.is_empty()
        || artifact.media_type.len() > 128
        || artifact.size_bytes > MAX_ARTIFACT_SIZE_BYTES
        || artifact.sha256.len() != 64
        || !artifact.sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
        || artifact
            .data_labels
            .iter()
            .any(|label| !allowed_labels.contains(label))
    {
        return Err(PairedHostError::Invalid(
            "remote artifact has an unsupported type, size, digest, or data label".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DbConfig, MigrationRunner};
    use ed25519_dalek::{Signer, SigningKey};

    fn store() -> PairedHostStore {
        let db = Database::open(&DbConfig::in_memory_unencrypted()).unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        PairedHostStore::new(db)
    }

    fn pair(store: &PairedHostStore, key: &SigningKey) {
        store
            .pair(&PairOperatorHostRequest {
                peer_id: "peer-a".into(),
                endpoint: "https://10.0.0.2:3131".into(),
                inference_endpoint: "https://10.0.0.2:8000/v1".into(),
                public_key_hex: hex::encode(key.verifying_key().as_bytes()),
                allowed_capabilities: vec!["read.files".into()],
                allowed_data_labels: vec!["public".into()],
                actor_trust: "Controller".into(),
                actor: "operator".into(),
                now: 100,
            })
            .unwrap();
    }

    #[test]
    fn rejects_unpaired_and_public_or_cloud_endpoints() {
        let store = store();
        assert!(matches!(
            store.receive_signed_task("not-paired", b"{}", &"00".repeat(64), 101),
            Err(PairedHostError::Unpaired(_))
        ));
        let key = SigningKey::from_bytes(&[7; 32]);
        for endpoint in ["https://8.8.8.8:443", "https://cloud.example/v1"] {
            assert!(
                store
                    .pair(&PairOperatorHostRequest {
                        peer_id: "peer-a".into(),
                        endpoint: endpoint.into(),
                        inference_endpoint: "https://10.0.0.2:8000".into(),
                        public_key_hex: hex::encode(key.verifying_key().as_bytes()),
                        allowed_capabilities: Vec::new(),
                        allowed_data_labels: Vec::new(),
                        actor_trust: "Controller".into(),
                        actor: "operator".into(),
                        now: 100,
                    })
                    .is_err()
            );
        }
    }

    #[test]
    fn signed_task_persists_across_reopen_and_cannot_expand_authority_or_labels() {
        let store = store();
        let key = SigningKey::from_bytes(&[9; 32]);
        pair(&store, &key);
        let payload = serde_json::to_vec(&serde_json::json!({
            "task_id":"task-1",
            "task":{"instruction":"summarize local report"},
            "requested_capabilities":["read.files","send.message"],
            "requested_data_labels":["public","secret"]
        }))
        .unwrap();
        let signature = hex::encode(key.sign(&payload).to_bytes());
        let task = store
            .receive_signed_task("peer-a", &payload, &signature, 101)
            .unwrap();
        assert_eq!(task.effective_capabilities, ["read.files"]);
        assert_eq!(task.effective_data_labels, ["public"]);
        assert_eq!(store.task("task-1").unwrap(), Some(task));
        store
            .transition_task("task-1", DelegatedHostTaskStatus::Accepted, 102)
            .unwrap();
        store
            .transition_task("task-1", DelegatedHostTaskStatus::Running, 103)
            .unwrap();
        assert_eq!(
            store.task("task-1").unwrap().unwrap().status,
            DelegatedHostTaskStatus::Running
        );
    }

    #[test]
    fn unknown_authority_fields_and_invalid_peer_signatures_fail_closed() {
        let store = store();
        let key = SigningKey::from_bytes(&[11; 32]);
        pair(&store, &key);
        let payload = br#"{"task_id":"task-2","task":{"instruction":"x"},"requested_capabilities":[],"requested_data_labels":[],"authority":["admin"]}"#;
        let signature = hex::encode(key.sign(payload).to_bytes());
        assert!(matches!(
            store.receive_signed_task("peer-a", payload, &signature, 101),
            Err(PairedHostError::Invalid(_))
        ));
        assert!(matches!(
            store.receive_signed_task("peer-a", b"{}", &"00".repeat(64), 101),
            Err(PairedHostError::InvalidSignature)
        ));
    }

    #[test]
    fn signed_typed_result_completes_only_the_matching_peer_task() {
        let store = store();
        let key = SigningKey::from_bytes(&[13; 32]);
        pair(&store, &key);
        let task_payload = serde_json::to_vec(&serde_json::json!({
            "task_id":"task-result",
            "task":{"instruction":"write summary"},
            "requested_capabilities":["read.files"],
            "requested_data_labels":["public"]
        }))
        .unwrap();
        let task_signature = hex::encode(key.sign(&task_payload).to_bytes());
        store
            .receive_signed_task("peer-a", &task_payload, &task_signature, 101)
            .unwrap();
        store
            .transition_task("task-result", DelegatedHostTaskStatus::Accepted, 102)
            .unwrap();
        store
            .transition_task("task-result", DelegatedHostTaskStatus::Running, 103)
            .unwrap();

        let result_payload = serde_json::to_vec(&serde_json::json!({
            "task_id":"task-result",
            "outcome":"completed",
            "artifacts":[{
                "artifact_id":"summary-1",
                "kind":"text",
                "media_type":"text/plain",
                "sha256":"a".repeat(64),
                "size_bytes":12,
                "data_labels":["public"]
            }]
        }))
        .unwrap();
        let signature = hex::encode(key.sign(&result_payload).to_bytes());
        let completed = store
            .record_signed_result("peer-a", &result_payload, &signature, 104)
            .unwrap();
        assert_eq!(completed.status, DelegatedHostTaskStatus::Completed);
        assert_eq!(completed.artifacts.len(), 1);
        assert_eq!(completed.artifacts[0].kind, "text");
        assert_eq!(store.task("task-result").unwrap(), Some(completed));
    }

    #[test]
    fn task_state_survives_database_reopen_for_transport_reconnect() {
        let dir = tempfile::tempdir().unwrap();
        let config = DbConfig {
            path: dir.path().join("paired-host.db"),
            key: None,
        };
        let db = Database::open(&config).unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        let store = PairedHostStore::new(db.clone());
        let key = SigningKey::from_bytes(&[17; 32]);
        pair(&store, &key);
        let payload = serde_json::to_vec(&serde_json::json!({
            "task_id":"reconnect-task",
            "task":{"instruction":"continue after reconnect"},
            "requested_capabilities":["read.files"],
            "requested_data_labels":["public"]
        }))
        .unwrap();
        let signature = hex::encode(key.sign(&payload).to_bytes());
        let before = store
            .receive_signed_task("peer-a", &payload, &signature, 101)
            .unwrap();
        drop(store);
        drop(db);

        let reopened = Database::open(&config).unwrap();
        MigrationRunner::new(&reopened).apply_all().unwrap();
        assert_eq!(
            PairedHostStore::new(reopened)
                .task("reconnect-task")
                .unwrap(),
            Some(before)
        );
    }

    #[test]
    fn outbound_task_is_durable_and_signed_with_pair_ceilings_applied() {
        let store = store();
        let remote = SigningKey::from_bytes(&[19; 32]);
        pair(&store, &remote);
        let local = SigningKey::from_bytes(&[23; 32]);
        let request = store
            .issue_signed_task(IssueDelegatedHostTask {
                peer_id: "peer-a",
                task_id: "outbound-1",
                task: serde_json::json!({"instruction":"inspect approved report"}),
                requested_capabilities: &["read.files".into(), "send.message".into()],
                requested_data_labels: &["public".into(), "secret".into()],
                local_identity: &local,
                now: 110,
            })
            .unwrap();
        let signature =
            Signature::from_slice(&hex::decode(request.signature_hex).unwrap()).unwrap();
        local
            .verifying_key()
            .verify(&request.payload, &signature)
            .unwrap();
        let envelope: SignedTaskEnvelope = serde_json::from_slice(&request.payload).unwrap();
        assert_eq!(envelope.requested_capabilities, ["read.files"]);
        assert_eq!(envelope.requested_data_labels, ["public"]);
        let task = store.task("outbound-1").unwrap().unwrap();
        assert_eq!(task.status, DelegatedHostTaskStatus::Pending);
        assert_eq!(task.effective_capabilities, ["read.files"]);
    }
}
