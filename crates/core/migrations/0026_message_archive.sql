-- Deterministic archive of transport messages. The event log remains the
-- audit source; these rows provide a stable, searchable conversation index.

CREATE TABLE message_archive_conversations (
    archive_id TEXT PRIMARY KEY,
    channel TEXT NOT NULL,
    remote_id TEXT NOT NULL,
    conversation_kind TEXT NOT NULL CHECK(conversation_kind IN ('group', 'direct')),
    display_name TEXT,
    conversation_id TEXT,
    first_seen_at INTEGER NOT NULL,
    last_seen_at INTEGER NOT NULL,
    UNIQUE(channel, remote_id)
);

CREATE INDEX idx_message_archive_conversations_channel
    ON message_archive_conversations(channel, conversation_kind, last_seen_at DESC);

CREATE TABLE message_archive_messages (
    archive_message_id TEXT PRIMARY KEY,
    archive_id TEXT NOT NULL,
    source_event_seq INTEGER,
    source_event_kind TEXT NOT NULL,
    direction TEXT NOT NULL CHECK(direction IN ('inbound', 'outbound')),
    sender_id TEXT,
    sender_name TEXT,
    body TEXT NOT NULL,
    occurred_at INTEGER NOT NULL,
    source_message_id TEXT,
    created_at INTEGER NOT NULL,
    FOREIGN KEY(archive_id) REFERENCES message_archive_conversations(archive_id) ON DELETE CASCADE,
    UNIQUE(archive_id, source_message_id),
    UNIQUE(archive_id, source_event_seq, source_event_kind)
);

CREATE INDEX idx_message_archive_messages_lookup
    ON message_archive_messages(archive_id, occurred_at, created_at);

CREATE VIRTUAL TABLE message_archive_search USING fts5(
    archive_message_id UNINDEXED,
    archive_id UNINDEXED,
    body,
    sender_name,
    tokenize = 'unicode61'
);

CREATE TRIGGER message_archive_search_insert AFTER INSERT ON message_archive_messages BEGIN
    INSERT INTO message_archive_search(archive_message_id, archive_id, body, sender_name)
    VALUES (new.archive_message_id, new.archive_id, new.body, COALESCE(new.sender_name, ''));
END;

CREATE TRIGGER message_archive_search_delete AFTER DELETE ON message_archive_messages BEGIN
    DELETE FROM message_archive_search WHERE archive_message_id = old.archive_message_id;
END;