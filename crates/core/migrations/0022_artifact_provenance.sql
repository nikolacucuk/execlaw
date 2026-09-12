-- Durable supply-chain identity and verification state for executable and
-- installable artifacts. Verification events are append-only audit records;
-- current policy remains SQLite-backed and Controller-owned.

CREATE TABLE config_artifact_verification (
    singleton_id INTEGER PRIMARY KEY CHECK (singleton_id = 1),
    allow_unsigned_local_development INTEGER NOT NULL DEFAULT 0
        CHECK (allow_unsigned_local_development IN (0, 1)),
    allowed_publishers_json TEXT NOT NULL DEFAULT '[]',
    allowed_source_repositories_json TEXT NOT NULL DEFAULT '[]',
    allowed_workflows_json TEXT NOT NULL DEFAULT '[]',
    updated_at INTEGER NOT NULL,
    updated_by TEXT NOT NULL
);

INSERT INTO config_artifact_verification(
    singleton_id, allow_unsigned_local_development,
    allowed_publishers_json, allowed_source_repositories_json,
    allowed_workflows_json, updated_at, updated_by
) VALUES (1, 0, '[]', '[]', '[]', strftime('%s','now'), 'migration');

CREATE TABLE state_artifact_provenance (
    artifact_id TEXT PRIMARY KEY,
    artifact_type TEXT NOT NULL CHECK (artifact_type IN (
        'plugin_zip', 'subprocess', 'sidecar', 'installer', 'runner_image'
    )),
    artifact_locator TEXT NOT NULL,
    sha256 TEXT NOT NULL CHECK (length(sha256) = 64),
    publisher_identity TEXT NOT NULL,
    source_repository TEXT NOT NULL,
    source_commit TEXT NOT NULL,
    workflow_identity TEXT NOT NULL,
    signature_reference TEXT NOT NULL,
    attestation_result TEXT NOT NULL,
    sbom_format TEXT NOT NULL CHECK (sbom_format IN ('cyclonedx', 'spdx')),
    sbom_location TEXT NOT NULL,
    sbom_sha256 TEXT NOT NULL CHECK (length(sbom_sha256) = 64),
    verified_at INTEGER NOT NULL,
    verification_status TEXT NOT NULL CHECK (verification_status IN (
        'verified', 'local_development_override', 'rejected'
    )),
    created_at INTEGER NOT NULL,
    UNIQUE(artifact_type, artifact_locator)
);

CREATE INDEX idx_state_artifact_provenance_digest
    ON state_artifact_provenance(sha256);

CREATE TABLE state_artifact_verification_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    artifact_id TEXT,
    event_type TEXT NOT NULL,
    status TEXT NOT NULL,
    actor TEXT NOT NULL,
    detail_json TEXT NOT NULL,
    created_at INTEGER NOT NULL
);

CREATE INDEX idx_state_artifact_verification_events_artifact
    ON state_artifact_verification_events(artifact_id, created_at);