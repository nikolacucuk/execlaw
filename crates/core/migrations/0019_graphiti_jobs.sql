CREATE TABLE state_graphiti_jobs (
    job_id            TEXT PRIMARY KEY,
    kind              TEXT NOT NULL CHECK(kind IN ('ingest', 'reconcile')),
    status            TEXT NOT NULL DEFAULT 'pending'
                      CHECK(status IN ('pending', 'running', 'completed', 'failed')),
    conversation_id   TEXT NOT NULL,
    trust_class       TEXT NOT NULL,
    source_event_seq  INTEGER NOT NULL CHECK(source_event_seq > 0),
    evidence_id       TEXT NOT NULL,
    payload_json      TEXT NOT NULL,
    attempt           INTEGER NOT NULL DEFAULT 0 CHECK(attempt >= 0),
    max_attempts      INTEGER NOT NULL DEFAULT 5 CHECK(max_attempts > 0),
    next_attempt_at   INTEGER NOT NULL,
    lease_owner       TEXT,
    lease_expires_at  INTEGER,
    last_error        TEXT,
    created_at        INTEGER NOT NULL,
    updated_at        INTEGER NOT NULL,
    completed_at      INTEGER,
    UNIQUE(kind, evidence_id),
    FOREIGN KEY(conversation_id, source_event_seq)
        REFERENCES state_events(conversation_id, seq),
    CHECK((lease_owner IS NULL) = (lease_expires_at IS NULL)),
    CHECK(status = 'running' OR lease_owner IS NULL),
    CHECK(status != 'running' OR lease_owner IS NOT NULL),
    CHECK(status = 'completed' OR completed_at IS NULL)
);

CREATE INDEX idx_state_graphiti_jobs_claim
    ON state_graphiti_jobs(status, next_attempt_at, lease_expires_at, created_at);

CREATE INDEX idx_state_graphiti_jobs_conversation
    ON state_graphiti_jobs(conversation_id, created_at);