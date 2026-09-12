-- Durable tool-contract decisions. Conversation events remain canonical history;
-- these rows preserve execution policy across worker and process restarts.

CREATE TABLE state_tool_retry_budgets (
    run_id              TEXT PRIMARY KEY,
    total_budget        INTEGER NOT NULL CHECK(total_budget >= 0),
    consumed_retries    INTEGER NOT NULL DEFAULT 0 CHECK(consumed_retries >= 0),
    updated_at_ms       INTEGER NOT NULL,
    FOREIGN KEY(run_id) REFERENCES state_runs(run_id) ON DELETE CASCADE,
    CHECK(consumed_retries <= total_budget)
);

CREATE TABLE state_tool_invocations (
    run_id                  TEXT NOT NULL,
    step_id                 TEXT NOT NULL,
    tool_name               TEXT NOT NULL,
    integration_id          TEXT NOT NULL,
    call_fingerprint        TEXT NOT NULL CHECK(length(call_fingerprint) = 64),
    repeated_call_count     INTEGER NOT NULL DEFAULT 1 CHECK(repeated_call_count > 0),
    input_schema_hash       TEXT CHECK(input_schema_hash IS NULL OR length(input_schema_hash) = 64),
    result_schema_hash      TEXT CHECK(result_schema_hash IS NULL OR length(result_schema_hash) = 64),
    retry_budget_total      INTEGER NOT NULL CHECK(retry_budget_total > 0),
    attempts_used           INTEGER NOT NULL DEFAULT 0 CHECK(attempts_used >= 0),
    next_retry_at_ms        INTEGER,
    backoff_ms              INTEGER CHECK(backoff_ms IS NULL OR backoff_ms >= 0),
    status                  TEXT NOT NULL DEFAULT 'pending'
                            CHECK(status IN ('pending', 'running', 'succeeded', 'failed', 'denied')),
    failure_kind            TEXT,
    failure_code            TEXT,
    failure_message         TEXT,
    failure_retryable       INTEGER CHECK(failure_retryable IS NULL OR failure_retryable IN (0, 1)),
    failure_retry_after_ms  INTEGER,
    failure_guidance        TEXT,
    created_at_ms           INTEGER NOT NULL,
    updated_at_ms           INTEGER NOT NULL,
    PRIMARY KEY(run_id, step_id),
    FOREIGN KEY(run_id, step_id)
        REFERENCES state_run_steps(run_id, step_id) ON DELETE CASCADE,
    CHECK(status IN ('pending', 'running') OR next_retry_at_ms IS NULL),
    CHECK(failure_kind IS NOT NULL OR failure_code IS NULL),
    CHECK(failure_kind IS NOT NULL OR failure_message IS NULL),
    CHECK(failure_kind IS NOT NULL OR failure_retryable IS NULL)
);

CREATE INDEX idx_state_tool_invocations_retry
    ON state_tool_invocations(status, next_retry_at_ms)
    WHERE next_retry_at_ms IS NOT NULL;

CREATE INDEX idx_state_tool_invocations_fingerprint
    ON state_tool_invocations(run_id, call_fingerprint);

CREATE TABLE state_tool_circuits (
    integration_id          TEXT PRIMARY KEY,
    state                   TEXT NOT NULL DEFAULT 'closed'
                            CHECK(state IN ('closed', 'open', 'half_open')),
    consecutive_failures    INTEGER NOT NULL DEFAULT 0 CHECK(consecutive_failures >= 0),
    open_until_ms           INTEGER,
    probe_run_id            TEXT,
    probe_step_id           TEXT,
    updated_at_ms           INTEGER NOT NULL,
    CHECK(state = 'open' OR open_until_ms IS NULL),
    CHECK(state = 'half_open' OR (probe_run_id IS NULL AND probe_step_id IS NULL)),
    CHECK((probe_run_id IS NULL) = (probe_step_id IS NULL))
);

CREATE INDEX idx_state_tool_circuits_open
    ON state_tool_circuits(open_until_ms)
    WHERE state = 'open';