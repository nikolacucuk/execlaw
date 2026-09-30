-- Immutable hashes of the effective inputs supplied to each model turn.
-- Content stays in the existing event log and configuration stores.
CREATE TABLE state_run_input_manifests (
    run_id TEXT PRIMARY KEY REFERENCES state_runs(run_id) ON DELETE CASCADE,
    input_version INTEGER NOT NULL CHECK(input_version > 0),
    prompt_hash TEXT NOT NULL,
    model_settings_hash TEXT NOT NULL,
    tool_catalog_hash TEXT NOT NULL,
    recorded_at INTEGER NOT NULL
);
