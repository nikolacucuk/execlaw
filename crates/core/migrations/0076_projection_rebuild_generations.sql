-- Durable control plane for bounded projection rebuilds. Rebuild output stays
-- hidden until a validated generation is atomically selected as active.
CREATE TABLE state_projection_generations (
    projection_name TEXT NOT NULL,
    generation INTEGER NOT NULL CHECK (generation > 0),
    projection_version TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('building', 'validated', 'active', 'retired', 'failed')),
    cursor BLOB,
    source_watermark INTEGER NOT NULL DEFAULT 0 CHECK (source_watermark >= 0),
    rows_written INTEGER NOT NULL DEFAULT 0 CHECK (rows_written >= 0),
    started_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    error TEXT,
    PRIMARY KEY (projection_name, generation)
);

CREATE TABLE state_projection_activation (
    projection_name TEXT PRIMARY KEY,
    generation INTEGER NOT NULL,
    activated_at INTEGER NOT NULL,
    FOREIGN KEY (projection_name, generation)
        REFERENCES state_projection_generations(projection_name, generation)
);
