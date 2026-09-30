ALTER TABLE state_bus_events ADD COLUMN completed_at INTEGER;
ALTER TABLE state_bus_events ADD COLUMN dispatch_lease_owner TEXT;
ALTER TABLE state_bus_events ADD COLUMN dispatch_lease_expires_at INTEGER;

UPDATE state_bus_events SET completed_at = dispatched_at WHERE dispatched_at IS NOT NULL;

CREATE INDEX idx_bus_events_recoverable
    ON state_bus_events(internal, received_at, dispatch_lease_expires_at)
    WHERE completed_at IS NULL;
