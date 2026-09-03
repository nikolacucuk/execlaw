-- First-class always-on child agents.
-- Definitions and execution state are durable so a service restart can
-- resume scheduling without relying on in-memory task handles.
CREATE TABLE config_agents (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    role_prompt TEXT NOT NULL,
    model TEXT,
    backend_purpose TEXT NOT NULL DEFAULT 'standard',
    tools_json TEXT NOT NULL DEFAULT '[]',
    trust_policy_json TEXT NOT NULL DEFAULT '{}',
    interval_secs INTEGER NOT NULL DEFAULT 300,
    token_budget INTEGER NOT NULL DEFAULT 1024,
    max_runtime_secs INTEGER NOT NULL DEFAULT 300,
    concurrency_limit INTEGER NOT NULL DEFAULT 1,
    enabled INTEGER NOT NULL DEFAULT 1,
    paused INTEGER NOT NULL DEFAULT 0,
    next_run_at INTEGER,
    last_run_at INTEGER,
    last_run_status TEXT,
    last_error TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    CHECK (enabled IN (0, 1)),
    CHECK (paused IN (0, 1)),
    CHECK (interval_secs > 0),
    CHECK (token_budget > 0),
    CHECK (max_runtime_secs > 0),
    CHECK (concurrency_limit > 0)
);

CREATE TABLE state_agent_runs (
    id TEXT PRIMARY KEY,
    agent_id TEXT NOT NULL,
    status TEXT NOT NULL,
    started_at INTEGER NOT NULL,
    finished_at INTEGER,
    tokens_used INTEGER,
    checkpoint_json TEXT NOT NULL DEFAULT '{}',
    output_text TEXT,
    error TEXT
);
CREATE INDEX idx_agent_runs_agent_started ON state_agent_runs(agent_id, started_at);
CREATE INDEX idx_agent_runs_status ON state_agent_runs(status);

CREATE TABLE state_agent_messages (
    id TEXT PRIMARY KEY,
    agent_id TEXT NOT NULL,
    parent_agent_id TEXT,
    direction TEXT NOT NULL,
    content TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    delivered_at INTEGER,
    result_run_id TEXT
);
CREATE INDEX idx_agent_messages_pending ON state_agent_messages(agent_id, delivered_at, created_at);
