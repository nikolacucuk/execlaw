CREATE TABLE state_run_child_budgets (
    parent_run_id TEXT PRIMARY KEY REFERENCES state_runs(run_id) ON DELETE CASCADE,
    token_limit INTEGER NOT NULL CHECK (token_limit > 0),
    tokens_reserved INTEGER NOT NULL DEFAULT 0 CHECK (tokens_reserved >= 0),
    updated_at INTEGER NOT NULL
);

CREATE TABLE state_run_child_tasks (
    child_run_id TEXT PRIMARY KEY REFERENCES state_runs(run_id) ON DELETE CASCADE,
    parent_run_id TEXT NOT NULL REFERENCES state_runs(run_id) ON DELETE CASCADE,
    task_json TEXT NOT NULL,
    task_hash TEXT NOT NULL,
    trust_ceiling_json TEXT NOT NULL,
    budget_tokens INTEGER NOT NULL CHECK (budget_tokens > 0),
    tokens_used INTEGER,
    dependencies_json TEXT NOT NULL DEFAULT '[]',
    result_artifact_id TEXT,
    budget_settled INTEGER NOT NULL DEFAULT 0 CHECK (budget_settled IN (0, 1)),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    FOREIGN KEY (parent_run_id) REFERENCES state_runs(run_id) ON DELETE CASCADE
);

CREATE INDEX idx_run_child_tasks_parent ON state_run_child_tasks(parent_run_id, created_at, child_run_id);
