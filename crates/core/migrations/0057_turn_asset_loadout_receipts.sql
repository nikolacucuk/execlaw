CREATE TABLE state_turn_asset_loadouts (
    conversation_id TEXT NOT NULL REFERENCES state_conversations(conversation_id) ON DELETE CASCADE,
    input_event_seq INTEGER NOT NULL CHECK(input_event_seq > 0),
    receipt_json TEXT NOT NULL,
    resolved_at INTEGER NOT NULL,
    PRIMARY KEY(conversation_id, input_event_seq)
);

CREATE INDEX idx_turn_asset_loadouts_input
    ON state_turn_asset_loadouts(input_event_seq, conversation_id);
