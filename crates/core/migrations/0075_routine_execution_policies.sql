ALTER TABLE config_routines
    ADD COLUMN missed_run_policy TEXT NOT NULL DEFAULT 'skip'
    CHECK (missed_run_policy IN ('skip', 'coalesce', 'catch_up'));

ALTER TABLE config_routines
    ADD COLUMN missed_run_limit INTEGER NOT NULL DEFAULT 1
    CHECK (missed_run_limit BETWEEN 1 AND 100);

ALTER TABLE config_routines
    ADD COLUMN overlap_policy TEXT NOT NULL DEFAULT 'forbid'
    CHECK (overlap_policy IN ('forbid', 'queue', 'replace'));

ALTER TABLE state_routine_runs ADD COLUMN occurrence_at INTEGER;

CREATE UNIQUE INDEX idx_state_routine_runs_occurrence
    ON state_routine_runs(routine_id, occurrence_at)
    WHERE occurrence_at IS NOT NULL;
