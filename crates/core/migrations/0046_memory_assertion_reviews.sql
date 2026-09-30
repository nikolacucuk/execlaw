CREATE TABLE state_memory_assertion_reviews (
    review_id       TEXT PRIMARY KEY,
    assertion_id    TEXT NOT NULL REFERENCES memory_assertions(assertion_id),
    conversation_id TEXT NOT NULL,
    event_seq       INTEGER NOT NULL,
    reviewer_id     TEXT NOT NULL,
    decision        TEXT NOT NULL CHECK(decision IN ('retracted', 'corrected')),
    reason          TEXT NOT NULL CHECK(length(reason) BETWEEN 1 AND 2000),
    created_at      INTEGER NOT NULL,
    FOREIGN KEY(conversation_id, event_seq)
        REFERENCES state_events(conversation_id, seq),
    UNIQUE(conversation_id, event_seq)
);

CREATE INDEX idx_memory_assertion_reviews_target
    ON state_memory_assertion_reviews(assertion_id, decision, created_at);

CREATE TRIGGER memory_assertion_reviews_append_only_update
BEFORE UPDATE ON state_memory_assertion_reviews BEGIN
    SELECT RAISE(ABORT, 'memory assertion reviews are append-only');
END;
CREATE TRIGGER memory_assertion_reviews_append_only_delete
BEFORE DELETE ON state_memory_assertion_reviews BEGIN
    SELECT RAISE(ABORT, 'memory assertion reviews are append-only');
END;

CREATE TRIGGER memory_projection_reject_retracted_insert
BEFORE INSERT ON memory_current_projection
WHEN EXISTS (
    WITH RECURSIVE lineage(assertion_id) AS (
        SELECT assertion_id FROM state_memory_assertion_reviews WHERE decision = 'retracted'
        UNION
        SELECT a.supersedes_id FROM memory_assertions a
        JOIN lineage l ON a.assertion_id = l.assertion_id
        WHERE a.supersedes_id IS NOT NULL
    )
    SELECT 1 FROM lineage WHERE assertion_id = NEW.assertion_id
)
BEGIN
    SELECT RAISE(ABORT, 'retracted memory assertion cannot be projected');
END;

CREATE TRIGGER memory_projection_reject_retracted_update
BEFORE UPDATE OF assertion_id ON memory_current_projection
WHEN EXISTS (
    WITH RECURSIVE lineage(assertion_id) AS (
        SELECT assertion_id FROM state_memory_assertion_reviews WHERE decision = 'retracted'
        UNION
        SELECT a.supersedes_id FROM memory_assertions a
        JOIN lineage l ON a.assertion_id = l.assertion_id
        WHERE a.supersedes_id IS NOT NULL
    )
    SELECT 1 FROM lineage WHERE assertion_id = NEW.assertion_id
)
BEGIN
    SELECT RAISE(ABORT, 'retracted memory assertion cannot be projected');
END;
