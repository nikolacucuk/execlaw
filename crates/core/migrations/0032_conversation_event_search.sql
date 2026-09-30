-- 0032_conversation_event_search.sql
--
-- Derived FTS projection of event-log text. state_events remains authoritative;
-- the server rebuilds this index only from HMAC-verified replay records.

CREATE VIRTUAL TABLE state_conversation_event_search USING fts5(
    conversation_id UNINDEXED,
    seq UNINDEXED,
    source UNINDEXED,
    committed_at UNINDEXED,
    text,
    tokenize = 'unicode61'
);
