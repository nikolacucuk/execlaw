CREATE TABLE state_tool_result_artifacts (
    artifact_id TEXT PRIMARY KEY REFERENCES state_artifacts(id) ON DELETE CASCADE,
    conversation_id TEXT NOT NULL REFERENCES state_conversations(conversation_id) ON DELETE CASCADE,
    run_id TEXT NOT NULL REFERENCES state_runs(run_id) ON DELETE CASCADE,
    sha256 TEXT NOT NULL CHECK(length(sha256)=64),
    byte_length INTEGER NOT NULL CHECK(byte_length >= 0),
    mime_type TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL
);
CREATE INDEX idx_tool_result_artifacts_scope
    ON state_tool_result_artifacts(conversation_id, run_id, expires_at);
