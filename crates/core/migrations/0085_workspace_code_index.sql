CREATE TABLE state_workspace_code_indexes (
    workspace_id TEXT PRIMARY KEY REFERENCES state_workspace_roots(workspace_id) ON DELETE CASCADE,
    revision TEXT NOT NULL,
    files_indexed INTEGER NOT NULL DEFAULT 0,
    files_excluded INTEGER NOT NULL DEFAULT 0,
    files_unsupported INTEGER NOT NULL DEFAULT 0,
    updated_at INTEGER NOT NULL
);

CREATE TABLE state_workspace_code_files (
    workspace_id TEXT NOT NULL REFERENCES state_workspace_roots(workspace_id) ON DELETE CASCADE,
    path TEXT NOT NULL,
    source_hash TEXT NOT NULL,
    PRIMARY KEY(workspace_id, path)
);

CREATE TABLE state_workspace_code_symbols (
    workspace_id TEXT NOT NULL REFERENCES state_workspace_roots(workspace_id) ON DELETE CASCADE,
    path TEXT NOT NULL,
    symbol TEXT NOT NULL,
    kind TEXT NOT NULL,
    start_line INTEGER NOT NULL,
    end_line INTEGER NOT NULL,
    source_hash TEXT NOT NULL,
    PRIMARY KEY(workspace_id, path, symbol, start_line)
);

CREATE INDEX idx_workspace_code_symbols_name
    ON state_workspace_code_symbols(workspace_id, symbol);
