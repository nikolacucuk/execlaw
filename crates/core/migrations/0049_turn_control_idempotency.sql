ALTER TABLE state_turn_controls ADD COLUMN client_request_id TEXT;
ALTER TABLE state_turn_controls ADD COLUMN request_body_sha256 TEXT;

CREATE UNIQUE INDEX idx_turn_controls_client_request
    ON state_turn_controls(conversation_id, client_request_id)
    WHERE client_request_id IS NOT NULL;
