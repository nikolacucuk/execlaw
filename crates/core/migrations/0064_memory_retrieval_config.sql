CREATE TABLE config_memory_retrieval (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    embedding_model_id TEXT NOT NULL CHECK (length(embedding_model_id) BETWEEN 1 AND 256),
    reranker_version TEXT NOT NULL DEFAULT 'local-hybrid-rrf-v1'
        CHECK (length(reranker_version) BETWEEN 1 AND 64),
    updated_at INTEGER NOT NULL
);
