CREATE TABLE state_resource_versions (
    resource_key TEXT PRIMARY KEY NOT NULL,
    version_token TEXT NOT NULL,
    conditional_updates INTEGER NOT NULL DEFAULT 0 CHECK (conditional_updates IN (0, 1)),
    observed_at INTEGER NOT NULL,
    CHECK (length(resource_key) BETWEEN 1 AND 256),
    CHECK (length(version_token) BETWEEN 1 AND 512)
);

CREATE INDEX idx_state_resource_versions_observed
    ON state_resource_versions(observed_at);

CREATE TABLE state_chain_compensations (
    id TEXT PRIMARY KEY NOT NULL,
    original_run_id TEXT NOT NULL REFERENCES state_chain_runs(id) ON DELETE CASCADE,
    original_step_index INTEGER NOT NULL,
    original_outbox_key TEXT NOT NULL,
    compensation_plan_id TEXT NOT NULL REFERENCES state_chain_plans(id) ON DELETE CASCADE,
    approval_run_id TEXT NOT NULL REFERENCES state_chain_runs(id) ON DELETE CASCADE,
    status TEXT NOT NULL CHECK (status IN ('awaiting_approval', 'enqueued', 'denied', 'failed')),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    UNIQUE(original_run_id, original_step_index)
);

CREATE INDEX idx_state_chain_compensations_original
    ON state_chain_compensations(original_run_id, original_step_index);
