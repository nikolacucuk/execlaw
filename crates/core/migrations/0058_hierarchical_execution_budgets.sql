ALTER TABLE state_run_child_budgets
    ADD COLUMN time_limit_ms INTEGER NOT NULL DEFAULT 3600000 CHECK(time_limit_ms > 0);
ALTER TABLE state_run_child_budgets
    ADD COLUMN time_reserved_ms INTEGER NOT NULL DEFAULT 0 CHECK(time_reserved_ms >= 0);
ALTER TABLE state_run_child_budgets
    ADD COLUMN retry_limit INTEGER NOT NULL DEFAULT 64 CHECK(retry_limit >= 0);
ALTER TABLE state_run_child_budgets
    ADD COLUMN retries_reserved INTEGER NOT NULL DEFAULT 0 CHECK(retries_reserved >= 0);
ALTER TABLE state_run_child_budgets
    ADD COLUMN effect_limit INTEGER NOT NULL DEFAULT 0 CHECK(effect_limit >= 0);
ALTER TABLE state_run_child_budgets
    ADD COLUMN effects_reserved INTEGER NOT NULL DEFAULT 0 CHECK(effects_reserved >= 0);

ALTER TABLE state_run_child_tasks
    ADD COLUMN budget_time_ms INTEGER NOT NULL DEFAULT 120000 CHECK(budget_time_ms > 0);
ALTER TABLE state_run_child_tasks
    ADD COLUMN time_used_ms INTEGER;
ALTER TABLE state_run_child_tasks
    ADD COLUMN budget_retries INTEGER NOT NULL DEFAULT 0 CHECK(budget_retries >= 0);
ALTER TABLE state_run_child_tasks
    ADD COLUMN retries_used INTEGER;
ALTER TABLE state_run_child_tasks
    ADD COLUMN budget_effects INTEGER NOT NULL DEFAULT 0 CHECK(budget_effects >= 0);
ALTER TABLE state_run_child_tasks
    ADD COLUMN effects_used INTEGER;

CREATE TABLE state_run_execution_budgets (
    run_id TEXT PRIMARY KEY REFERENCES state_runs(run_id) ON DELETE CASCADE,
    time_limit_ms INTEGER NOT NULL CHECK(time_limit_ms > 0),
    deadline_at_ms INTEGER NOT NULL,
    retry_limit INTEGER NOT NULL CHECK(retry_limit >= 0),
    retries_used INTEGER NOT NULL DEFAULT 0 CHECK(retries_used >= 0),
    effect_limit INTEGER NOT NULL CHECK(effect_limit >= 0),
    effects_used INTEGER NOT NULL DEFAULT 0 CHECK(effects_used >= 0),
    updated_at_ms INTEGER NOT NULL,
    CHECK(retries_used <= retry_limit),
    CHECK(effects_used <= effect_limit)
);

CREATE TABLE state_run_effect_claims (
    run_id TEXT NOT NULL REFERENCES state_runs(run_id) ON DELETE CASCADE,
    step_id TEXT NOT NULL,
    claimed_at_ms INTEGER NOT NULL,
    PRIMARY KEY(run_id, step_id)
);
