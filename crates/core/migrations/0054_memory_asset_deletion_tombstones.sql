-- Durable tombstones prevent deleted governed assets from being recreated
-- by a delayed or replayed projection writer.
CREATE TABLE state_memory_asset_deletion_tombstones (
    asset_id TEXT PRIMARY KEY,
    requested_by TEXT NOT NULL,
    requested_at INTEGER NOT NULL,
    completed_at INTEGER NOT NULL
);
