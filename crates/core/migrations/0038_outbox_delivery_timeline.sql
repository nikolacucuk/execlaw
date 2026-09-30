ALTER TABLE state_outbox ADD COLUMN lease_owner TEXT;
ALTER TABLE state_outbox ADD COLUMN lease_expires_at INTEGER;

CREATE TABLE state_outbox_delivery_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    outbox_id INTEGER NOT NULL REFERENCES state_outbox(id),
    transition TEXT NOT NULL,
    occurred_at INTEGER NOT NULL,
    attempt INTEGER NOT NULL,
    detail TEXT,
    external_receipt TEXT
);

CREATE INDEX idx_outbox_delivery_events_outbox
    ON state_outbox_delivery_events(outbox_id, id);

CREATE TRIGGER state_outbox_delivery_events_no_update
BEFORE UPDATE ON state_outbox_delivery_events
BEGIN
    SELECT RAISE(ABORT, 'outbox delivery events are append-only');
END;

CREATE TRIGGER state_outbox_delivery_events_no_delete
BEFORE DELETE ON state_outbox_delivery_events
BEGIN
    SELECT RAISE(ABORT, 'outbox delivery events are append-only');
END;
