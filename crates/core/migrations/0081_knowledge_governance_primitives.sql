CREATE TABLE qualified_read_cache (
    cache_key TEXT PRIMARY KEY,
    conversation_id TEXT NOT NULL,
    authority_scope TEXT NOT NULL,
    tool_name TEXT NOT NULL,
    tool_version TEXT NOT NULL,
    canonical_args_hash TEXT NOT NULL,
    source_revision TEXT NOT NULL,
    result_json TEXT NOT NULL,
    provenance_json TEXT NOT NULL,
    expires_at INTEGER NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE INDEX qualified_read_cache_scope_idx
    ON qualified_read_cache(conversation_id, authority_scope, expires_at);

CREATE TABLE knowledge_entities (
    entity_id TEXT PRIMARY KEY,
    entity_kind TEXT NOT NULL,
    canonical_label TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    retired_at INTEGER
);
CREATE TABLE knowledge_entity_aliases (
    alias_id TEXT PRIMARY KEY,
    entity_id TEXT NOT NULL REFERENCES knowledge_entities(entity_id),
    label TEXT NOT NULL,
    valid_from INTEGER NOT NULL,
    valid_to INTEGER,
    evidence_ref TEXT NOT NULL,
    authoritative INTEGER NOT NULL DEFAULT 0 CHECK(authoritative IN (0,1))
);
CREATE INDEX knowledge_entity_alias_label_idx
    ON knowledge_entity_aliases(label, valid_from, valid_to);
CREATE TABLE knowledge_entity_merge_proposals (
    proposal_id TEXT PRIMARY KEY,
    left_entity_id TEXT NOT NULL REFERENCES knowledge_entities(entity_id),
    right_entity_id TEXT NOT NULL REFERENCES knowledge_entities(entity_id),
    status TEXT NOT NULL CHECK(status IN ('proposed','accepted','rejected','reversed')),
    evidence_ref TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    decided_at INTEGER,
    CHECK(left_entity_id <> right_entity_id)
);
CREATE TABLE knowledge_entity_redirects (
    source_entity_id TEXT PRIMARY KEY REFERENCES knowledge_entities(entity_id),
    target_entity_id TEXT NOT NULL REFERENCES knowledge_entities(entity_id),
    proposal_id TEXT NOT NULL REFERENCES knowledge_entity_merge_proposals(proposal_id),
    CHECK(source_entity_id <> target_entity_id)
);

CREATE TABLE operator_preferences (
    preference_id TEXT PRIMARY KEY,
    owner_principal_id TEXT NOT NULL,
    scope TEXT NOT NULL,
    preference_key TEXT NOT NULL,
    value_json TEXT NOT NULL,
    origin TEXT NOT NULL CHECK(origin IN ('explicit','inferred')),
    status TEXT NOT NULL CHECK(status IN ('proposed','approved','rejected','retracted')),
    evidence_ref TEXT NOT NULL,
    expires_at INTEGER,
    updated_at INTEGER NOT NULL
);
CREATE INDEX operator_preferences_loadout_idx
    ON operator_preferences(owner_principal_id, scope, status, expires_at);
CREATE UNIQUE INDEX operator_preferences_identity_idx
    ON operator_preferences(owner_principal_id, scope, preference_key);
