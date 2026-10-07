CREATE TABLE config_safety_profiles (
    profile_id TEXT PRIMARY KEY CHECK(profile_id IN (
        'inspect_only', 'workspace_edit', 'approved_integration'
    )),
    display_name TEXT NOT NULL,
    capability_set_json TEXT NOT NULL,
    approved_tools_json TEXT NOT NULL DEFAULT '[]',
    revision INTEGER NOT NULL CHECK(revision > 0),
    updated_by TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE config_safety_profile_revisions (
    profile_id TEXT NOT NULL REFERENCES config_safety_profiles(profile_id),
    revision INTEGER NOT NULL CHECK(revision > 0),
    capability_set_json TEXT NOT NULL,
    approved_tools_json TEXT NOT NULL,
    saved_by TEXT NOT NULL,
    saved_at INTEGER NOT NULL,
    PRIMARY KEY(profile_id, revision)
);

CREATE TABLE config_tool_access_policy_revisions (
    revision_id INTEGER PRIMARY KEY AUTOINCREMENT,
    tool_name TEXT NOT NULL,
    enabled INTEGER NOT NULL CHECK(enabled IN (0, 1)),
    allowed_classes_json TEXT NOT NULL,
    revised_by TEXT NOT NULL,
    revised_at INTEGER NOT NULL,
    rollback_of INTEGER REFERENCES config_tool_access_policy_revisions(revision_id)
);

CREATE INDEX idx_tool_policy_revisions_tool
    ON config_tool_access_policy_revisions(tool_name, revision_id DESC);

INSERT INTO config_tool_access_policy_revisions
    (tool_name, enabled, allowed_classes_json, revised_by, revised_at)
SELECT tool_name, enabled, allowed_classes, 'migration-0073', first_seen_at
FROM config_tool_access;

CREATE TRIGGER tool_access_initial_policy_revision
AFTER INSERT ON config_tool_access BEGIN
    INSERT INTO config_tool_access_policy_revisions
        (tool_name, enabled, allowed_classes_json, revised_by, revised_at)
    VALUES (NEW.tool_name, NEW.enabled, NEW.allowed_classes, 'registration', NEW.first_seen_at);
END;

CREATE TABLE state_tool_policy_decisions (
    decision_id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL,
    conversation_id TEXT NOT NULL,
    input_event_seq INTEGER NOT NULL CHECK(input_event_seq > 0),
    tool_name TEXT NOT NULL,
    caller_trust TEXT NOT NULL CHECK(caller_trust IN (
        'Controller', 'Delegated', 'KnownTrusted', 'KnownLimited',
        'UnknownPending', 'Blocked'
    )),
    trust_floor TEXT,
    required_capabilities_json TEXT NOT NULL,
    globally_enabled INTEGER NOT NULL CHECK(globally_enabled IN (0, 1)),
    allowed_classes_json TEXT NOT NULL,
    profile_id TEXT,
    profile_revision INTEGER,
    outcome TEXT NOT NULL CHECK(outcome IN ('allowed', 'denied', 'approval_gated')),
    reason_code TEXT NOT NULL,
    sensitive INTEGER NOT NULL CHECK(sensitive IN (0, 1)),
    external_effect INTEGER NOT NULL CHECK(external_effect IN (0, 1)),
    approval_required INTEGER NOT NULL CHECK(approval_required IN (0, 1)),
    policy_revision INTEGER NOT NULL CHECK(policy_revision >= 0),
    decided_at INTEGER NOT NULL,
    FOREIGN KEY(conversation_id, input_event_seq)
        REFERENCES state_events(conversation_id, seq) ON DELETE CASCADE
);

CREATE TRIGGER safety_profile_revisions_append_only_update
BEFORE UPDATE ON config_safety_profile_revisions BEGIN
    SELECT RAISE(ABORT, 'safety profile revisions are append-only');
END;

CREATE TRIGGER safety_profile_revisions_append_only_delete
BEFORE DELETE ON config_safety_profile_revisions BEGIN
    SELECT RAISE(ABORT, 'safety profile revisions are append-only');
END;

CREATE TRIGGER tool_policy_revisions_append_only_update
BEFORE UPDATE ON config_tool_access_policy_revisions BEGIN
    SELECT RAISE(ABORT, 'tool policy revisions are append-only');
END;

CREATE TRIGGER tool_policy_revisions_append_only_delete
BEFORE DELETE ON config_tool_access_policy_revisions BEGIN
    SELECT RAISE(ABORT, 'tool policy revisions are append-only');
END;

CREATE TRIGGER tool_policy_decisions_append_only_update
BEFORE UPDATE ON state_tool_policy_decisions BEGIN
    SELECT RAISE(ABORT, 'tool policy decisions are append-only');
END;

CREATE TRIGGER tool_policy_decisions_append_only_delete
BEFORE DELETE ON state_tool_policy_decisions BEGIN
    SELECT RAISE(ABORT, 'tool policy decisions are append-only');
END;

CREATE INDEX idx_tool_policy_decisions_run
    ON state_tool_policy_decisions(run_id, decided_at, decision_id);

INSERT INTO config_safety_profiles
    (profile_id, display_name, capability_set_json, approved_tools_json,
     revision, updated_by, updated_at)
VALUES
    ('inspect_only', 'Inspect only', '["data.read","workspace.read"]', '[]', 1, 'system', 0),
    ('workspace_edit', 'Workspace edit',
     '["data.read","workspace.read","workspace.write","workspace.process"]', '[]', 1, 'system', 0),
    ('approved_integration', 'Approved integrations',
     '["data.read","workspace.read","integration.approved","network.approved","secret.brokered","destination.approved"]', '[]', 1, 'system', 0);

INSERT INTO config_safety_profile_revisions
    (profile_id, revision, capability_set_json, approved_tools_json,
     saved_by, saved_at)
SELECT profile_id, revision, capability_set_json, approved_tools_json,
       updated_by, updated_at
FROM config_safety_profiles;
