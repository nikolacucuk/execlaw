-- Tencent-inspired memory assets, agent loadouts, and local knowledge indexes.
-- Content remains in existing stores; these tables hold governed metadata and
-- derived indexes so SQLite remains the source of truth.

CREATE TABLE memory_assets (
    asset_id TEXT PRIMARY KEY,
    asset_type TEXT NOT NULL CHECK(asset_type IN ('memory', 'skill', 'wiki', 'code_graph', 'research')),
    name TEXT NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    owner_scope TEXT NOT NULL,
    visibility TEXT NOT NULL DEFAULT 'private'
        CHECK(visibility IN ('private', 'team', 'restricted', 'agent')),
    trust_floor TEXT NOT NULL DEFAULT 'Controller',
    status TEXT NOT NULL DEFAULT 'active'
        CHECK(status IN ('active', 'draft', 'processing', 'ready', 'failed', 'archived')),
    version INTEGER NOT NULL DEFAULT 1 CHECK(version > 0),
    source_ref TEXT,
    content_ref TEXT,
    source_hash TEXT,
    expires_at INTEGER,
    last_used_at INTEGER,
    usage_count INTEGER NOT NULL DEFAULT 0 CHECK(usage_count >= 0),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE INDEX idx_memory_assets_scope ON memory_assets(owner_scope, asset_type, status);
CREATE INDEX idx_memory_assets_expiry ON memory_assets(expires_at) WHERE expires_at IS NOT NULL;

CREATE TABLE memory_asset_bindings (
    asset_id TEXT NOT NULL,
    agent_scope TEXT NOT NULL,
    injection_mode TEXT NOT NULL DEFAULT 'tool_only'
        CHECK(injection_mode IN ('hot', 'discoverable', 'tool_only')),
    priority INTEGER NOT NULL DEFAULT 0,
    max_chars INTEGER NOT NULL DEFAULT 0 CHECK(max_chars >= 0),
    created_at INTEGER NOT NULL,
    PRIMARY KEY(asset_id, agent_scope),
    FOREIGN KEY(asset_id) REFERENCES memory_assets(asset_id) ON DELETE CASCADE
);

CREATE INDEX idx_memory_asset_bindings_agent
    ON memory_asset_bindings(agent_scope, priority DESC);

CREATE VIRTUAL TABLE memory_asset_search USING fts5(
    asset_id UNINDEXED,
    name,
    description,
    searchable_text,
    tokenize = 'unicode61'
);

CREATE TRIGGER memory_assets_search_insert AFTER INSERT ON memory_assets BEGIN
    INSERT INTO memory_asset_search(asset_id, name, description, searchable_text)
    VALUES (new.asset_id, new.name, new.description, COALESCE(new.source_ref, '') || ' ' || COALESCE(new.content_ref, ''));
END;

CREATE TRIGGER memory_assets_search_update AFTER UPDATE OF name, description, source_ref, content_ref ON memory_assets BEGIN
    DELETE FROM memory_asset_search WHERE asset_id = old.asset_id;
    INSERT INTO memory_asset_search(asset_id, name, description, searchable_text)
    VALUES (new.asset_id, new.name, new.description, COALESCE(new.source_ref, '') || ' ' || COALESCE(new.content_ref, ''));
END;

CREATE TRIGGER memory_assets_search_delete AFTER DELETE ON memory_assets BEGIN
    DELETE FROM memory_asset_search WHERE asset_id = old.asset_id;
END;

CREATE TABLE memory_asset_embeddings (
    asset_id TEXT NOT NULL,
    model_id TEXT NOT NULL,
    dimensions INTEGER NOT NULL CHECK(dimensions > 0),
    vector_json TEXT NOT NULL CHECK(json_valid(vector_json)),
    source_hash TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY(asset_id, model_id),
    FOREIGN KEY(asset_id) REFERENCES memory_assets(asset_id) ON DELETE CASCADE
);

CREATE TABLE knowledge_wikis (
    wiki_id TEXT PRIMARY KEY,
    asset_id TEXT NOT NULL UNIQUE,
    root_path TEXT NOT NULL,
    revision TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending'
        CHECK(status IN ('pending', 'processing', 'ready', 'failed')),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    FOREIGN KEY(asset_id) REFERENCES memory_assets(asset_id) ON DELETE CASCADE
);

CREATE TABLE knowledge_wiki_pages (
    wiki_id TEXT NOT NULL,
    page_ref TEXT NOT NULL,
    title TEXT NOT NULL,
    body TEXT NOT NULL,
    source_path TEXT NOT NULL,
    source_hash TEXT NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY(wiki_id, page_ref),
    FOREIGN KEY(wiki_id) REFERENCES knowledge_wikis(wiki_id) ON DELETE CASCADE
);

CREATE VIRTUAL TABLE knowledge_wiki_search USING fts5(
    wiki_id UNINDEXED,
    page_ref UNINDEXED,
    title,
    body,
    tokenize = 'unicode61'
);

CREATE TABLE knowledge_code_graphs (
    graph_id TEXT PRIMARY KEY,
    asset_id TEXT NOT NULL UNIQUE,
    root_path TEXT NOT NULL,
    revision TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending'
        CHECK(status IN ('pending', 'processing', 'ready', 'failed')),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    FOREIGN KEY(asset_id) REFERENCES memory_assets(asset_id) ON DELETE CASCADE
);

CREATE TABLE knowledge_code_nodes (
    graph_id TEXT NOT NULL,
    symbol TEXT NOT NULL,
    kind TEXT NOT NULL,
    file_path TEXT NOT NULL,
    start_line INTEGER NOT NULL,
    end_line INTEGER NOT NULL,
    source TEXT,
    PRIMARY KEY(graph_id, symbol, file_path, start_line),
    FOREIGN KEY(graph_id) REFERENCES knowledge_code_graphs(graph_id) ON DELETE CASCADE
);

CREATE TABLE knowledge_code_edges (
    graph_id TEXT NOT NULL,
    caller TEXT NOT NULL,
    callee TEXT NOT NULL,
    kind TEXT NOT NULL DEFAULT 'calls',
    PRIMARY KEY(graph_id, caller, callee, kind),
    FOREIGN KEY(graph_id) REFERENCES knowledge_code_graphs(graph_id) ON DELETE CASCADE
);

CREATE INDEX idx_code_nodes_symbol ON knowledge_code_nodes(graph_id, symbol);
CREATE INDEX idx_code_edges_caller ON knowledge_code_edges(graph_id, caller);
CREATE INDEX idx_code_edges_callee ON knowledge_code_edges(graph_id, callee);
