-- Immutable identities that an in-flight durable run is authorized to invoke.
CREATE TABLE state_run_tool_implementation_pins (
    run_id TEXT NOT NULL REFERENCES state_runs(run_id) ON DELETE CASCADE,
    runtime_kind TEXT NOT NULL CHECK(runtime_kind IN ('plugin', 'mcp')),
    plugin_id TEXT NOT NULL,
    plugin_version TEXT NOT NULL,
    tool_name TEXT NOT NULL,
    artifact_sha256 TEXT NOT NULL CHECK(length(artifact_sha256) = 64),
    input_schema_sha256 TEXT,
    result_schema_sha256 TEXT,
    server_identity TEXT,
    created_at INTEGER NOT NULL,
    PRIMARY KEY(run_id, tool_name)
);

CREATE INDEX idx_run_tool_pins_plugin
    ON state_run_tool_implementation_pins(runtime_kind, plugin_id, run_id);

CREATE TABLE state_plugin_mutation_leases (
    plugin_id TEXT PRIMARY KEY,
    acquired_at INTEGER NOT NULL
);
