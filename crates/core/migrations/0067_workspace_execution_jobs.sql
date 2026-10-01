CREATE TABLE config_workspace_execution (
    singleton_id INTEGER PRIMARY KEY CHECK (singleton_id = 1),
    image_reference TEXT,
    language_servers_json TEXT NOT NULL DEFAULT '{}',
    updated_at INTEGER NOT NULL,
    updated_by TEXT NOT NULL
);

INSERT INTO config_workspace_execution(singleton_id, image_reference, language_servers_json, updated_at, updated_by)
VALUES (1, NULL, '{}', strftime('%s','now'), 'migration');

CREATE TABLE state_workspace_execution_jobs (
    run_id TEXT NOT NULL REFERENCES state_runs(run_id) ON DELETE CASCADE,
    job_id TEXT NOT NULL,
    request_hash TEXT NOT NULL CHECK (length(request_hash) = 64),
    operation TEXT NOT NULL CHECK (operation IN ('terminal','diagnostics')),
    status TEXT NOT NULL CHECK (status IN ('running','succeeded','failed')),
    result_json TEXT,
    error_code TEXT,
    lease_owner TEXT,
    lease_expires_at INTEGER,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY(run_id, job_id),
    CHECK ((lease_owner IS NULL) = (lease_expires_at IS NULL)),
    CHECK (status = 'running' OR lease_owner IS NULL)
);

CREATE INDEX idx_workspace_execution_jobs_status
    ON state_workspace_execution_jobs(status, lease_expires_at);
