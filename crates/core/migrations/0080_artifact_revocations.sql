-- Operator-approved revocations are separate from provenance: a valid
-- signature proves identity, but cannot override a later compromise decision.
CREATE TABLE state_artifact_revocations (
    revocation_id TEXT PRIMARY KEY,
    scope TEXT NOT NULL CHECK (scope IN ('publisher', 'digest')),
    subject TEXT NOT NULL,
    source TEXT NOT NULL,
    freshness TEXT NOT NULL CHECK (freshness IN ('online_verified', 'offline_snapshot')),
    issued_at INTEGER NOT NULL,
    expires_at INTEGER,
    revoked_at INTEGER NOT NULL,
    revoked_by TEXT NOT NULL,
    recovery_package TEXT NOT NULL DEFAULT '',
    active INTEGER NOT NULL DEFAULT 1 CHECK (active IN (0, 1)),
    UNIQUE(scope, subject)
);

CREATE INDEX idx_artifact_revocations_active ON state_artifact_revocations(active, scope, subject);
