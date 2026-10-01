CREATE TABLE state_workspace_patch_jobs (
    run_id TEXT NOT NULL REFERENCES state_runs(run_id) ON DELETE CASCADE,
    request_id TEXT NOT NULL,
    request_hash TEXT NOT NULL CHECK (length(request_hash) = 64),
    status TEXT NOT NULL CHECK (status IN ('running','succeeded','failed')),
    result_json TEXT,
    error_code TEXT,
    lease_owner TEXT,
    lease_expires_at INTEGER,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY(run_id, request_id),
    CHECK ((lease_owner IS NULL) = (lease_expires_at IS NULL)),
    CHECK (status = 'running' OR lease_owner IS NULL)
);
