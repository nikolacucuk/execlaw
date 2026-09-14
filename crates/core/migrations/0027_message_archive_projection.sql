-- Archive policy and lossless metadata for the deterministic Obsidian
-- projection. SQLite remains authoritative; files can be regenerated.

ALTER TABLE message_archive_messages ADD COLUMN delivery_status TEXT NOT NULL DEFAULT 'delivered'
    CHECK(delivery_status IN ('generated', 'queued', 'delivered', 'failed'));
ALTER TABLE message_archive_messages ADD COLUMN reply_to_message_id TEXT;
ALTER TABLE message_archive_messages ADD COLUMN edited_at INTEGER;
ALTER TABLE message_archive_messages ADD COLUMN deleted_at INTEGER;

CREATE TABLE message_archive_participants (
    archive_id TEXT NOT NULL,
    participant_id TEXT NOT NULL,
    display_name TEXT,
    first_seen_at INTEGER NOT NULL,
    last_seen_at INTEGER NOT NULL,
    PRIMARY KEY(archive_id, participant_id),
    FOREIGN KEY(archive_id) REFERENCES message_archive_conversations(archive_id) ON DELETE CASCADE
);

CREATE TABLE config_message_archive (
    id INTEGER PRIMARY KEY CHECK(id = 1),
    archive_group_messages INTEGER NOT NULL DEFAULT 1 CHECK(archive_group_messages IN (0, 1)),
    archive_direct_messages INTEGER NOT NULL DEFAULT 1 CHECK(archive_direct_messages IN (0, 1)),
    markdown_projection_enabled INTEGER NOT NULL DEFAULT 1 CHECK(markdown_projection_enabled IN (0, 1)),
    message_retention_days INTEGER,
    attachment_retention_days INTEGER
);

INSERT INTO config_message_archive(id) VALUES (1);

CREATE INDEX idx_message_archive_messages_period
    ON message_archive_messages(archive_id, occurred_at, delivery_status);