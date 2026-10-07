ALTER TABLE knowledge_entity_merge_proposals ADD COLUMN decided_by TEXT;
ALTER TABLE knowledge_entity_merge_proposals ADD COLUMN proposed_by TEXT;
CREATE TABLE knowledge_entity_merge_decisions (
    decision_id TEXT PRIMARY KEY,
    proposal_id TEXT NOT NULL REFERENCES knowledge_entity_merge_proposals(proposal_id),
    actor_id TEXT NOT NULL,
    decision TEXT NOT NULL CHECK(decision IN ('accepted','rejected','reversed')),
    reason TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE TRIGGER knowledge_entity_merge_decisions_no_update
BEFORE UPDATE ON knowledge_entity_merge_decisions
BEGIN
    SELECT RAISE(ABORT, 'entity merge decisions are append-only');
END;
CREATE TRIGGER knowledge_entity_merge_decisions_no_delete
BEFORE DELETE ON knowledge_entity_merge_decisions
BEGIN
    SELECT RAISE(ABORT, 'entity merge decisions are append-only');
END;
