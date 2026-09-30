CREATE TABLE state_run_workspace_bindings (
    run_id TEXT PRIMARY KEY REFERENCES state_runs(run_id) ON DELETE CASCADE,
    workspace_id TEXT NOT NULL REFERENCES state_workspace_roots(workspace_id),
    checkpoint_id TEXT NOT NULL REFERENCES state_workspace_checkpoints(checkpoint_id),
    checkout_path TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
