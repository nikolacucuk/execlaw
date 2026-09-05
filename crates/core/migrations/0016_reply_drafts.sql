-- Inbound-agent reply workflow. Drafts remain unsent until approved unless
-- the owning agent is explicitly switched to automatic mode.
ALTER TABLE config_agents ADD COLUMN trigger_json TEXT NOT NULL DEFAULT '{}';
ALTER TABLE config_agents ADD COLUMN reply_mode TEXT NOT NULL DEFAULT 'draft';

CREATE TABLE state_reply_drafts (
    id TEXT PRIMARY KEY,
    agent_id TEXT NOT NULL,
    conversation_id TEXT NOT NULL,
    channel TEXT NOT NULL,
    recipient TEXT NOT NULL,
    inbound_text TEXT NOT NULL,
    draft_text TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    created_at INTEGER NOT NULL,
    reviewed_at INTEGER,
    sent_at INTEGER,
    review_note TEXT,
    CHECK (status IN ('pending', 'approved', 'rejected', 'sent', 'failed'))
);
CREATE INDEX idx_reply_drafts_pending ON state_reply_drafts(status, created_at);
CREATE INDEX idx_reply_drafts_conversation ON state_reply_drafts(conversation_id, created_at);