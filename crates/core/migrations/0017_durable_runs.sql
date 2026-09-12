CREATE TABLE state_runs (
    run_id          TEXT PRIMARY KEY,
    conversation_id TEXT NOT NULL,
    parent_run_id   TEXT,
    status          TEXT NOT NULL DEFAULT 'pending'
                    CHECK(status IN ('pending', 'running', 'waiting', 'completed', 'failed', 'cancelled')),
    cursor          INTEGER NOT NULL DEFAULT 0 CHECK(cursor >= 0),
    input_event_seq INTEGER NOT NULL CHECK(input_event_seq > 0),
    started_at      INTEGER NOT NULL,
    updated_at      INTEGER NOT NULL,
    deadline_at     INTEGER,
    FOREIGN KEY(conversation_id, input_event_seq)
        REFERENCES state_events(conversation_id, seq),
    FOREIGN KEY(parent_run_id)
        REFERENCES state_runs(run_id)
);

CREATE INDEX idx_state_runs_conversation
    ON state_runs(conversation_id, started_at);

CREATE INDEX idx_state_runs_parent
    ON state_runs(parent_run_id)
    WHERE parent_run_id IS NOT NULL;

CREATE TABLE state_run_steps (
    run_id                 TEXT NOT NULL,
    step_id                TEXT NOT NULL,
    ordinal                INTEGER NOT NULL CHECK(ordinal >= 0),
    kind                   TEXT NOT NULL
                           CHECK(kind IN (
                               'model_request',
                               'deterministic_compute',
                               'tool_dispatch',
                               'approval_wait',
                               'outbox_enqueue',
                               'child_run_spawn',
                               'child_run_join',
                               'artifact_publish'
                           )),
    status                 TEXT NOT NULL DEFAULT 'pending'
                           CHECK(status IN ('pending', 'running', 'waiting', 'completed', 'failed')),
    attempt                INTEGER NOT NULL DEFAULT 0 CHECK(attempt >= 0),
    input_hash             TEXT NOT NULL,
    output_ref             TEXT,
    approval_id            TEXT,
    outbox_idempotency_key TEXT,
    lease_owner            TEXT,
    lease_expires_at       INTEGER,
    started_at             INTEGER,
    completed_at           INTEGER,
    PRIMARY KEY(run_id, step_id),
    UNIQUE(run_id, ordinal),
    FOREIGN KEY(run_id) REFERENCES state_runs(run_id) ON DELETE CASCADE,
    CHECK((lease_owner IS NULL) = (lease_expires_at IS NULL)),
    CHECK(status = 'running' OR lease_owner IS NULL),
    CHECK(status != 'running' OR lease_owner IS NOT NULL),
    CHECK(status IN ('completed', 'failed') OR completed_at IS NULL),
    CHECK(status NOT IN ('completed', 'failed') OR completed_at IS NOT NULL)
);

CREATE INDEX idx_state_run_steps_recovery
    ON state_run_steps(run_id, status, ordinal);

CREATE INDEX idx_state_run_steps_expired_lease
    ON state_run_steps(lease_expires_at)
    WHERE status = 'running';

CREATE UNIQUE INDEX idx_state_run_steps_approval
    ON state_run_steps(approval_id)
    WHERE approval_id IS NOT NULL;

CREATE UNIQUE INDEX idx_state_run_steps_outbox_idempotency
    ON state_run_steps(outbox_idempotency_key)
    WHERE outbox_idempotency_key IS NOT NULL;