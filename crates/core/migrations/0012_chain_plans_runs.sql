-- 0012_chain_plans_runs.sql
--
-- Persisted tool-chain phase 2 state:
--   * state_chain_plans: deterministic plan payloads
--   * state_chain_runs: execution attempts with approval halt/resume
--   * state_chain_run_steps: per-step execution/audit rows

CREATE TABLE IF NOT EXISTS state_chain_plans (
    id TEXT PRIMARY KEY,
    conversation_id TEXT NOT NULL,
    objective TEXT NOT NULL,
    constraints_json TEXT NOT NULL,
    plan_json BLOB NOT NULL,
    has_external_effects INTEGER NOT NULL DEFAULT 0,
    created_by_trust TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_state_chain_plans_conversation
    ON state_chain_plans(conversation_id, created_at DESC);

CREATE TABLE IF NOT EXISTS state_chain_runs (
    id TEXT PRIMARY KEY,
    plan_id TEXT NOT NULL,
    conversation_id TEXT NOT NULL,
    run_seq INTEGER NOT NULL,
    status TEXT NOT NULL,
    approval_id TEXT,
    next_step_index INTEGER NOT NULL DEFAULT 0,
    error_text TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    finished_at INTEGER,
    UNIQUE(conversation_id, run_seq),
    UNIQUE(approval_id),
    FOREIGN KEY(plan_id) REFERENCES state_chain_plans(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_state_chain_runs_plan
    ON state_chain_runs(plan_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_state_chain_runs_approval
    ON state_chain_runs(approval_id);

CREATE TABLE IF NOT EXISTS state_chain_run_steps (
    run_id TEXT NOT NULL,
    step_index INTEGER NOT NULL,
    kind TEXT NOT NULL,
    status TEXT NOT NULL,
    tool_name TEXT,
    effect_kind TEXT,
    args_json TEXT,
    result_json TEXT,
    error_text TEXT,
    outbox_idempotency_key TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY(run_id, step_index),
    FOREIGN KEY(run_id) REFERENCES state_chain_runs(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_state_chain_run_steps_run
    ON state_chain_run_steps(run_id, step_index);
