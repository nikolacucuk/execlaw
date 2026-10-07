CREATE TABLE state_information_label_events (
    event_id TEXT PRIMARY KEY,
    subject_kind TEXT NOT NULL CHECK(length(subject_kind) BETWEEN 1 AND 64),
    subject_id TEXT NOT NULL CHECK(length(subject_id) BETWEEN 1 AND 256),
    content_sha256 TEXT NOT NULL CHECK(length(content_sha256) = 64),
    operation TEXT NOT NULL CHECK(operation IN ('observed', 'transformed', 'declassified')),
    information_label_json TEXT NOT NULL,
    actor_id TEXT NOT NULL,
    scope TEXT,
    created_at INTEGER NOT NULL
);

CREATE INDEX idx_information_label_subject
    ON state_information_label_events(subject_kind, subject_id, content_sha256, created_at, event_id);

CREATE TRIGGER information_label_events_append_only_update
BEFORE UPDATE ON state_information_label_events BEGIN
    SELECT RAISE(ABORT, 'information label events are append-only');
END;

CREATE TRIGGER information_label_events_append_only_delete
BEFORE DELETE ON state_information_label_events BEGIN
    SELECT RAISE(ABORT, 'information label events are append-only');
END;
