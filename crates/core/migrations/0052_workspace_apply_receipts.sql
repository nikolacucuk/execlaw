CREATE TABLE state_workspace_apply_receipts (
    apply_id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL REFERENCES state_runs(run_id),
    workspace_id TEXT NOT NULL REFERENCES state_workspace_roots(workspace_id),
    checkpoint_id TEXT NOT NULL REFERENCES state_workspace_checkpoints(checkpoint_id),
    client_request_id TEXT NOT NULL,
    request_hash TEXT NOT NULL,
    preview_hash TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('applying','applied','conflict','failed')),
    detail_json TEXT,
    lease_owner TEXT,
    lease_expires_at INTEGER,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    UNIQUE(run_id,client_request_id),
    CHECK ((lease_owner IS NULL) = (lease_expires_at IS NULL)),
    CHECK (status='applying' OR lease_owner IS NULL)
);

CREATE TABLE state_workspace_apply_files (
    apply_id TEXT NOT NULL REFERENCES state_workspace_apply_receipts(apply_id) ON DELETE CASCADE,
    path TEXT NOT NULL,
    base_sha256 TEXT,
    proposed_sha256 TEXT,
    status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending','applied','conflict','failed')),
    error TEXT,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY(apply_id,path)
);

CREATE TABLE state_workspace_apply_locks (
    workspace_id TEXT PRIMARY KEY REFERENCES state_workspace_roots(workspace_id) ON DELETE CASCADE,
    apply_id TEXT NOT NULL REFERENCES state_workspace_apply_receipts(apply_id) ON DELETE CASCADE,
    lease_owner TEXT NOT NULL,
    lease_expires_at INTEGER NOT NULL
);
