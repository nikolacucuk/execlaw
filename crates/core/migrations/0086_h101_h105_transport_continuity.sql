-- Durable normalized transport operations, explicit identity links,
-- webhook replay receipts, and takeover hand-back checkpoints.

ALTER TABLE message_archive_messages ADD COLUMN source_version INTEGER NOT NULL DEFAULT 0;
ALTER TABLE message_archive_messages ADD COLUMN source_revision_event_id TEXT NOT NULL DEFAULT '';
ALTER TABLE state_agent_messages ADD COLUMN superseded_by TEXT;
ALTER TABLE state_agent_ownership ADD COLUMN previous_agent_id TEXT;
ALTER TABLE state_outbox ADD COLUMN ownership_scope_key TEXT;
ALTER TABLE state_outbox ADD COLUMN ownership_generation INTEGER;
CREATE INDEX idx_outbox_owner_generation
    ON state_outbox(ownership_scope_key, ownership_generation, status);
CREATE INDEX idx_agent_messages_pending_due
    ON state_agent_messages(agent_id, available_at, created_at)
    WHERE delivered_at IS NULL AND superseded_by IS NULL;
DROP TRIGGER IF EXISTS message_archive_search_insert;
DROP TRIGGER IF EXISTS message_archive_search_delete;
DROP TRIGGER IF EXISTS message_archive_search_update;
DROP TABLE message_archive_search;
CREATE VIRTUAL TABLE message_archive_search USING fts5(
    archive_message_id UNINDEXED,
    archive_id UNINDEXED,
    body,
    sender_name,
    topic_keywords,
    tokenize = 'unicode61'
);
INSERT INTO message_archive_search(archive_message_id,archive_id,body,sender_name,topic_keywords)
SELECT archive_message_id,archive_id,body,COALESCE(sender_name,''),topic_keywords
FROM message_archive_messages WHERE deleted_at IS NULL AND body <> '';
CREATE TRIGGER message_archive_search_insert AFTER INSERT ON message_archive_messages BEGIN
    INSERT INTO message_archive_search(archive_message_id,archive_id,body,sender_name,topic_keywords)
    SELECT new.archive_message_id,new.archive_id,new.body,COALESCE(new.sender_name,''),new.topic_keywords
    WHERE new.deleted_at IS NULL AND new.body <> '';
END;
CREATE TRIGGER message_archive_search_delete AFTER DELETE ON message_archive_messages BEGIN
    DELETE FROM message_archive_search WHERE archive_message_id = old.archive_message_id;
END;
CREATE TRIGGER message_archive_search_update AFTER UPDATE OF body,sender_name,topic_keywords,deleted_at ON message_archive_messages BEGIN
    DELETE FROM message_archive_search WHERE archive_message_id = old.archive_message_id;
    INSERT INTO message_archive_search(archive_message_id,archive_id,body,sender_name,topic_keywords)
    SELECT new.archive_message_id,new.archive_id,new.body,COALESCE(new.sender_name,''),new.topic_keywords
    WHERE new.deleted_at IS NULL AND new.body <> '';
END;

CREATE TABLE message_archive_revisions (
    event_id TEXT NOT NULL,
    archive_id TEXT NOT NULL REFERENCES message_archive_conversations(archive_id) ON DELETE CASCADE,
    source_message_id TEXT NOT NULL,
    operation TEXT NOT NULL CHECK(operation IN ('create','edit','delete','reaction_add','reaction_remove')),
    source_version INTEGER NOT NULL,
    occurred_at INTEGER NOT NULL,
    body TEXT,
    reply_to_message_id TEXT,
    reaction TEXT,
    actor_id TEXT,
    recorded_at INTEGER NOT NULL,
    PRIMARY KEY(archive_id,event_id),
    UNIQUE(archive_id, source_message_id, source_version, operation, event_id)
);
CREATE INDEX idx_message_archive_revisions_target
    ON message_archive_revisions(archive_id, source_message_id, source_version, recorded_at);
CREATE TABLE message_archive_reactions (
    archive_id TEXT NOT NULL REFERENCES message_archive_conversations(archive_id) ON DELETE CASCADE,
    source_message_id TEXT NOT NULL,
    actor_id TEXT NOT NULL,
    reaction TEXT NOT NULL,
    source_version INTEGER NOT NULL,
    source_event_id TEXT NOT NULL,
    active INTEGER NOT NULL CHECK(active IN (0,1)),
    updated_at INTEGER NOT NULL,
    PRIMARY KEY(archive_id,source_message_id,actor_id,reaction)
);
CREATE TRIGGER message_archive_revisions_no_update BEFORE UPDATE ON message_archive_revisions
BEGIN SELECT RAISE(ABORT, 'message archive revisions are append-only'); END;
CREATE TRIGGER message_archive_revisions_no_delete BEFORE DELETE ON message_archive_revisions
BEGIN SELECT RAISE(ABORT, 'message archive revisions are append-only'); END;

CREATE TABLE state_transport_identity_links (
    link_id TEXT PRIMARY KEY,
    controller_id TEXT NOT NULL,
    left_channel TEXT NOT NULL,
    left_subject TEXT NOT NULL,
    right_channel TEXT NOT NULL,
    right_subject TEXT NOT NULL,
    verification_id TEXT NOT NULL,
    selected_history_json TEXT NOT NULL DEFAULT '[]',
    linked_at INTEGER NOT NULL,
    revoked_at INTEGER,
    CHECK(left_channel <> right_channel OR left_subject <> right_subject),
    UNIQUE(left_channel, left_subject, right_channel, right_subject, linked_at)
);
CREATE INDEX idx_transport_identity_links_active_left
    ON state_transport_identity_links(left_channel, left_subject, revoked_at);
CREATE INDEX idx_transport_identity_links_active_right
    ON state_transport_identity_links(right_channel, right_subject, revoked_at);
CREATE TABLE state_transport_continuity_transfers (
    transfer_id TEXT PRIMARY KEY,
    link_id TEXT NOT NULL REFERENCES state_transport_identity_links(link_id),
    origin_channel TEXT NOT NULL,
    origin_subject TEXT NOT NULL,
    origin_conversation TEXT NOT NULL,
    destination_channel TEXT NOT NULL,
    destination_subject TEXT NOT NULL,
    destination_conversation TEXT NOT NULL,
    audience_kind TEXT NOT NULL CHECK(audience_kind IN ('direct','group')),
    selected_message_ids_json TEXT NOT NULL,
    selected_context_json TEXT NOT NULL DEFAULT '[]',
    created_at INTEGER NOT NULL
);
CREATE TRIGGER transport_continuity_transfers_no_update BEFORE UPDATE ON state_transport_continuity_transfers
BEGIN SELECT RAISE(ABORT, 'continuity transfer records are append-only'); END;
CREATE TRIGGER transport_continuity_transfers_no_delete BEFORE DELETE ON state_transport_continuity_transfers
BEGIN SELECT RAISE(ABORT, 'continuity transfer records are append-only'); END;

CREATE TABLE state_webhook_receipts (
    plugin_id TEXT NOT NULL,
    route_key TEXT NOT NULL,
    event_id TEXT NOT NULL,
    received_at INTEGER NOT NULL,
    outcome TEXT NOT NULL CHECK(outcome IN ('processing','accepted','rejected')),
    lease_expires_at INTEGER,
    PRIMARY KEY(plugin_id, route_key, event_id)
);
CREATE INDEX idx_webhook_receipts_expiry ON state_webhook_receipts(received_at);

CREATE TABLE state_agent_handoff_events (
    scope_key TEXT NOT NULL,
    generation INTEGER NOT NULL,
    event_kind TEXT NOT NULL CHECK(event_kind IN ('takeover','operator_action','handback')),
    event_id TEXT NOT NULL,
    conversation_seq INTEGER,
    payload_json TEXT NOT NULL,
    occurred_at INTEGER NOT NULL,
    PRIMARY KEY(scope_key, generation, event_id)
);
CREATE INDEX idx_agent_handoff_events_scope
    ON state_agent_handoff_events(scope_key, generation, conversation_seq);
CREATE TRIGGER state_agent_handoff_events_no_update BEFORE UPDATE ON state_agent_handoff_events
BEGIN SELECT RAISE(ABORT, 'agent handoff events are append-only'); END;
CREATE TRIGGER state_agent_handoff_events_no_delete BEFORE DELETE ON state_agent_handoff_events
BEGIN SELECT RAISE(ABORT, 'agent handoff events are append-only'); END;
