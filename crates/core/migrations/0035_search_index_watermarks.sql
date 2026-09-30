-- Incremental cursor for the rebuildable conversation search projection.
-- The event log remains authoritative; a mismatch causes a full rebuild.
CREATE TABLE state_conversation_event_search_state (
    conversation_id TEXT PRIMARY KEY,
    indexed_seq INTEGER NOT NULL DEFAULT 0,
    updated_at INTEGER NOT NULL DEFAULT 0
);

