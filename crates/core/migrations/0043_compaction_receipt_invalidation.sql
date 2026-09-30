ALTER TABLE state_compaction_receipts ADD COLUMN invalidated_at INTEGER;
CREATE INDEX idx_compaction_receipts_active
    ON state_compaction_receipts(conversation_id, source_fingerprint, invalidated_at);
