-- Retained skill IDs remain as scrubbed audit shells so invocation foreign
-- keys stay valid. The hashed name fence prevents delayed imports from
-- restoring a deleted skill name.
CREATE TABLE state_skill_privacy_tombstones (
    skill_id INTEGER PRIMARY KEY REFERENCES state_skills(id),
    name_sha256 TEXT NOT NULL UNIQUE CHECK (length(name_sha256) = 64),
    requested_by TEXT NOT NULL,
    requested_at INTEGER NOT NULL
);

CREATE TRIGGER skill_privacy_tombstones_append_only_update
BEFORE UPDATE ON state_skill_privacy_tombstones BEGIN
    SELECT RAISE(ABORT, 'skill privacy tombstones are append-only');
END;

CREATE TRIGGER skill_privacy_tombstones_append_only_delete
BEFORE DELETE ON state_skill_privacy_tombstones BEGIN
    SELECT RAISE(ABORT, 'skill privacy tombstones are append-only');
END;
