-- Memory-source tombstones hide append-only assertions/evidence when the
-- Controller removes a source event and fence extraction workers that finish
-- after the deletion transaction.
CREATE TABLE state_memory_privacy_tombstones (
    target_kind TEXT NOT NULL CHECK (target_kind IN ('source_event', 'assertion', 'evidence')),
    target_id TEXT NOT NULL,
    assertion_id TEXT,
    source_conversation_id TEXT NOT NULL,
    source_event_seq INTEGER NOT NULL CHECK (source_event_seq > 0),
    request_id TEXT NOT NULL,
    requested_by TEXT NOT NULL,
    requested_at INTEGER NOT NULL,
    PRIMARY KEY (target_kind, target_id)
);

CREATE INDEX idx_memory_privacy_tombstones_assertion
    ON state_memory_privacy_tombstones(assertion_id, target_kind)
    WHERE assertion_id IS NOT NULL;

CREATE INDEX idx_memory_privacy_tombstones_source
    ON state_memory_privacy_tombstones(source_conversation_id, source_event_seq, target_kind);

CREATE TRIGGER memory_privacy_tombstones_append_only_update
BEFORE UPDATE ON state_memory_privacy_tombstones BEGIN
    SELECT RAISE(ABORT, 'memory privacy tombstones are append-only');
END;

CREATE TRIGGER memory_privacy_tombstones_append_only_delete
BEFORE DELETE ON state_memory_privacy_tombstones BEGIN
    SELECT RAISE(ABORT, 'memory privacy tombstones are append-only');
END;

CREATE TRIGGER memory_projection_reject_privacy_tombstone_insert
BEFORE INSERT ON memory_current_projection
WHEN EXISTS (
    SELECT 1 FROM state_memory_privacy_tombstones
    WHERE target_kind = 'assertion' AND target_id = NEW.assertion_id
)
BEGIN
    SELECT RAISE(ABORT, 'privacy-deleted memory assertion cannot be projected');
END;

CREATE TRIGGER memory_projection_reject_privacy_tombstone_update
BEFORE UPDATE OF assertion_id ON memory_current_projection
WHEN EXISTS (
    SELECT 1 FROM state_memory_privacy_tombstones
    WHERE target_kind = 'assertion' AND target_id = NEW.assertion_id
)
BEGIN
    SELECT RAISE(ABORT, 'privacy-deleted memory assertion cannot be projected');
END;
