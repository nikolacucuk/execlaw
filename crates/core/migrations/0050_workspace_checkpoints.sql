CREATE TABLE state_workspace_roots (
    workspace_id TEXT PRIMARY KEY,
    canonical_path TEXT NOT NULL UNIQUE,
    created_at INTEGER NOT NULL,
    created_by TEXT NOT NULL
);

CREATE TABLE state_workspace_blobs (
    sha256 TEXT PRIMARY KEY,
    byte_length INTEGER NOT NULL CHECK (byte_length >= 0),
    contents BLOB NOT NULL,
    created_at INTEGER NOT NULL
);

CREATE TABLE state_workspace_checkpoints (
    checkpoint_id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL REFERENCES state_workspace_roots(workspace_id),
    run_id TEXT NOT NULL REFERENCES state_runs(run_id),
    parent_checkpoint_id TEXT REFERENCES state_workspace_checkpoints(checkpoint_id),
    manifest_json TEXT NOT NULL,
    total_bytes INTEGER NOT NULL CHECK (total_bytes >= 0),
    file_count INTEGER NOT NULL CHECK (file_count >= 0),
    created_at INTEGER NOT NULL
);

CREATE INDEX idx_workspace_checkpoints_run ON state_workspace_checkpoints(run_id,created_at);
CREATE INDEX idx_workspace_blobs_retention ON state_workspace_blobs(created_at);
