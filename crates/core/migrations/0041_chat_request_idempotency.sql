CREATE TABLE state_chat_request_keys (
    principal_id TEXT NOT NULL,
    conversation_id TEXT NOT NULL,
    client_request_id TEXT NOT NULL,
    body_hash TEXT NOT NULL CHECK (length(body_hash) = 64),
    run_id TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('pending', 'completed', 'unknown')),
    response_status INTEGER,
    response_json TEXT,
    outcome_detail TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (principal_id, conversation_id, client_request_id),
    CHECK ((status = 'completed') = (response_status IS NOT NULL AND response_json IS NOT NULL))
);

CREATE INDEX idx_chat_request_keys_run ON state_chat_request_keys(run_id);
