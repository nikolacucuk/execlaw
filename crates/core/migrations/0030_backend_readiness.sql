CREATE TABLE state_backend_readiness (
    purpose TEXT PRIMARY KEY REFERENCES config_backends(purpose) ON DELETE CASCADE,
    model_id TEXT,
    stage TEXT NOT NULL,
    last_observed_at INTEGER NOT NULL,
    last_success_at INTEGER,
    stage_changed_at INTEGER NOT NULL
);