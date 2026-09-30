-- Versioned specialist definitions, source identity, review freshness, and
-- explicit conversation ownership. Existing agent rows remain readable.
ALTER TABLE config_agents ADD COLUMN definition_version INTEGER NOT NULL DEFAULT 1;

CREATE TABLE config_agent_definition_revisions (
    agent_id TEXT NOT NULL REFERENCES config_agents(id) ON DELETE CASCADE,
    version INTEGER NOT NULL,
    definition_json TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (agent_id, version)
);
INSERT INTO config_agent_definition_revisions(agent_id,version,definition_json,created_at)
SELECT id,1,json_object('name',name,'role_prompt',role_prompt,'model',model,
    'backend_purpose',backend_purpose,'tools',json(tools_json),
    'trust_policy',json(trust_policy_json),'trigger',json(trigger_json),
    'reply_mode',reply_mode,'token_budget',token_budget,
    'max_runtime_secs',max_runtime_secs,'interval_secs',interval_secs,
    'concurrency_limit',concurrency_limit),updated_at
FROM config_agents;

ALTER TABLE state_agent_messages ADD COLUMN source_kind TEXT NOT NULL DEFAULT 'manual';
ALTER TABLE state_agent_messages ADD COLUMN source_event_id TEXT;
ALTER TABLE state_agent_messages ADD COLUMN source_occurred_at INTEGER;
ALTER TABLE state_agent_messages ADD COLUMN conversation_id TEXT;
ALTER TABLE state_agent_messages ADD COLUMN recipient TEXT;
ALTER TABLE state_agent_messages ADD COLUMN definition_version INTEGER;
ALTER TABLE state_agent_messages ADD COLUMN available_at INTEGER NOT NULL DEFAULT 0;
CREATE UNIQUE INDEX idx_agent_messages_source_identity
    ON state_agent_messages(agent_id, source_kind, source_event_id)
    WHERE source_event_id IS NOT NULL;
CREATE INDEX idx_agent_messages_due
    ON state_agent_messages(agent_id, delivered_at, available_at, created_at);

ALTER TABLE state_agent_runs ADD COLUMN mailbox_id TEXT;
ALTER TABLE state_agent_runs ADD COLUMN definition_version INTEGER;
ALTER TABLE state_agent_runs ADD COLUMN outcome_kind TEXT;
CREATE INDEX idx_agent_runs_mailbox ON state_agent_runs(mailbox_id, started_at);

ALTER TABLE state_reply_drafts ADD COLUMN run_id TEXT;
ALTER TABLE state_reply_drafts ADD COLUMN source_event_id TEXT;
ALTER TABLE state_reply_drafts ADD COLUMN source_event_seq INTEGER;
ALTER TABLE state_reply_drafts ADD COLUMN model_seq INTEGER;
ALTER TABLE state_reply_drafts ADD COLUMN revision INTEGER NOT NULL DEFAULT 1;
ALTER TABLE state_reply_drafts ADD COLUMN audience_json TEXT NOT NULL DEFAULT '{}';
ALTER TABLE state_reply_drafts ADD COLUMN source_last_seq INTEGER;
ALTER TABLE state_reply_drafts ADD COLUMN stale_at INTEGER;
ALTER TABLE state_reply_drafts ADD COLUMN reviewer_id TEXT;
ALTER TABLE state_reply_drafts ADD COLUMN approved_text TEXT;
ALTER TABLE state_reply_drafts ADD COLUMN superseded_by TEXT;
CREATE UNIQUE INDEX idx_reply_drafts_model_event
    ON state_reply_drafts(conversation_id, model_seq)
    WHERE model_seq IS NOT NULL;
CREATE INDEX idx_reply_drafts_source
    ON state_reply_drafts(conversation_id, source_event_id, status);

CREATE TABLE state_agent_ownership (
    scope_key TEXT PRIMARY KEY,
    conversation_id TEXT NOT NULL,
    owner_kind TEXT NOT NULL CHECK (owner_kind IN ('controller', 'agent')),
    agent_id TEXT,
    generation INTEGER NOT NULL DEFAULT 1,
    updated_at INTEGER NOT NULL,
    CHECK ((owner_kind = 'controller' AND agent_id IS NULL) OR
           (owner_kind = 'agent' AND agent_id IS NOT NULL))
);
