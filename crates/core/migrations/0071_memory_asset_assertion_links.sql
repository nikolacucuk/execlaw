CREATE TABLE state_memory_asset_assertion_links (
    asset_id TEXT NOT NULL REFERENCES memory_assets(asset_id) ON DELETE CASCADE,
    assertion_id TEXT NOT NULL REFERENCES memory_assertions(assertion_id),
    linked_by TEXT NOT NULL,
    linked_at INTEGER NOT NULL,
    PRIMARY KEY (asset_id, assertion_id)
);

CREATE INDEX idx_memory_asset_assertion_links_assertion
    ON state_memory_asset_assertion_links(assertion_id, asset_id);
