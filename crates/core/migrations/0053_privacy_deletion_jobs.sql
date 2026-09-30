-- Durable privacy deletion queue and tombstones. Completed rows remain so
-- restoring a database snapshot cannot make a deleted research job visible.
CREATE TABLE state_privacy_deletion_jobs (
    deletion_id TEXT PRIMARY KEY,
    resource_kind TEXT NOT NULL CHECK (resource_kind = 'research_job'),
    resource_id TEXT NOT NULL,
    requested_by TEXT NOT NULL,
    request_source TEXT NOT NULL CHECK (request_source IN ('controller', 'retention')),
    payload_json TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('pending', 'complete')),
    attempt_count INTEGER NOT NULL DEFAULT 0 CHECK (attempt_count >= 0),
    last_error TEXT,
    requested_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    completed_at INTEGER,
    UNIQUE (resource_kind, resource_id),
    CHECK ((status = 'complete') = (completed_at IS NOT NULL))
);

CREATE INDEX idx_privacy_deletion_pending
    ON state_privacy_deletion_jobs(status, requested_at)
    WHERE status = 'pending';
