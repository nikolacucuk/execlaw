-- 0033_inference_attempts.sql
-- Durable per-request attempt history for local inference retries.

CREATE TABLE state_run_inference_attempts (
    run_id TEXT NOT NULL,
    step_id TEXT NOT NULL,
    attempt_no INTEGER NOT NULL CHECK(attempt_no > 0),
    status TEXT NOT NULL CHECK(status IN ('started', 'retrying', 'succeeded', 'failed')),
    error_class TEXT,
    started_at INTEGER NOT NULL,
    finished_at INTEGER,
    PRIMARY KEY(run_id, step_id, attempt_no),
    FOREIGN KEY(run_id, step_id) REFERENCES state_run_steps(run_id, step_id) ON DELETE CASCADE
);

CREATE INDEX idx_run_inference_attempts_status
    ON state_run_inference_attempts(run_id, status);
