CREATE TABLE state_turn_controls (
    control_id TEXT PRIMARY KEY,
    conversation_id TEXT NOT NULL,
    turn_id TEXT,
    kind TEXT NOT NULL CHECK (kind IN ('queue_next_turn','steer','pause','resume','cancel')),
    payload_json TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'accepted' CHECK (status IN ('accepted','delivered','applied','acknowledged','cancelled','failed')),
    acknowledgement_json TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    acknowledged_at INTEGER
);

CREATE INDEX idx_turn_controls_pending ON state_turn_controls(conversation_id,kind,status,created_at);
CREATE TABLE state_turn_control_transitions (
    control_id TEXT NOT NULL REFERENCES state_turn_controls(control_id) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('accepted','delivered','applied','acknowledged','cancelled','failed')),
    detail_json TEXT,
    occurred_at INTEGER NOT NULL,
    PRIMARY KEY(control_id,ordinal)
);
