CREATE TABLE state_nexus_annotations (
    conversation_id TEXT NOT NULL REFERENCES state_conversations(conversation_id) ON DELETE CASCADE,
    seq INTEGER NOT NULL,
    branch_id TEXT,
    PRIMARY KEY (conversation_id, seq),
    FOREIGN KEY (conversation_id, seq) REFERENCES state_events(conversation_id, seq) ON DELETE CASCADE
);

CREATE TABLE state_nexus_links (
    conversation_id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    target_seq INTEGER NOT NULL,
    relation TEXT NOT NULL CHECK (relation IN ('replies_to', 'forwarded_from', 'mentions', 'generated_from')),
    PRIMARY KEY (conversation_id, seq, target_seq, relation),
    FOREIGN KEY (conversation_id, seq) REFERENCES state_events(conversation_id, seq) ON DELETE CASCADE,
    FOREIGN KEY (conversation_id, target_seq) REFERENCES state_events(conversation_id, seq) ON DELETE CASCADE
);

CREATE TABLE state_nexus_tags (
    conversation_id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    tag TEXT NOT NULL,
    PRIMARY KEY (conversation_id, seq, tag),
    FOREIGN KEY (conversation_id, seq) REFERENCES state_events(conversation_id, seq) ON DELETE CASCADE
);

CREATE TABLE config_nexus_views (
    conversation_id TEXT NOT NULL REFERENCES state_conversations(conversation_id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    filters_json TEXT NOT NULL,
    PRIMARY KEY (conversation_id, name)
);