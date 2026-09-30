CREATE TABLE config_agent_completion_contracts (
    agent_id TEXT PRIMARY KEY REFERENCES config_agents(id) ON DELETE CASCADE,
    acceptance_criteria_json TEXT NOT NULL,
    required_artifacts_json TEXT NOT NULL,
    delivery_required INTEGER NOT NULL CHECK (delivery_required IN (0, 1)),
    updated_at INTEGER NOT NULL
);

CREATE TABLE state_agent_run_completion_contracts (
    run_id TEXT PRIMARY KEY REFERENCES state_agent_runs(id) ON DELETE CASCADE,
    acceptance_criteria_json TEXT NOT NULL,
    required_artifacts_json TEXT NOT NULL,
    delivery_required INTEGER NOT NULL CHECK (delivery_required IN (0, 1)),
    delivery_confirmed INTEGER NOT NULL DEFAULT 0 CHECK (delivery_confirmed IN (0, 1)),
    delivery_evidence_ref TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE state_agent_run_completion_verifications (
    run_id TEXT NOT NULL REFERENCES state_agent_run_completion_contracts(run_id) ON DELETE CASCADE,
    criterion_id TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('pending', 'passed', 'failed', 'blocked')),
    evidence_refs_json TEXT NOT NULL DEFAULT '[]',
    detail TEXT,
    verified_at INTEGER NOT NULL,
    PRIMARY KEY (run_id, criterion_id)
);

CREATE TABLE state_agent_run_completion_artifacts (
    run_id TEXT NOT NULL REFERENCES state_agent_run_completion_contracts(run_id) ON DELETE CASCADE,
    artifact_id TEXT NOT NULL,
    present INTEGER NOT NULL CHECK (present IN (0, 1)),
    evidence_ref TEXT,
    detail TEXT,
    checked_at INTEGER NOT NULL,
    PRIMARY KEY (run_id, artifact_id)
);
