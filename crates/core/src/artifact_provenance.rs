//! Durable, fail-closed artifact provenance verification.

use crate::{Database, DbError};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::process::Command;
use thiserror::Error;

/// Executable or installable artifact classes covered by supply-chain policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactType {
    PluginZip,
    Subprocess,
    Sidecar,
    Installer,
    RunnerImage,
}

impl ArtifactType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PluginZip => "plugin_zip",
            Self::Subprocess => "subprocess",
            Self::Sidecar => "sidecar",
            Self::Installer => "installer",
            Self::RunnerImage => "runner_image",
        }
    }
}

/// Offline-verifiable SLSA/Sigstore claims shipped beside an artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProvenanceStatement {
    pub artifact_id: String,
    pub artifact_type: ArtifactType,
    pub artifact_locator: String,
    pub sha256: String,
    pub publisher_identity: String,
    pub source_repository: String,
    pub source_commit: String,
    pub workflow_identity: String,
    pub signature_reference: String,
    pub attestation_result: String,
    pub sbom_format: String,
    pub sbom_location: String,
    pub sbom_sha256: String,
}

/// Controller-owned verification policy loaded from SQLite.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactVerificationPolicy {
    pub allow_unsigned_local_development: bool,
    pub allowed_publishers: Vec<String>,
    pub allowed_source_repositories: Vec<String>,
    pub allowed_workflows: Vec<String>,
}

/// An operator-approved publisher or digest revocation with import provenance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactRevocation {
    pub revocation_id: String,
    pub scope: String,
    pub subject: String,
    pub source: String,
    pub freshness: String,
    pub issued_at: i64,
    pub expires_at: Option<i64>,
    pub revoked_at: i64,
    pub revoked_by: String,
    pub recovery_package: String,
    pub active: bool,
}

/// Installed artifacts affected by a revocation, including their enabled state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevocationImpact {
    pub artifact_id: String,
    pub artifact_type: String,
    pub artifact_locator: String,
    pub sha256: String,
    pub publisher_identity: String,
}

/// Offline imports retain where the snapshot came from and its validity window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RevocationFreshness {
    OnlineVerified,
    OfflineSnapshot,
}

impl RevocationFreshness {
    fn as_str(self) -> &'static str {
        match self {
            Self::OnlineVerified => "online_verified",
            Self::OfflineSnapshot => "offline_snapshot",
        }
    }
}

/// Operator-approved publisher or digest revocation imported as one audited decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactRevocationRequest {
    pub scope: String,
    pub subject: String,
    pub source: String,
    pub freshness: RevocationFreshness,
    pub issued_at: i64,
    pub expires_at: Option<i64>,
    pub recovery_package: String,
    pub actor_trust: String,
    pub actor: String,
}

/// Cryptographic verification boundary. Production can inject a cosign-backed
/// implementation; tests use deterministic offline fakes.
pub trait AttestationVerifier: Send + Sync {
    fn verify(&self, statement: &ProvenanceStatement) -> Result<(), String>;
}

/// Offline cosign adapter. The bundle must carry its transparency proof and
/// certificate material because production verification never uses a network
/// fallback.
#[derive(Debug, Clone)]
pub struct CosignCliVerifier {
    executable: String,
}

impl CosignCliVerifier {
    pub fn new(executable: impl Into<String>) -> Self {
        Self {
            executable: executable.into(),
        }
    }
}

impl AttestationVerifier for CosignCliVerifier {
    fn verify(&self, statement: &ProvenanceStatement) -> Result<(), String> {
        let output = Command::new(&self.executable)
            .env_clear()
            .envs(crate::process_environment::current_minimal_environment())
            .args([
                "verify-blob-attestation",
                "--offline",
                "--type",
                "slsaprovenance",
                "--bundle",
                &statement.signature_reference,
                "--certificate-identity",
                &statement.workflow_identity,
                "--certificate-oidc-issuer",
                &statement.publisher_identity,
                "--output-json",
                &statement.artifact_locator,
            ])
            .output()
            .map_err(|error| format!("could not execute '{}': {error}", self.executable))?;
        if !output.status.success() {
            return Err(format!(
                "cosign exited {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        let verified = String::from_utf8_lossy(&output.stdout);
        if !verified.contains(&statement.source_repository)
            || !verified.contains(&statement.source_commit)
        {
            return Err(
                "verified SLSA statement did not contain the allowlisted repository and commit"
                    .into(),
            );
        }
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum ArtifactVerificationError {
    #[error("database: {0}")]
    Db(#[from] DbError),
    #[error("invalid artifact metadata: {0}")]
    InvalidMetadata(String),
    #[error("artifact digest mismatch: expected {expected}, got {actual}")]
    DigestMismatch { expected: String, actual: String },
    #[error("publisher identity is not allowlisted: {0}")]
    PublisherNotAllowed(String),
    #[error("source repository is not allowlisted: {0}")]
    RepositoryNotAllowed(String),
    #[error("workflow identity is not allowlisted: {0}")]
    WorkflowNotAllowed(String),
    #[error("signature verification unavailable or failed: {0}")]
    Attestation(String),
    #[error("unsigned local development requires an explicit Controller override")]
    LocalOverrideRequired,
    #[error("only a Controller may change artifact verification policy")]
    ControllerRequired,
    #[error("artifact is revoked: {0}")]
    Revoked(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Clone)]
pub struct ArtifactProvenanceStore {
    db: Database,
}

impl ArtifactProvenanceStore {
    pub fn new(db: Database) -> Self {
        Self { db }
    }

    pub fn policy(&self) -> Result<ArtifactVerificationPolicy, ArtifactVerificationError> {
        self.db
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT allow_unsigned_local_development, allowed_publishers_json, \
                        allowed_source_repositories_json, allowed_workflows_json \
                   FROM config_artifact_verification WHERE singleton_id = 1",
                    [],
                    |row| {
                        let publishers: String = row.get(1)?;
                        let repositories: String = row.get(2)?;
                        let workflows: String = row.get(3)?;
                        Ok(ArtifactVerificationPolicy {
                            allow_unsigned_local_development: row.get::<_, i64>(0)? != 0,
                            allowed_publishers: serde_json::from_str(&publishers)
                                .unwrap_or_default(),
                            allowed_source_repositories: serde_json::from_str(&repositories)
                                .unwrap_or_default(),
                            allowed_workflows: serde_json::from_str(&workflows).unwrap_or_default(),
                        })
                    },
                )
                .map_err(DbError::from)
            })
            .map_err(Into::into)
    }

    pub fn configure(
        &self,
        actor_trust: &str,
        actor: &str,
        policy: &ArtifactVerificationPolicy,
    ) -> Result<(), ArtifactVerificationError> {
        if actor_trust != "Controller" {
            return Err(ArtifactVerificationError::ControllerRequired);
        }
        let now = chrono::Utc::now().timestamp();
        let publishers = serde_json::to_string(&policy.allowed_publishers)
            .map_err(|error| ArtifactVerificationError::InvalidMetadata(error.to_string()))?;
        let repositories = serde_json::to_string(&policy.allowed_source_repositories)
            .map_err(|error| ArtifactVerificationError::InvalidMetadata(error.to_string()))?;
        let workflows = serde_json::to_string(&policy.allowed_workflows)
            .map_err(|error| ArtifactVerificationError::InvalidMetadata(error.to_string()))?;
        self.db.transaction(|tx| {
            tx.execute(
                "UPDATE config_artifact_verification SET \
                    allow_unsigned_local_development=?1, allowed_publishers_json=?2, \
                    allowed_source_repositories_json=?3, allowed_workflows_json=?4, \
                    updated_at=?5, updated_by=?6 WHERE singleton_id=1",
                params![policy.allow_unsigned_local_development, publishers, repositories, workflows, now, actor],
            )?;
            tx.execute(
                "INSERT INTO state_artifact_verification_events(artifact_id,event_type,status,actor,detail_json,created_at) \
                 VALUES(NULL,'policy_changed','recorded',?1,?2,?3)",
                params![actor, serde_json::json!({"allow_unsigned_local_development": policy.allow_unsigned_local_development}).to_string(), now],
            )?;
            Ok(())
        })?;
        Ok(())
    }

    /// Install an operator-approved revocation. Offline snapshots are durable
    /// but retain their source, issuance, and expiry so their freshness is visible.
    pub fn revoke(
        &self,
        request: &ArtifactRevocationRequest,
    ) -> Result<(), ArtifactVerificationError> {
        if request.actor_trust != "Controller" {
            return Err(ArtifactVerificationError::ControllerRequired);
        }
        let normalized_subject = match request.scope.as_str() {
            "publisher" => request.subject.trim().to_owned(),
            "digest" => request
                .subject
                .trim()
                .strip_prefix("sha256:")
                .unwrap_or(request.subject.trim())
                .to_ascii_lowercase(),
            _ => {
                return Err(ArtifactVerificationError::InvalidMetadata(
                    "revocation scope must be publisher or digest".into(),
                ));
            }
        };
        if normalized_subject.is_empty()
            || normalized_subject.len() > 512
            || request.source.trim().is_empty()
            || request.source.len() > 1024
            || request.actor.trim().is_empty()
            || request.actor.len() > 128
            || request.recovery_package.len() > 4096
            || request.issued_at <= 0
            || request
                .expires_at
                .is_some_and(|expiry| expiry <= request.issued_at)
            || (request.scope == "digest"
                && (normalized_subject.len() != 64
                    || !normalized_subject
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit())))
        {
            return Err(ArtifactVerificationError::InvalidMetadata(
                "revocation metadata is invalid".into(),
            ));
        }
        let revoked_at = chrono::Utc::now().timestamp();
        let revocation_id = format!(
            "{}:{}",
            request.scope,
            sha256_bytes(normalized_subject.as_bytes())
        );
        let detail = serde_json::json!({"scope":request.scope,"subject":normalized_subject,"source":request.source,"freshness":request.freshness.as_str(),"issued_at":request.issued_at,"expires_at":request.expires_at,"recovery_package":request.recovery_package});
        self.db.transaction(|tx| {
            tx.execute(
                "INSERT INTO state_artifact_revocations(revocation_id,scope,subject,source,freshness,issued_at,expires_at,revoked_at,revoked_by,recovery_package,active) \
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,1) \
                 ON CONFLICT(scope,subject) DO UPDATE SET revocation_id=excluded.revocation_id,source=excluded.source,freshness=excluded.freshness,issued_at=excluded.issued_at,expires_at=excluded.expires_at,revoked_at=excluded.revoked_at,revoked_by=excluded.revoked_by,recovery_package=excluded.recovery_package,active=1",
                params![revocation_id, request.scope, normalized_subject, request.source, request.freshness.as_str(), request.issued_at, request.expires_at, revoked_at, request.actor, request.recovery_package],
            )?;
            tx.execute(
                "INSERT INTO state_artifact_verification_events(artifact_id,event_type,status,actor,detail_json,created_at) VALUES(NULL,'revocation_imported','active',?1,?2,?3)",
                params![request.actor, detail.to_string(), revoked_at],
            )?;
            Ok(())
        })?;
        Ok(())
    }

    /// Re-enable a subject only through an explicit Controller recovery action.
    pub fn clear_revocation(
        &self,
        scope: &str,
        subject: &str,
        actor_trust: &str,
        actor: &str,
    ) -> Result<bool, ArtifactVerificationError> {
        if actor_trust != "Controller" {
            return Err(ArtifactVerificationError::ControllerRequired);
        }
        if !matches!(scope, "publisher" | "digest") || actor.trim().is_empty() || actor.len() > 128
        {
            return Err(ArtifactVerificationError::InvalidMetadata(
                "revocation recovery metadata is invalid".into(),
            ));
        }
        let subject = if scope == "digest" {
            subject
                .strip_prefix("sha256:")
                .unwrap_or(subject)
                .to_ascii_lowercase()
        } else {
            subject.to_owned()
        };
        let now = chrono::Utc::now().timestamp();
        self.db.transaction(|tx| {
            let changed = tx.execute("UPDATE state_artifact_revocations SET active=0, revoked_by=?1, revoked_at=?2 WHERE scope=?3 AND subject=?4 AND active=1", params![actor, now, scope, subject])? != 0;
            if changed {
                tx.execute("INSERT INTO state_artifact_verification_events(artifact_id,event_type,status,actor,detail_json,created_at) VALUES(NULL,'revocation_cleared','recorded',?1,?2,?3)", params![actor, serde_json::json!({"scope":scope,"subject":subject}).to_string(), now])?;
            }
            Ok(changed)
        }).map_err(Into::into)
    }

    /// Return the active revocation metadata for audit and offline freshness review.
    pub fn revocations(&self) -> Result<Vec<ArtifactRevocation>, ArtifactVerificationError> {
        self.db.with_conn(|conn| {
            let mut query = conn.prepare("SELECT revocation_id,scope,subject,source,freshness,issued_at,expires_at,revoked_at,revoked_by,recovery_package,active FROM state_artifact_revocations ORDER BY scope,subject")?;
            let rows = query.query_map([], |row| Ok(ArtifactRevocation { revocation_id:row.get(0)?,scope:row.get(1)?,subject:row.get(2)?,source:row.get(3)?,freshness:row.get(4)?,issued_at:row.get(5)?,expires_at:row.get(6)?,revoked_at:row.get(7)?,revoked_by:row.get(8)?,recovery_package:row.get(9)?,active:row.get::<_,i64>(10)? != 0 }))?;
            rows.collect::<Result<Vec<_>,_>>().map_err(DbError::from)
        }).map_err(Into::into)
    }

    /// Inventory currently verified artifacts covered by an active revocation.
    pub fn revocation_impact(
        &self,
        scope: &str,
        subject: &str,
    ) -> Result<Vec<RevocationImpact>, ArtifactVerificationError> {
        let normalized = if scope == "digest" {
            subject
                .strip_prefix("sha256:")
                .unwrap_or(subject)
                .to_ascii_lowercase()
        } else {
            subject.to_owned()
        };
        self.db.with_conn(|conn| {
            let mut query = conn.prepare("SELECT artifact_id,artifact_type,artifact_locator,sha256,publisher_identity FROM state_artifact_provenance WHERE verification_status IN ('verified','local_development_override') AND (?1='publisher' AND publisher_identity=?2 OR ?1='digest' AND lower(sha256)=lower(?2))")?;
            let rows = query.query_map(params![scope, normalized], |row| Ok(RevocationImpact { artifact_id:row.get(0)?,artifact_type:row.get(1)?,artifact_locator:row.get(2)?,sha256:row.get(3)?,publisher_identity:row.get(4)? }))?;
            rows.collect::<Result<Vec<_>,_>>().map_err(DbError::from)
        }).map_err(Into::into)
    }

    fn ensure_not_revoked(
        &self,
        publisher: &str,
        digest: &str,
    ) -> Result<(), ArtifactVerificationError> {
        let current = chrono::Utc::now().timestamp();
        let revoked: Option<String> = self.db.with_conn(|conn| conn.query_row(
            "SELECT scope || ':' || subject FROM state_artifact_revocations WHERE active=1 AND (expires_at IS NULL OR expires_at>?1) AND ((scope='publisher' AND subject=?2) OR (scope='digest' AND lower(subject)=lower(?3))) LIMIT 1",
            params![current, publisher, digest], |row| row.get(0)).optional().map_err(DbError::from))?;
        if let Some(subject) = revoked {
            Err(ArtifactVerificationError::Revoked(subject))
        } else {
            Ok(())
        }
    }

    /// Refuse to start a previously installed artifact whose provenance is now revoked.
    pub fn ensure_locator_not_revoked(
        &self,
        artifact_type: ArtifactType,
        locator: &str,
    ) -> Result<(), ArtifactVerificationError> {
        let provenance: Option<(String, String)> = self.db.with_conn(|conn| {
            conn.query_row(
                "SELECT publisher_identity,sha256 FROM state_artifact_provenance WHERE artifact_type=?1 AND artifact_locator=?2",
                params![artifact_type.as_str(), locator],
                |row| Ok((row.get(0)?, row.get(1)?)),
            ).optional().map_err(DbError::from)
        })?;
        if let Some((publisher, digest)) = provenance {
            self.ensure_not_revoked(&publisher, &digest)?;
        }
        Ok(())
    }

    pub fn verify_bytes(
        &self,
        bytes: &[u8],
        statement: &ProvenanceStatement,
        verifier: Option<&dyn AttestationVerifier>,
    ) -> Result<(), ArtifactVerificationError> {
        validate_statement(statement)?;
        let actual = sha256_bytes(bytes);
        if actual != statement.sha256.to_ascii_lowercase() {
            return Err(ArtifactVerificationError::DigestMismatch {
                expected: statement.sha256.clone(),
                actual,
            });
        }
        self.ensure_not_revoked(&statement.publisher_identity, &actual)?;
        let policy = self.policy()?;
        require_allowlisted(&policy.allowed_publishers, &statement.publisher_identity)
            .map_err(ArtifactVerificationError::PublisherNotAllowed)?;
        require_allowlisted(
            &policy.allowed_source_repositories,
            &statement.source_repository,
        )
        .map_err(ArtifactVerificationError::RepositoryNotAllowed)?;
        require_allowlisted(&policy.allowed_workflows, &statement.workflow_identity)
            .map_err(ArtifactVerificationError::WorkflowNotAllowed)?;
        verifier
            .ok_or_else(|| ArtifactVerificationError::Attestation("no verifier configured".into()))?
            .verify(statement)
            .map_err(ArtifactVerificationError::Attestation)?;
        self.record(statement, "verified", "verifier")
    }

    pub fn verify_file_digest(
        &self,
        path: &Path,
        expected_sha256: &str,
    ) -> Result<(), ArtifactVerificationError> {
        let actual = sha256_bytes(&std::fs::read(path)?);
        if actual != expected_sha256.to_ascii_lowercase() {
            return Err(ArtifactVerificationError::DigestMismatch {
                expected: expected_sha256.to_owned(),
                actual,
            });
        }
        Ok(())
    }

    pub fn use_local_development_override(
        &self,
        artifact_id: &str,
        artifact_type: ArtifactType,
        locator: &str,
        actor_trust: &str,
        actor: &str,
    ) -> Result<(), ArtifactVerificationError> {
        if actor_trust != "Controller" || !self.policy()?.allow_unsigned_local_development {
            return Err(ArtifactVerificationError::LocalOverrideRequired);
        }
        let now = chrono::Utc::now().timestamp();
        self.db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO state_artifact_verification_events(artifact_id,event_type,status,actor,detail_json,created_at) \
                 VALUES(?1,'local_override_used','local_development_override',?2,?3,?4)",
                params![artifact_id, actor, serde_json::json!({"artifact_type": artifact_type.as_str(), "locator": locator}).to_string(), now],
            )?;
            Ok(())
        })?;
        Ok(())
    }

    pub fn statement_for(
        &self,
        artifact_type: ArtifactType,
        locator: &str,
    ) -> Result<Option<ProvenanceStatement>, ArtifactVerificationError> {
        let statement = self.db.with_conn(|conn| {
            let mut statement = conn.prepare(
                "SELECT artifact_id,artifact_locator,sha256,publisher_identity,source_repository,source_commit,workflow_identity,signature_reference,attestation_result,sbom_format,sbom_location,sbom_sha256 \
                 FROM state_artifact_provenance WHERE artifact_type=?1 AND artifact_locator=?2 AND verification_status='verified'",
            )?;
            let mut rows = statement.query(params![artifact_type.as_str(), locator])?;
            let Some(row) = rows.next()? else { return Ok(None); };
            Ok(Some(ProvenanceStatement {
                artifact_id: row.get(0)?, artifact_type, artifact_locator: row.get(1)?, sha256: row.get(2)?,
                publisher_identity: row.get(3)?, source_repository: row.get(4)?, source_commit: row.get(5)?,
                workflow_identity: row.get(6)?, signature_reference: row.get(7)?, attestation_result: row.get(8)?,
                sbom_format: row.get(9)?, sbom_location: row.get(10)?, sbom_sha256: row.get(11)?,
            }))
        }).map_err(ArtifactVerificationError::from)?;
        if let Some(statement) = &statement {
            self.ensure_not_revoked(&statement.publisher_identity, &statement.sha256)?;
        }
        Ok(statement)
    }

    pub fn digest_for_artifact_id(
        &self,
        artifact_id: &str,
    ) -> Result<Option<String>, ArtifactVerificationError> {
        self.db
            .with_conn(|conn| {
                let result = conn.query_row(
                    "SELECT sha256 FROM state_artifact_provenance WHERE artifact_id=?1 \
                     AND verification_status IN ('verified','local_development_override')",
                    params![artifact_id],
                    |row| row.get(0),
                );
                match result {
                    Ok(digest) => Ok(Some(digest)),
                    Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
                    Err(error) => Err(DbError::from(error)),
                }
            })
            .map_err(Into::into)
    }

    pub fn record_derived_file(
        &self,
        parent: &ProvenanceStatement,
        artifact_id: &str,
        artifact_type: ArtifactType,
        path: &Path,
    ) -> Result<(), ArtifactVerificationError> {
        let mut derived = parent.clone();
        derived.artifact_id = artifact_id.to_owned();
        derived.artifact_type = artifact_type;
        derived.artifact_locator = path.to_string_lossy().into_owned();
        derived.sha256 = sha256_bytes(&std::fs::read(path)?);
        derived.attestation_result = format!("derived from verified {}", parent.artifact_id);
        self.record(&derived, "verified", "derived-artifact")
    }

    pub fn record_local_file_override(
        &self,
        artifact_id: &str,
        artifact_type: ArtifactType,
        path: &Path,
        actor_trust: &str,
        actor: &str,
    ) -> Result<(), ArtifactVerificationError> {
        self.use_local_development_override(
            artifact_id,
            artifact_type,
            &path.to_string_lossy(),
            actor_trust,
            actor,
        )?;
        let local = ProvenanceStatement {
            artifact_id: artifact_id.to_owned(),
            artifact_type,
            artifact_locator: path.to_string_lossy().into_owned(),
            sha256: sha256_bytes(&std::fs::read(path)?),
            publisher_identity: "local-development-override".into(),
            source_repository: "local-development-override".into(),
            source_commit: "local-development-override".into(),
            workflow_identity: "local-development-override".into(),
            signature_reference: "local-development-override".into(),
            attestation_result: "Controller-approved unsigned local artifact".into(),
            sbom_format: "cyclonedx".into(),
            sbom_location: "local-development-override".into(),
            sbom_sha256: "0".repeat(64),
        };
        self.ensure_not_revoked(&local.publisher_identity, &local.sha256)?;
        self.record(&local, "local_development_override", actor)
    }

    /// Record a content-addressed local artifact when its original archive is unavailable.
    pub fn record_local_artifact_override(
        &self,
        statement: &ProvenanceStatement,
        actor_trust: &str,
        actor: &str,
    ) -> Result<(), ArtifactVerificationError> {
        if actor_trust != "Controller" || !self.policy()?.allow_unsigned_local_development {
            return Err(ArtifactVerificationError::LocalOverrideRequired);
        }
        validate_statement(statement)?;
        self.ensure_not_revoked(&statement.publisher_identity, &statement.sha256)?;
        self.record(statement, "local_development_override", actor)
    }

    /// Record a Controller's explicit per-installation approval for one exact OCI image digest.
    pub fn approve_controller_oci_reference(
        &self,
        artifact_type: ArtifactType,
        reference: &str,
        actor_trust: &str,
        actor: &str,
    ) -> Result<(), ArtifactVerificationError> {
        if actor_trust != "Controller" {
            return Err(ArtifactVerificationError::ControllerRequired);
        }
        let digest = reference
            .rsplit_once("@sha256:")
            .map(|(_, digest)| digest)
            .or_else(|| reference.strip_prefix("sha256:"))
            .ok_or_else(|| {
                ArtifactVerificationError::InvalidMetadata(
                    "Controller-approved OCI references must be digest-pinned".into(),
                )
            })?;
        if reference.trim().is_empty()
            || reference.len() > 512
            || digest.len() != 64
            || !digest.bytes().all(|byte| byte.is_ascii_hexdigit())
            || actor.trim().is_empty()
            || actor.len() > 128
        {
            return Err(ArtifactVerificationError::InvalidMetadata(
                "Controller-approved OCI reference metadata is invalid".into(),
            ));
        }
        let digest = digest.to_ascii_lowercase();
        let statement = ProvenanceStatement {
            artifact_id: format!("{}:workspace-toolchain:{digest}", artifact_type.as_str()),
            artifact_type,
            artifact_locator: reference.to_owned(),
            sha256: digest.clone(),
            publisher_identity: "controller-digest-approval".into(),
            source_repository: "controller-digest-approval".into(),
            source_commit: "controller-digest-approval".into(),
            workflow_identity: "controller-digest-approval".into(),
            signature_reference: "controller-digest-approval".into(),
            attestation_result: "Controller approved this exact OCI digest on this installation"
                .into(),
            sbom_format: "spdx".into(),
            sbom_location: "not-provided-local-controller-approval".into(),
            sbom_sha256: "0".repeat(64),
        };
        self.record(&statement, "local_development_override", actor)?;
        let now = chrono::Utc::now().timestamp();
        self.db.with_conn(|connection| {
            connection.execute(
                "INSERT INTO state_artifact_verification_events \
                 (artifact_id,event_type,status,actor,detail_json,created_at) \
                 VALUES (?1,'controller_approved_oci_reference','recorded',?2,?3,?4)",
                params![
                    statement.artifact_id,
                    actor,
                    serde_json::json!({"artifact_type":artifact_type.as_str(),"reference":reference,"sha256":digest}).to_string(),
                    now
                ],
            )?;
            Ok(())
        })?;
        Ok(())
    }

    pub fn authorize_oci_reference(
        &self,
        artifact_id: &str,
        artifact_type: ArtifactType,
        reference: &str,
        actor: &str,
    ) -> Result<(), ArtifactVerificationError> {
        if let Some((_, digest)) = reference.rsplit_once("@sha256:") {
            self.ensure_not_revoked("", digest)?;
        }
        if is_pinned_oci_reference(reference)
            && self.statement_for(artifact_type, reference)?.is_some()
        {
            return Ok(());
        }
        if self.has_local_override(artifact_type, reference)? {
            return Ok(());
        }
        if self.policy()?.allow_unsigned_local_development {
            return self.use_local_development_override(
                artifact_id,
                artifact_type,
                reference,
                "Controller",
                actor,
            );
        }
        if !is_pinned_oci_reference(reference) {
            return Err(ArtifactVerificationError::InvalidMetadata(format!(
                "floating OCI reference '{reference}' is forbidden; use name@sha256:<64 hex>"
            )));
        }
        Err(ArtifactVerificationError::Attestation(format!(
            "no verified provenance record for OCI reference '{reference}'"
        )))
    }

    /// Record a narrowly scoped compatibility override for a sidecar that was
    /// already operator-installed before provenance migration 0022 landed.
    /// Newer plugin installs are never grandfathered.
    pub fn grandfather_legacy_sidecar(
        &self,
        plugin_id: &str,
        service_name: &str,
        reference: &str,
    ) -> Result<bool, ArtifactVerificationError> {
        let eligible = self.db.with_conn(|conn| {
            conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM state_plugins p, schema_version m \
                 WHERE p.plugin_id = ?1 AND m.id = 22 AND p.installed_at <= m.applied_at)",
                [plugin_id],
                |row| row.get::<_, i64>(0),
            )
            .map(|value| value != 0)
            .map_err(DbError::from)
        })?;
        if !eligible || self.has_local_override(ArtifactType::Sidecar, reference)? {
            return Ok(false);
        }

        let artifact_id = format!("sidecar:{plugin_id}:{service_name}");
        let now = chrono::Utc::now().timestamp();
        let statement = ProvenanceStatement {
            artifact_id: artifact_id.clone(),
            artifact_type: ArtifactType::Sidecar,
            artifact_locator: reference.to_owned(),
            sha256: sha256_bytes(reference.as_bytes()),
            publisher_identity: "legacy-install-override".into(),
            source_repository: "legacy-install-override".into(),
            source_commit: "legacy-install-override".into(),
            workflow_identity: "legacy-install-override".into(),
            signature_reference: "legacy-install-override".into(),
            attestation_result: "grandfathered pre-migration sidecar".into(),
            sbom_format: "spdx".into(),
            sbom_location: "legacy-install-override".into(),
            sbom_sha256: "0".repeat(64),
        };
        self.record(&statement, "local_development_override", "migration-0022")?;
        self.db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO state_artifact_verification_events \
                 (artifact_id,event_type,status,actor,detail_json,created_at) \
                 VALUES(?1,'legacy_upgrade_override','recorded','migration-0022',?2,?3)",
                params![artifact_id, serde_json::json!({"plugin_id": plugin_id, "service_name": service_name, "reference": reference}).to_string(), now],
            )?;
            Ok(())
        })?;
        Ok(true)
    }

    fn has_local_override(
        &self,
        artifact_type: ArtifactType,
        reference: &str,
    ) -> Result<bool, ArtifactVerificationError> {
        self.db
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM state_artifact_provenance \
                     WHERE artifact_type=?1 AND artifact_locator=?2 \
                       AND verification_status='local_development_override') \
                     OR EXISTS(SELECT 1 FROM state_artifact_verification_events \
                     WHERE event_type='local_override_used' \
                       AND status='local_development_override' \
                       AND json_extract(detail_json,'$.artifact_type')=?1 \
                       AND json_extract(detail_json,'$.locator')=?2)",
                    params![artifact_type.as_str(), reference],
                    |row| row.get::<_, i64>(0),
                )
                .map(|value| value != 0)
                .map_err(DbError::from)
            })
            .map_err(Into::into)
    }

    fn record(
        &self,
        statement: &ProvenanceStatement,
        status: &str,
        actor: &str,
    ) -> Result<(), ArtifactVerificationError> {
        self.ensure_not_revoked(&statement.publisher_identity, &statement.sha256)?;
        let now = chrono::Utc::now().timestamp();
        self.db.transaction(|tx| {
            tx.execute(
                "INSERT INTO state_artifact_provenance(artifact_id,artifact_type,artifact_locator,sha256,publisher_identity,source_repository,source_commit,workflow_identity,signature_reference,attestation_result,sbom_format,sbom_location,sbom_sha256,verified_at,verification_status,created_at) \
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?14) \
                 ON CONFLICT DO UPDATE SET artifact_id=excluded.artifact_id,artifact_type=excluded.artifact_type,artifact_locator=excluded.artifact_locator,sha256=excluded.sha256,publisher_identity=excluded.publisher_identity,source_repository=excluded.source_repository,source_commit=excluded.source_commit,workflow_identity=excluded.workflow_identity,signature_reference=excluded.signature_reference,attestation_result=excluded.attestation_result,sbom_format=excluded.sbom_format,sbom_location=excluded.sbom_location,sbom_sha256=excluded.sbom_sha256,verified_at=excluded.verified_at,verification_status=excluded.verification_status",
                params![statement.artifact_id, statement.artifact_type.as_str(), statement.artifact_locator, statement.sha256.to_ascii_lowercase(), statement.publisher_identity, statement.source_repository, statement.source_commit, statement.workflow_identity, statement.signature_reference, statement.attestation_result, statement.sbom_format, statement.sbom_location, statement.sbom_sha256.to_ascii_lowercase(), now, status],
            )?;
            tx.execute(
                "INSERT INTO state_artifact_verification_events(artifact_id,event_type,status,actor,detail_json,created_at) VALUES(?1,'verification',?2,?3,'{}',?4)",
                params![statement.artifact_id, status, actor, now],
            )?;
            Ok(())
        })?;
        Ok(())
    }
}

pub fn sha256_bytes(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

pub fn verify_file_sha256(
    path: &Path,
    expected_sha256: &str,
) -> Result<(), ArtifactVerificationError> {
    let actual = sha256_bytes(&std::fs::read(path)?);
    if actual != expected_sha256.to_ascii_lowercase() {
        return Err(ArtifactVerificationError::DigestMismatch {
            expected: expected_sha256.to_owned(),
            actual,
        });
    }
    Ok(())
}

pub fn is_pinned_oci_reference(reference: &str) -> bool {
    let Some((_, digest)) = reference.rsplit_once("@sha256:") else {
        return false;
    };
    digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn require_allowlisted(allowed: &[String], actual: &str) -> Result<(), String> {
    allowed
        .iter()
        .any(|value| value == actual)
        .then_some(())
        .ok_or_else(|| actual.to_owned())
}

fn validate_statement(statement: &ProvenanceStatement) -> Result<(), ArtifactVerificationError> {
    let required = [
        ("artifact_id", statement.artifact_id.as_str()),
        ("artifact_locator", statement.artifact_locator.as_str()),
        ("publisher_identity", statement.publisher_identity.as_str()),
        ("source_repository", statement.source_repository.as_str()),
        ("source_commit", statement.source_commit.as_str()),
        ("workflow_identity", statement.workflow_identity.as_str()),
        (
            "signature_reference",
            statement.signature_reference.as_str(),
        ),
        ("attestation_result", statement.attestation_result.as_str()),
        ("sbom_location", statement.sbom_location.as_str()),
    ];
    if let Some((field, _)) = required
        .into_iter()
        .find(|(_, value)| value.trim().is_empty())
    {
        return Err(ArtifactVerificationError::InvalidMetadata(format!(
            "missing {field}"
        )));
    }
    if statement.sha256.len() != 64 || statement.sbom_sha256.len() != 64 {
        return Err(ArtifactVerificationError::InvalidMetadata(
            "SHA-256 digests must be 64 hexadecimal characters".into(),
        ));
    }
    if !matches!(statement.sbom_format.as_str(), "cyclonedx" | "spdx") {
        return Err(ArtifactVerificationError::InvalidMetadata(
            "SBOM format must be cyclonedx or spdx".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DbConfig, MigrationRunner};

    struct Accept;
    impl AttestationVerifier for Accept {
        fn verify(&self, _: &ProvenanceStatement) -> Result<(), String> {
            Ok(())
        }
    }

    fn store() -> ArtifactProvenanceStore {
        let db = Database::open(&DbConfig::in_memory_unencrypted()).unwrap();
        MigrationRunner::new(&db).apply_all().unwrap();
        ArtifactProvenanceStore::new(db)
    }

    fn set_local_override(store: &ArtifactProvenanceStore, enabled: bool) {
        let mut policy = store.policy().unwrap();
        policy.allow_unsigned_local_development = enabled;
        store.configure("Controller", "test", &policy).unwrap();
    }

    fn statement(bytes: &[u8]) -> ProvenanceStatement {
        ProvenanceStatement {
            artifact_id: "plugin:hello:1".into(),
            artifact_type: ArtifactType::PluginZip,
            artifact_locator: "hello-1.zip".into(),
            sha256: sha256_bytes(bytes),
            publisher_identity: "https://github.com/example".into(),
            source_repository: "https://github.com/example/execlaw".into(),
            source_commit: "0123456789abcdef".into(),
            workflow_identity: ".github/workflows/release.yml@refs/heads/main".into(),
            signature_reference: "hello.sigstore.json".into(),
            attestation_result: "slsa-v1.2 verified".into(),
            sbom_format: "cyclonedx".into(),
            sbom_location: "hello.cdx.json".into(),
            sbom_sha256: "a".repeat(64),
        }
    }

    #[test]
    fn audited_oci_override_survives_policy_being_disabled() {
        let store = store();
        let reference = format!("asternic/wuzapi@sha256:{}", "a".repeat(64));
        set_local_override(&store, true);
        store
            .authorize_oci_reference(
                "sidecar:whatsapp:wuzapi",
                ArtifactType::Sidecar,
                &reference,
                "test",
            )
            .unwrap();

        set_local_override(&store, false);
        store
            .authorize_oci_reference(
                "sidecar:whatsapp:wuzapi",
                ArtifactType::Sidecar,
                &reference,
                "test",
            )
            .unwrap();
    }

    #[test]
    fn only_plugins_installed_before_provenance_migration_are_grandfathered() {
        let store = store();
        let migration_time: i64 = store
            .db
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT applied_at FROM schema_version WHERE id = 22",
                    [],
                    |row| row.get(0),
                )
                .map_err(DbError::from)
            })
            .unwrap();
        store
            .db
            .with_conn(|conn| {
                conn.execute(
                    "INSERT INTO state_plugins \
                     (plugin_id,version,manifest_toml,stage_path,enabled,installed_at,updated_at) \
                     VALUES('whatsapp','0.2.14','','/tmp/whatsapp',1,?1,?1)",
                    [migration_time - 1],
                )?;
                conn.execute(
                    "INSERT INTO state_plugins \
                     (plugin_id,version,manifest_toml,stage_path,enabled,installed_at,updated_at) \
                     VALUES('new-plugin','1','','/tmp/new',1,?1,?1)",
                    [migration_time + 1],
                )?;
                Ok(())
            })
            .unwrap();

        let reference = "asternic/wuzapi:latest";
        assert!(
            store
                .grandfather_legacy_sidecar("whatsapp", "wuzapi", reference)
                .unwrap()
        );
        store
            .authorize_oci_reference(
                "sidecar:whatsapp:wuzapi",
                ArtifactType::Sidecar,
                reference,
                "test",
            )
            .unwrap();
        assert!(
            !store
                .grandfather_legacy_sidecar("new-plugin", "new", "example/new:latest")
                .unwrap()
        );
    }

    fn allow(store: &ArtifactProvenanceStore, local: bool) {
        store
            .configure(
                "Controller",
                "test",
                &ArtifactVerificationPolicy {
                    allow_unsigned_local_development: local,
                    allowed_publishers: vec!["https://github.com/example".into()],
                    allowed_source_repositories: vec!["https://github.com/example/execlaw".into()],
                    allowed_workflows: vec![".github/workflows/release.yml@refs/heads/main".into()],
                },
            )
            .unwrap();
    }

    #[test]
    fn valid_statement_is_persisted() {
        let store = store();
        allow(&store, false);
        store
            .verify_bytes(b"artifact", &statement(b"artifact"), Some(&Accept))
            .unwrap();
        assert!(
            store
                .statement_for(ArtifactType::PluginZip, "hello-1.zip")
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn controller_revocation_blocks_reinstall_and_reports_impact() {
        let store = store();
        allow(&store, false);
        let artifact = statement(b"artifact");
        store
            .verify_bytes(b"artifact", &artifact, Some(&Accept))
            .unwrap();
        assert_eq!(
            store
                .revocation_impact("publisher", &artifact.publisher_identity)
                .unwrap()
                .len(),
            1
        );
        assert!(matches!(
            store.revoke(&ArtifactRevocationRequest {
                scope: "publisher".into(),
                subject: artifact.publisher_identity.clone(),
                source: "operator incident package".into(),
                freshness: RevocationFreshness::OfflineSnapshot,
                issued_at: 100,
                expires_at: Some(i64::MAX),
                recovery_package: "recovery.tar".into(),
                actor_trust: "KnownTrusted".into(),
                actor: "operator".into(),
            }),
            Err(ArtifactVerificationError::ControllerRequired)
        ));
        store
            .revoke(&ArtifactRevocationRequest {
                scope: "publisher".into(),
                subject: artifact.publisher_identity.clone(),
                source: "operator incident package".into(),
                freshness: RevocationFreshness::OfflineSnapshot,
                issued_at: chrono::Utc::now().timestamp(),
                expires_at: None,
                recovery_package: "recovery.tar".into(),
                actor_trust: "Controller".into(),
                actor: "operator".into(),
            })
            .unwrap();
        assert!(matches!(
            store.verify_bytes(b"artifact", &artifact, Some(&Accept)),
            Err(ArtifactVerificationError::Revoked(_))
        ));
        assert!(matches!(
            store.statement_for(ArtifactType::PluginZip, "hello-1.zip"),
            Err(ArtifactVerificationError::Revoked(_))
        ));
        let stored = store.revocations().unwrap();
        assert_eq!(stored[0].freshness, "offline_snapshot");
        assert_eq!(stored[0].source, "operator incident package");
        assert_eq!(stored[0].recovery_package, "recovery.tar");
        assert!(
            store
                .clear_revocation(
                    "publisher",
                    &artifact.publisher_identity,
                    "Controller",
                    "operator"
                )
                .unwrap()
        );
        store
            .verify_bytes(b"artifact", &artifact, Some(&Accept))
            .unwrap();
    }

    #[test]
    fn digest_revocation_blocks_digest_pinned_cache_reuse() {
        let store = store();
        let digest = sha256_bytes(b"artifact");
        store
            .revoke(&ArtifactRevocationRequest {
                scope: "digest".into(),
                subject: format!("sha256:{digest}"),
                source: "offline advisory".into(),
                freshness: RevocationFreshness::OfflineSnapshot,
                issued_at: chrono::Utc::now().timestamp(),
                expires_at: None,
                recovery_package: String::new(),
                actor_trust: "Controller".into(),
                actor: "operator".into(),
            })
            .unwrap();
        let reference = format!("example/plugin@sha256:{digest}");
        assert!(matches!(
            store.authorize_oci_reference(
                "sidecar:x",
                ArtifactType::Sidecar,
                &reference,
                "install"
            ),
            Err(ArtifactVerificationError::Revoked(_))
        ));
        assert!(
            store
                .revocation_impact("digest", &digest)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn mismatched_digest_is_rejected() {
        let store = store();
        allow(&store, false);
        let error = store
            .verify_bytes(b"mutated", &statement(b"artifact"), Some(&Accept))
            .unwrap_err();
        assert!(matches!(
            error,
            ArtifactVerificationError::DigestMismatch { .. }
        ));
    }

    #[test]
    fn wrong_identity_repo_and_workflow_are_rejected() {
        let store = store();
        allow(&store, false);
        for field in ["publisher", "repository", "workflow"] {
            let mut value = statement(b"artifact");
            match field {
                "publisher" => value.publisher_identity = "wrong".into(),
                "repository" => value.source_repository = "wrong".into(),
                _ => value.workflow_identity = "wrong".into(),
            }
            assert!(
                store
                    .verify_bytes(b"artifact", &value, Some(&Accept))
                    .is_err()
            );
        }
    }

    #[test]
    fn missing_signature_or_sbom_is_rejected() {
        let store = store();
        allow(&store, false);
        let mut missing_signature = statement(b"artifact");
        missing_signature.signature_reference.clear();
        assert!(matches!(
            store.verify_bytes(b"artifact", &missing_signature, Some(&Accept)),
            Err(ArtifactVerificationError::InvalidMetadata(_))
        ));
        let mut missing_sbom = statement(b"artifact");
        missing_sbom.sbom_location.clear();
        assert!(matches!(
            store.verify_bytes(b"artifact", &missing_sbom, Some(&Accept)),
            Err(ArtifactVerificationError::InvalidMetadata(_))
        ));
    }

    #[test]
    fn local_override_requires_controller_and_is_audited() {
        let store = store();
        allow(&store, true);
        assert!(
            store
                .use_local_development_override(
                    "local:x",
                    ArtifactType::PluginZip,
                    "x.zip",
                    "KnownTrusted",
                    "user"
                )
                .is_err()
        );
        store
            .use_local_development_override(
                "local:x",
                ArtifactType::PluginZip,
                "x.zip",
                "Controller",
                "operator",
            )
            .unwrap();
        let count: i64 = store.db.with_conn(|conn| conn.query_row("SELECT count(*) FROM state_artifact_verification_events WHERE event_type='local_override_used'", [], |row| row.get(0)).map_err(DbError::from)).unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn floating_oci_tags_are_rejected() {
        assert!(!is_pinned_oci_reference("example/service:latest"));
        assert!(!is_pinned_oci_reference("example/service:1.2.3"));
        assert!(is_pinned_oci_reference(&format!(
            "example/service@sha256:{}",
            "a".repeat(64)
        )));
    }

    #[test]
    fn oci_launch_requires_persisted_provenance_or_audited_override() {
        let store = store();
        let floating = "example/service:latest";
        assert!(
            store
                .authorize_oci_reference("sidecar:x", ArtifactType::Sidecar, floating, "test")
                .is_err()
        );
        allow(&store, true);
        store
            .authorize_oci_reference("sidecar:x", ArtifactType::Sidecar, floating, "test")
            .unwrap();
        let count: i64 = store.db.with_conn(|conn| conn.query_row("SELECT count(*) FROM state_artifact_verification_events WHERE artifact_id='sidecar:x' AND event_type='local_override_used'", [], |row| row.get(0)).map_err(DbError::from)).unwrap();
        assert_eq!(count, 1);
    }
}
