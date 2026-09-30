CREATE TABLE state_model_capability_profiles (
    identity_hash TEXT PRIMARY KEY,
    model_id TEXT NOT NULL,
    quantization TEXT NOT NULL,
    chat_template TEXT NOT NULL,
    backend_version TEXT NOT NULL,
    parser_version TEXT NOT NULL,
    context_tokens INTEGER NOT NULL CHECK(context_tokens > 0),
    observed_json TEXT NOT NULL,
    qualified_at INTEGER NOT NULL,
    invalidated_at INTEGER
);

CREATE TABLE state_compaction_receipts (
    receipt_id TEXT PRIMARY KEY,
    conversation_id TEXT NOT NULL,
    source_start_seq INTEGER NOT NULL,
    source_end_seq INTEGER NOT NULL,
    source_fingerprint TEXT NOT NULL,
    summary_version INTEGER NOT NULL,
    retained_constraints_json TEXT NOT NULL,
    pending_work_json TEXT NOT NULL,
    discarded_content_json TEXT NOT NULL,
    trust_class TEXT NOT NULL,
    summary TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    FOREIGN KEY(conversation_id) REFERENCES state_conversations(conversation_id)
);
CREATE INDEX idx_compaction_receipts_source
    ON state_compaction_receipts(conversation_id, source_start_seq, source_end_seq);

CREATE TABLE state_run_trace_events (
    cursor INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id TEXT NOT NULL REFERENCES state_runs(run_id) ON DELETE CASCADE,
    event_kind TEXT NOT NULL,
    subject_id TEXT,
    status TEXT NOT NULL,
    metadata_json TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE INDEX idx_run_trace_events_run_cursor ON state_run_trace_events(run_id, cursor);
