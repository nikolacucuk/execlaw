-- Scheduled specialist fires are independent of mailbox retry due times.
ALTER TABLE config_agents ADD COLUMN schedule_next_at INTEGER;

CREATE TABLE state_agent_schedule_fires (
    agent_id TEXT NOT NULL REFERENCES config_agents(id) ON DELETE CASCADE,
    due_at INTEGER NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('queued', 'skipped')),
    reason TEXT,
    mailbox_id TEXT,
    recorded_at INTEGER NOT NULL,
    PRIMARY KEY (agent_id, due_at)
);
CREATE INDEX idx_agent_schedule_fires_recent
    ON state_agent_schedule_fires(agent_id, due_at DESC);
