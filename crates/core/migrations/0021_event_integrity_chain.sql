-- Event integrity v2. Existing rows remain version 1 and retain their
-- original independent HMAC interpretation. A keyed EventLog lazily creates
-- a signed v2 checkpoint before the next append (or explicitly through the
-- checkpoint backfill API), then chains all subsequent rows from that anchor.

ALTER TABLE state_events
    ADD COLUMN integrity_version INTEGER NOT NULL DEFAULT 1
    CHECK (integrity_version IN (1, 2));

ALTER TABLE state_events
    ADD COLUMN prev_tag BLOB;

CREATE TABLE state_event_integrity_heads (
    conversation_id TEXT PRIMARY KEY,
    integrity_version INTEGER NOT NULL CHECK (integrity_version = 2),
    chain_start_seq INTEGER NOT NULL,
    head_seq INTEGER NOT NULL,
    head_tag BLOB NOT NULL CHECK (length(head_tag) = 32),
    genesis_key_id INTEGER NOT NULL,
    key_id INTEGER NOT NULL,
    checkpoint_tag BLOB NOT NULL CHECK (length(checkpoint_tag) = 32),
    updated_at INTEGER NOT NULL
);

CREATE INDEX idx_state_events_integrity_version
    ON state_events(conversation_id, integrity_version, seq);