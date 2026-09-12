//! Durable, fail-closed artifact provenance verification.

use crate::{Database, DbError};
use rusqlite::params;
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
        self.db.with_conn(|conn| {
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
        }).map_err(Into::into)
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
        self.record(&local, "local_development_override", actor)
    }

    pub fn authorize_oci_reference(
        &self,
        artifact_id: &str,
        artifact_type: ArtifactType,
        reference: &str,
        actor: &str,
    ) -> Result<(), ArtifactVerificationError> {
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
                       AND verification_status='local_development_override')",
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
        let now = chrono::Utc::now().timestamp();
        self.db.transaction(|tx| {
            tx.execute(
                "INSERT INTO state_artifact_provenance(artifact_id,artifact_type,artifact_locator,sha256,publisher_identity,source_repository,source_commit,workflow_identity,signature_reference,attestation_result,sbom_format,sbom_location,sbom_sha256,verified_at,verification_status,created_at) \
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?14) \
                 ON CONFLICT(artifact_id) DO UPDATE SET sha256=excluded.sha256,publisher_identity=excluded.publisher_identity,source_repository=excluded.source_repository,source_commit=excluded.source_commit,workflow_identity=excluded.workflow_identity,signature_reference=excluded.signature_reference,attestation_result=excluded.attestation_result,sbom_format=excluded.sbom_format,sbom_location=excluded.sbom_location,sbom_sha256=excluded.sbom_sha256,verified_at=excluded.verified_at,verification_status=excluded.verification_status",
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
        let reference = "asternic/wuzapi:latest";
        set_local_override(&store, true);
        store
            .authorize_oci_reference(
                "sidecar:whatsapp:wuzapi",
                ArtifactType::Sidecar,
                reference,
                "test",
            )
            .unwrap();

        set_local_override(&store, false);
        store
            .authorize_oci_reference(
                "sidecar:whatsapp:wuzapi",
                ArtifactType::Sidecar,
                reference,
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
