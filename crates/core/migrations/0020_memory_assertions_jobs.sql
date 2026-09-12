CREATE TABLE memory_assertions (
    assertion_id       TEXT PRIMARY KEY,
    scope              TEXT NOT NULL,
    trust_class        TEXT NOT NULL,
    kind               TEXT NOT NULL
                       CHECK(kind IN ('profile', 'semantic', 'episodic', 'procedural', 'summary', 'decision')),
    subject            TEXT NOT NULL,
    predicate          TEXT NOT NULL,
    object_json        TEXT NOT NULL CHECK(json_valid(object_json)),
    confidence         REAL NOT NULL CHECK(confidence >= 0.0 AND confidence <= 1.0),
    status             TEXT NOT NULL
                       CHECK(status IN ('proposed', 'approved', 'rejected', 'retracted')),
    observed_from      INTEGER NOT NULL,
    observed_to        INTEGER,
    valid_from         INTEGER NOT NULL,
    valid_to           INTEGER,
    supersedes_id      TEXT,
    extraction_run_id  TEXT NOT NULL,
    created_event_seq  INTEGER NOT NULL CHECK(created_event_seq > 0),
    created_at         INTEGER NOT NULL,
    FOREIGN KEY(supersedes_id) REFERENCES memory_assertions(assertion_id),
    CHECK(observed_to IS NULL OR observed_to >= observed_from),
    CHECK(valid_to IS NULL OR valid_to > valid_from),
    CHECK(supersedes_id IS NULL OR supersedes_id <> assertion_id)
);

CREATE INDEX idx_memory_assertions_current
    ON memory_assertions(scope, trust_class, status, valid_from, valid_to);
CREATE INDEX idx_memory_assertions_supersedes
    ON memory_assertions(supersedes_id) WHERE supersedes_id IS NOT NULL;

CREATE TABLE memory_evidence (
    evidence_id       TEXT PRIMARY KEY,
    assertion_id      TEXT NOT NULL,
    conversation_id  TEXT NOT NULL,
    event_seq         INTEGER NOT NULL CHECK(event_seq > 0),
    payload_path      TEXT NOT NULL,
    quote_hash        TEXT NOT NULL CHECK(length(quote_hash) = 64),
    evidence_kind     TEXT NOT NULL
                      CHECK(evidence_kind IN ('direct_quote', 'tool_result', 'operator_correction', 'derived')),
    created_at        INTEGER NOT NULL,
    FOREIGN KEY(assertion_id) REFERENCES memory_assertions(assertion_id),
    FOREIGN KEY(conversation_id, event_seq)
        REFERENCES state_events(conversation_id, seq),
    UNIQUE(assertion_id, conversation_id, event_seq, payload_path, quote_hash)
);

CREATE INDEX idx_memory_evidence_assertion ON memory_evidence(assertion_id);

CREATE TABLE memory_current_projection (
    scope          TEXT NOT NULL,
    trust_class    TEXT NOT NULL,
    key            TEXT NOT NULL,
    assertion_id   TEXT NOT NULL UNIQUE,
    tier           TEXT NOT NULL DEFAULT 'warm'
                   CHECK(tier IN ('hot', 'warm', 'cold')),
    hits           INTEGER NOT NULL DEFAULT 0 CHECK(hits >= 0),
    last_used_at   INTEGER,
    projected_at   INTEGER NOT NULL,
    PRIMARY KEY(scope, trust_class, key),
    FOREIGN KEY(assertion_id) REFERENCES memory_assertions(assertion_id)
);

CREATE TRIGGER memory_projection_requires_approved_evidence_insert
BEFORE INSERT ON memory_current_projection
BEGIN
    SELECT CASE WHEN NOT EXISTS (
        SELECT 1 FROM memory_assertions a
        WHERE a.assertion_id = NEW.assertion_id
          AND a.scope = NEW.scope
          AND a.trust_class = NEW.trust_class
          AND a.status = 'approved'
          AND EXISTS (SELECT 1 FROM memory_evidence e WHERE e.assertion_id = a.assertion_id)
    ) THEN RAISE(ABORT, 'projection requires approved assertion with evidence') END;
END;

CREATE TRIGGER memory_projection_requires_approved_evidence_update
BEFORE UPDATE OF assertion_id, scope, trust_class ON memory_current_projection
BEGIN
    SELECT CASE WHEN NOT EXISTS (
        SELECT 1 FROM memory_assertions a
        WHERE a.assertion_id = NEW.assertion_id
          AND a.scope = NEW.scope
          AND a.trust_class = NEW.trust_class
          AND a.status = 'approved'
          AND EXISTS (SELECT 1 FROM memory_evidence e WHERE e.assertion_id = a.assertion_id)
    ) THEN RAISE(ABORT, 'projection requires approved assertion with evidence') END;
END;

CREATE TRIGGER memory_assertions_append_only_update
BEFORE UPDATE ON memory_assertions BEGIN
    SELECT RAISE(ABORT, 'memory assertions are append-only');
END;
CREATE TRIGGER memory_assertions_append_only_delete
BEFORE DELETE ON memory_assertions BEGIN
    SELECT RAISE(ABORT, 'memory assertions are append-only');
END;
CREATE TRIGGER memory_evidence_append_only_update
BEFORE UPDATE ON memory_evidence BEGIN
    SELECT RAISE(ABORT, 'memory evidence is append-only');
END;
CREATE TRIGGER memory_evidence_append_only_delete
BEFORE DELETE ON memory_evidence BEGIN
    SELECT RAISE(ABORT, 'memory evidence is append-only');
END;

CREATE TABLE memory_jobs (
    job_id            TEXT PRIMARY KEY,
    kind              TEXT NOT NULL CHECK(kind IN ('memory_extract', 'skill_capture')),
    status            TEXT NOT NULL DEFAULT 'pending'
                      CHECK(status IN ('pending', 'running', 'completed', 'failed')),
    conversation_id   TEXT NOT NULL,
    event_start_seq   INTEGER NOT NULL CHECK(event_start_seq > 0),
    event_end_seq     INTEGER NOT NULL CHECK(event_end_seq >= event_start_seq),
    run_id            TEXT NOT NULL,
    policy_hash       TEXT NOT NULL,
    model_hash        TEXT NOT NULL,
    attempt           INTEGER NOT NULL DEFAULT 0 CHECK(attempt >= 0),
    max_attempts      INTEGER NOT NULL DEFAULT 5 CHECK(max_attempts > 0),
    next_attempt_at   INTEGER NOT NULL,
    lease_owner       TEXT,
    lease_expires_at  INTEGER,
    last_error        TEXT,
    created_at        INTEGER NOT NULL,
    updated_at        INTEGER NOT NULL,
    completed_at      INTEGER,
    UNIQUE(kind, conversation_id, event_start_seq, event_end_seq, policy_hash, model_hash),
    FOREIGN KEY(conversation_id, event_start_seq)
        REFERENCES state_events(conversation_id, seq),
    FOREIGN KEY(conversation_id, event_end_seq)
        REFERENCES state_events(conversation_id, seq),
    CHECK((lease_owner IS NULL) = (lease_expires_at IS NULL)),
    CHECK(status = 'running' OR lease_owner IS NULL),
    CHECK(status != 'running' OR lease_owner IS NOT NULL),
    CHECK(status = 'completed' OR completed_at IS NULL)
);

CREATE INDEX idx_memory_jobs_claim
    ON memory_jobs(status, next_attempt_at, lease_expires_at, created_at);
CREATE INDEX idx_memory_jobs_conversation_range
    ON memory_jobs(conversation_id, event_start_seq, event_end_seq);