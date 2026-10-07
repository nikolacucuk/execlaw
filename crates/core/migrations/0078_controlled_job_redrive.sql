-- Operator initiated redrives retain their original effect identity and
-- append an actor/reason record so queue recovery is attributable.
CREATE TABLE state_job_redrive_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    job_kind TEXT NOT NULL CHECK (job_kind IN ('outbox', 'memory_extraction', 'automation')),
    job_id TEXT NOT NULL,
    effect_identity TEXT NOT NULL,
    actor TEXT NOT NULL,
    reason TEXT NOT NULL CHECK (length(reason) BETWEEN 1 AND 512),
    prior_attempt INTEGER NOT NULL CHECK (prior_attempt >= 0),
    occurred_at INTEGER NOT NULL
);

CREATE INDEX idx_job_redrive_events_job
    ON state_job_redrive_events(job_kind, job_id, id);

CREATE TRIGGER job_redrive_events_append_only_update
BEFORE UPDATE ON state_job_redrive_events BEGIN
    SELECT RAISE(ABORT, 'job redrive audit is append-only');
END;

CREATE TRIGGER job_redrive_events_append_only_delete
BEFORE DELETE ON state_job_redrive_events BEGIN
    SELECT RAISE(ABORT, 'job redrive audit is append-only');
END;

ALTER TABLE state_bus_events
    ADD COLUMN dispatch_attempts INTEGER NOT NULL DEFAULT 0 CHECK (dispatch_attempts >= 0);
ALTER TABLE state_bus_events
    ADD COLUMN dispatch_error TEXT;
ALTER TABLE state_bus_events
    ADD COLUMN dead_lettered_at INTEGER;

CREATE INDEX idx_bus_events_dead_letter
    ON state_bus_events(dead_lettered_at, received_at)
    WHERE dead_lettered_at IS NOT NULL;
